use std::{
    fmt::{Debug, Formatter},
    io::Cursor,
    time::Duration,
};

use thiserror::Error;
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, byteorder::little_endian::*};

use rkafp::rkaf::{Image, RK_CRC};
pub use rkafp::rkfw::{ImageHeader as RkFwHeader, MAGIC as RKFW_TAG, RkTime};

type Uchar = u8;
type Ushort = U16;
type Uint = U32;
type Dword = U32;

pub const RKBOOT_TAG: u32 = U32::from_bytes(*b"BOOT").get();
pub const RKLDR_TAG: u32 = U32::from_bytes(*b"LDR ").get();

type RkDeviceType = Dword;

#[allow(unused)]
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub enum RkBootEntryType {
    Entry471 = 1,
    Entry472 = 2,
    EntryLoader = 4,
}

#[derive(FromBytes)]
#[repr(C, packed)]
pub struct RkBootHeader {
    pub tag: Uint,
    pub size: Ushort,
    pub version: Dword,
    pub merge_version: Dword,
    pub release_time: RkTime,
    pub support_chip: RkDeviceType,

    pub entry_741_count: Uchar,
    pub entry_741_offset: Dword,
    pub entry_741_size: Uchar,

    pub entry_742_count: Uchar,
    pub entry_742_offset: Dword,
    pub entry_742_size: Uchar,

    pub loader_entry_count: Uchar,
    pub loader_entry_offset: Dword,
    pub loader_entry_size: Uchar,

    pub sign_flag: Uchar,
    pub rc4_flag: Uchar,

    _reserved: [u8; 57],
}

impl Debug for RkBootHeader {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RkBootHeader")
            .field("tag", &self.tag.get())
            .field("size", &self.size.get())
            .field("version", &self.version.get())
            .field("merge_version", &self.merge_version.get())
            .field("release_time", &self.release_time.to_string())
            .field("support_chip", &self.support_chip.get())
            .field("entry_741_count", &self.entry_741_count)
            .field("entry_741_offset", &self.entry_741_offset.get())
            .field("entry_741_size", &self.entry_741_size)
            .field("entry_742_count", &self.entry_742_count)
            .field("entry_742_offset", &self.entry_742_offset.get())
            .field("entry_742_size", &self.entry_742_size)
            .field("loader_entry_count", &self.loader_entry_count)
            .field("loader_entry_offset", &self.loader_entry_offset.get())
            .field("loader_entry_size", &self.loader_entry_size)
            .field("sign_flag", &self.sign_flag)
            .field("rc4_flag", &self.rc4_flag)
            .finish()
    }
}

#[derive(FromBytes, KnownLayout, Immutable)]
#[repr(C, packed)]
pub struct RkBootEntry {
    pub size: Uchar,
    pub r#type: Dword, // should be RkBootEntryType
    pub name: [u16; 20],
    pub data_offset: Dword,
    pub data_size: Dword,
    pub data_delay: Dword,
}

#[allow(non_snake_case)]
pub struct RkBootImage<'data> {
    data: &'data [u8],
    entries_471: Vec<&'data RkBootEntry>,
    entries_472: Vec<&'data RkBootEntry>,
    entries_loader: Vec<&'data RkBootEntry>,
}

pub struct RkFwImage<'data> {
    data: &'data [u8],
    fw: &'data [u8],
    md5: &'data [u8],
    sign: Option<&'data [u8]>,
}

#[derive(Debug, Error)]
pub enum ImageError {
    #[error("unknown tag")]
    UnknownTag,
    #[error("image data too short")]
    TooShort,
    #[error("boot entry table offset out of range")]
    BootEntryOutOfRange,
    #[error("boot entry header too short")]
    BootEntryTooShort,
    #[error("firmware offset out of range")]
    FwOutOfRange,
    #[error("md5 data out of range")]
    MD5OutOfRange,
}

fn parse_entries(
    data: &[u8],
    offset: Dword,
    count: Uchar,
    size: Uchar,
) -> Result<Vec<&RkBootEntry>, ImageError> {
    if (size as usize) < (std::mem::size_of::<RkBootEntry>()) {
        return Err(ImageError::BootEntryTooShort);
    }
    let offset = offset.get() as usize;
    let end = ((size as usize) * (count as usize))
        .checked_add(offset)
        .ok_or(ImageError::BootEntryOutOfRange)?;
    let entries = data
        .get(offset..end)
        .ok_or(ImageError::BootEntryOutOfRange)?
        .chunks_exact(size as usize)
        .map(<RkBootEntry as FromBytes>::ref_from_bytes)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| ImageError::TooShort)?;
    Ok(entries)
}

impl<'data> RkBootImage<'data> {
    pub fn new(data: &'data [u8]) -> Result<Self, ImageError> {
        let header = data
            .get(0..std::mem::size_of::<RkBootHeader>())
            .ok_or(ImageError::TooShort)?
            .as_ptr() as *const RkBootHeader;

        unsafe {
            if !matches!((*header).tag.get(), RKBOOT_TAG | RKLDR_TAG) {
                return Err(ImageError::UnknownTag);
            }

            let entries471 = parse_entries(
                data,
                (*header).entry_741_offset,
                (*header).entry_741_count,
                (*header).entry_741_size,
            )?;
            let entries472 = parse_entries(
                data,
                (*header).entry_742_offset,
                (*header).entry_742_count,
                (*header).entry_742_size,
            )?;
            let entries_loader = parse_entries(
                data,
                (*header).loader_entry_offset,
                (*header).loader_entry_count,
                (*header).loader_entry_size,
            )?;

            Ok(Self {
                data,
                entries_471: entries471,
                entries_472: entries472,
                entries_loader,
            })
        }
    }

    pub fn boot_header_ptr(&self) -> *const RkBootHeader {
        assert!(self.data.len() > std::mem::size_of::<RkBootHeader>());
        self.data.as_ptr() as *const RkBootHeader
    }

    pub fn get_entry_data(&self, offset: usize, size: usize) -> &'data [u8] {
        &self.data[offset..size]
    }

    pub fn get_crc32(&self) -> u32 {
        let crc_bytes = self.data.split_last_chunk::<4>().unwrap().1;
        unsafe { std::ptr::read_unaligned(crc_bytes.as_ptr() as *const Dword).get() }
    }

    pub fn calculate_crc32(&self) -> u32 {
        RK_CRC.checksum(self.data.split_last_chunk::<4>().unwrap().0)
    }

    pub fn iter_entries(
        &self,
        typ: RkBootEntryType,
    ) -> impl Iterator<Item = (String, &'data [u8], Duration)> {
        match typ {
            RkBootEntryType::Entry471 => &self.entries_471,
            RkBootEntryType::Entry472 => &self.entries_472,
            RkBootEntryType::EntryLoader => &self.entries_loader,
        }
        .iter()
        .map(move |entry_ptr| {
            let entry = unsafe { std::ptr::read_unaligned(*entry_ptr) };
            let name = entry.name;
            let name = String::from_utf16_lossy(&name[..]);
            let name = name.trim_end_matches('\0').to_owned();
            let offset = entry.data_offset.get() as usize;
            let size = entry.data_size.get() as usize;
            let delay = entry.data_delay.get() as u64;
            (
                name,
                &self.data[offset..offset + size],
                Duration::from_millis(delay),
            )
        })
    }
}

impl<'data> RkFwImage<'data> {
    pub fn new(data: &'data [u8]) -> Result<Self, ImageError> {
        // Layout: [ header | fw | md5(32) | optional sign(128+) ]
        let header = data
            .get(0..std::mem::size_of::<RkFwHeader>())
            .ok_or(ImageError::TooShort)?
            .as_ptr() as *const RkFwHeader;
        if unsafe { (*header).tag } != RKFW_TAG {
            return Err(ImageError::UnknownTag);
        }
        let (fw_offset, fw_end) = unsafe {
            let fw_size = (*header).fw_size.get() as usize;
            let fw_offset = (*header).fw_offset() as usize;
            let fw_end = fw_offset
                .checked_add(fw_size)
                .ok_or(ImageError::FwOutOfRange)?;
            (fw_offset, fw_end)
        };
        let fw = data
            .get(fw_offset..fw_end)
            .ok_or(ImageError::FwOutOfRange)?;
        // NOTE: We believe anchoring MD5 at `fw_end` is more reasonable than
        // the legacy C++ behavior, which may fall back to file-end in some
        // cases and pick the wrong MD5 when extra trailing bytes are present.
        let md5_end = fw_end.checked_add(32).ok_or(ImageError::MD5OutOfRange)?;
        let md5 = data.get(fw_end..md5_end).ok_or(ImageError::MD5OutOfRange)?;
        // Ignoring signature if length < 128, should we report an error?
        let sign = data.get(md5_end..).filter(|x| x.len() >= 128);
        Ok(Self {
            data,
            fw,
            md5,
            sign,
        })
    }

    fn header_ptr(&self) -> *const RkFwHeader {
        assert!(self.data.len() > std::mem::size_of::<RkFwHeader>());
        self.data.as_ptr() as *const RkFwHeader
    }

    pub fn boot_data(&self) -> Result<RkBootImage<'data>, ImageError> {
        let header = self.header_ptr();
        unsafe {
            let offset = (*header).boot_offset.get() as usize;
            let end = offset + (*header).boot_size.get() as usize;
            self.data
                .get(offset..end)
                .ok_or(ImageError::BootEntryOutOfRange)
                .and_then(RkBootImage::new)
        }
    }

    pub fn fw_data(&self) -> Result<&'data [u8], ImageError> {
        Ok(self.fw)
    }
}

impl Debug for RkBootImage<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let boot_header = unsafe { std::ptr::read_unaligned(self.boot_header_ptr()) };
        let mut ds = f.debug_struct("RkBootImage");
        ds.field("header", &boot_header);

        for entry_header in [&self.entries_471, &self.entries_472, &self.entries_loader]
            .into_iter()
            .flatten()
        {
            let RkBootEntry {
                size,
                r#type,
                name,
                data_offset,
                data_size,
                data_delay,
            } = **entry_header;
            let name = String::from_utf16_lossy(&name[..]);
            let name = name.trim_end_matches('\0').to_owned();
            ds.field(&name, &format_args!("{type:?} {{ size: {size:#X}, data_offset: {data_offset:#X}, data_size: {data_size:#X}, data_delay: {data_delay} }}"));
        }

        ds.finish()
    }
}

impl Debug for RkFwImage<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let header = unsafe { std::ptr::read_unaligned(self.header_ptr()) };
        let mut ds = f.debug_struct("RkFwImage");
        ds.field("header", &header);
        ds.field("fw_len", &self.fw.len());
        ds.field("md5", &String::from_utf8_lossy(self.md5));
        ds.field("sign_len", &self.sign.map(hex::encode));
        ds.field("boot", &self.boot_data());
        ds.finish()
    }
}
