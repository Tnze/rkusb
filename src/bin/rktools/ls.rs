use rkusb::RkUsbType;
use rusb::UsbContext;

#[derive(clap::Args)]
pub struct Args {
    #[arg(short, long, help = "Show MASKROM devices")]
    maskrom: bool,
    #[arg(short, long, help = "Show Loader devices")]
    loader: bool,
    #[arg(long, help = "Show MSC devices")]
    msc: bool,
}

pub fn exec(usb_ctx: rusb::Context, args: &Args) -> Result<(), Box<dyn std::error::Error>> {
    for device in usb_ctx.devices()?.iter() {
        match device.device_descriptor() {
            Err(err) => {
                eprintln!("Failed to get device descriptor for {device:?}: {err}");
                continue;
            }
            Ok(desc) => {
                let Some(rkusb_type) = RkUsbType::detect(&desc) else {
                    continue;
                };

                match rkusb_type {
                    RkUsbType::Maskrom if args.maskrom => {}
                    RkUsbType::Loader if args.loader => {}
                    RkUsbType::MSC if args.msc => {}
                    _ if !(args.maskrom || args.loader || args.msc) => {}
                    _ => {
                        continue;
                    }
                }
                println!("{device:?} Mode: {rkusb_type:?}");
            }
        }
    }
    Ok(())
}
