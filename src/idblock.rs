use std::num::TryFromIntError;

use rc4::{KeyInit, StreamCipher};
use thiserror::Error;

use crate::checksum::{ROCKCHIP_CRC16, ROCKCHIP_CRC32};

const SECTOR_SIZE: usize = 512;
const IDBLOCK_ALIGNMENT: usize = 2048;
const IDBLOCK_HEADER_SECTORS: usize = 4;
const IDBLOCK_TAG: u32 = 0x0FF0AA55;
const IDBLOCK_CHIP_TAG: u32 = 0x38324B52;
const IDBLOCK_SYS_RESERVED_BLOCK: u16 = 0x000C;
const IDBLOCK_DISK0_SIZE: u16 = 0xFFFF;
const RC4_KEY: [u8; 16] = [124, 78, 3, 4, 85, 5, 9, 7, 45, 44, 123, 56, 23, 13, 23, 17];
type Rc4Cipher = rc4::Rc4<rc4::consts::U16>;

#[derive(Debug, Error)]
pub enum IdBlockError {
    #[error("ddr data too large")]
    DdrTooLarge(#[source] TryFromIntError),
    #[error("loader data too large")]
    LoaderTooLarge(#[source] TryFromIntError),
    #[error("idblock size overflow")]
    SizeOverflow,
}

pub fn build_idblock(
    ddr: &[u8],
    loader: &[u8],
    rc4_enabled: bool,
) -> Result<Vec<u8>, IdBlockError> {
    let ddr_len = ddr.len().next_multiple_of(IDBLOCK_ALIGNMENT);
    let loader_len = loader.len().next_multiple_of(IDBLOCK_ALIGNMENT);

    let ddr_sector_count = ddr_len / SECTOR_SIZE;
    let loader_sector_count = loader_len / SECTOR_SIZE;
    let ddr_sector_count_u16 = ddr_sector_count
        .try_into()
        .map_err(IdBlockError::DdrTooLarge)?;
    let loader_sector_count_u16 = loader_sector_count
        .try_into()
        .map_err(IdBlockError::LoaderTooLarge)?;

    let total_sectors = IDBLOCK_HEADER_SECTORS
        .checked_add(ddr_sector_count)
        .and_then(|value| value.checked_add(loader_sector_count))
        .ok_or(IdBlockError::SizeOverflow)?;
    let total_len = total_sectors
        .checked_mul(SECTOR_SIZE)
        .ok_or(IdBlockError::SizeOverflow)?;

    let mut idblock = vec![0u8; total_len];
    let (header, payload) = idblock
        .split_first_chunk_mut::<{ SECTOR_SIZE * IDBLOCK_HEADER_SECTORS }>()
        .unwrap();
    let header = header.as_mut_slice();
    let (sector_0, header_rest) = header.split_first_chunk_mut::<SECTOR_SIZE>().unwrap();
    let (sector_1, header_rest) = header_rest.split_first_chunk_mut::<SECTOR_SIZE>().unwrap();
    let (sector_2, header_rest) = header_rest.split_first_chunk_mut::<SECTOR_SIZE>().unwrap();
    let (sector_3, header_rest) = header_rest.split_first_chunk_mut::<SECTOR_SIZE>().unwrap();
    debug_assert!(header_rest.is_empty());

    let (ddr_area, loader_area) = payload.split_at_mut(ddr_len);

    build_sector_0(
        sector_0,
        rc4_enabled,
        ddr_sector_count_u16,
        ddr_sector_count_u16
            .checked_add(loader_sector_count_u16)
            .ok_or(IdBlockError::SizeOverflow)?,
    );
    build_sector_1(sector_1);
    build_sector_3(sector_3);

    ddr_area[..ddr.len()].copy_from_slice(ddr);
    loader_area[..loader.len()].copy_from_slice(loader);

    if rc4_enabled {
        for chunk in ddr_area.chunks_exact_mut(SECTOR_SIZE) {
            Rc4Cipher::new((&RC4_KEY).into()).apply_keystream(chunk);
        }
        for chunk in loader_area.chunks_exact_mut(SECTOR_SIZE) {
            Rc4Cipher::new((&RC4_KEY).into()).apply_keystream(chunk);
        }
    }

    let sec0_crc = ROCKCHIP_CRC16.checksum(sector_0);
    let sec1_crc = ROCKCHIP_CRC16.checksum(sector_1);
    let sec3_crc = ROCKCHIP_CRC16.checksum(sector_3);
    let boot_code_crc = ROCKCHIP_CRC32.checksum(payload);
    build_sector_2(sector_2, sec0_crc, sec1_crc, boot_code_crc, sec3_crc);

    if rc4_enabled {
        Rc4Cipher::new((&RC4_KEY).into()).apply_keystream(sector_0);
        Rc4Cipher::new((&RC4_KEY).into()).apply_keystream(sector_2);
        Rc4Cipher::new((&RC4_KEY).into()).apply_keystream(sector_3);
    }

    Ok(idblock)
}

fn build_sector_0(
    sector: &mut [u8; SECTOR_SIZE],
    rc4_enabled: bool,
    boot_data_sectors: u16,
    boot_code_sectors: u16,
) {
    sector.fill(0);
    sector[0..4].copy_from_slice(&IDBLOCK_TAG.to_le_bytes());
    sector[8..12].copy_from_slice(&u32::from(rc4_enabled).to_le_bytes());
    sector[12..14].copy_from_slice(&0x0004u16.to_le_bytes());
    sector[14..16].copy_from_slice(&0x0004u16.to_le_bytes());
    sector[500..502].copy_from_slice(&boot_data_sectors.to_le_bytes());
    sector[502..504].copy_from_slice(&boot_code_sectors.to_le_bytes());
}

fn build_sector_1(sector: &mut [u8; SECTOR_SIZE]) {
    sector.fill(0);
    sector[0..2].copy_from_slice(&IDBLOCK_SYS_RESERVED_BLOCK.to_le_bytes());
    sector[2..4].copy_from_slice(&IDBLOCK_DISK0_SIZE.to_le_bytes());
    sector[8..12].copy_from_slice(&IDBLOCK_CHIP_TAG.to_le_bytes());
}

fn build_sector_2(
    sector: &mut [u8; SECTOR_SIZE],
    sec0_crc: u16,
    sec1_crc: u16,
    boot_code_crc: u32,
    sec3_crc: u16,
) {
    sector.fill(0);
    sector[491..494].copy_from_slice(b"VC\0");
    sector[494..496].copy_from_slice(&sec0_crc.to_le_bytes());
    sector[496..498].copy_from_slice(&sec1_crc.to_le_bytes());
    sector[498..502].copy_from_slice(&boot_code_crc.to_le_bytes());
    sector[504..508].copy_from_slice(b"CRC\0");
    sector[508..510].copy_from_slice(&sec3_crc.to_le_bytes());
}

fn build_sector_3(sector: &mut [u8; SECTOR_SIZE]) {
    sector.fill(0);
}
