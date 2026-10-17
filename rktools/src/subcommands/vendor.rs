use std::time::Duration;

use clap::Subcommand;
use rkusb::{
    format_mac, parse_mac, RkDevice, VendorBackend, VendorItemId,
};

use crate::{common, util::hexdump};

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
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    #[command(about = "Read a vendor-storage item (sn, wifi-mac, lan-mac, bt-mac, imei, or numeric id)")]
    Read {
        #[arg(help = "Item name or numeric vendor id")]
        item: String,
    },
    #[command(about = "Write a vendor-storage item")]
    Write {
        #[arg(help = "Item name or numeric vendor id")]
        item: String,
        #[arg(help = "Value (string for SN/IMEI, hex MAC for wifi/lan/bt)")]
        value: String,
    },
    #[command(about = "Dump SN and MAC items")]
    Dump,
}

fn parse_backend(s: &str) -> Result<VendorBackend, Box<dyn std::error::Error>> {
    VendorBackend::parse(s).ok_or_else(|| format!("unknown backend '{s}' (vendor|rpmb)").into())
}

fn parse_item(s: &str) -> Result<u16, Box<dyn std::error::Error>> {
    if let Some(item) = VendorItemId::parse_slug(s) {
        return Ok(item as u16);
    }
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        return u16::from_str_radix(hex, 16).map_err(|e| e.to_string().into());
    }
    s.parse::<u16>()
        .map_err(|_| format!("unknown vendor item '{s}'").into())
}

pub fn exec(usb_ctx: rusb::Context, args: &Args) -> Result<(), Box<dyn std::error::Error>> {
    let backend = parse_backend(&args.backend)?;
    let selected_device = common::find_device(&usb_ctx, args.bus, args.addr, args.wait)?;
    let mut rkdev = RkDevice::open(&selected_device)?;

    match &args.command {
        Command::Read { item } => {
            let id = parse_item(item)?;
            print_item(&mut rkdev, id, backend)?;
        }
        Command::Write { item, value } => {
            let id = parse_item(item)?;
            write_item(&mut rkdev, id, backend, value)?;
            println!(
                "Wrote {} via {} backend",
                VendorItemId::from_code(id)
                    .map(VendorItemId::name)
                    .unwrap_or("item"),
                backend.name()
            );
        }
        Command::Dump => {
            println!("Vendor storage backend: {}", backend.name());
            for id in [
                VendorItemId::Sn,
                VendorItemId::WifiMac,
                VendorItemId::LanMac,
                VendorItemId::BtMac,
                VendorItemId::Imei,
            ] {
                if let Err(err) = print_item(&mut rkdev, id as u16, backend) {
                    println!("  {}: <error: {err}>", id.name());
                }
            }
        }
    }
    Ok(())
}

fn print_item<T: rusb::UsbContext>(
    rkdev: &mut RkDevice<T>,
    id: u16,
    backend: VendorBackend,
) -> Result<(), Box<dyn std::error::Error>> {
    let name = VendorItemId::from_code(id)
        .map(VendorItemId::name)
        .unwrap_or("item");
    match VendorItemId::from_code(id) {
        Some(VendorItemId::Sn) => match rkdev.read_sn_with_backend(backend)? {
            Some(sn) => println!("SN: {sn}"),
            None => println!("SN: <empty>"),
        },
        Some(item) if item.is_mac() => match rkdev.read_mac(item, backend)? {
            Some(mac) => println!("{}: {}", item.name(), format_mac(&mac)),
            None => println!("{}: <empty>", item.name()),
        },
        _ => {
            let mut buf = [0u8; 512];
            let n = rkdev.read_vendor_storage(id, backend, &mut buf)?;
            let data = &buf[..n];
            let end = data.iter().position(|&b| b == 0).unwrap_or(data.len());
            let text = String::from_utf8_lossy(&data[..end]);
            println!("{name} (id={id}, {n} bytes):");
            if !text.is_empty() && text.chars().all(|c| c.is_ascii_graphic() || c == ' ') {
                println!("  {text}");
            }
            hexdump(&data[..n.min(64)]);
        }
    }
    Ok(())
}

fn write_item<T: rusb::UsbContext>(
    rkdev: &mut RkDevice<T>,
    id: u16,
    backend: VendorBackend,
    value: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    match VendorItemId::from_code(id) {
        Some(VendorItemId::Sn) => rkdev.write_sn_with_backend(value, backend)?,
        Some(item) if item.is_mac() => {
            rkdev.write_mac(item, backend, parse_mac(value)?)?;
        }
        _ => rkdev.write_vendor_storage(id, backend, value.as_bytes())?,
    }
    Ok(())
}
