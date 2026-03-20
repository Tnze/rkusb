use std::time::Duration;

use crate::common;

#[derive(clap::Args)]
pub struct Args {
    #[arg(long, help = "Bus number of the device")]
    bus: Option<u8>,
    #[arg(long, help = "Address of the device")]
    addr: Option<u8>,
    #[arg(
        long,
        value_parser = humantime::parse_duration,
        help = "Timeout (e.g., 30s, 1m, 2h; default: wait indefinitely)"
    )]
    timeout: Option<Duration>,
}

pub fn exec(usb_ctx: rusb::Context, args: &Args) -> Result<(), Box<dyn std::error::Error>> {
    let device = common::find_device(&usb_ctx, args.bus, args.addr, args.timeout)?;
    println!(
        "Device found: Bus {:03} Device {:03}",
        device.bus_number(),
        device.address()
    );
    Ok(())
}
