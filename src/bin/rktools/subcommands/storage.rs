use clap::Subcommand;
use rkusb::RkDevice;
use std::time::Duration;

use crate::{common, util::parse_u8};

#[derive(clap::Args)]
pub struct Args {
    #[arg(long, help = "Bus number of the device")]
    bus: Option<u8>,
    #[arg(long, help = "Address of the device")]
    addr: Option<u8>,
    #[arg(long, value_parser = humantime::parse_duration, help = "Wait for device with timeout (e.g., 30s, 1m)")]
    wait: Option<Duration>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    #[command(about = "Get current storage selection", visible_alias("g"))]
    Get,
    #[command(about = "Set current storage selection", visible_alias("s"))]
    Set(SetArgs),
    #[command(about = "Read flash info", visible_alias("i"))]
    Info,
}

#[derive(clap::Args)]
struct SetArgs {
    #[arg(
        help = "Storage code (1=emmc, 2=sd, 9=spinor; supports decimal or 0x-prefixed hex)",
        value_parser = parse_u8
    )]
    storage: u8,
}

pub fn exec(usb_ctx: rusb::Context, args: &Args) -> Result<(), Box<dyn std::error::Error>> {
    let selected_device = common::find_device(&usb_ctx, args.bus, args.addr, args.wait)?;
    let mut rkdev = RkDevice::open(&selected_device)?;

    match &args.command {
        Command::Get => {
            let storage = rkdev.read_storage()?;
            match storage {
                Some(code) => println!("Current storage: {} ({})", code, storage_name(code)),
                None => println!("Current storage: none"),
            }
        }
        Command::Set(set_args) => {
            rkdev.switch_storage(set_args.storage)?;
            println!(
                "Storage switched to {} ({})",
                set_args.storage,
                storage_name(set_args.storage)
            );
        }
        Command::Info => {
            println!("{:#?}", rkdev.read_storage_info()?);
        }
    }

    Ok(())
}
fn storage_name(code: u8) -> &'static str {
    match code {
        1 => "eMMC",
        2 => "SD",
        9 => "SPI NOR",
        11 => "NVMe",
        _ => "Unknown",
    }
}
