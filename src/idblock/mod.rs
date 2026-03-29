use std::num::TryFromIntError;

use thiserror::Error;

pub mod new;
pub mod old;

pub const SECTOR_SIZE: usize = 512;
pub(crate) const IDBLOCK_ALIGNMENT: usize = 2048;
pub const RC4_KEY: [u8; 16] = [124, 78, 3, 4, 85, 5, 9, 7, 45, 44, 123, 56, 23, 13, 23, 17];
pub type Rc4Cipher = rc4::Rc4<rc4::consts::U16>;

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
    loader_head: Option<&[u8]>,
    ddr: &[u8],
    loader: &[u8],
    rc4_enabled: bool,
) -> Result<Vec<u8>, IdBlockError> {
    match loader_head {
        Some(loader_head) => new::build_idblock(loader_head, ddr, loader, rc4_enabled),
        None => old::build_idblock(ddr, loader, rc4_enabled),
    }
}
