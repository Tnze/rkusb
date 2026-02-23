use std::fs::File;

use memmap2::Mmap;
use rkusb::{RkDevice, RkUsbType, image::BootImage};

#[derive(clap::Args)]
pub struct Args {
    #[arg(help = "The image file path, eg: rk3588_spl_loader_v1.19.113.bin")]
    path: String,
}

fn select_device<T: rusb::UsbContext>(usb_ctx: T) -> rusb::Result<Option<rusb::Device<T>>> {
    for dev in usb_ctx.devices()?.iter() {
        if let Some(RkUsbType::Maskrom) = RkUsbType::detect(&dev.device_descriptor()?) {
            return Ok(Some(dev));
        }
    }
    Ok(None)
}

pub fn exec(usb_ctx: rusb::Context, args: &Args) -> Result<(), Box<dyn std::error::Error>> {
    let Some(device) = select_device(usb_ctx)? else {
        eprintln!("No device found");
        return Ok(());
    };
    let mut device = RkDevice::open(&device)?;

    let file = File::open(&args.path)?;
    let mmap = unsafe { Mmap::map(&file)? };
    device.download_boot(BootImage::new(&mmap[..]))?;
    Ok(())
}
