#[derive(clap::Args)]
pub struct Args;

pub fn exec(usb_ctx: rusb::Context, args: &Args) -> Result<(), Box<dyn std::error::Error>> {
    Ok(())
}
