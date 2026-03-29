use std::fmt::Debug;

use super::{IDBLOCK_ALIGNMENT, IdBlockError, RC4_KEY, Rc4Cipher, SECTOR_SIZE};
use rc4::{KeyInit, StreamCipher};
use thiserror::Error;
use zerocopy::{
    FromBytes,
    byteorder::little_endian::{U16, U32},
};

pub const RKNS_TAG: u32 = 0x534E4B52;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RkNsHashType {
    None,
    Sha256,
    Sha512,
    Unknown(u32),
}

#[derive(Debug, Error)]
pub enum RkNsParseError {
    #[error(
        "idblock too short for RKNS header: expected at least {expected:#x} bytes, got {actual:#x}"
    )]
    TooShort { expected: usize, actual: usize },
    #[error("invalid RKNS magic: {0:#010X}")]
    InvalidMagic(u32),
}

#[derive(FromBytes, Clone, Copy)]
#[repr(C, packed)]
pub struct RkNsImageEntry {
    offset: U16,
    size: U16,
    address: U32,
    flag: U32,
    counter: U32,
    _reserved: [u8; 8],
    hash: [u8; 64],
}

#[derive(FromBytes, Clone, Copy)]
#[repr(C, packed)]
pub struct RkNsHeader {
    magic: U32,
    _reserved: [u8; 4],
    nimage: U16, // header hash offset in 4-byte words
    images_count: U16,
    boot_flag: U32,
    _reserved1: [u8; 104],
    images: [RkNsImageEntry; 4],
    _reserved2: [u8; 1064],
    hash: [u8; 512],
}

pub struct RkNsImage<'a> {
    pub data: &'a [u8],
}

impl Debug for RkNsImage<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        unsafe {
            let header = self.data.as_ptr().cast::<RkNsHeader>();
            f.debug_struct("RkNsImage")
                .field("magic", &(*header).magic.get())
                .field("hash_offset", &((*header).nimage.get() * 4))
                .field("hash_type", &self.hash_type())
                .field("images_count", &(*header).images_count.get())
                .field("boot_flag", &(*header).boot_flag.get())
                .finish()
        }
    }
}

impl<'a> RkNsImage<'a> {
    pub fn new(data: &'a [u8]) -> Result<Self, RkNsParseError> {
        if data.len() < std::mem::size_of::<RkNsHeader>() {
            return Err(RkNsParseError::TooShort {
                expected: std::mem::size_of::<RkNsHeader>(),
                actual: data.len(),
            });
        }
        Ok(Self { data })
    }

    pub fn hash_type(&self) -> Option<RkNsHashType> {
        unsafe {
            let header = self.data.as_ptr().cast::<RkNsHeader>();
            match (*header).boot_flag.get() & 0b111 {
                0 => None,
                1 => Some(RkNsHashType::Sha256),
                2 => Some(RkNsHashType::Sha512),
                x => Some(RkNsHashType::Unknown(x)),
            }
        }
    }
}

pub fn build_idblock(
    loader_head: &[u8],
    ddr: &[u8],
    loader: &[u8],
    rc4_enabled: bool,
) -> Result<Vec<u8>, IdBlockError> {
    let head_len = loader_head.len().next_multiple_of(IDBLOCK_ALIGNMENT);
    let ddr_len = ddr.len().next_multiple_of(IDBLOCK_ALIGNMENT);
    let loader_len = loader.len().next_multiple_of(IDBLOCK_ALIGNMENT);

    let total_len = head_len
        .checked_add(ddr_len)
        .and_then(|value| value.checked_add(loader_len))
        .ok_or(IdBlockError::SizeOverflow)?;

    let mut idblock = vec![0u8; total_len];

    let (head_area, payload_rest) = idblock.split_at_mut(head_len);
    let (ddr_area, loader_area) = payload_rest.split_at_mut(ddr_len);

    head_area[..loader_head.len()].copy_from_slice(loader_head);
    ddr_area[..ddr.len()].copy_from_slice(ddr);
    loader_area[..loader.len()].copy_from_slice(loader);

    if rc4_enabled {
        for sectors in [head_area, ddr_area, loader_area] {
            for chunk in sectors.chunks_exact_mut(SECTOR_SIZE) {
                Rc4Cipher::new((&RC4_KEY).into()).apply_keystream(chunk);
            }
        }
    }

    Ok(idblock)
}
