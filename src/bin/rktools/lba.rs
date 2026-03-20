use std::{
    fs::File,
    time::{Duration, Instant},
};

use clap::Subcommand;
use memmap2::{Mmap, MmapMut};
use rkusb::RkDevice;

use crate::{
    common,
    util::{parse_u8, parse_u32, timeout_to},
};

const SECTOR_SIZE: usize = 512;
const DEFAULT_RW_SECTORS: usize = 128;

#[derive(clap::Args)]
pub struct Args {
    #[arg(long, help = "Bus number of the device")]
    bus: Option<u8>,
    #[arg(long, help = "Address of the device")]
    addr: Option<u8>,
    #[arg(long, value_parser = humantime::parse_duration, help = "Wait for device with timeout (e.g., 30s, 1m)")]
    wait: Option<Duration>,
    #[arg(
        long,
        value_parser = humantime::parse_duration,
        default_value = "300s",
        help = "Total timeout for this LBA command; all LBA ops share this budget"
    )]
    timeout: Duration,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    #[command(about = "Read sectors by LBA", visible_alias("r"))]
    Read(ReadArgs),
    #[command(about = "Write file to sectors by LBA", visible_alias("w"))]
    Write(WriteArgs),
    #[command(about = "Erase sectors by LBA", visible_alias("e"))]
    Erase(EraseArgs),
}

#[derive(clap::Args)]
struct ReadArgs {
    #[arg(help = "Begin sector (supports decimal or 0x-prefixed hex)", value_parser = parse_u32)]
    begin_sector: u32,
    #[arg(help = "Sector count (supports decimal or 0x-prefixed hex)", value_parser = parse_u32)]
    sector_count: u32,
    #[arg(help = "Output file path")]
    path: String,
    #[arg(short, long, default_value_t = 0, value_parser = parse_u8, help = "Read subcode")]
    subcode: u8,
}

#[derive(clap::Args)]
struct WriteArgs {
    #[arg(help = "Begin sector (supports decimal or 0x-prefixed hex)", value_parser = parse_u32)]
    begin_sector: u32,
    #[arg(help = "Input file path")]
    path: String,
    #[arg(short, long, default_value_t = 0, value_parser = parse_u8, help = "Write subcode")]
    subcode: u8,
}

#[derive(clap::Args)]
struct EraseArgs {
    #[arg(help = "Begin sector (supports decimal or 0x-prefixed hex)", value_parser = parse_u32)]
    begin_sector: u32,
    #[arg(help = "Sector count (supports decimal or 0x-prefixed hex)", value_parser = parse_u32)]
    sector_count: u32,
}

pub fn exec(usb_ctx: rusb::Context, args: &Args) -> Result<(), Box<dyn std::error::Error>> {
    let selected_device = common::find_device(&usb_ctx, args.bus, args.addr, args.wait)?;
    let mut rkdev = RkDevice::open(&selected_device)?;

    let deadline = Instant::now() + args.timeout;
    match &args.command {
        Command::Read(args) => exec_read(&mut rkdev, args, deadline),
        Command::Write(args) => exec_write(&mut rkdev, args, deadline),
        Command::Erase(args) => exec_erase(&mut rkdev, args, deadline),
    }
}

fn exec_read<T: rusb::UsbContext>(
    rkdev: &mut RkDevice<T>,
    args: &ReadArgs,
    deadline: Instant,
) -> Result<(), Box<dyn std::error::Error>> {
    let output_bytes = args.sector_count as usize * SECTOR_SIZE;
    let file = File::create(&args.path)?;
    file.set_len(output_bytes as u64)?;
    // Safety: file length is fixed before mapping and buffer is only written in-bounds.
    let mut mmap = unsafe { MmapMut::map_mut(&file)? };

    for (i, chunk) in mmap
        .chunks_mut(DEFAULT_RW_SECTORS * SECTOR_SIZE)
        .enumerate()
    {
        let pos = args.begin_sector + (i * DEFAULT_RW_SECTORS) as u32;
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "LBA command total timeout exceeded",
            )
            .into());
        };
        rkdev.read_lba(pos, chunk, args.subcode, remaining)?;
    }

    mmap.flush()?;

    println!("Read LBA OK, read {} sectors", args.sector_count);
    Ok(())
}

fn exec_write<T: rusb::UsbContext>(
    rkdev: &mut RkDevice<T>,
    args: &WriteArgs,
    deadline: Instant,
) -> Result<(), Box<dyn std::error::Error>> {
    let file = File::open(&args.path)?;
    // Safety: input file is opened read-only and mapping is read-only.
    let mmap = unsafe { Mmap::map(&file)? };
    let timeout = timeout_to(deadline, rusb::Error::Timeout);

    for (i, chunk) in mmap.chunks(DEFAULT_RW_SECTORS * SECTOR_SIZE).enumerate() {
        let pos = args.begin_sector + i as u32;
        let rem = chunk.len() % SECTOR_SIZE;
        if rem == 0 {
            rkdev.write_lba(pos, chunk, args.subcode, timeout()?)?;
        } else {
            let mut padded = vec![0u8; SECTOR_SIZE - rem];
            padded[..chunk.len()].copy_from_slice(chunk);
            rkdev.write_lba(pos, &padded, args.subcode, timeout()?)?;
        }
    }

    println!("Write LBA OK, wrote {} bytes", mmap.len());
    Ok(())
}

fn exec_erase<T: rusb::UsbContext>(
    rkdev: &mut RkDevice<T>,
    args: &EraseArgs,
    deadline: Instant,
) -> Result<(), Box<dyn std::error::Error>> {
    let timeout = timeout_to(deadline, rusb::Error::Timeout);
    for i in (0..args.sector_count).step_by(u16::MAX as usize) {
        let chunk_sectors = (args.sector_count - i).min(u16::MAX as u32);
        let pos = args.begin_sector + i;
        rkdev.erase_lba(pos, chunk_sectors as u16, timeout()?)?;
    }

    println!("Erase LBA OK, erased {} sectors", args.sector_count);
    Ok(())
}
