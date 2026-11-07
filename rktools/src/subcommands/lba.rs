use std::{fs::OpenOptions, time::Duration};

use clap::Subcommand;
use memmap2::{Mmap, MmapMut};
use rkusb::{RkDevice, STORAGE_SECTOR_SIZE, USB_TIMEOUT};

use crate::{
    common,
    progress::ProgressBar,
    util::{format_bytes, parse_u8, parse_u32},
};

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
        default_value = "5s",
        help = "USB timeout per transfer chunk (the whole operation is bounded by the requested LBA range, not this value)"
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
    let timeout = if args.timeout.is_zero() {
        USB_TIMEOUT
    } else {
        args.timeout
    };

    match &args.command {
        Command::Read(sub_args) => exec_read(&mut rkdev, sub_args, timeout),
        Command::Write(sub_args) => exec_write(&mut rkdev, sub_args, timeout),
        Command::Erase(sub_args) => exec_erase(&mut rkdev, sub_args, timeout),
    }
}

fn exec_read<T: rusb::UsbContext>(
    rkdev: &mut RkDevice<T>,
    args: &ReadArgs,
    timeout: Duration,
) -> Result<(), Box<dyn std::error::Error>> {
    let output_bytes = args.sector_count as usize * STORAGE_SECTOR_SIZE;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&args.path)?;
    file.set_len(output_bytes as u64)?;
    // Safety: file length is fixed before mapping and buffer is only written in-bounds.
    let mut mmap = unsafe { MmapMut::map_mut(&file)? };
    let mut bar = ProgressBar::new(
        format!("Read LBA {}", format_bytes(output_bytes as u64)),
        output_bytes as u64,
    );
    rkdev.read_lba_with_progress(
        args.begin_sector,
        &mut mmap,
        args.subcode,
        timeout,
        &mut |done, total| bar.set(done, total),
    )?;
    bar.finish();

    mmap.flush()?;

    println!(
        "Read LBA OK, read {} sectors ({}) -> {}",
        args.sector_count,
        format_bytes(output_bytes as u64),
        args.path
    );
    Ok(())
}

fn exec_write<T: rusb::UsbContext>(
    rkdev: &mut RkDevice<T>,
    args: &WriteArgs,
    timeout: Duration,
) -> Result<(), Box<dyn std::error::Error>> {
    let file = std::fs::File::open(&args.path)?;
    // Safety: input file is opened read-only and mapping is read-only.
    let mmap = unsafe { Mmap::map(&file)? };
    let mut bar = ProgressBar::new(
        format!("Write LBA {}", format_bytes(mmap.len() as u64)),
        mmap.len() as u64,
    );
    rkdev.write_lba_with_progress(
        args.begin_sector,
        &mmap,
        args.subcode,
        timeout,
        &mut |done, total| bar.set(done, total),
    )?;
    bar.finish();

    println!(
        "Write LBA OK, wrote {} ({}) <- {}",
        mmap.len(),
        format_bytes(mmap.len() as u64),
        args.path
    );
    Ok(())
}

fn exec_erase<T: rusb::UsbContext>(
    rkdev: &mut RkDevice<T>,
    args: &EraseArgs,
    timeout: Duration,
) -> Result<(), Box<dyn std::error::Error>> {
    let total_bytes = args.sector_count as u64 * STORAGE_SECTOR_SIZE as u64;
    let mut bar = ProgressBar::new(format!("Erase LBA {}", format_bytes(total_bytes)), total_bytes);
    rkdev.erase_lba_with_progress(
        args.begin_sector,
        args.sector_count,
        timeout,
        &mut |done, total| bar.set(done, total),
    )?;
    bar.finish();

    println!(
        "Erase LBA OK, erased {} sectors ({})",
        args.sector_count,
        format_bytes(total_bytes)
    );
    Ok(())
}
