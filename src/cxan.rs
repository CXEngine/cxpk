use std::fs;
use std::io;
use std::path::Path;

use image::io::Reader as ImageReader;
use image::{GenericImage, RgbaImage, GenericImageView};
use png;
use rayon::prelude::*;
use toml::Value;

use crate::driver::AssetDriver;

pub const CXAN_MAGIC: &[u8; 4] = b"CXAN";
pub const CXAP_MAGIC: &[u8; 4] = b"CXAP";

const MAX_TEX: u32 = 16384;

pub struct CxanDriver;

impl AssetDriver for CxanDriver {
    fn magic(&self) -> &[u8; 4] {
        CXAN_MAGIC
    }

    fn extension(&self) -> &str {
        ".cxan"
    }

    fn entry_file(&self) -> &str {
        "animation.entry"
    }

    fn pack(&self, folder: &Path) -> io::Result<Vec<u8>> {
        let config = load_animation_config(folder)?;
        let frames = load_png_frames(folder)?;

        let (fw, fh) = frames[0].dimensions();
        let frame_count = frames.len();
        if frame_count == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "No frames to pack",
            ));
        }
        if frame_count > (u16::MAX as usize) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Too many frames for CXAN (max 65535)",
            ));
        }
        let frame_count_u16 = frame_count as u16;

        let max_cols = (MAX_TEX / fw) as usize;
        let max_rows = (MAX_TEX / fh) as usize;
        if max_cols == 0 || max_rows == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Frame is larger than MAX_TEX",
            ));
        }
        let cells_per_page = max_cols * max_rows;

        // Build atlas pages and frame map
        let mut pages: Vec<Vec<u8>> = Vec::new();
        let mut frame_map: Vec<(u16, u16)> = Vec::with_capacity(frame_count);

        for (page_idx, chunk) in frames.chunks(cells_per_page).enumerate() {
            let cols = max_cols.min(chunk.len());
            let rows = (chunk.len() + cols - 1) / cols;

            let mut atlas = RgbaImage::new(cols as u32 * fw, rows as u32 * fh);
            for (i, img) in chunk.iter().enumerate() {
                let x = (i % cols) as u32 * fw;
                let y = (i / cols) as u32 * fh;
                // copy_from is faster than per-pixel writes
                atlas
                    .copy_from(img, x, y)
                    .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
                frame_map.push((page_idx as u16, i as u16));
            }

            // Encode atlas as PNG
            let mut png_data = Vec::new();
            {
                let mut encoder = png::Encoder::new(&mut png_data, atlas.width(), atlas.height());
                encoder.set_color(png::ColorType::Rgba);
                encoder.set_depth(png::BitDepth::Eight);
                let mut writer = encoder.write_header().map_err(io::Error::other)?;
                writer.write_image_data(&atlas)?;
            }

            // Build CXAP chunk: magic + cols u16 + rows u16 + fw u32 + fh u32 + png_len u32 + png bytes
            let mut cxap = Vec::with_capacity(32 + png_data.len());
            cxap.extend_from_slice(CXAP_MAGIC);
            cxap.extend_from_slice(&(cols as u16).to_le_bytes());
            cxap.extend_from_slice(&(rows as u16).to_le_bytes());
            cxap.extend_from_slice(&fw.to_le_bytes());
            cxap.extend_from_slice(&fh.to_le_bytes());
            cxap.extend_from_slice(&(png_data.len() as u32).to_le_bytes());
            cxap.extend_from_slice(&png_data);

            pages.push(cxap);
        }

        // Assemble final CXAN: header + pages + frame map
        let mut cxan = Vec::new();
        cxan.extend_from_slice(CXAN_MAGIC);
        cxan.extend_from_slice(&(pages.len() as u16).to_le_bytes());
        cxan.extend_from_slice(&frame_count_u16.to_le_bytes());
        cxan.extend_from_slice(&config.fps.to_le_bytes());

        // Reserve two u32 offset fields (pages_offset, frame_map_offset)
        let offsets_pos = cxan.len();
        cxan.extend_from_slice(&0u32.to_le_bytes());
        cxan.extend_from_slice(&0u32.to_le_bytes());

        let pages_offset = cxan.len() as u32;
        for p in &pages {
            cxan.extend_from_slice(p);
        }

        let frame_map_offset = cxan.len() as u32;
        for (p, c) in frame_map {
            cxan.extend_from_slice(&p.to_le_bytes());
            cxan.extend_from_slice(&c.to_le_bytes());
        }

        // Fill in offsets
        cxan[offsets_pos..offsets_pos + 4].copy_from_slice(&pages_offset.to_le_bytes());
        cxan[offsets_pos + 4..offsets_pos + 8].copy_from_slice(&frame_map_offset.to_le_bytes());

        Ok(cxan)
    }

    fn unpack(&self, data: &[u8], folder: &Path) -> io::Result<()> {
        if data.len() < 24 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "CXAN data too short"));
        }
        if &data[0..4] != CXAN_MAGIC {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "Invalid CXAN magic"));
        }

        let page_count = u16::from_le_bytes(data[4..6].try_into().unwrap()) as usize;
        let frame_count = u16::from_le_bytes(data[6..8].try_into().unwrap()) as usize;
        let fps = f32::from_le_bytes(data[8..12].try_into().unwrap());
        let pages_offset = u32::from_le_bytes(data[12..16].try_into().unwrap()) as usize;
        let frame_map_offset = u32::from_le_bytes(data[16..20].try_into().unwrap()) as usize;

        fs::create_dir_all(folder)?;

        // Read pages
        let mut pages = Vec::with_capacity(page_count);
        let mut current_pos = pages_offset;
        for _ in 0..page_count {
            if current_pos + 20 > data.len() {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "Unexpected end of CXAN data (pages)"));
            }
            if &data[current_pos..current_pos+4] != CXAP_MAGIC {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "Invalid CXAP magic"));
            }
            let cols = u16::from_le_bytes(data[current_pos+4..current_pos+6].try_into().unwrap()) as usize;
            let rows = u16::from_le_bytes(data[current_pos+6..current_pos+8].try_into().unwrap()) as usize;
            let fw = u32::from_le_bytes(data[current_pos+8..current_pos+12].try_into().unwrap());
            let fh = u32::from_le_bytes(data[current_pos+12..current_pos+16].try_into().unwrap());
            let png_len = u32::from_le_bytes(data[current_pos+16..current_pos+20].try_into().unwrap()) as usize;
            current_pos += 20;

            if current_pos + png_len > data.len() {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "Unexpected end of CXAP PNG data"));
            }
            let png_data = &data[current_pos..current_pos + png_len];
            current_pos += png_len;

            let img = ImageReader::new(io::Cursor::new(png_data))
                .with_guessed_format()?
                .decode()
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?
                .into_rgba8();
            
            pages.push((cols, rows, fw, fh, img));
        }

        // Read frame map
        let mut frame_map = Vec::with_capacity(frame_count);
        let mut current_pos = frame_map_offset;
        for _ in 0..frame_count {
            if current_pos + 4 > data.len() {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "Unexpected end of CXAN data (frame map)"));
            }
            let page_idx = u16::from_le_bytes(data[current_pos..current_pos+2].try_into().unwrap()) as usize;
            let cell_idx = u16::from_le_bytes(data[current_pos+2..current_pos+4].try_into().unwrap()) as usize;
            current_pos += 4;
            frame_map.push((page_idx, cell_idx));
        }

        // Export frames
        for (i, (p_idx, c_idx)) in frame_map.iter().enumerate() {
            let (cols, _rows, fw, fh, atlas) = &pages[*p_idx];
            let x = (*c_idx % *cols) as u32 * *fw;
            let y = (*c_idx / *cols) as u32 * *fh;

            let frame = atlas.view(x, y, *fw, *fh).to_image();
            let frame_path = folder.join(format!("frame_{:05}.png", i));
            frame.save(frame_path).map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
        }

        // Export animation.entry
        let entry_content = format!("fps = {}\n", fps);
        fs::write(folder.join("animation.entry"), entry_content)?;

        Ok(())
    }
}

#[derive(Debug, Clone, Copy)]
struct AnimationConfig {
    fps: f32,
}

fn load_animation_config(folder: &Path) -> io::Result<AnimationConfig> {
    let path = folder.join("animation.entry");
    let text = fs::read_to_string(path)?;
    let value: Value =
        toml::from_str(&text).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

    let fps = value.get("fps").and_then(|v| v.as_float()).unwrap_or(24.0) as f32;

    Ok(AnimationConfig { fps })
}

/// Loads PNG frames from `folder`, sorted by file name (deterministic).
/// Decodes each image into `RgbaImage` and validates equal dimensions.
fn load_png_frames(folder: &Path) -> io::Result<Vec<RgbaImage>> {
    // Collect PNG files in a deterministic order (by file name).
    let mut png_paths: Vec<_> = fs::read_dir(folder)?
        .filter_map(|e| e.ok().map(|entry| entry.path()))
        .filter(|p| {
            p.extension()
                .and_then(|s| s.to_str())
                .map_or(false, |s| s.eq_ignore_ascii_case("png"))
        })
        .collect();

    png_paths.sort_by_key(|p| p.file_name().and_then(|s| s.to_str().map(str::to_owned)));

    if png_paths.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "No PNG frames found",
        ));
    }

    // Decode images in parallel for speed.
    let decoded_results: Vec<io::Result<RgbaImage>> = png_paths
        .par_iter()
        .map(|path| {
            // open file (may return io::Error)
            let reader =
                ImageReader::open(path).map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
            let dyn_img = reader
                .decode()
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            Ok(dyn_img.into_rgba8())
        })
        .collect();

    // Gather results, returning the first error we encounter.
    let mut frames = Vec::with_capacity(decoded_results.len());
    for r in decoded_results {
        frames.push(r?);
    }

    // Validate consistent dimensions
    let (w, h) = frames[0].dimensions();
    for (i, f) in frames.iter().enumerate().skip(1) {
        let (fw, fh) = f.dimensions();
        if fw != w || fh != h {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "Frame {} has inconsistent dimensions ({}x{}) vs first frame ({}x{})",
                    i, fw, fh, w, h
                ),
            ));
        }
    }

    Ok(frames)
}

