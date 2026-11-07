use std::time::Duration;

use rkusb::{otp, RkDevice};

use crate::common;

#[derive(clap::Args)]
pub struct Args {
    #[arg(long, help = "Bus number of the device")]
    bus: Option<u8>,
    #[arg(long, help = "Address of the device")]
    addr: Option<u8>,
    #[arg(long, value_parser = humantime::parse_duration, help = "Wait for device with timeout (e.g., 30s, 1m)")]
    wait: Option<Duration>,
    #[arg(long, help = "Also print the raw miniloader console log")]
    log: bool,
    #[arg(
        long,
        default_value_t = 8192,
        help = "COM log read size in bytes"
    )]
    log_size: usize,
}

pub fn exec(usb_ctx: rusb::Context, args: &Args) -> Result<(), Box<dyn std::error::Error>> {
    let selected_device = common::find_device(&usb_ctx, args.bus, args.addr, args.wait)?;
    let mut rkdev = RkDevice::open(&selected_device)?;

    match rkdev.read_capability(std::time::Duration::from_secs(5)) {
        Ok(cap) => {
            println!("{}", otp::format_capability(&cap));
            if cap[0] & (1 << 7) == 0 {
                println!(
                    "note: loader did not advertise Read Secure Mode; using COM log instead"
                );
            }
        }
        Err(err) => eprintln!("warning: READ_CAPABILITY failed: {err}"),
    }

    let mut buf = vec![0u8; args.log_size.max(64)];
    match rkdev.read_com_log(&mut buf) {
        Ok(n) => {
            let raw = &buf[..n];
            let end = raw
                .iter()
                .rposition(|&b| b != 0)
                .map(|i| i + 1)
                .unwrap_or(0);
            let log = String::from_utf8_lossy(&raw[..end]);
            let (verdict, hits) = otp::parse_secure_boot_log(&log);
            println!("Secure Boot (from USB COM log, no UART needed):");
            println!("  {}", verdict.as_str());
            if hits.is_empty() {
                println!("  (no SecureMode/SecureBootEn lines in the log)");
            } else {
                println!("  matching lines:");
                for h in hits {
                    println!("    {h}");
                }
            }
            if args.log {
                println!("\n--- COM log ({} bytes) ---", end);
                println!("{log}");
            }
        }
        Err(err) => {
            eprintln!("READ_COM_LOG failed: {err}");
            eprintln!(
                "This loader may not export the UART ring buffer over USB.\n\
                 Without COM log or a debug UART, chip-level Secure Boot cannot be read\n\
                 from NS OTP. Put the board in Loader mode (not Maskrom) and retry."
            );
        }
    }
    Ok(())
}
