use rkusb::RkDevice;

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
}

pub fn exec(usb_ctx: rusb::Context, args: &Args) -> Result<(), Box<dyn std::error::Error>> {
    let selected_device = common::select_device_by_bus_addr(usb_ctx, args.bus, args.addr)?;
    let mut rkdev = RkDevice::open(&selected_device)?;
    rkdev.reset_device(args.subcode)?;
    println!("Device reset with subcode {}", args.subcode);
    Ok(())
}
