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

type RkDeviceType = DWORD;

pub enum RkBootEntryType {
    Entry471,
    Entry472,
    EntryLoader,
}

#[derive(FromBytes)]
#[repr(C, packed)]
pub struct IDBlock {
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

pub struct RkBootEntry {
    pub size: UCHAR,
    pub r#type: RkBootEntryType,
    pub name: [WCHAR; 20],
    pub data_offset: DWORD,
    pub data_size: DWORD,
    pub data_delay: DWORD,
}
