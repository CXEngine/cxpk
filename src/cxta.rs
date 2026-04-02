use std::collections::HashMap;
use std::fs;
use std::io::{self, Write};
use std::path::Path;

use image::io::Reader as ImageReader;
use image::{GenericImage, RgbaImage, GenericImageView};
use png;
use rayon::prelude::*;
use serde::Deserialize;
use xxhash_rust::xxh64::xxh64;

use crate::driver::AssetDriver;

pub const CXTA_MAGIC: &[u8; 4] = b"CXTA";
const MAX_ATLAS_DIM: u32 = 4096;

pub struct CxtaDriver;

impl AssetDriver for CxtaDriver {
    fn magic(&self) -> &[u8; 4] {
        CXTA_MAGIC
    }

    fn extension(&self) -> &str { ".cxta" }
    fn entry_file(&self) -> &str { "atlas.entry" }

    fn pack(&self, folder: &Path) -> io::Result<Vec<u8>> {
        let config = load_atlas_config(folder)?;

        let images_to_load: Vec<_> = config.images.into_iter().collect();

        let loaded_images: Vec<io::Result<(String, RgbaImage)>> = images_to_load
            .par_iter()
            .map(|entry| {
                let path = folder.join(&entry.file);
                let img = ImageReader::open(&path)
                    .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("Failed to open image {:?}: {}", path, e)))?
                    .decode()
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("Failed to decode image {:?}: {}", path, e)))?
                    .into_rgba8();
                Ok((entry.name.clone(), img))
            })
            .collect();

        let mut successful_images = Vec::new();
        for result in loaded_images {
            successful_images.push(result?);
        }
    
        if successful_images.is_empty() {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "No images found for atlas"));
        }
    
        // Sort by name for deterministic packing order
        successful_images.sort_by(|a, b| a.0.cmp(&b.0));

        let (first_w, first_h) = successful_images[0].1.dimensions();
        for (name, img) in &successful_images {
            if img.dimensions() != (first_w, first_h) {
                return Err(io::Error::new(io::ErrorKind::InvalidData, format!("Image '{}' has different dimensions ({}x{}) than the first one ({}x{})", name, img.width(), img.height(), first_w, first_h)));
            }
        }

        let (img_w, img_h) = (first_w, first_h);
        let num_images = successful_images.len();
        let cols = (MAX_ATLAS_DIM / img_w).min(num_images as u32);
        let rows = (num_images as u32 + cols - 1) / cols;

        let atlas_w = cols * img_w;
        let atlas_h = rows * img_h;
    
        if atlas_w > MAX_ATLAS_DIM || atlas_h > MAX_ATLAS_DIM {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "Atlas dimensions exceed MAX_ATLAS_DIM"));
        }

        let mut atlas_image = RgbaImage::new(atlas_w, atlas_h);
        let mut rect_map = HashMap::new();

        for (i, (name, img)) in successful_images.iter().enumerate() {
            let col = (i as u32) % cols;
            let row = (i as u32) / cols;
            let x = col * img_w;
            let y = row * img_h;

            atlas_image.copy_from(img, x, y).unwrap();
            rect_map.insert(name.clone(), (x, y, img_w, img_h));
        }
    
        // Encode atlas to PNG
        let mut png_data = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut png_data, atlas_image.width(), atlas_image.height());
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().map_err(io::Error::other)?;
            writer.write_image_data(&atlas_image)?;
        }
    
        let mut out = Vec::new();
        out.extend_from_slice(CXTA_MAGIC);
        out.extend_from_slice(&(rect_map.len() as u32).to_le_bytes());
        out.extend_from_slice(&(png_data.len() as u32).to_le_bytes());
        out.extend_from_slice(&png_data);
    
        let mut sorted_names: Vec<_> = rect_map.keys().cloned().collect();
        sorted_names.sort();
    
        for name in sorted_names {
            let (x, y, w, h) = rect_map[&name];
            
            // Use a fixed seed for reproducibility. 0 is a common choice.
            let hash = xxh64(name.as_bytes(), 0); 
            out.write_all(&hash.to_le_bytes())?;
            
            out.write_all(&(x as u16).to_le_bytes())?;
            out.write_all(&(y as u16).to_le_bytes())?;
            out.write_all(&(w as u16).to_le_bytes())?;
            out.write_all(&(h as u16).to_le_bytes())?;
        }
    
        Ok(out)    }

    fn unpack(&self, data: &[u8], folder: &Path) -> io::Result<()> {
        if data.len() < 12 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "CXTA data too short"));
        }
        if &data[0..4] != CXTA_MAGIC {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "Invalid CXTA magic"));
        }

        let item_count = u32::from_le_bytes(data[4..8].try_into().unwrap()) as usize;
        let png_len = u32::from_le_bytes(data[8..12].try_into().unwrap()) as usize;
        
        let mut offset = 12;
        if offset + png_len > data.len() {
             return Err(io::Error::new(io::ErrorKind::InvalidData, "Unexpected end of CXTA PNG data"));
        }
        let png_data = &data[offset..offset + png_len];
        offset += png_len;

        let atlas = ImageReader::new(io::Cursor::new(png_data))
            .with_guessed_format()?
            .decode()
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?
            .into_rgba8();

        fs::create_dir_all(folder)?;
        let mut images_toml = Vec::new();

        for _ in 0..item_count {
            if offset + 16 > data.len() {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "Unexpected end of CXTA item data"));
            }
            let hash = u64::from_le_bytes(data[offset..offset+8].try_into().unwrap());
            let x = u16::from_le_bytes(data[offset+8..offset+10].try_into().unwrap()) as u32;
            let y = u16::from_le_bytes(data[offset+10..offset+12].try_into().unwrap()) as u32;
            let w = u16::from_le_bytes(data[offset+12..offset+14].try_into().unwrap()) as u32;
            let h = u16::from_le_bytes(data[offset+14..offset+16].try_into().unwrap()) as u32;
            offset += 16;

            let img = atlas.view(x, y, w, h).to_image();
            let name = format!("{:016x}", hash);
            let file_name = format!("{}.png", name);
            img.save(folder.join(&file_name)).map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;

            images_toml.push(format!("{{ name = \"{}\", file = \"{}\" }}", name, file_name));
        }

        let entry_content = format!("[[images]]\n{}", images_toml.join("\n[[images]]\n"));
        fs::write(folder.join("atlas.entry"), entry_content)?;

        Ok(())
    }
}

#[derive(Deserialize, Debug)]
struct AtlasImageEntry {
    name: String,
    file: String,
}

#[derive(Deserialize, Debug)]
struct AtlasConfig {
    images: Vec<AtlasImageEntry>,
}

fn load_atlas_config(folder: &Path) -> io::Result<AtlasConfig> {
    let path = folder.join("atlas.entry");
    let text = fs::read_to_string(path)?;
    toml::from_str(&text).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}
