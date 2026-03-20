use std::{fmt::Display, time::Duration};

use crc::{Algorithm, Crc};
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
pub struct IDBlockHeader {
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

impl std::fmt::Debug for IDBlockHeader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IDBlockHeader")
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
pub struct RkBootEntryHeader {
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
    pub fw_offset_hi_flag: [u8; 2],
    pub fw_offset_hi: Dword,
    pub reserved_tail: [u8; 41],
}

impl std::fmt::Debug for RkFwHeader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
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

pub struct BootImage<'data> {
    data: &'data [u8],
}

pub struct RkFwImage<'data> {
    data: &'data [u8],
}

impl<'data> BootImage<'data> {
    pub fn new(data: &'data [u8]) -> Self {
        Self { data }
    }

    pub fn get_idblock(&self) -> *const IDBlockHeader {
        assert!(self.data.len() > std::mem::size_of::<IDBlockHeader>());
        self.data.as_ptr() as *const IDBlockHeader
    }

    pub fn get_entry_header(
        &self,
        entry_type: RkBootEntryType,
        entry_index: usize,
    ) -> *const RkBootEntryHeader {
        let header = self.get_idblock();
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

            entry as *const RkBootEntryHeader
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
        let idblock = self.get_idblock();
        unsafe {
            let count = match typ {
                RkBootEntryType::Entry471 => (*idblock).entry_741_count,
                RkBootEntryType::Entry472 => (*idblock).entry_742_count,
                RkBootEntryType::EntryLoader => (*idblock).loader_entry_count,
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
    pub fn new(data: &'data [u8]) -> Self {
        Self { data }
    }

    fn header_ptr(&self) -> *const RkFwHeader {
        assert!(self.data.len() > std::mem::size_of::<RkFwHeader>());
        self.data.as_ptr() as *const RkFwHeader
    }

    fn fw_end_offset(&self) -> u64 {
        let header = self.header_ptr();
        unsafe {
            if (*header).fw_offset_hi_flag == [b'H', b'I'] {
                let fw_hi = (*header).fw_offset_hi.get() as u64;
                (fw_hi << 32) + (*header).fw_offset.get() as u64 + (*header).fw_size.get() as u64
            } else {
                (*header).fw_offset.get() as u64 + (*header).fw_size.get() as u64
            }
        }
    }

    pub fn boot_data(&self) -> Option<BootImage<'data>> {
        let header = self.header_ptr();
        unsafe {
            let offset = (*header).boot_offset.get() as usize;
            let size = (*header).boot_size.get() as usize;
            let end = offset.checked_add(size)?;
            self.data.get(offset..end).map(BootImage::new)
        }
    }

    fn md5_and_sign(&self) -> Option<(&'data [u8], Option<&'data [u8]>)> {
        let fw_end = self.fw_end_offset() as usize;
        if fw_end > self.data.len() {
            return None;
        }

        let trailer_size = self.data.len() - fw_end;
        if trailer_size >= 160 {
            let md5 = self.data.get(fw_end..fw_end + 32)?;
            let sign = self.data.get(fw_end + 32..);
            Some((md5, sign))
        } else {
            let md5 = self.data.get(self.data.len().checked_sub(32)?..)?;
            Some((md5, None))
        }
    }
}

impl std::fmt::Debug for RkFwImage<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let header = unsafe { std::ptr::read_unaligned(self.header_ptr()) };
        let (md5, sign_size) = match self.md5_and_sign() {
            Some((md5, Some(sig))) => (Some(md5), sig.len()),
            Some((md5, None)) => (Some(md5), 0),
            None => (None, 0),
        };
        let embedded_boot = self.boot_data();
        let embedded_boot_tag = embedded_boot
            .as_ref()
            .and_then(|boot| boot.data.get(0..4))
            .map(|bytes| unsafe { std::ptr::read_unaligned(bytes.as_ptr() as *const Dword).get() });

        f.debug_struct("RkFwImage")
            .field("header", &header)
            .field("os_type", &format_args!("{:#X}", header.os_type.get()))
            .field("backup_size", &format_args!("{:#X}", header.backup_size.get()))
            .field(
                "fw_end_offset",
                &format_args!("{:#X}", self.fw_end_offset()),
            )
            .field("sign_data_size", &format_args!("{:#X}", sign_size))
            .field("md5", &format_args!("{:02X?}", md5))
            .field(
                "embedded_boot_size",
                &format_args!("{:#X}", embedded_boot.as_ref().map_or(0, |x| x.data.len())),
            )
            .field("embedded_boot_tag", &embedded_boot_tag)
            .finish()
    }
}
