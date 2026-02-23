use clap::{Parser, Subcommand};

mod db;
mod ls;
mod info;

#[derive(Parser)]
#[command(version, about, long_about = None, propagate_version = true)]
struct Cli {
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
    Info(info::Args)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    match &cli.command {
        Commands::List(args) => ls::exec(rusb::Context::new()?, args),
        Commands::DownloadBoot(args) => db::exec(rusb::Context::new()?, args),
        Commands::Info(args) => info::exec(args),
    }
}
