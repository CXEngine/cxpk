use std::fs;
use std::io;
use std::mem;
use std::path::Path;

use serde::Deserialize;
use serde_json::{json, Value};

use crate::driver::AssetDriver;

pub struct CxmpDriver;

pub const CXMP_MAGIC: &[u8; 4] = b"CXMP";

impl AssetDriver for CxmpDriver {
    fn magic(&self) -> &[u8; 4] { CXMP_MAGIC }
    fn extension(&self) -> &str { ".cxmp" }
    fn entry_file(&self) -> &str { "map.entry" }

    fn pack(&self, folder: &Path) -> io::Result<Vec<u8>> {
        pack_map_cxmp(folder)
    }

    fn unpack(&self, data: &[u8], folder: &Path) -> io::Result<()> {
        if data.len() < mem::size_of::<TdmpHeader>() {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "CXMP data too short"));
        }
        let header = unsafe { &*(data.as_ptr() as *const TdmpHeader) };
        if &header.magic != b"CXMP" {
             return Err(io::Error::new(io::ErrorKind::InvalidData, "Invalid CXMP magic"));
        }

        fs::create_dir_all(folder)?;

        // Reconstruct texture
        let mut img = image::RgbaImage::new(header.map_width, header.map_height);
        let pixel_data = &data[header.pixel_offset as usize..];
        let tile_size = header.tile_size as usize;
        let mut tile_idx = 0;

        for ty in 0..header.tiles_y {
            for tx in 0..header.tiles_x {
                let offset = tile_idx * tile_size * tile_size * 4;
                let tile_pixels = &pixel_data[offset..offset + tile_size * tile_size * 4];
                
                for y in 0..tile_size as u32 {
                    for x in 0..tile_size as u32 {
                        let sx = tx * header.tile_size as u32 + x;
                        let sy = ty * header.tile_size as u32 + y;
                        if sx < header.map_width && sy < header.map_height {
                            let src = ((y * header.tile_size as u32 + x) * 4) as usize;
                            let p = &tile_pixels[src..src + 4];
                            img.put_pixel(sx, sy, image::Rgba([p[0], p[1], p[2], p[3]]));
                        }
                    }
                }
                tile_idx += 1;
            }
        }
        img.save(folder.join("texture.png")).map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;

        // Reconstruct hitboxes
        let mut objects = Vec::new();
        let hitbox_data = &data[header.hitbox_offset as usize..];
        let hitboxes = unsafe {
            std::slice::from_raw_parts(
                hitbox_data.as_ptr() as *const Hitbox,
                header.hitbox_count as usize,
            )
        };

        // We need to deduplicate hitboxes since they were assigned to multiple tiles.
        // We can do this by using the global coordinates we reconstructed.
        let mut seen = std::collections::HashSet::new();

        let tile_table_data = &data[header.tile_table_offset as usize..];
        let tile_table = unsafe {
             std::slice::from_raw_parts(
                tile_table_data.as_ptr() as *const TileEntry,
                header.tile_count as usize,
            )
        };

        for (t_idx, entry) in tile_table.iter().enumerate() {
            let tx = (t_idx as u32 % header.tiles_x) as f32 * header.tile_size as f32;
            let ty = (t_idx as u32 / header.tiles_x) as f32 * header.tile_size as f32;

            for i in 0..entry.hitbox_count {
                let h = &hitboxes[(entry.first_hitbox + i) as usize];
                let gx = h.x + tx;
                let gy = h.y + ty;
                
                // Key for deduplication
                let key = ( (gx * 100.0) as i32, (gy * 100.0) as i32, (h.w * 100.0) as i32, (h.h * 100.0) as i32, (h.rotation * 100.0) as i32 );
                if seen.insert(key) {
                    objects.push(json!({
                        "x": gx,
                        "y": gy,
                        "width": h.w,
                        "height": h.h,
                        "rotation": h.rotation,
                        "visible": true
                    }));
                }
            }
        }

        let hitboxes_json = json!({
            "layers": [
                {
                    "name": "Hitboxes",
                    "objects": objects,
                    "type": "objectgroup",
                    "visible": true
                }
            ]
        });
        fs::write(folder.join("hitboxes.json"), serde_json::to_string_pretty(&hitboxes_json).unwrap())?;

        // Reconstruct map.entry
        let entry_content = format!(
            "version = {}\ntile_size = {}\ntexture = \"texture.png\"\nhitboxes = \"hitboxes.json\"\n",
            header.version, header.tile_size
        );
        fs::write(folder.join("map.entry"), entry_content)?;

        Ok(())
    }
}

/// CXMP header layout (C-compatible)
#[repr(C)]
#[derive(Clone, Copy)]
struct TdmpHeader {
    magic: [u8; 4], // "CXMP"
    version: u16,
    tile_size: u16,
    map_width: u32,
    map_height: u32,
    tiles_x: u32,
    tiles_y: u32,
    tile_count: u32,
    hitbox_count: u32,
    uv_offset: u32,
    pixel_offset: u32,
    tile_table_offset: u32,
    hitbox_offset: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct TileUV {
    u0: f32,
    v0: f32,
    u1: f32,
    v1: f32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct TileEntry {
    first_hitbox: u32,
    hitbox_count: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Hitbox {
    x: f32, // local to tile
    y: f32, // local to tile
    w: f32,
    h: f32,
    rotation: f32,
}

#[derive(Deserialize)]
struct MapEntry {
    #[serde(default = "default_version")]
    version: u32,
    tile_size: u32,
    texture: String,
    hitboxes: String,
}

fn default_version() -> u32 {
    1
}

/// Write a plain POD struct to a Vec<u8> by copying its bytes
fn write_struct<T: Copy>(out: &mut Vec<u8>, v: &T) {
    let p = v as *const T as *const u8;
    let s = mem::size_of::<T>();
    unsafe {
        out.extend_from_slice(std::slice::from_raw_parts(p, s));
    }
}

/// Write a slice of POD items into a Vec<u8>
fn write_slice<T: Copy>(out: &mut Vec<u8>, v: &[T]) {
    if v.is_empty() {
        return;
    }
    let p = v.as_ptr() as *const u8;
    let s = mem::size_of::<T>() * v.len();
    unsafe {
        out.extend_from_slice(std::slice::from_raw_parts(p, s));
    }
}

/// Pack a folder containing `map.entry` + texture + hitboxes JSON into a CXMP blob.
pub fn pack_map_cxmp(folder: &Path) -> io::Result<Vec<u8>> {
    // Read and parse `map.entry` (TOML preferred, fallback to JSON)
    let entry_str = fs::read_to_string(folder.join("map.entry"))?;
    let entry: MapEntry =
        toml::from_str(&entry_str).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

    // Open texture
    let img = image::open(folder.join(&entry.texture))
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?
        .into_rgba8();

    let (map_w, map_h) = img.dimensions();
    let pixels = img.into_raw();

    let tile_size = entry.tile_size;
    if tile_size == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "tile_size must be > 0",
        ));
    }

    let tiles_x = (map_w + tile_size - 1) / tile_size;
    let tiles_y = (map_h + tile_size - 1) / tile_size;
    let tile_count = tiles_x * tiles_y;

    // Build UVs
    let mut uvs = Vec::with_capacity(tile_count as usize);
    for ty in 0..tiles_y {
        for tx in 0..tiles_x {
            let u0 = (tx * tile_size) as f32 / map_w as f32;
            let v0 = (ty * tile_size) as f32 / map_h as f32;
            let u1 = ((tx + 1) * tile_size).min(map_w) as f32 / map_w as f32;
            let v1 = ((ty + 1) * tile_size).min(map_h) as f32 / map_h as f32;
            uvs.push(TileUV { u0, v0, u1, v1 });
        }
    }

    // Split texture into tiles (RGBA8)
    let mut tile_pixels: Vec<Vec<u8>> = Vec::with_capacity(tile_count as usize);
    for ty in 0..tiles_y {
        for tx in 0..tiles_x {
            let mut tile = vec![0u8; tile_size as usize * tile_size as usize * 4];
            for y in 0..tile_size {
                for x in 0..tile_size {
                    let sx = tx * tile_size + x;
                    let sy = ty * tile_size + y;
                    if sx >= map_w || sy >= map_h {
                        continue;
                    }
                    let src = ((sy * map_w + sx) * 4) as usize;
                    let dst = ((y * tile_size + x) * 4) as usize;
                    tile[dst..dst + 4].copy_from_slice(&pixels[src..src + 4]);
                }
            }
            tile_pixels.push(tile);
        }
    }

    // Load hitboxes JSON and extract only Hitboxes layer objects (tolerant)
    let tiled_path = folder.join(&entry.hitboxes);
    let tiled_str = fs::read_to_string(&tiled_path)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

    let mut global_hitboxes: Vec<(f32, f32, f32, f32, f32)> = Vec::new();

    // Parse once into Value and extract safely
    let json_v: Value = match serde_json::from_str(&tiled_str) {
        Ok(v) => v,
        Err(_) => {
            // Malformed JSON — treat as empty hitbox list (do not fail)
            Value::Null
        }
    };

    if let Some(layers) = json_v.get("layers").and_then(|v| v.as_array()) {
        for layer in layers {
            if let Some(name) = layer.get("name").and_then(|n| n.as_str()) {
                if name.eq_ignore_ascii_case("Hitboxes") {
                    if let Some(objects) = layer.get("objects").and_then(|o| o.as_array()) {
                        for obj in objects {
                            let x = obj.get("x").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
                            let y = obj.get("y").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
                            let w = obj.get("width").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
                            let h =
                                obj.get("height").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
                             let rotation = obj.get("rotation").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
                            global_hitboxes.push((x, y, w, h, rotation));
                        }
                    }
                    // Found the Hitboxes layer → stop (only this layer is considered)
                    break;
                }
            }
        }
    }

    // Assign hitboxes to tiles, convert to local coordinates per tile
    let mut buckets: Vec<Vec<Hitbox>> = vec![Vec::new(); tile_count as usize];

    let tiles_x_i32 = tiles_x as i32;
    let tiles_y_i32 = tiles_y as i32;
    let tile_size_f = tile_size as f32;

    for (gx, gy, gw, gh, rotation) in global_hitboxes {
        // ignore degenerate hitboxes
        if gw <= 0.0 || gh <= 0.0 {
            continue;
        }

        // Calculate AABB of the rotated hitbox
        let rad = rotation.to_radians();
        let c = rad.cos();
        let s = rad.sin();
        
        let x_coords = [
            gx, 
            gx + gw * c, 
            gx + gw * c - gh * s, 
            gx - gh * s
        ];
        let y_coords = [
            gy, 
            gy + gw * s, 
            gy + gw * s + gh * c, 
            gy + gh * c
        ];

        let min_gx = x_coords.iter().fold(f32::INFINITY, |a, &b| a.min(b));
        let max_gx = x_coords.iter().fold(f32::NEG_INFINITY, |a, &b| a.max(b));
        let min_gy = y_coords.iter().fold(f32::INFINITY, |a, &b| a.min(b));
        let max_gy = y_coords.iter().fold(f32::NEG_INFINITY, |a, &b| a.max(b));

        // Compute tile index range (floor) and clamp
        let min_tx = (min_gx / tile_size_f).floor() as i32;
        let min_ty = (min_gy / tile_size_f).floor() as i32;
        let max_tx = (max_gx / tile_size_f).floor() as i32;
        let max_ty = (max_gy / tile_size_f).floor() as i32;

        let min_tx = min_tx.max(0);
        let min_ty = min_ty.max(0);
        let max_tx = max_tx.min(tiles_x_i32 - 1);
        let max_ty = max_ty.min(tiles_y_i32 - 1);

        if min_tx > max_tx || min_ty > max_ty {
            continue;
        }

        // Assign the hitbox to all tiles it intersects
        for ty in min_ty..=max_ty {
            for tx in min_tx..=max_tx {
                let tile_x_start = (tx as f32) * tile_size_f;
                let tile_y_start = (ty as f32) * tile_size_f;

                let local = Hitbox {
                    x: gx - tile_x_start,
                    y: gy - tile_y_start,
                    w: gw,
                    h: gh,
                    rotation,
                };

                let idx = (ty as u32 * tiles_x + tx as u32) as usize;
                if idx < buckets.len() {
                    buckets[idx].push(local);
                }
            }
        }
    }

    // Build tile table and flat hitbox list
    let mut tile_table = Vec::with_capacity(tile_count as usize);
    let mut flat_hitboxes = Vec::new();

    for b in buckets {
        let first = flat_hitboxes.len() as u32;
        let count = b.len() as u32;
        flat_hitboxes.extend(b);
        tile_table.push(TileEntry {
            first_hitbox: first,
            hitbox_count: count,
        });
    }

    // Build output blob
    let mut out = Vec::new();

    // Write header placeholder (we will patch offsets later)
    let header_placeholder = TdmpHeader {
        magic: *b"CXMP",
        version: entry.version as u16,
        tile_size: tile_size as u16,
        map_width: map_w,
        map_height: map_h,
        tiles_x,
        tiles_y,
        tile_count,
        hitbox_count: flat_hitboxes.len() as u32,
        uv_offset: 0,
        pixel_offset: 0,
        tile_table_offset: 0,
        hitbox_offset: 0,
    };
    let header_pos = out.len();
    write_struct(&mut out, &header_placeholder);

    // UVs
    let uv_offset = out.len() as u32;
    write_slice(&mut out, &uvs);

    // Pixels (tile-by-tile)
    let pixel_offset = out.len() as u32;
    for t in tile_pixels {
        out.extend_from_slice(&t);
    }

    // Tile table
    let tile_table_offset = out.len() as u32;
    write_slice(&mut out, &tile_table);

    // Hitboxes
    let hitbox_offset = out.len() as u32;
    write_slice(&mut out, &flat_hitboxes);

    // Patch header with offsets
    let header = TdmpHeader {
        magic: *CXMP_MAGIC,
        version: entry.version as u16,
        tile_size: tile_size as u16,
        map_width: map_w,
        map_height: map_h,
        tiles_x,
        tiles_y,
        tile_count,
        hitbox_count: flat_hitboxes.len() as u32,
        uv_offset,
        pixel_offset,
        tile_table_offset,
        hitbox_offset,
    };

    let header_bytes = unsafe {
        std::slice::from_raw_parts(
            &header as *const TdmpHeader as *const u8,
            mem::size_of::<TdmpHeader>(),
        )
    };
    out[header_pos..header_pos + header_bytes.len()].copy_from_slice(header_bytes);

    Ok(out)
}
