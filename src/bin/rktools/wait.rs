use crate::common;

#[derive(clap::Args)]
pub struct Args {
    #[arg(long, help = "Bus number of the device")]
    bus: Option<u8>,
    #[arg(long, help = "Address of the device")]
    addr: Option<u8>,
    #[arg(long, help = "Timeout (e.g., 30s, 1m, 2h; default: wait indefinitely)")]
    timeout: Option<String>,
}

pub fn exec(usb_ctx: rusb::Context, args: &Args) -> Result<(), Box<dyn std::error::Error>> {
    let timeout = args
        .timeout
        .as_ref()
        .map(|s| humantime::parse_duration(s))
        .transpose()?;
    let device = common::find_device(&usb_ctx, args.bus, args.addr, timeout)?;
    println!(
        "Device found: Bus {:03} Device {:03}",
        device.bus_number(),
        device.address()
    );
    Ok(())
}
