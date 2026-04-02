use std::fs;
use std::io::{self, BufWriter, Write};
use std::path::Path;

mod driver;
mod packer;

mod cxan;
mod cxsi;
mod cxmp;
mod cxta;

use packer::CXPK_MAGIC;

fn print_help() {
    println!("Usage: cxpk <command> [options]");
    println!("\nCommands:");
    println!("  pack   <assets-dir/> <output.cxpk> [-jN]  Pack assets into a container");
    println!("  unpack <input.cxpk>  <output-dir/>        Unpack a container");
    println!("\nOptions:");
    println!("  -jN, -j N    Number of worker threads (default: hardware threads)");
    println!("  -h, --help   Show this help");
}

fn main() -> io::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 || args.contains(&"-h".to_string()) || args.contains(&"--help".to_string()) {
        print_help();
        return Ok(());
    }

    let command = &args[1];

    if command == "pack" {
        if args.len() < 4 {
            print_help();
            return Ok(());
        }
        let assets_dir = Path::new(&args[2]);
        let output_file = Path::new(&args[3]);

        let mut threads = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1);

        let mut i = 4;
        while i < args.len() {
            let arg = &args[i];
            if arg == "-j" {
                if i + 1 < args.len() {
                    if let Ok(n) = args[i+1].parse::<usize>() {
                        threads = n;
                        i += 1;
                    }
                }
            } else if arg.starts_with("-j") {
                if let Ok(n) = arg[2..].parse::<usize>() {
                    threads = n;
                }
            }
            i += 1;
        }

        if !assets_dir.exists() {
            return Err(io::Error::new(io::ErrorKind::NotFound, format!("Assets directory '{}' not found", assets_dir.display())));
        }

        println!("Packing assets from {} to {}... ({} workers)", assets_dir.display(), output_file.display(), threads);

        let files = packer::process_directory(assets_dir, threads)?;
        println!("Found {} files to pack", files.len());

        let index_entry_size = 64u64 + 4u64 + 4u64;
        let mut current_offset: u64 = 4 + 4 + files.len() as u64 * index_entry_size;

        let mut entries: Vec<(String, u32, u32)> = Vec::with_capacity(files.len());
        for (name, data) in &files {
            let size = data.len() as u64;
            if size > (u32::MAX as u64) {
                return Err(io::Error::new(io::ErrorKind::InvalidData, format!("File '{}' is too large", name)));
            }
            entries.push((name.clone(), current_offset as u32, size as u32));
            current_offset += size;
        }

        let mut writer = BufWriter::new(fs::File::create(output_file)?);
        writer.write_all(CXPK_MAGIC)?;
        writer.write_all(&(entries.len() as u32).to_le_bytes())?;

        for (name, offset, size) in &entries {
            let mut name_bytes = [0u8; 64];
            let bytes = name.as_bytes();
            let len = bytes.len().min(63);
            name_bytes[..len].copy_from_slice(&bytes[..len]);
            writer.write_all(&name_bytes)?;
            writer.write_all(&offset.to_le_bytes())?;
            writer.write_all(&size.to_le_bytes())?;
        }

        for (_name, data) in files {
            writer.write_all(&data)?;
        }
        writer.flush()?;
        println!("Successfully created {}", output_file.display());

    } else if command == "unpack" {
        if args.len() < 4 {
            print_help();
            return Ok(());
        }
        let input_file = Path::new(&args[2]);
        let output_dir = Path::new(&args[3]);

        if !input_file.exists() {
             return Err(io::Error::new(io::ErrorKind::NotFound, format!("Input file '{}' not found", input_file.display())));
        }

        println!("Unpacking container {} to {}...", input_file.display(), output_dir.display());
        packer::unpack_container(input_file, output_dir)?;
        println!("Successfully unpacked to {}", output_dir.display());

    } else {
        println!("Unknown command: {}", command);
        print_help();
    }

    Ok(())
}
