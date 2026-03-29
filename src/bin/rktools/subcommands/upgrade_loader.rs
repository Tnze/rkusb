use std::{
    fs::File,
    time::{Duration, Instant},
};

use memmap2::Mmap;
use rkusb::{
    RkDevice, RkUsbError,
    idblock::{self, IdBlockError},
    image::{ImageError, RkBootEntryType, RkBootImage},
};
use thiserror::Error;

use crate::{
    common::{self, DeviceSelectionError},
    util::{parse_u8, parse_u32, timeout_to},
};

const SECTOR_SIZE: usize = 512;
const IDBLOCK_LBA_START: u32 = 64;
const ENTRY_FLASH_BOOT: &str = "FlashBoot";
const ENTRY_FLASH_DATA: &str = "FlashData";
const ENTRY_FLASH_HEAD: &str = "FlashHead";

#[derive(clap::Args)]
pub struct Args {
    #[arg(help = "The Rockchip loader image path, e.g. rk3588_spl_loader_v1.19.113.bin")]
    path: String,
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
        help = "Total timeout for this upgrade-loader command"
    )]
    timeout: Duration,
    #[arg(
        short,
        long,
        default_value_t = IDBLOCK_LBA_START,
        value_parser = parse_u32,
        help = "Start LBA for IDBlock write"
    )]
    lba: u32,
    #[arg(short, long, default_value_t = 0, value_parser = parse_u8, help = "Write subcode")]
    subcode: u8,
}

#[derive(Error, Debug)]
pub enum UpgradeLoaderError {
    #[error("Device selection error: {0}")]
    DeviceSelection(#[from] DeviceSelectionError),
    #[error("Parse timeout: {0}")]
    ParseTimeout(#[from] humantime::DurationError),
    #[error("RkUsb error: {0}")]
    RkUsb(#[from] RkUsbError),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("IDBlock build error: {0}")]
    IdBlock(#[from] IdBlockError),
    #[error("Image parse error: {0}")]
    Image(#[from] ImageError),
    #[error("loader entry not found: {0}")]
    MissingEntry(&'static str),
    #[error("device does not support upgrading loader with FlashHead")]
    FlashHeadNotSupported,
}

pub fn exec(usb_ctx: rusb::Context, args: &Args) -> Result<(), UpgradeLoaderError> {
    let selected_device = common::find_device(&usb_ctx, args.bus, args.addr, args.wait)?;
    let mut rkdev = RkDevice::open(&selected_device)?;

    let file = File::open(&args.path)?;
    let mmap = unsafe { Mmap::map(&file)? };
    let boot_img = RkBootImage::new(&mmap)?;

    let loader_code = find_loader_entry(&boot_img, ENTRY_FLASH_BOOT)?;
    let loader_data = find_loader_entry(&boot_img, ENTRY_FLASH_DATA)?;
    let loader_head = find_loader_entry(&boot_img, ENTRY_FLASH_HEAD).ok();
    let rc4_enabled = unsafe { (*boot_img.boot_header_ptr()).rc4_flag != 0 };

    let timeout = timeout_to(
        Instant::now() + args.timeout,
        RkUsbError::Usb(rusb::Error::Timeout),
    );

    if loader_head.is_some() {
        let capability = rkdev.read_capability(timeout()?)?;
        if (capability[1] & 1) == 0 {
            return Err(UpgradeLoaderError::FlashHeadNotSupported);
        }
    }

    let idblock_data = idblock::build_idblock(loader_head, loader_data, loader_code, rc4_enabled)?;
    rkdev.write_lba(args.lba, &idblock_data, args.subcode, timeout()?)?;
    println!(
        "Upgrade loader OK, wrote {} sectors to LBA {}",
        idblock_data.len() / SECTOR_SIZE,
        args.lba
    );
    Ok(())
}

fn find_loader_entry<'a>(
    boot_img: &'a RkBootImage<'a>,
    name: &'static str,
) -> Result<&'a [u8], UpgradeLoaderError> {
    boot_img
        .iter_entries(RkBootEntryType::EntryLoader)
        .find_map(|(entry_name, data, _)| (entry_name == name).then_some(data))
        .ok_or(UpgradeLoaderError::MissingEntry(name))
}
