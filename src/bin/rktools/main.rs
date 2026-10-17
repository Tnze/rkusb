use clap::{ArgAction, Parser, Subcommand};

mod common;
mod nolimit;
mod progress;
mod storage;
mod subcommands;
mod util;

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
    List(subcommands::list::Args),
    #[command(about = "Download bootloader", visible_alias("db"))]
    DownloadBoot(subcommands::download_boot::Args),
    #[command(about = "Detect file content")]
    Info(subcommands::info::Args),
    #[command(about = "Reset device", visible_alias("rst"))]
    Reset(subcommands::reset::Args),
    #[command(about = "LBA operations (read/write/erase)")]
    Lba(subcommands::lba::Args),
    #[command(about = "Wait for device to be available")]
    Wait(subcommands::wait::Args),
    #[command(about = "Query or switch current storage", visible_alias("st"))]
    Storage(subcommands::storage::Args),
    #[command(
        about = "Upgrade loader by writing generated IDBlock",
        visible_alias("ul")
    )]
    UpgradeLoader(subcommands::upgrade_loader::Args),
    #[command(about = "Read or write device serial number", visible_alias("serial"))]
    Sn(subcommands::sn::Args),
    #[command(
        about = "Read/write vendor storage (SN, MAC, IMEI) like RKDevInfoWriteTool",
        visible_alias("vs")
    )]
    Vendor(subcommands::vendor::Args),
    #[command(about = "Dump chip OTP / eFuse (xrock otp)")]
    Otp(subcommands::otp::Args),
    #[command(about = "Upgrade device by using update.img")]
    Upgrade(subcommands::upgrade::Args),
    #[command(
        about = "Batch-backup partitions per GPT (detects the 32 MiB 0xCC read wall)"
    )]
    Dump(subcommands::dump::Args),
    #[command(
        about = "Remove the rockusb 32 MiB read limit from a flash dump",
        visible_alias("nl")
    )]
    Nolimit {
        #[command(subcommand)]
        cmd: nolimit::NolimitCommands,
    },
}

fn main() {
    let cli = Cli::parse();
    init_logger(cli.verbose);
    if let Err(err) = handle_command(&cli.command) {
        eprintln!("error: {err}");
        std::process::exit(1);
    }
}

fn handle_command(cmd: &Commands) -> Result<(), Box<dyn std::error::Error>> {
    match cmd {
        Commands::List(args) => subcommands::list::exec(rusb::Context::new()?, args)?,
        Commands::DownloadBoot(args) => {
            subcommands::download_boot::exec(rusb::Context::new()?, args)?
        }
        Commands::Info(args) => subcommands::info::exec(args)?,
        Commands::Reset(args) => subcommands::reset::exec(rusb::Context::new()?, args)?,
        Commands::Lba(args) => subcommands::lba::exec(rusb::Context::new()?, args)?,
        Commands::Wait(args) => subcommands::wait::exec(rusb::Context::new()?, args)?,
        Commands::Storage(args) => subcommands::storage::exec(rusb::Context::new()?, args)?,
        Commands::UpgradeLoader(args) => {
            subcommands::upgrade_loader::exec(rusb::Context::new()?, args)?
        }
        Commands::Sn(args) => subcommands::sn::exec(rusb::Context::new()?, args)?,
        Commands::Vendor(args) => subcommands::vendor::exec(rusb::Context::new()?, args)?,
        Commands::Otp(args) => subcommands::otp::exec(rusb::Context::new()?, args)?,
        Commands::Upgrade(args) => subcommands::upgrade::exec(rusb::Context::new()?, args)?,
        Commands::Dump(args) => subcommands::dump::exec(rusb::Context::new()?, args)?,
        Commands::Nolimit { cmd } => nolimit::exec(cmd)?,
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
