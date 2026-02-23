use crc::{Algorithm, Crc};
use zerocopy::{FromBytes, byteorder::little_endian::*};

type UCHAR = u8;
type WCHAR = U16;
type USHORT = U16;
type UINT = U32;
type DWORD = U32;

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

impl ToString for RkTime {
    fn to_string(&self) -> String {
        format!(
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
            self.year, self.month, self.day, self.hour, self.minute, self.second
        )
    }
}

type RkDeviceType = DWORD;

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
    pub tag: UINT,
    pub size: USHORT,
    pub version: DWORD,
    pub merge_version: DWORD,
    pub release_time: RkTime,
    pub support_chip: RkDeviceType,

    pub entry_741_count: UCHAR,
    pub entry_741_offset: DWORD,
    pub entry_741_size: UCHAR,

    pub entry_742_count: UCHAR,
    pub entry_742_offset: DWORD,
    pub entry_742_size: UCHAR,

    pub loader_entry_count: UCHAR,
    pub loader_entry_offset: DWORD,
    pub loader_entry_size: UCHAR,

    pub sign_flag: UCHAR,
    pub rc4_flag: UCHAR,

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
    pub size: UCHAR,
    pub r#type: RkBootEntryType,
    pub name: [u16; 20],
    pub data_offset: DWORD,
    pub data_size: DWORD,
    pub data_delay: DWORD,
}

pub struct BootImage<'data> {
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

    pub fn get_entry(
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
            let offset = offset.get() as usize + (size as usize) * (entry_index as usize);
            let entry = self.data.as_ptr().add(offset);

            entry as *const RkBootEntryHeader
        }
    }

    pub fn get_crc32(&self) -> u32 {
        u32::from_le_bytes(*self.data.split_last_chunk::<4>().unwrap().1)
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
        Crc::<u32>::new(&ALGO).checksum(self.data.split_last_chunk::<4>().unwrap().0)
    }
}
