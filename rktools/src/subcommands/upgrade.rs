use std::{
    ffi::FromBytesUntilNulError,
    fs::File,
    io::{Cursor, Read, Seek, SeekFrom},
    time::Duration,
};

use gpt::{GptDisk, GptError, disk::LogicalBlockSize, partition::Partition};
use memmap2::Mmap;
use rkafp::rkaf::Image;
use rkusb::{
    RkDevice, RkUsbError, RkUsbType,
    image::{ImageError, RkFwImage},
};
use thiserror::Error;
use zerocopy::FromBytes;

use crate::{
    common::{self, DeviceSelectionError},
    storage::RkBlockDevice,
};

#[derive(clap::Args)]
pub struct Args {
    #[arg(help = "The path to update.img")]
    path: String,
    #[arg(long, help = "Bus number of the device")]
    bus: Option<u8>,
    #[arg(long, help = "Address of the device")]
    addr: Option<u8>,
    #[arg(long, value_parser = humantime::parse_duration, help = "Wait for device with timeout (e.g., 30s, 1m)")]
    wait: Option<Duration>,
    #[arg(
        long,
        value_parser = humantime::parse_duration,
        default_value = "300s",
        help = "Total timeout for this upgrade-loader command"
    )]
    timeout: Duration,
}

#[derive(Error, Debug)]
pub enum UpgradeError {
    #[error("Device selection error: {0}")]
    DeviceSelection(#[from] DeviceSelectionError),
    #[error("RkUsb error: {0}")]
    RkUsb(#[from] RkUsbError),
    #[error("GptError: {0}")]
    GptError(#[from] GptError),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Image parse error: {0}")]
    Image(#[from] ImageError),
    #[error("Cannot detect RkUsbType")]
    UnknownRkUsbType,
    #[error("parameter not found")]
    ParameterNotFound,
    #[error("partition not found")]
    PartitionNotFound,
    #[error("Invalid partition name: {0}")]
    InvalidPartitionName(FromBytesUntilNulError),
    #[error("Failed to write sparse image: {0}")]
    WriteSparseImageError(#[from] WriteSparseImageError),
    #[error("Parameter too large: {0}")]
    ParameterTooLarge(u64),
    #[error("Parameter too short: {0}")]
    ParameterTooShort(u64),
}

pub fn exec(usb_ctx: rusb::Context, args: &Args) -> Result<(), UpgradeError> {
    let selected_device = common::find_device(&usb_ctx, args.bus, args.addr, args.wait)?;
    let mut rkdev = RkDevice::open(&selected_device)?;

    let desc = selected_device
        .device_descriptor()
        .map_err(RkUsbError::from)?;
    let device_type = RkUsbType::detect(&desc).ok_or(UpgradeError::UnknownRkUsbType)?;
    let bus = selected_device.bus_number();
    let addr = selected_device.address();

    let file = File::open(&args.path)?;
    let mmap = unsafe { Mmap::map(&file)? };

    let rkfw = RkFwImage::new(&mmap)?;
    let boot = rkfw.boot_data()?;

    if device_type == RkUsbType::Maskrom {
        // TODO: Read capability to determine whether need download boot?
        println!("Download boot");
        rkdev.download_boot(boot)?;

        // TODO: Device might reset? re-enumerate it!
        let selected_device = common::find_device(&usb_ctx, Some(bus), Some(addr), args.wait)?;
        rkdev = RkDevice::open(&selected_device)?;
    }

    let mut fw = Cursor::new(rkfw.fw_data()?);
    let param = {
        let mut param = fw
            .by_name(c"parameter")?
            .ok_or(UpgradeError::ParameterNotFound)?;

        let param_size = param.metadata().size();
        if param_size > 64 * 1024 {
            return Err(UpgradeError::ParameterTooLarge(param_size));
        }
        if param_size <= 2 {
            return Err(UpgradeError::ParameterTooShort(param_size));
        }

        let mut param_content = Vec::with_capacity(param_size as usize);
        param.reader()?.read_to_end(&mut param_content)?;

        let (data, crc32) = param_content.split_last_chunk::<2>().unwrap();
        // TODO: Check CRC32

        param_content.truncate(param_content.len() - 2);
        param_content
    };

    // Check partition table
    println!("Check partition table");
    let lb_size = match rkdev.read_storage_info()?.lba_size() {
        1 => LogicalBlockSize::Lb512,
        8 => LogicalBlockSize::Lb4096,
        x => {
            eprintln!("Unsupported LBA size: {x}");
            return Ok(());
        }
    };
    let mut disk = gpt::GptConfig::new()
        .writable(true)
        .logical_block_size(lb_size)
        .open_from_device(RkBlockDevice::try_from(&mut rkdev)?)?;
    // TODO: Check it here

    // Write partitions
    disk = gpt::GptConfig::new()
        .writable(false)
        .logical_block_size(lb_size)
        .open_from_device(disk.take_device())?;
    let partitions = disk.partitions().clone();

    for i in 0..fw.len()? {
        let mut entry = fw.by_index(i)?.expect("entry must exist");
        let name = entry
            .metadata()
            .name()
            .map_err(UpgradeError::InvalidPartitionName)?
            .to_string_lossy();
        println!("Writing file {name:?}");

        match &*name {
            "package-file" | "parameter" => { /* Skip */ }
            x => {
                let part = partitions
                    .iter()
                    .find(|(_, p)| p.name == x)
                    .map(|(_, p)| p)
                    .ok_or(UpgradeError::PartitionNotFound)?;
                if x == "super" {
                    let mut reader = entry.reader()?;
                    write_sparse_image(&mut disk, part, &mut reader)?;
                } else {
                    let mut reader = entry.reader()?;
                    write_regular_image(&mut disk, part, &mut reader)?;
                }
            }
        }
    }

    Ok(())
}

fn write_regular_image<R: Read + Seek>(
    disk: &mut GptDisk<RkBlockDevice<rusb::Context>>,
    part: &Partition,
    img: &mut R,
) -> Result<(), UpgradeError> {
    let lb_size = *disk.logical_block_size();
    let part_size = part.bytes_len(lb_size)?;
    let part_start = part.bytes_start(lb_size)?;

    let device = disk.device_mut();
    device.seek(SeekFrom::Start(part_start))?;

    std::io::copy(&mut img.take(part_size), device)?;
    Ok(())
}

#[derive(Error, Debug)]
pub enum WriteSparseImageError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Invalid magic")]
    InvalidMagic,
    #[error("Unsupported format version")]
    UnsupportedVersion,
    #[error("Unknown sparse chunk type")]
    UnknownSparseChunkType,
    #[error("Image too large: part_size={part_size}, total_size={total_size}")]
    ImageTooLarge { part_size: u64, total_size: u64 },
}

fn write_sparse_image<R: Read + Seek>(
    disk: &mut GptDisk<RkBlockDevice<rusb::Context>>,
    part: &Partition,
    img: &mut R,
) -> Result<(), WriteSparseImageError> {
    use sparse::{
        CHUNK_TYPE_CRC32, CHUNK_TYPE_DONT_CARE, CHUNK_TYPE_FILL, CHUNK_TYPE_RAW, ChunkHeader,
        SPARSE_HEADER_MAGIC, SparseHeader, filling_chunk,
    };
    let sparse_header = SparseHeader::read_from_io(&mut *img)?;

    // Check magic
    if sparse_header.magic.get() != SPARSE_HEADER_MAGIC {
        return Err(WriteSparseImageError::InvalidMagic);
    }

    // Check version
    if sparse_header.major_version.get() > 1 || sparse_header.minor_version.get() > 0 {
        return Err(WriteSparseImageError::UnsupportedVersion);
    }

    // Check size
    let block_size = sparse_header.blk_sz.get() as u64;
    let total_size = block_size * sparse_header.total_blks.get() as u64;
    let lb_size = *disk.logical_block_size();
    let part_size = part.bytes_len(lb_size)?;
    let part_start = part.bytes_start(lb_size)?;
    if total_size > part_size {
        return Err(WriteSparseImageError::ImageTooLarge {
            part_size,
            total_size,
        });
    }

    let chunk_header_size = sparse_header.chunk_hdr_sz.get() as u64;
    let mut pos = sparse_header.file_hdr_sz.get() as u64;

    let device = disk.device_mut();
    device.seek(SeekFrom::Start(part_start))?;

    for _ in 0..sparse_header.total_chunks.get() {
        let chunk_start = img.seek(SeekFrom::Start(pos))?;
        let chunk_header = ChunkHeader::read_from_io(&mut *img)?;

        let data_size = block_size * chunk_header.chunk_sz.get() as u64;

        img.seek(SeekFrom::Start(chunk_start + chunk_header_size))?;
        match chunk_header.chunk_type.get() {
            CHUNK_TYPE_RAW => {
                std::io::copy(&mut img.take(data_size), device)?;
            }
            CHUNK_TYPE_FILL => {
                let mut buf = [0u8; 4];
                img.read_exact(&mut buf)?;
                std::io::copy(&mut filling_chunk(&buf, data_size), device)?;
            }
            CHUNK_TYPE_DONT_CARE => {
                device.seek(SeekFrom::Current(data_size as i64))?;
            }
            CHUNK_TYPE_CRC32 => { /* TODO */ }
            _ => return Err(WriteSparseImageError::UnknownSparseChunkType),
        }

        pos += chunk_header.total_sz.get() as u64;
    }

    Ok(())
}

mod sparse {
    use std::io::Read;

    use zerocopy::{FromBytes, Immutable, KnownLayout, LE, U16, U32};

    pub const SPARSE_HEADER_MAGIC: u32 = 0xed26ff3a;

    pub const CHUNK_TYPE_RAW: u16 = 0xCAC1;
    pub const CHUNK_TYPE_FILL: u16 = 0xCAC2;
    pub const CHUNK_TYPE_DONT_CARE: u16 = 0xCAC3;
    pub const CHUNK_TYPE_CRC32: u16 = 0xCAC4;

    #[derive(FromBytes, Immutable, KnownLayout)]
    #[repr(C)]
    pub struct SparseHeader {
        pub magic: U32<LE>,
        pub major_version: U16<LE>,
        pub minor_version: U16<LE>,
        pub file_hdr_sz: U16<LE>,
        pub chunk_hdr_sz: U16<LE>,
        pub blk_sz: U32<LE>,
        pub total_blks: U32<LE>,
        pub total_chunks: U32<LE>,
        pub image_checksum: U32<LE>,
    }

    #[derive(FromBytes, Immutable, KnownLayout)]
    #[repr(C)]
    pub struct ChunkHeader {
        pub chunk_type: U16<LE>,
        pub _reserved1: U16<LE>,
        pub chunk_sz: U32<LE>,
        pub total_sz: U32<LE>,
    }

    struct Repeat<'a>(&'a [u8]);

    impl<'a> std::io::Read for Repeat<'a> {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let len = buf.len();
            let mut chunks = buf.chunks_exact_mut(self.0.len());
            for chunk in &mut chunks {
                chunk.copy_from_slice(self.0);
            }
            Ok(len - chunks.into_remainder().len())
        }
    }

    pub fn filling_chunk(data: &[u8], len: u64) -> impl std::io::Read {
        Repeat(data).take(len)
    }
}
