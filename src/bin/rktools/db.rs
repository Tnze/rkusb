use std::fs::File;

use memmap2::Mmap;
use rkusb::{RkDevice, image::BootImage};

use crate::common;

#[derive(clap::Args)]
pub struct Args {
    #[arg(help = "The image file path, eg: rk3588_spl_loader_v1.19.113.bin")]
    path: String,
    #[arg(long, help = "Bus number of the device")]
    bus: Option<u8>,
    #[arg(long, help = "Address of the device")]
    addr: Option<u8>,
}

pub fn exec(usb_ctx: rusb::Context, args: &Args) -> Result<(), Box<dyn std::error::Error>> {
    let selected_device = common::select_device_by_bus_addr(usb_ctx, args.bus, args.addr)?;
    let mut device = RkDevice::open(&selected_device).expect("Failed to open Rockusb");

    let file = File::open(&args.path).expect("Failed to open image file");
    let mmap = unsafe { Mmap::map(&file).expect("Failed to create memory map") };
    device.download_boot(BootImage::new(&mmap[..]))?;
    Ok(())
}
