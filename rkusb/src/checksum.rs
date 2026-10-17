use crc::{Algorithm, Crc};

pub const ROCKCHIP_CRC16_UNKNOWN: Algorithm<u16> = Algorithm {
    width: 16,
    poly: 0x1021,
    init: 0x0000,
    refin: false,
    refout: false,
    xorout: 0x0000,
    check: 0x0000,
    residue: 0x0000,
};

pub const ROCKCHIP_CRC16: Crc<u16> = Crc::<u16>::new(&ROCKCHIP_CRC16_UNKNOWN);
