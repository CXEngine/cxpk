use std::collections::HashMap;
use std::fs;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use rayon::prelude::*;
use walkdir::WalkDir;

use crate::driver::{self, AssetDriver};

/// CXPK container magic
pub const CXPK_MAGIC: &[u8; 4] = b"CXPK";

/// Find folders that match any registered asset driver.
pub fn find_asset_folders(
    root: &Path,
    drivers: &[Box<dyn AssetDriver>],
) -> HashMap<PathBuf, usize> {
    let mut folders = HashMap::new();

    for entry in WalkDir::new(root).into_iter().filter_map(|e| e.ok()) {
        if !entry.file_type().is_file() {
            continue;
        }
        if let Some(name) = entry.file_name().to_str() {
            for (idx, driver) in drivers.iter().enumerate() {
                if name == driver.entry_file() {
                    if let Some(p) = entry.path().parent() {
                        folders.insert(p.to_path_buf(), idx);
                    }
                }
            }
        }
    }

    folders
}

/// Walk the given assets directory and produce a deterministic list of entries.
pub fn process_directory(assets_dir: &Path, threads: usize) -> io::Result<Vec<(String, Vec<u8>)>> {
    let drivers = driver::get_drivers();
    let asset_folders = find_asset_folders(assets_dir, &drivers);

    let mut raw_paths: Vec<PathBuf> = WalkDir::new(assets_dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .map(|e| e.path().to_path_buf())
        .filter(|path| !asset_folders.keys().any(|f| path.starts_with(f)))
        .collect();

    enum Job {
        Asset(PathBuf, String, usize),
        Raw(PathBuf, String),
    }

    let mut jobs: Vec<Job> = Vec::new();

    let mut asset_folder_list: Vec<_> = asset_folders.into_iter().collect();
    asset_folder_list.sort_by_key(|(p, _)| p.to_string_lossy().into_owned());

    for (folder, driver_idx) in asset_folder_list {
        let rel = folder
            .strip_prefix(assets_dir)
            .map_err(|_| io::Error::new(io::ErrorKind::Other, "invalid asset path"))?;
        let mut name = rel.to_string_lossy().replace('\\', "/");
        if name.is_empty() {
            name = ".".to_string();
        }
        name.push_str(drivers[driver_idx].extension());
        jobs.push(Job::Asset(folder, name, driver_idx));
    }

    // Deterministically order raw file paths
    raw_paths.sort_by_key(|p| p.to_string_lossy().into_owned());
    for path in raw_paths {
        let rel = path
            .strip_prefix(assets_dir)
            .map_err(|_| io::Error::new(io::ErrorKind::Other, "invalid asset path"))?;
        let name = rel.to_string_lossy().replace('\\', "/");
        jobs.push(Job::Raw(path, name));
    }

    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .build()
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;

    let results: Vec<io::Result<(String, Vec<u8>)>> = pool.install(|| {
        jobs.into_par_iter()
            .map(|job| match job {
                Job::Asset(folder, name, idx) => drivers[idx].pack(&folder).map(|d| (name, d)),
                Job::Raw(path, name) => fs::read(&path).map(|d| (name, d)),
            })
            .collect()
    });

    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    for r in results {
        files.push(r?);
    }

    files.sort_by(|a, b| a.0.cmp(&b.0));

    Ok(files)
}

pub fn unpack_container(input_file: &Path, output_dir: &Path) -> io::Result<()> {
    let mut file = fs::File::open(input_file)?;
    let mut magic = [0u8; 4];
    file.read_exact(&mut magic)?;
    if &magic != CXPK_MAGIC {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Invalid CXPK magic",
        ));
    }

    let mut count_bytes = [0u8; 4];
    file.read_exact(&mut count_bytes)?;
    let count = u32::from_le_bytes(count_bytes);

    struct Entry {
        name: String,
        offset: u32,
        size: u32,
    }

    let mut entries = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let mut name_bytes = [0u8; 64];
        file.read_exact(&mut name_bytes)?;
        let name = String::from_utf8_lossy(&name_bytes)
            .trim_matches('\0')
            .to_string();

        let mut off_bytes = [0u8; 4];
        file.read_exact(&mut off_bytes)?;
        let offset = u32::from_le_bytes(off_bytes);

        let mut size_bytes = [0u8; 4];
        file.read_exact(&mut size_bytes)?;
        let size = u32::from_le_bytes(size_bytes);

        entries.push(Entry { name, offset, size });
    }

    let drivers = driver::get_drivers();

    for entry in entries {
        file.seek(SeekFrom::Start(entry.offset as u64))?;
        let mut data = vec![0u8; entry.size as usize];
        file.read_exact(&mut data)?;

        let mut unpacked = false;
        if data.len() >= 4 {
            let magic: [u8; 4] = data[0..4].try_into().unwrap();
            if let Some(driver) = drivers.iter().find(|d| d.magic() == &magic) {
                // If it ends with the driver's extension, we unpack it into a folder
                if entry.name.ends_with(driver.extension()) {
                    let folder_name = &entry.name[..entry.name.len() - driver.extension().len()];
                    let folder_path = output_dir.join(folder_name);
                    println!(
                        "Unpacking asset: {} -> {}",
                        entry.name,
                        folder_path.display()
                    );
                    driver.unpack(&data, &folder_path)?;
                    unpacked = true;
                }
            }
        }

        if !unpacked {
            let out_path = output_dir.join(&entry.name);
            if let Some(parent) = out_path.parent() {
                fs::create_dir_all(parent)?;
            }
            println!("Extracting raw file: {}", entry.name);
            fs::write(out_path, data)?;
        }
    }

    Ok(())
}
