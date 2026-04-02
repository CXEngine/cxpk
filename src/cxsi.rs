use std::fs;
use std::io;
use std::path::Path;

use image::io::Reader as ImageReader;
use png;
use toml::Value;

use crate::driver::AssetDriver;

pub const CXSI_MAGIC: &[u8; 4] = b"CXSI";

pub struct CxsiDriver;

impl AssetDriver for CxsiDriver {
    fn magic(&self) -> &[u8; 4] { CXSI_MAGIC }
    fn extension(&self) -> &str { ".cxsi" }
    fn entry_file(&self) -> &str { "simage.entry" }

    fn pack(&self, folder: &Path) -> io::Result<Vec<u8>> {
        let entry_path = folder.join("simage.entry");
        let text = fs::read_to_string(&entry_path)?;
        let value: Value =
            toml::from_str(&text).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

        let variants = value
            .get("variants")
            .and_then(|v| v.as_array())
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "variants missing"))?;

        if variants.len() > (u16::MAX as usize) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Too many variants (max 65535)",
            ));
        }

        // Prepare header
        let mut out = Vec::new();
        out.extend_from_slice(CXSI_MAGIC);
        out.extend_from_slice(&(variants.len() as u16).to_le_bytes());

        // Process each variant sequentially. Packing tasks are distributed across
        // worker threads by the caller, so we avoid nested parallelism here.
        let mut parts: Vec<(u16, u16, Vec<u8>)> = Vec::with_capacity(variants.len());
        for v in variants {
            let file = v
                .get("file")
                .and_then(|s| s.as_str())
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "variant.file missing"))?;

            // Load and decode
            let img = ImageReader::open(folder.join(file))
                .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?
                .decode()
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?
                .into_rgba8();

            let w = img.width();
            let h = img.height();

            // Encode to PNG (RGBA 8-bit)
            let mut png_data = Vec::new();
            {
                let mut encoder = png::Encoder::new(&mut png_data, w, h);
                encoder.set_color(png::ColorType::Rgba);
                encoder.set_depth(png::BitDepth::Eight);
                let mut writer = encoder.write_header().map_err(io::Error::other)?;
                writer.write_image_data(&img)?;
            }

            parts.push((w as u16, h as u16, png_data));
        }

        // Append variant entries in the original order
        for (w, h, png) in parts {
            out.extend_from_slice(&w.to_le_bytes());
            out.extend_from_slice(&h.to_le_bytes());
            out.extend_from_slice(&(png.len() as u32).to_le_bytes());
            out.extend_from_slice(&png);
        }

        Ok(out)
    }

    fn unpack(&self, data: &[u8], folder: &Path) -> io::Result<()> {
        if data.len() < 6 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "CXSI data too short"));
        }
        if &data[0..4] != CXSI_MAGIC {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "Invalid CXSI magic"));
        }

        let variant_count = u16::from_le_bytes(data[4..6].try_into().unwrap()) as usize;
        let mut offset = 6;

        fs::create_dir_all(folder)?;
        let mut variants_toml = Vec::new();

        for i in 0..variant_count {
            if offset + 8 > data.len() {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "Unexpected end of CXSI data"));
            }
            let _w = u16::from_le_bytes(data[offset..offset+2].try_into().unwrap());
            let _h = u16::from_le_bytes(data[offset+2..offset+4].try_into().unwrap());
            let png_len = u32::from_le_bytes(data[offset+4..offset+8].try_into().unwrap()) as usize;
            offset += 8;

            if offset + png_len > data.len() {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "Unexpected end of CXSI PNG data"));
            }
            let png_data = &data[offset..offset + png_len];
            offset += png_len;

            let mut reader = ImageReader::new(io::Cursor::new(png_data))
                .with_guessed_format()?;
            reader.no_limits();

            let _img = reader.decode()
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?
                .into_rgba8();

            let file_name = format!("variant_{:03}.png", i);
            fs::write(folder.join(&file_name), png_data)?;
            variants_toml.push(format!("{{ file = \"{}\" }}", file_name));
        }

        let entry_content = format!("variants = [\n  {}\n]\n", variants_toml.join(",\n  "));
        fs::write(folder.join("simage.entry"), entry_content)?;

        Ok(())
    }
}

