use std::fs;
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};

mod driver;
mod packer;

mod cxan;
mod cxsi;
mod cxmp;
mod cxta;

use packer::CXPK_MAGIC;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "cxpk")]
#[command(about = "Tool for packing, unpacking and processing CXPK archives")]
struct Cli {
    #[command(subcommand)]
    command: Command,

    #[arg(short = 'j', long = "jobs")]
    jobs: Option<usize>,
}

#[derive(Subcommand)]
enum Command {
    Pack {
        dir: PathBuf,
        out: PathBuf,
    },
    Unpack {
        input: PathBuf,
        dir: PathBuf,
    },
}

fn pack(assets_dir: &Path, output_file: &Path, max_jobs: usize) -> io::Result<()> {
    if !assets_dir.exists() {
        return Err(io::Error::new(io::ErrorKind::NotFound, format!("Assets directory '{}' not found", assets_dir.display())));
    }

    println!("Packing assets from {} to {}... ({} workers)", assets_dir.display(), output_file.display(), max_jobs);

    let files = packer::process_directory(assets_dir, max_jobs)?;
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
    Ok(())
}

fn unpack(input_file: &Path, output_dir: &Path) -> io::Result<()> {
    if !input_file.exists() {
         return Err(io::Error::new(io::ErrorKind::NotFound, format!("Input file '{}' not found", input_file.display())));
    }

    println!("Unpacking container {} to {}...", input_file.display(), output_dir.display());
    packer::unpack_container(input_file, output_dir)?;
    println!("Successfully unpacked to {}", output_dir.display());
    Ok(())
}

fn main() -> io::Result<()> {
    let cli = Cli::parse();

    let hardware_threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);

    let jobs = cli.jobs.unwrap_or(hardware_threads);

    match cli.command {
        Command::Pack { dir, out } => pack(&dir, &out, jobs),
        Command::Unpack { input, dir } => unpack(&input, &dir),
    }
}
