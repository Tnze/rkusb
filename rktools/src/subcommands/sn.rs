use std::time::Duration;

use clap::Subcommand;
use rkusb::{RkDevice, VendorBackend};

use crate::common;

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
        default_value = "vendor",
        help = "RKDevInfoWriteTool backend: vendor (vnvm) or rpmb"
    )]
    backend: String,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    #[command(about = "Write serial number to vendor storage", visible_alias("w"))]
    Write {
        #[arg(help = "Serial number string (max 504 bytes)")]
        sn: String,
    },
}

pub fn exec(usb_ctx: rusb::Context, args: &Args) -> Result<(), Box<dyn std::error::Error>> {
    let backend = VendorBackend::parse(&args.backend)
        .ok_or_else(|| format!("unknown backend '{}' (vendor|rpmb)", args.backend))?;
    let selected_device = common::find_device(&usb_ctx, args.bus, args.addr, args.wait)?;
    let mut rkdev = RkDevice::open(&selected_device)?;

    match &args.command {
        None => match rkdev.read_sn_with_backend(backend)? {
            Some(sn) => println!("SN: {sn}"),
            None => println!("No serial number"),
        },
        Some(Command::Write { sn }) => {
            rkdev.write_sn_with_backend(sn, backend)?;
            println!("Write serial number '{sn}'");
        }
    }
    Ok(())
}
