use clap::Subcommand;
use gpt::{GptDisk, disk::LogicalBlockSize, partition::Partition};
use log::error;
use memmap2::{Mmap, MmapOptions};
use rkusb::{RkDevice, RkFlashInfo, storage_name};
use std::{
    collections::{BTreeMap, HashSet},
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
    time::Duration,
};
use thiserror::Error;

use crate::{
    common,
    progress::ProgressBar,
    storage::{DEFAULT_IO_TIMEOUT, DEFAULT_LBA_SUBCODE, RkBlockDevice},
    util::{
        format_bytes, parse_u8, partition_table_filename, sanitize_filename, storage_label,
    },
};

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
        value_parser = humantime::parse_duration,
        default_value = "5s",
        help = "USB timeout per transfer chunk (the whole operation is bounded by partition/range size)"
    )]
    timeout: Duration,

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
        about = "Print GPT partition table of current storage and save it to a file",
        visible_alias("ls")
    )]
    Table(TableArgs),
    #[command(about = "Read a GPT partition to file", visible_alias("r"))]
    Read(PartitionTransferArgs),
    #[command(about = "Write file to a GPT partition", visible_alias("w"))]
    Write(PartitionTransferArgs),
    #[command(
        about = "Dump all GPT partitions with progress and save a partition table file",
        visible_alias("rall")
    )]
    Dump(DumpArgs),
}

#[derive(clap::Args)]
struct TableArgs {
    #[arg(short, long, help = "Override output path of the generated partition table file")]
    output: Option<String>,
    #[arg(long, help = "Print the table only; do not write a file")]
    no_save: bool,
}

#[derive(clap::Args)]
struct DumpArgs {
    #[arg(help = "Output directory (default: <storage>_<size>)")]
    dir: Option<String>,
    #[arg(
        long,
        value_delimiter = ',',
        help = "Comma-separated GPT partition names to skip (e.g. userdata,cache)"
    )]
    exclude: Vec<String>,
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
    let timeout = if args.timeout.is_zero() {
        DEFAULT_IO_TIMEOUT
    } else {
        args.timeout
    };

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
            let info = rkdev.read_storage_info()?;
            let storage = rkdev.read_storage()?;
            let lb_size = match info.lba_size() {
                1 => LogicalBlockSize::Lb512,
                8 => LogicalBlockSize::Lb4096,
                x => {
                    eprintln!("Unsupported LBA size: {x}");
                    return Ok(());
                }
            };
            let mut disk = gpt::GptConfig::new()
                .writable(false)
                .logical_block_size(lb_size)
                .open_from_device(RkBlockDevice::try_from(&mut rkdev)?)?;
            let partitions = disk.partitions();

            match &partition_args.command {
                PartitionCommand::Table(table_args) => {
                    let text = format_partition_table(storage, &info, lb_size, partitions);
                    print!("{text}");
                    if !table_args.no_save {
                        let path = table_args.output.clone().unwrap_or_else(|| {
                            partition_table_filename(storage, &info)
                        });
                        fs::write(&path, &text)?;
                        println!("Wrote partition table to {path}");
                    }
                }
                PartitionCommand::Read(read_args) => {
                    let part = select_partition(partitions, read_args)?.clone();
                    exec_partition_read(&mut disk, part, &read_args.path, timeout)?;
                }
                PartitionCommand::Write(write_args) => {
                    let part = select_partition(partitions, write_args)?.clone();
                    exec_partition_write(&mut disk, part, &write_args.path, timeout)?;
                }
                PartitionCommand::Dump(dump_args) => {
                    let parts: Vec<(u32, Partition)> =
                        partitions.iter().map(|(i, p)| (*i, p.clone())).collect();
                    exec_partition_dump(
                        &mut disk,
                        storage,
                        &info,
                        lb_size,
                        &parts,
                        dump_args,
                        timeout,
                    )?;
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
    #[error("input file size exceeds partition size")]
    InputTooLarge,
}

fn select_partition<'a>(
    partitions: &'a BTreeMap<u32, Partition>,
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

fn format_partition_table(
    storage: Option<u8>,
    info: &RkFlashInfo,
    lb_size: LogicalBlockSize,
    partitions: &BTreeMap<u32, Partition>,
) -> String {
    let mut out = String::new();
    let storage_desc = match storage {
        Some(code) => format!("{} (code={code})", storage_name(code)),
        None => "None".to_string(),
    };
    let lb_bytes = match lb_size {
        LogicalBlockSize::Lb512 => 512,
        LogicalBlockSize::Lb4096 => 4096,
    };
    out.push_str("# rkusb GPT partition table\n");
    out.push_str(&format!("# Storage: {storage_desc}\n"));
    out.push_str(&format!(
        "# Flash size: {} ({} sectors of 512 bytes)\n",
        format_bytes(info.flash_size_bytes()),
        info.flash_size_sectors()
    ));
    out.push_str(&format!("# GPT logical block size: {lb_bytes} bytes\n"));
    out.push_str(&format!("# Label: {}\n", storage_label(storage, info)));
    out.push('\n');
    out.push_str(&format!(
        "{:>4}  {:<20} {:>12} {:>12} {:>12} {:>12}  {}\n",
        "idx", "name", "first_lba", "last_lba", "sectors", "size", "guid"
    ));

    for (index, part) in partitions {
        let sectors = part.last_lba.saturating_sub(part.first_lba).saturating_add(1);
        let bytes = sectors.saturating_mul(lb_bytes);
        out.push_str(&format!(
            "{index:>4}  {:<20} {:>12} {:>12} {:>12} {:>12}  {}\n",
            part.name,
            part.first_lba,
            part.last_lba,
            sectors,
            format_bytes(bytes),
            part.part_guid
        ));
    }
    if partitions.is_empty() {
        out.push_str("# (no GPT partitions found)\n");
    }
    out
}

fn exec_partition_read<T: rusb::UsbContext>(
    disk: &mut GptDisk<RkBlockDevice<T>>,
    part: Partition,
    path: &str,
    timeout: Duration,
) -> Result<(), PartitionTransferError> {
    let output_len = part
        .bytes_len(*disk.logical_block_size())
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
    let pos = u32::try_from(part.first_lba)
        .inspect_err(|e| error!("partition first_lba is out of u32 range for read: {e}"))
        .map_err(|_| PartitionTransferError::LbaOverflow)?;
    let mut bar = ProgressBar::new(
        format!(
            "Read '{}' {}",
            part.name,
            format_bytes(output_len)
        ),
        output_len,
    );
    disk.device_mut()
        .read_lba_with_progress(
            pos,
            &mut output_map,
            DEFAULT_LBA_SUBCODE,
            timeout,
            &mut |done, total| bar.set(done, total),
        )
        .inspect_err(|e| error!("device read_lba failed: {e}"))
        .map_err(|_| PartitionTransferError::DeviceTransfer)?;
    bar.finish();
    output_map
        .flush()
        .inspect_err(|e| error!("failed to flush output map: {e}"))?;

    println!(
        "Read partition '{}' OK, {} bytes ({}) -> {}",
        part.name,
        output_len,
        format_bytes(output_len),
        path
    );
    Ok(())
}

fn exec_partition_write<T: rusb::UsbContext>(
    disk: &mut GptDisk<RkBlockDevice<T>>,
    part: Partition,
    path: &str,
    timeout: Duration,
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
        .bytes_len(*disk.logical_block_size())
        .inspect_err(|e| error!("failed to get partition byte length for write: {e}"))?;
    if input_len > partition_bytes {
        error!(
            "input file is larger than partition: input_len={} partition='{}' partition_bytes={}",
            input_len, part.name, partition_bytes
        );
        return Err(PartitionTransferError::InputTooLarge);
    }
    let pos = u32::try_from(part.first_lba)
        .inspect_err(|e| error!("partition first_lba is out of u32 range for write: {e}"))
        .map_err(|_| PartitionTransferError::LbaOverflow)?;
    let mut bar = ProgressBar::new(
        format!(
            "Write '{}' {}",
            part.name,
            format_bytes(input_len)
        ),
        input_len,
    );
    disk.device_mut()
        .write_lba_with_progress(
            pos,
            &input_map,
            DEFAULT_LBA_SUBCODE,
            timeout,
            &mut |done, total| bar.set(done, total),
        )
        .inspect_err(|e| error!("device write_lba failed: {e}"))
        .map_err(|_| PartitionTransferError::DeviceTransfer)?;
    bar.finish();

    println!(
        "Wrote partition '{}' OK, {} bytes ({}) <- {}",
        part.name,
        input_len,
        format_bytes(input_len),
        path
    );
    Ok(())
}

fn exec_partition_dump<T: rusb::UsbContext>(
    disk: &mut GptDisk<RkBlockDevice<T>>,
    storage: Option<u8>,
    info: &RkFlashInfo,
    lb_size: LogicalBlockSize,
    parts: &[(u32, Partition)],
    args: &DumpArgs,
    timeout: Duration,
) -> Result<(), PartitionTransferError> {
    let label = storage_label(storage, info);
    let out_dir = PathBuf::from(args.dir.clone().unwrap_or_else(|| label.clone()));
    fs::create_dir_all(&out_dir)?;

    let table_name = partition_table_filename(storage, info);
    let table_path = out_dir.join(&table_name);
    let mut table_map = BTreeMap::new();
    for (index, part) in parts {
        table_map.insert(*index, part.clone());
    }
    let table_text = format_partition_table(storage, info, lb_size, &table_map);
    fs::write(&table_path, &table_text)?;
    print!("{table_text}");
    println!("Wrote partition table to {}", table_path.display());

    let exclude: HashSet<String> = args.exclude.iter().cloned().collect();
    let selected: Vec<(u32, Partition)> = parts
        .iter()
        .filter(|(_, part)| !exclude.contains(&part.name))
        .cloned()
        .collect();

    if selected.is_empty() {
        println!("No partitions to dump");
        return Ok(());
    }

    println!(
        "Dumping {} partition(s) to {} (storage {}, {})",
        selected.len(),
        out_dir.display(),
        storage.map(storage_name).unwrap_or("Unknown"),
        format_bytes(info.flash_size_bytes())
    );

    let mut used_names = HashSet::new();
    for (i, (index, part)) in selected.iter().enumerate() {
        let file_name = unique_image_name(&part.name, &mut used_names);
        let path = out_dir.join(&file_name);
        println!(
            "[{}/{}] #{} {} -> {}",
            i + 1,
            selected.len(),
            index,
            part.name,
            path.display()
        );
        exec_partition_read(disk, part.clone(), path_to_str(&path)?, timeout)?;
    }

    println!(
        "Dump complete: {} partition(s) saved under {}",
        selected.len(),
        out_dir.display()
    );
    Ok(())
}

fn unique_image_name(name: &str, used: &mut HashSet<String>) -> String {
    let base = sanitize_filename(name);
    let mut candidate = format!("{base}.img");
    let mut n = 2;
    while used.contains(&candidate) {
        candidate = format!("{base}_{n}.img");
        n += 1;
    }
    used.insert(candidate.clone());
    candidate
}

fn path_to_str(path: &Path) -> Result<&str, PartitionTransferError> {
    path.to_str().ok_or_else(|| {
        PartitionTransferError::FileIo(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "path is not valid UTF-8",
        ))
    })
}
