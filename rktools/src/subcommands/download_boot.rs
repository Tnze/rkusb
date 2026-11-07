use std::fs::File;
use std::time::Duration;

use memmap2::Mmap;
use rkusb::{RkDevice, RkUsbError, image::ImageError};
use thiserror::Error;

use crate::common::{self, DeviceSelectionError};

#[derive(clap::Args)]
pub struct Args {
    #[arg(help = "The image file path, eg: rk3588_spl_loader_v1.19.113.bin")]
    path: String,
    #[arg(long, help = "Bus number of the device")]
    bus: Option<u8>,
    #[arg(long, help = "Address of the device")]
    addr: Option<u8>,
    #[arg(
        long,
        value_parser = humantime::parse_duration,
        help = "Wait for device with timeout (e.g., 30s, 1m)"
    )]
    wait: Option<Duration>,
}

#[derive(Error, Debug)]
pub enum DownloadBootError {
    #[error("Device selection error: {0}")]
    DeviceSelection(#[from] DeviceSelectionError),
    #[error("RkUsb error: {0}")]
    RkUsb(#[from] RkUsbError),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Image parse error: {0}")]
    Image(#[from] ImageError),
}

pub fn exec(usb_ctx: rusb::Context, args: &Args) -> Result<(), DownloadBootError> {
    let selected_device = common::find_device(&usb_ctx, args.bus, args.addr, args.wait)?;
    let mut device = RkDevice::open(&selected_device)?;
    let file = File::open(&args.path)?;
    let mmap = unsafe { Mmap::map(&file)? };
    device.download_boot(rkusb::image::RkBootImage::new(&mmap[..])?)?;
    Ok(())
}
