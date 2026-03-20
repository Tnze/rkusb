use std::{
    fmt::{Debug, Display, Formatter},
    time::Duration,
};

use crc::{Algorithm, Crc};
use thiserror::Error;
use zerocopy::{FromBytes, byteorder::little_endian::*};

type Uchar = u8;
type Ushort = U16;
type Uint = U32;
type Dword = U32;

pub const RKFW_TAG: u32 = 0x57464B52;
pub const RKBOOT_TAG: u32 = 0x544F4F42;
pub const RKLDR_TAG: u32 = 0x2052444C;

#[derive(FromBytes)]
#[repr(C, packed)]
pub struct RkTime {
    pub year: U16,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
}

impl Display for RkTime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
            self.year, self.month, self.day, self.hour, self.minute, self.second
        )
    }
}

type RkDeviceType = Dword;

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

#[repr(C, packed)]
pub struct RkBootEntry {
    pub size: Uchar,
    pub r#type: RkBootEntryType,
    pub name: [u16; 20],
    pub data_offset: Dword,
    pub data_size: Dword,
    pub data_delay: Dword,
}

#[derive(FromBytes)]
#[repr(C, packed)]
pub struct RkFwHeader {
    pub tag: Uint,
    pub size: Ushort,
    pub version: Dword,
    pub merge_version: Dword,
    pub release_time: RkTime,
    pub support_chip: RkDeviceType,
    pub boot_offset: Dword,
    pub boot_size: Dword,
    pub fw_offset: Dword,
    pub fw_size: Dword,
    pub reserved_0: [u8; 4],
    pub os_type: Dword,
    pub reserved_1: [u8; 4],
    pub backup_size: Ushort,
    pub reserved_2: [u8; 2],
    pub fw_offset_hi: Dword,
    pub reserved_3: [u8; 41],
}

impl Debug for RkFwHeader {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RkFwHeader")
            .field("tag", &self.tag.get())
            .field("size", &self.size.get())
            .field("version", &self.version.get())
            .field("merge_version", &self.merge_version.get())
            .field("release_time", &self.release_time.to_string())
            .field("support_chip", &self.support_chip.get())
            .field("boot_offset", &self.boot_offset.get())
            .field("boot_size", &self.boot_size.get())
            .field("fw_offset", &self.fw_offset.get())
            .field("fw_size", &self.fw_size.get())
            .field("os_type", &self.os_type.get())
            .field("backup_size", &self.backup_size.get())
            .finish()
    }
}

pub struct RkBootImage<'data> {
    data: &'data [u8],
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
    #[error("firmware offset out of range")]
    FwOutOfRange,
    #[error("md5 data out of range")]
    MD5OutOfRange,
}

impl<'data> RkBootImage<'data> {
    pub fn new(data: &'data [u8]) -> Self {
        Self { data }
    }

    pub fn boot_header_ptr(&self) -> *const RkBootHeader {
        assert!(self.data.len() > std::mem::size_of::<RkBootHeader>());
        self.data.as_ptr() as *const RkBootHeader
    }

    pub fn get_entry_header(
        &self,
        entry_type: RkBootEntryType,
        entry_index: usize,
    ) -> *const RkBootEntry {
        let header = self.boot_header_ptr();
        unsafe {
            let (offset, count, size) = match entry_type {
                RkBootEntryType::Entry471 => (
                    (*header).entry_741_offset,
                    (*header).entry_741_count,
                    (*header).entry_741_size,
                ),
                RkBootEntryType::Entry472 => (
                    (*header).entry_742_offset,
                    (*header).entry_742_count,
                    (*header).entry_742_size,
                ),
                RkBootEntryType::EntryLoader => (
                    (*header).loader_entry_offset,
                    (*header).loader_entry_count,
                    (*header).loader_entry_size,
                ),
            };
            assert!(
                entry_index < count as usize,
                "Index {entry_index} out of range: [0, {count})"
            );
            let offset = offset.get() as usize + (size as usize) * entry_index;
            let entry = self.data.as_ptr().add(offset);

            entry as *const RkBootEntry
        }
    }

    pub fn get_entry_data(&self, offset: usize, size: usize) -> &'data [u8] {
        &self.data[offset..size]
    }

    pub fn get_crc32(&self) -> u32 {
        let crc_bytes = self.data.split_last_chunk::<4>().unwrap().1;
        unsafe { std::ptr::read_unaligned(crc_bytes.as_ptr() as *const Dword).get() }
    }

    pub fn calculate_crc32(&self) -> u32 {
        const ALGO: Algorithm<u32> = Algorithm {
            width: 32,
            poly: 0x04C10DB7,
            init: 0x00000000,
            refin: false,
            refout: false,
            xorout: 0x00000000,
            check: 0x00000000,
            residue: 0x00000000,
        };
        const CRC: Crc<u32> = Crc::<u32>::new(&ALGO);
        CRC.checksum(self.data.split_last_chunk::<4>().unwrap().0)
    }

    pub fn iter_entries(
        &self,
        typ: RkBootEntryType,
    ) -> impl Iterator<Item = (String, &'data [u8], Duration)> {
        let boot_header = self.boot_header_ptr();
        unsafe {
            let count = match typ {
                RkBootEntryType::Entry471 => (*boot_header).entry_741_count,
                RkBootEntryType::Entry472 => (*boot_header).entry_742_count,
                RkBootEntryType::EntryLoader => (*boot_header).loader_entry_count,
            };
            (0..count).map(move |i| {
                let entry_header = self.get_entry_header(typ, i as usize);
                let name = (*entry_header).name;
                let name = String::from_utf16_lossy(&name[..]);
                let offset = (*entry_header).data_offset.get() as usize;
                let size = (*entry_header).data_size.get() as usize;
                let delay = (*entry_header).data_delay.get() as u64;
                (
                    name,
                    &self.data[offset..offset + size],
                    Duration::from_millis(delay),
                )
            })
        }
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
            let mut fw_offset = (*header).fw_offset.get() as usize;
            if (*header).reserved_2 == [b'H', b'I'] {
                fw_offset |= ((*header).fw_offset_hi.get() as usize) << 32;
            }
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

    pub fn boot_data(&self) -> Option<RkBootImage<'data>> {
        let header = self.header_ptr();
        unsafe {
            let offset = (*header).boot_offset.get() as usize;
            let size = (*header).boot_size.get() as usize;
            let end = offset.checked_add(size)?;
            self.data.get(offset..end).map(RkBootImage::new)
        }
    }
}

impl Debug for RkBootImage<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let boot_header = unsafe { std::ptr::read_unaligned(self.boot_header_ptr()) };
        let mut ds = f.debug_struct("RkBootImage");
        ds.field("header", &boot_header);

        for (entry_count, entry_type) in [
            (boot_header.entry_741_count, RkBootEntryType::Entry471),
            (boot_header.entry_742_count, RkBootEntryType::Entry472),
            (boot_header.loader_entry_count, RkBootEntryType::EntryLoader),
        ] {
            for entry_index in 0..entry_count {
                unsafe {
                    let RkBootEntry {
                        size,
                        r#type,
                        name,
                        data_offset,
                        data_size,
                        data_delay,
                    } = *self.get_entry_header(entry_type, entry_index as usize);
                    ds.field(&String::from_utf16_lossy(&name[..]), &format_args!("{type:?} {{ size: {size:#X}, data_offset: {data_offset:#X}, data_size: {data_size:#X}, data_delay: {data_delay} }}"));
                }
            }
        }

        ds.finish()
    }
}

impl Debug for RkFwImage<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let header = unsafe { std::ptr::read_unaligned(self.header_ptr()) };
        f.debug_struct("RkFwImage")
            .field("header", &header)
            .field("os_type", &format_args!("{:#X}", header.os_type.get()))
            .field(
                "backup_size",
                &format_args!("{:#X}", header.backup_size.get()),
            )
            .field("fw_len", &self.fw.len())
            .field("md5", &String::from_utf8_lossy(self.md5))
            .field("sign_len", &self.sign.map(hex::encode))
            .field("boot", &self.boot_data())
            .finish()
    }
}
