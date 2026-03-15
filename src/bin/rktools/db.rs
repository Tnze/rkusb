use std::fs::File;

use memmap2::Mmap;
use rkusb::RkDevice;

use crate::common;

#[derive(clap::Args)]
pub struct Args {
    #[arg(help = "The image file path, eg: rk3588_spl_loader_v1.19.113.bin")]
    path: String,
    #[arg(long, help = "Bus number of the device")]
    bus: Option<u8>,
    #[arg(long, help = "Address of the device")]
    addr: Option<u8>,
    #[arg(long, help = "Wait for device with timeout (e.g., 30s, 1m)")]
    wait: Option<String>,
}

pub fn exec(usb_ctx: rusb::Context, args: &Args) -> Result<(), Box<dyn std::error::Error>> {
    let timeout = args
        .wait
        .as_ref()
        .map(|s| humantime::parse_duration(s))
        .transpose()?;
    let selected_device = common::find_device(&usb_ctx, args.bus, args.addr, timeout)?;
    let mut device = RkDevice::open(&selected_device)?;
    let file = File::open(&args.path)?;
    let mmap = unsafe { Mmap::map(&file)? };
    device.download_boot(rkusb::image::BootImage::new(&mmap[..]))?;
    Ok(())
}
