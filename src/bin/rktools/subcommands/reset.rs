use rkusb::RkDevice;
use std::time::Duration;

use crate::common;

#[derive(clap::Args)]
pub struct Args {
    #[arg(long, help = "Bus number of the device")]
    bus: Option<u8>,
    #[arg(long, help = "Address of the device")]
    addr: Option<u8>,
    #[arg(
        short,
        long,
        default_value_t = 0,
        help = "Reset subcode (0 for normal reset, 1 for power off)"
    )]
    subcode: u8,
    #[arg(long, help = "Wait for device with timeout (e.g., 30s, 1m)")]
    #[arg(long, value_parser = humantime::parse_duration, help = "Wait for device with timeout (e.g., 30s, 1m)")]
    wait: Option<Duration>,
}

pub fn exec(usb_ctx: rusb::Context, args: &Args) -> Result<(), Box<dyn std::error::Error>> {
    let selected_device = common::find_device(&usb_ctx, args.bus, args.addr, args.wait)?;

    let mut rkdev = RkDevice::open(&selected_device)?;
    rkdev.reset_device(args.subcode)?;
    println!("Device reset with subcode {}", args.subcode);

    Ok(())
}
