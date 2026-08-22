use std::{fs::File, io::Cursor, time::Duration};

use memmap2::Mmap;
use rkafp::rkaf::Image;
use rkusb::{
    RkDevice, RkUsbError, RkUsbType,
    image::{ImageError, RkFwImage},
};
use thiserror::Error;

use crate::common::{self, DeviceSelectionError};

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
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Image parse error: {0}")]
    Image(#[from] ImageError),
    #[error("Cannot detect RkUsbType")]
    UnknownRkUsbType,
    #[error("parameter not found")]
    ParameterNotFound,
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
        rkdev.download_boot(boot)?;

        // TODO: Device might reset? re-enumerate it!
    }

    let mut fw = Cursor::new(rkfw.fw_data()?);
    let param = fw
        .by_name(c"parameter")?
        .ok_or(UpgradeError::ParameterNotFound)?;

    // Check partition table

    // Write regular images

    // Write super image

    Ok(())
}

mod sparse {
    use zerocopy::{FromBytes, Immutable, KnownLayout, LE, U16, U32};

    const SPARSE_HEADER_MAGIC: u32 = 0xed26ff3a;

    const CHUNK_TYPE_RAW: u16 = 0xCAC1;
    const CHUNK_TYPE_FILL: u16 = 0xCAC2;
    const CHUNK_TYPE_DONT_CARE: u16 = 0xCAC3;
    const CHUNK_TYPE_CRC32: u16 = 0xCAC4;

    #[repr(C)]
    struct SparseHeader {
        magic: U32<LE>,
        major_version: U16<LE>,
        minor_version: U16<LE>,
        file_hdr_sz: U16<LE>,
        chunk_hdr_sz: U16<LE>,
        blk_sz: U32<LE>,
        total_blks: U32<LE>,
        total_chunks: U32<LE>,
        image_checksum: U32<LE>,
    }

    #[derive(FromBytes, Immutable, KnownLayout)]
    #[repr(C)]
    struct ChunkHeader {
        chunk_type: U16<LE>,
        _reserved1: U16<LE>,
        chunk_sz: U32<LE>,
        total_sz: U32<LE>,
    }

    pub enum Chunk<'data> {
        Raw(&'data [u8]),
        Fill(&'data [u8; 4]),
        Skip,
        CRC32(&'data [u8; 4]),
    }

    pub struct Chunks<'data> {
        sparse_image: &'data [u8],
        position: usize,
    }

    impl<'data> Chunks<'data> {
        pub fn new(sparse_image: &'data [u8]) -> Self {
            let position = todo!("check sparse_header");
            Self {
                sparse_image,
                position,
            }
        }
    }

    impl<'data> Iterator for Chunks<'data> {
        type Item = (u32, Chunk<'data>);

        fn next(&mut self) -> Option<Self::Item> {
            let data = &self.sparse_image[self.position..];
            let header = ChunkHeader::ref_from_bytes(data).ok()?;

            let chunk = match header.chunk_type.get() {
                CHUNK_TYPE_RAW => {
                    Chunk::Raw(&data[size_of::<ChunkHeader>()..header.total_sz.get() as usize])
                }
                CHUNK_TYPE_FILL => Chunk::Fill(
                    data[size_of::<ChunkHeader>()..header.total_sz.get() as usize].as_array()?,
                ),
                CHUNK_TYPE_DONT_CARE => Chunk::Skip,
                CHUNK_TYPE_CRC32 => Chunk::CRC32(
                    data[size_of::<ChunkHeader>()..header.total_sz.get() as usize].as_array()?,
                ),
                _ => return None,
            };

            self.position += header.total_sz.get() as usize;
            Some((header.chunk_sz.get(), chunk))
        }
    }
}
