use clap::{ArgAction, Parser, Subcommand};

mod common;
mod db;
mod info;
mod ls;
mod rst;
mod wait;

#[derive(Parser)]
#[command(version, about, long_about = None, propagate_version = true)]
struct Cli {
    #[arg(
        short,
        long,
        action = ArgAction::Count,
        global = true,
        help = "Increase log verbosity (-v: info, -vv: debug, -vvv: trace)"
    )]
    verbose: u8,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    #[command(about = "List all rkusb devices", visible_alias("ls"))]
    List(ls::Args),
    #[command(about = "Download bootloader", visible_alias("db"))]
    DownloadBoot(db::Args),
    #[command(about = "Detect file content")]
    Info(info::Args),
    #[command(about = "Reset device", visible_alias("rst"))]
    Reset(rst::Args),
    #[command(about = "Wait for device to be available")]
    Wait(wait::Args),
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    init_logger(cli.verbose);

    match &cli.command {
        Commands::List(args) => ls::exec(rusb::Context::new()?, args)?,
        Commands::DownloadBoot(args) => db::exec(rusb::Context::new()?, args)?,
        Commands::Info(args) => info::exec(args)?,
        Commands::Reset(args) => rst::exec(rusb::Context::new()?, args)?,
        Commands::Wait(args) => wait::exec(rusb::Context::new()?, args)?,
    }
    Ok(())
}

fn init_logger(verbose: u8) {
    let default_level = match verbose {
        0 => "warn",
        1 => "info",
        2 => "debug",
        _ => "trace",
    };

    let mut builder =
        env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(default_level));
    builder.format_timestamp_secs();
    builder.init();
}
