use std::time::Duration;

use rkusb::{chip_info_tag, otp, RkDevice};

use crate::{common, util::parse_u32};

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
        default_value = "auto",
        help = "OTP map: auto, rk3588, rk3568, px30, generic"
    )]
    map: String,
    #[arg(long, help = "Print raw hex dump only, skip named-field decode")]
    raw: bool,
    #[arg(
        help = "Number of OTP bytes to read (decimal or 0x-prefixed hex)",
        default_value = "128",
        value_parser = parse_u32
    )]
    length: u32,
}

pub fn exec(usb_ctx: rusb::Context, args: &Args) -> Result<(), Box<dyn std::error::Error>> {
    if args.length == 0 {
        return Err("OTP length must be greater than 0".into());
    }
    let map = otp::OtpMapKind::parse(&args.map)
        .ok_or_else(|| format!("unknown OTP map '{}' (auto|rk3588|rk3568|px30|generic)", args.map))?;
    let selected_device = common::find_device(&usb_ctx, args.bus, args.addr, args.wait)?;
    let mut rkdev = RkDevice::open(&selected_device)?;

    let chip_tag = match rkdev.read_chip_info() {
        Ok(info) => Some(chip_info_tag(&info)),
        Err(err) => {
            eprintln!("warning: READ_CHIP_INFO failed ({err}), map detection may be generic");
            None
        }
    };

    let mut buf = vec![0u8; args.length as usize];
    rkdev.read_otp(&mut buf)?;

    if args.raw {
        println!("OTP ({} bytes):", buf.len());
        crate::util::hexdump(&buf);
        return Ok(());
    }

    print!(
        "{}",
        otp::format_otp_report(&buf, chip_tag.as_deref(), map)
    );
    Ok(())
}
