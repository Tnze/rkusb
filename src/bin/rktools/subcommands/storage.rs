use clap::Subcommand;
use gpt::{GptDisk, disk::LogicalBlockSize::Lb512, partition::Partition};
use log::error;
use memmap2::{Mmap, MmapOptions};
use rkusb::RkDevice;
use std::{
    fs::{File, OpenOptions},
    time::Duration,
};
use thiserror::Error;

use crate::{
    common,
    storage::{DEFAULT_IO_TIMEOUT, DEFAULT_LBA_SUBCODE, RkBlockDevice, SECTOR_SIZE},
    util::parse_u8,
};

const RW_SECTORS_PER_CHUNK: u64 = 128;

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
    #[command(about = "Get or set current storage selection", visible_alias("sl"))]
    Select(SelectArgs),
    #[command(about = "Read flash info", visible_alias("i"))]
    Info,
    #[command(about = "Partition operations")]
    Partition(PartitionArgs),
}

#[derive(clap::Args)]
struct SelectArgs {
    #[arg(
        help = "Optional storage code (1=emmc, 2=sd, 9=spinor, 11=nvme; decimal or 0x-prefixed hex). Omit to query current selection.",
        value_parser = parse_u8
    )]
    target: Option<u8>,
}

#[derive(clap::Args)]
struct PartitionArgs {
    #[command(subcommand)]
    command: PartitionCommand,
}

#[derive(Subcommand)]
enum PartitionCommand {
    #[command(
        about = "Print GPT partition table of current storage",
        visible_alias("ls")
    )]
    Table,
    #[command(about = "Read a GPT partition to file", visible_alias("r"))]
    Read(PartitionTransferArgs),
    #[command(about = "Write file to a GPT partition", visible_alias("w"))]
    Write(PartitionTransferArgs),
}

#[derive(clap::Args)]
#[command(group(
    clap::ArgGroup::new("partition_selector")
        .required(true)
        .multiple(false)
        .args(["name", "guid", "index"])
))]
struct PartitionTransferArgs {
    #[arg(long, help = "Select partition by GPT name")]
    name: Option<String>,
    #[arg(long, help = "Select partition by partition GUID")]
    guid: Option<String>,
    #[arg(long, help = "Select partition by GPT index")]
    index: Option<u32>,
    #[arg(help = "Input/output file path")]
    path: String,
}

pub fn exec(usb_ctx: rusb::Context, args: &Args) -> Result<(), Box<dyn std::error::Error>> {
    let selected_device = common::find_device(&usb_ctx, args.bus, args.addr, args.wait)?;
    let mut rkdev = RkDevice::open(&selected_device)?;

    match &args.command {
        Command::Select(select_args) => {
            if let Some(target) = select_args.target {
                rkdev.switch_storage(target)?;
                println!("Switch to {} ({})", target, storage_name(target));
            }
            match rkdev.read_storage()? {
                Some(code) => println!("Current storage: {} ({})", code, storage_name(code)),
                None => println!("Current storage: None"),
            }
        }
        Command::Info => {
            println!("{:#?}", rkdev.read_storage_info()?);
        }
        Command::Partition(partition_args) => {
            let mut disk = gpt::GptConfig::new()
                .writable(false)
                .logical_block_size(Lb512)
                .open_from_device(RkBlockDevice::try_from(&mut rkdev)?)?;
            let partitions = disk.partitions();

            match &partition_args.command {
                PartitionCommand::Table => {
                    println!("GPT partitions: {:#X?}", partitions);
                }
                PartitionCommand::Read(read_args) => {
                    let part = select_partition(partitions, read_args)?.clone();
                    exec_partition_read(&mut disk, part, &read_args.path)?;
                }
                PartitionCommand::Write(write_args) => {
                    let part = select_partition(partitions, write_args)?.clone();
                    exec_partition_write(&mut disk, part, &write_args.path)?;
                }
            }
        }
    }

    Ok(())
}

#[derive(Debug, Error)]
enum SelectPartitionError {
    #[error("partition name '{0}' not found")]
    NameNotFound(String),
    #[error("partition GUID '{0}' not found")]
    GuidNotFound(String),
    #[error("partition index '{0}' not found")]
    IndexNotFound(u32),
    #[error("unreachable selector state")]
    UnreachableSelectorState,
}

#[derive(Debug, Error)]
enum PartitionTransferError {
    #[error("file operation failed")]
    FileIo(#[from] std::io::Error),
    #[error("memory map operation failed")]
    MemoryMap(std::io::Error),
    #[error("size calculation overflow")]
    SizeOverflow,
    #[error("LBA calculation overflow")]
    LbaOverflow,
    #[error("device transfer failed")]
    DeviceTransfer,
    #[error("input file size does not match partition size")]
    InputSizeMismatch,
}

fn select_partition<'a>(
    partitions: &'a std::collections::BTreeMap<u32, Partition>,
    selector: &PartitionTransferArgs,
) -> Result<&'a Partition, SelectPartitionError> {
    if let Some(name) = selector.name.as_deref() {
        return partitions
            .iter()
            .find(|(_, p)| p.name == name)
            .map(|(_, p)| p)
            .ok_or_else(|| SelectPartitionError::NameNotFound(name.to_owned()));
    }

    if let Some(guid) = selector.guid.as_deref() {
        let wanted = guid.to_ascii_lowercase();
        return partitions
            .iter()
            .find(|(_, p)| p.part_guid.to_string().to_ascii_lowercase() == wanted)
            .map(|(_, p)| p)
            .ok_or_else(|| SelectPartitionError::GuidNotFound(guid.to_owned()));
    }

    if let Some(index) = selector.index {
        return partitions
            .iter()
            .find(|(id, _)| **id == index)
            .map(|(_, p)| p)
            .ok_or(SelectPartitionError::IndexNotFound(index));
    }

    Err(SelectPartitionError::UnreachableSelectorState)
}

fn exec_partition_read<T: rusb::UsbContext>(
    disk: &mut GptDisk<RkBlockDevice<T>>,
    part: Partition,
    path: &str,
) -> Result<(), PartitionTransferError> {
    let output_len = part
        .bytes_len(Lb512)
        .inspect_err(|e| error!("failed to get partition byte length for read: {e}"))?;
    let output = OpenOptions::new()
        .read(true)
        .create(true)
        .truncate(true)
        .write(true)
        .open(path)
        .inspect_err(|e| error!("failed to open output file: {e}"))?;
    output
        .set_len(output_len)
        .inspect_err(|e| error!("failed to resize output file: {e}"))?;

    let output_len_usize = usize::try_from(output_len)
        .inspect_err(|e| error!("output length does not fit in usize: {e}"))
        .map_err(|_| PartitionTransferError::SizeOverflow)?;
    let mut output_map = unsafe {
        // SAFETY: The file is opened read/write, resized to output_len, and the mapping
        // is only accessed within bounds we compute from checked arithmetic below.
        MmapOptions::new()
            .len(output_len_usize)
            .map_mut(&output)
            .inspect_err(|e| error!("failed to map output file: {e}"))
            .map_err(PartitionTransferError::MemoryMap)?
    };

    const CHUNK_BYTES: u64 = RW_SECTORS_PER_CHUNK * SECTOR_SIZE;

    for (i, chunk) in output_map.chunks_mut(CHUNK_BYTES as usize).enumerate() {
        let step = u64::try_from(i)
            .ok()
            .and_then(|v| v.checked_mul(RW_SECTORS_PER_CHUNK))
            .ok_or_else(|| {
                error!("step overflow while reading partition: chunk_index={i}, sectors_per_chunk={RW_SECTORS_PER_CHUNK}");
                PartitionTransferError::LbaOverflow
            })?;
        let pos = part.first_lba.checked_add(step).ok_or_else(|| {
            error!(
                "LBA add overflow while reading partition: first_lba={}, step={step}",
                part.first_lba
            );
            PartitionTransferError::LbaOverflow
        })?;
        let pos = u32::try_from(pos)
            .inspect_err(|e| error!("LBA is out of u32 range while reading partition: {e}"))
            .map_err(|_| PartitionTransferError::LbaOverflow)?;

        disk.device_mut()
            .read_lba(pos, chunk, DEFAULT_LBA_SUBCODE, DEFAULT_IO_TIMEOUT)
            .inspect_err(|e| error!("device read_lba failed: {e}"))
            .map_err(|_| PartitionTransferError::DeviceTransfer)?;
    }
    output_map
        .flush()
        .inspect_err(|e| error!("failed to flush output map: {e}"))?;

    println!(
        "Read partition '{}' OK, {} bytes -> {}",
        part.name, output_len, path
    );
    Ok(())
}

fn exec_partition_write<T: rusb::UsbContext>(
    disk: &mut GptDisk<RkBlockDevice<T>>,
    part: Partition,
    path: &str,
) -> Result<(), PartitionTransferError> {
    let input = File::open(path).inspect_err(|e| error!("failed to open input file: {e}"))?;
    let input_map = unsafe {
        // SAFETY: input file is opened read-only and the mapping is read-only.
        Mmap::map(&input)
            .inspect_err(|e| error!("failed to map input file: {e}"))
            .map_err(PartitionTransferError::MemoryMap)?
    };
    let input_len = u64::try_from(input_map.len())
        .inspect_err(|e| error!("input length does not fit in u64: {e}"))
        .map_err(|_| PartitionTransferError::SizeOverflow)?;
    let partition_bytes = part
        .bytes_len(Lb512)
        .inspect_err(|e| error!("failed to get partition byte length for write: {e}"))?;
    if input_len != partition_bytes {
        error!(
            "input file size mismatch: input_len={} partition='{}' partition_bytes={}",
            input_len, part.name, partition_bytes
        );
        return Err(PartitionTransferError::InputSizeMismatch);
    }

    const CHUNK_BYTES: u64 = RW_SECTORS_PER_CHUNK * SECTOR_SIZE;

    for (i, chunk) in input_map.chunks(CHUNK_BYTES as usize).enumerate() {
        let step = u64::try_from(i)
            .ok()
            .and_then(|v| v.checked_mul(RW_SECTORS_PER_CHUNK))
            .ok_or_else(|| {
                error!("step overflow while writing partition: chunk_index={i}, sectors_per_chunk={RW_SECTORS_PER_CHUNK}");
                PartitionTransferError::LbaOverflow
            })?;
        let pos = part.first_lba.checked_add(step).ok_or_else(|| {
            error!(
                "LBA add overflow while writing partition: first_lba={}, step={step}",
                part.first_lba
            );
            PartitionTransferError::LbaOverflow
        })?;
        let pos = u32::try_from(pos)
            .inspect_err(|e| error!("LBA is out of u32 range while writing partition: {e}"))
            .map_err(|_| PartitionTransferError::LbaOverflow)?;

        debug_assert!(
            chunk.len().is_multiple_of(SECTOR_SIZE as usize),
            "input chunk must be sector-aligned"
        );
        disk.device_mut()
            .write_lba(pos, chunk, DEFAULT_LBA_SUBCODE, DEFAULT_IO_TIMEOUT)
            .inspect_err(|e| error!("device write_lba failed: {e}"))
            .map_err(|_| PartitionTransferError::DeviceTransfer)?;
    }

    println!(
        "Wrote partition '{}' OK, {} bytes <- {}",
        part.name, input_len, path
    );
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
