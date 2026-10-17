use std::{
    cell::OnceCell,
    thread::sleep,
    time::{Duration, Instant},
};

use crc::{Crc, CRC_16_IBM_3740};
use humansize::SizeFormatter;
use log::{debug, info, trace};
use thiserror::Error;
use zerocopy::{
    little_endian::{U16, U32},
    FromBytes, TryFromBytes,
};

use crate::{
    image::{RkBootEntryType, RkBootImage},
    usb::CSW_SIGN,
};

pub(crate) mod checksum;
pub mod idblock;
pub mod image;
pub mod otp;
pub mod usb;

#[cfg(all(target_os = "windows", feature = "rockusb"))]
mod rockusb;

/// Per USB CBW-transaction timeout. Large LBA transfers iterate in chunks and
/// apply this budget to each chunk instead of the whole operation.
pub const USB_TIMEOUT: Duration = Duration::from_secs(5);
pub const STORAGE_SECTOR_SIZE: usize = 512;
const MAX_LBA_TRANSFER_SECTORS: usize = 128;
const MAX_LBA_TRANSFER_BYTES: usize = MAX_LBA_TRANSFER_SECTORS * STORAGE_SECTOR_SIZE;
/// Loader-interpreted vendor-storage LBA used by xrock for the serial number.
pub const VENDOR_SN_LBA: u32 = 0xFFF0_0001;
const VENDOR_LBA_BASE: u32 = 0xFFF0_0000;
/// Maximum payload in bytes for one vendor-storage read/write (opcodes
/// 0x26/0x27). xrock clamps `vs` transfers to 512 bytes and a Rockchip vendor
/// item fits in a single 512-byte sector, so one sector bounds any item.
pub const VENDOR_STORAGE_MAX: usize = STORAGE_SECTOR_SIZE;
const SN_HEADER_LEN: usize = 8;
const SN_MAX_LEN: usize = STORAGE_SECTOR_SIZE - SN_HEADER_LEN;

#[derive(Error, Debug, Clone)]
pub enum RkUsbError {
    #[error("USB error: {0}")]
    Usb(#[from] rusb::Error),
    #[error("LBA range overflow")]
    LbaOverflow,
    #[error(
        "LBA out of range: start={start:#x} count={count} exceeds flash size {flash_sectors} sectors"
    )]
    LbaOutOfRange {
        start: u32,
        count: u32,
        flash_sectors: u32,
    },
    #[error("serial number too long (max {max} bytes)")]
    SnTooLong { max: usize },
    #[error("invalid serial number data")]
    SnInvalid,
    #[error("OTP length must be greater than 0")]
    InvalidOtpLength,
    #[error("loader does not support vendor storage (serial number)")]
    VendorStorageUnsupported,
    #[error("loader does not support OTP read")]
    OtpUnsupported,
    #[error("vendor storage payload too large (max {max} bytes)")]
    VendorPayloadTooLarge { max: usize },
    #[error("invalid MAC address '{0}'")]
    InvalidMac(String),
    #[error("Duplicate bulk endpoint detected in USB interface descriptor")]
    DuplicateBulkEndpoint,
    #[error("CBW/CSW tag mismatch")]
    TagMismatch,
    #[error("Command failed with status {0}")]
    CommandFailed(u8),
    #[error("Invalid CSW data")]
    InvalidCsw,
    #[error("Invalid flash info length: {0}")]
    InvalidFlashInfoLength(usize),
}

#[derive(FromBytes, Clone, Copy)]
#[repr(C, packed)]
pub struct RkFlashInfo {
    /// Total flash size in 512-byte sectors.
    pub flash_size: U32,
    /// Block size in 512-byte sectors.
    pub block_size: U16,
    /// Page size in 512-byte units.
    pub page_size: u8,
    pub ecc_bits: u8,
    pub access_time: u8,
    /// The manufacture code of the flash.
    ///
    /// If this field > 200, it is not manufacture anymore,
    /// it tells the LBA size (in sectors) of the flash is ([RkFlashInfo::manufacture] + 200) instead.
    pub manufacture: u8,
    pub flash_mask: u8,
}

impl std::fmt::Debug for RkFlashInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let flash_size_sectors = self.flash_size.get() as u64;
        let block_size_sectors = self.block_size.get() as u64;
        let page_size_sectors = self.page_size as u64;
        let flash_size = SizeFormatter::new(
            flash_size_sectors.saturating_mul(STORAGE_SECTOR_SIZE as u64),
            humansize::BINARY,
        );
        let block_size = SizeFormatter::new(
            block_size_sectors.saturating_mul(STORAGE_SECTOR_SIZE as u64),
            humansize::BINARY,
        );
        let page_size = SizeFormatter::new(
            page_size_sectors.saturating_mul(STORAGE_SECTOR_SIZE as u64),
            humansize::BINARY,
        );
        let ecc_bits = self.ecc_bits;
        let access_time = self.access_time;
        let manuf_code = self.manufacture;
        let flash_cs = self.flash_mask;

        let mut s = f.debug_struct("RkFlashInfo");
        if self.manufacture <= 200 {
            s.field(
                "manufacturer",
                &format_args!("{}, value={manuf_code:02X}", flash_manuf_name(manuf_code)),
            );
        } else {
            let lba_size_sectors = (self.manufacture - 200) as u64;
            let lba_size = SizeFormatter::new(
                lba_size_sectors.saturating_mul(STORAGE_SECTOR_SIZE as u64),
                humansize::BINARY,
            );
            s.field(
                "lba_size",
                &format_args!("{lba_size} ({lba_size_sectors} sectors)"),
            );
        }

        s.field(
            "flash_size",
            &format_args!("{flash_size} ({flash_size_sectors} sectors)"),
        );
        s.field(
            "block_size",
            &format_args!("{block_size} ({block_size_sectors} sectors)"),
        );
        s.field(
            "page_size",
            &format_args!("{page_size} ({page_size_sectors} sectors)"),
        );
        s.field("ecc_bits", &ecc_bits);
        s.field("access_time", &access_time);
        s.field("flash_cs", &flash_cs);
        s.finish()
    }
}

impl RkFlashInfo {
    /// Get the Logical Block Address size in sectors of the flash device
    pub fn lba_size(&self) -> u64 {
        if self.manufacture <= 200 {
            1
        } else {
            self.manufacture as u64 - 200
        }
    }

    /// Total flash size in 512-byte sectors.
    pub fn flash_size_sectors(&self) -> u32 {
        self.flash_size.get()
    }

    /// Total flash size in bytes (512-byte protocol sectors).
    pub fn flash_size_bytes(&self) -> u64 {
        self.flash_size_sectors() as u64 * STORAGE_SECTOR_SIZE as u64
    }
}

fn flash_manuf_name(code: u8) -> &'static str {
    match code {
        0 => "Samsung",
        1 => "TOSHIBA",
        2 => "HYNIX",
        3 => "Infineon",
        4 => "Micron",
        5 => "Renesas",
        6 => "ST",
        7 => "Intel",
        _ => "Unknown",
    }
}

#[repr(C)]
pub enum RkDeviceType {
    RKNone = 0,
    RK27 = 0x10,
    RKCAYMAN,
    RK28 = 0x20,
    RK281X,
    RKPANDA,
    RKNANO = 0x30,
    RKSMART,
    RKCROWN = 0x40,
    RK29 = 0x50,
    RK292X,
    RK30 = 0x60,
    RK30B,
    RK31 = 0x70,
    RK32 = 0x80,
}

impl RkDeviceType {
    /// Convert a USB VID/PID pair to a known Rockchip device type.
    pub fn from_pid_vid(pid: u16, vid: u16) -> Option<Self> {
        match (pid, vid) {
            (0x3201, 0x071B) => Some(Self::RK27),
            (0x3228, 0x071B) => Some(Self::RK28),
            (0x3226, 0x071B) => Some(Self::RKNANO),
            (0x261A, 0x2207) => Some(Self::RKCROWN),
            (0x281A, 0x2207) => Some(Self::RK281X),
            (0x273A, 0x2207) => Some(Self::RKCAYMAN),
            (0x290A, 0x2207) => Some(Self::RK29),
            (0x282B, 0x2207) => Some(Self::RKPANDA),
            (0x262C, 0x2207) => Some(Self::RKSMART),
            (0x292A, 0x2207) => Some(Self::RK292X),
            (0x300A, 0x2207) => Some(Self::RK30),
            (0x300B, 0x2207) => Some(Self::RK30B),
            (0x310B, 0x2207) => Some(Self::RK31),
            (0x310C, 0x2207) => Some(Self::RK31),
            (0x320A, 0x2207) => Some(Self::RK32),
            _ => None,
        }
    }

    /// Convert a Rockchip device type to its representative USB VID/PID pair.
    pub fn to_pid_vid(&self) -> Option<(u16, u16)> {
        match self {
            Self::RKNone => None,
            Self::RK27 => Some((0x3201, 0x071B)),
            Self::RK28 => Some((0x3228, 0x071B)),
            Self::RKNANO => Some((0x3226, 0x071B)),
            Self::RKCROWN => Some((0x261A, 0x2207)),
            Self::RK281X => Some((0x281A, 0x2207)),
            Self::RKCAYMAN => Some((0x273A, 0x2207)),
            Self::RK29 => Some((0x290A, 0x2207)),
            Self::RKPANDA => Some((0x282B, 0x2207)),
            Self::RKSMART => Some((0x262C, 0x2207)),
            Self::RK292X => Some((0x292A, 0x2207)),
            Self::RK30 => Some((0x300A, 0x2207)),
            Self::RK30B => Some((0x300B, 0x2207)),
            Self::RK31 => Some((0x310B, 0x2207)),
            // Self::RK31 => Some((0x310C, 0x2207)),
            Self::RK32 => Some((0x320A, 0x2207)),
        }
    }
}

fn is_msc_device(pid: u16, vid: u16) -> bool {
    matches!(
        (pid, vid),
        (0x3203, 0x071B)
            | (0x3205, 0x071B)
            | (0x2910, 0x0BB4)
            | (0x0000, 0x2207)
            | (0x0010, 0x2207)
    )
}

#[derive(Debug, PartialEq, Clone, Copy)]
pub enum RkUsbType {
    Unknown = 0x00,
    Maskrom = 0x01,
    Loader = 0x02,
    MSC = 0x04,
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum RkStorageType {
    Emmc = 1,
    Sd0 = 2,
    Sd1 = 3,
    Nand = 7,
    SpiNand = 8,
    SpiNor = 9,
    Sata = 10,
    Pcie = 11,
    Ufs = 12,
}

impl RkStorageType {
    pub fn from_code(code: u8) -> Option<Self> {
        match code {
            1 => Some(Self::Emmc),
            2 => Some(Self::Sd0),
            3 => Some(Self::Sd1),
            7 => Some(Self::Nand),
            8 => Some(Self::SpiNand),
            9 => Some(Self::SpiNor),
            10 => Some(Self::Sata),
            11 => Some(Self::Pcie),
            12 => Some(Self::Ufs),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Emmc => "eMMC",
            Self::Sd0 => "SD",
            Self::Sd1 => "SD1",
            Self::Nand => "NAND",
            Self::SpiNand => "SPI NAND",
            Self::SpiNor => "SPI NOR",
            Self::Sata => "SATA",
            Self::Pcie => "NVMe",
            Self::Ufs => "UFS",
        }
    }

    pub fn slug(self) -> &'static str {
        match self {
            Self::Emmc => "eMMC",
            Self::Sd0 => "SD",
            Self::Sd1 => "SD1",
            Self::Nand => "NAND",
            Self::SpiNand => "SPINAND",
            Self::SpiNor => "SPINOR",
            Self::Sata => "SATA",
            Self::Pcie => "NVMe",
            Self::Ufs => "UFS",
        }
    }
}

impl std::fmt::Display for RkStorageType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// Human-readable name for a Rockchip storage code.
pub fn storage_name(code: u8) -> &'static str {
    RkStorageType::from_code(code)
        .map(RkStorageType::name)
        .unwrap_or("Unknown")
}

/// Filename-safe storage type token used for dump labels.
pub fn storage_slug(code: u8) -> String {
    RkStorageType::from_code(code)
        .map(|t| t.slug().to_string())
        .unwrap_or_else(|| format!("storage{code}"))
}

/// Encode a serial number into the 512-byte vendor-storage SN sector used by xrock.
pub fn encode_serial_number(sn: &str) -> Result<[u8; STORAGE_SECTOR_SIZE], RkUsbError> {
    let bytes = sn.as_bytes();
    if bytes.len() > SN_MAX_LEN {
        return Err(RkUsbError::SnTooLong { max: SN_MAX_LEN });
    }
    let mut buf = [0u8; STORAGE_SECTOR_SIZE];
    buf[0..4].copy_from_slice(&1u32.to_le_bytes());
    buf[4..8].copy_from_slice(&(bytes.len() as u32).to_le_bytes());
    buf[8..8 + bytes.len()].copy_from_slice(bytes);
    Ok(buf)
}

/// Decode a serial number from the 512-byte vendor-storage SN sector used by xrock.
///
/// Returns `Ok(None)` when the sector is present but marked invalid (no SN).
pub fn decode_serial_number(buf: &[u8]) -> Result<Option<String>, RkUsbError> {
    if buf.len() < SN_HEADER_LEN {
        return Err(RkUsbError::SnInvalid);
    }
    let valid = u32::from_le_bytes(buf[0..4].try_into().map_err(|_| RkUsbError::SnInvalid)?);
    let len = u32::from_le_bytes(buf[4..8].try_into().map_err(|_| RkUsbError::SnInvalid)?) as usize;
    if valid != 1 {
        return Ok(None);
    }
    if len > buf.len().saturating_sub(SN_HEADER_LEN) || len > SN_MAX_LEN {
        return Err(RkUsbError::SnInvalid);
    }
    Ok(Some(
        String::from_utf8_lossy(&buf[SN_HEADER_LEN..SN_HEADER_LEN + len]).into_owned(),
    ))
}

/// Vendor-storage backend selected by the CDB `type` field of the
/// vendor-storage opcodes (0x26/0x27).
///
/// Values match Rockchip's loader-side implementation
/// (`drivers/usb/gadget/f_rockusb.c` in the BSP U-Boot): 0 selects the
/// `vnvm` vendor-storage partition, 1 selects eMMC RPMB — the backend
/// RKDevInfoWriteTool defaults to on RK3588.
#[repr(u16)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VendorBackend {
    /// Vendor-storage (`vnvm`) partition.
    Vendor = 0,
    /// eMMC RPMB secure storage.
    Rpmb = 1,
}

impl VendorBackend {
    /// Parse a backend name (`vendor`/`vnvm` or `rpmb`, case-insensitive).
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "vendor" | "vnvm" => Some(Self::Vendor),
            "rpmb" => Some(Self::Rpmb),
            _ => None,
        }
    }

    /// CLI/log-friendly backend name.
    pub fn name(self) -> &'static str {
        match self {
            Self::Vendor => "vendor",
            Self::Rpmb => "rpmb",
        }
    }
}

/// Well-known vendor-storage item IDs, matching the kernel
/// `rk_vendor_storage.h` / RKDevInfoWriteTool numbering.
#[repr(u16)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VendorItemId {
    Sn = 1,
    WifiMac = 2,
    LanMac = 3,
    BtMac = 4,
    Imei = 15,
}

impl VendorItemId {
    /// Map a vendor-storage item id to a known item.
    pub fn from_code(code: u16) -> Option<Self> {
        match code {
            1 => Some(Self::Sn),
            2 => Some(Self::WifiMac),
            3 => Some(Self::LanMac),
            4 => Some(Self::BtMac),
            15 => Some(Self::Imei),
            _ => None,
        }
    }

    /// Parse a CLI-friendly item name (`sn`, `wifi-mac`, `lan-mac`,
    /// `bt-mac`, `imei`, case-insensitive).
    pub fn parse_slug(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "sn" => Some(Self::Sn),
            "wifi-mac" => Some(Self::WifiMac),
            "lan-mac" => Some(Self::LanMac),
            "bt-mac" => Some(Self::BtMac),
            "imei" => Some(Self::Imei),
            _ => None,
        }
    }

    /// Display name used by the CLI.
    pub fn name(self) -> &'static str {
        match self {
            Self::Sn => "SN",
            Self::WifiMac => "WiFi MAC",
            Self::LanMac => "LAN MAC",
            Self::BtMac => "BT MAC",
            Self::Imei => "IMEI",
        }
    }

    /// Whether this item holds a 6-byte MAC address.
    pub fn is_mac(self) -> bool {
        matches!(self, Self::WifiMac | Self::LanMac | Self::BtMac)
    }
}

/// Parse a MAC address given as `XX:XX:XX:XX:XX:XX`, with `-`/`.` separators,
/// or as 12 bare hex digits.
pub fn parse_mac(s: &str) -> Result<[u8; 6], RkUsbError> {
    let digits: String = s
        .chars()
        .filter(|c| !matches!(c, ':' | '-' | '.'))
        .collect();
    if digits.len() != 12 || !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(RkUsbError::InvalidMac(s.to_string()));
    }
    let mut mac = [0u8; 6];
    for (i, byte) in mac.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&digits[i * 2..i * 2 + 2], 16)
            .map_err(|_| RkUsbError::InvalidMac(s.to_string()))?;
    }
    Ok(mac)
}

/// Format a MAC address as uppercase `XX:XX:XX:XX:XX:XX`.
pub fn format_mac(mac: &[u8; 6]) -> String {
    format!(
        "{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
        mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
    )
}

pub fn is_vendor_storage_lba(pos: u32) -> bool {
    pos >= VENDOR_LBA_BASE
}

/// LE 4-character chip tag from READ_CHIP_INFO, e.g. `"3588"`.
pub fn chip_info_tag(buf: &[u8; 16]) -> String {
    let chars = [buf[3], buf[2], buf[1], buf[0]];
    if chars.iter().all(|c| c.is_ascii_alphanumeric()) {
        String::from_utf8_lossy(&chars).into_owned()
    } else {
        format!("{:02x}{:02x}{:02x}{:02x}", buf[3], buf[2], buf[1], buf[0])
    }
}

/// Check that `[start, start+count)` fits in `flash_sectors`.
///
/// Vendor-storage LBAs (0xFFF00000+) and an unknown flash size (`0`) skip the check.
pub fn check_lba_range(start: u32, count: u32, flash_sectors: u32) -> Result<(), RkUsbError> {
    if count == 0 || is_vendor_storage_lba(start) || flash_sectors == 0 {
        return Ok(());
    }
    let end = start.checked_add(count).ok_or(RkUsbError::LbaOverflow)?;
    if end > flash_sectors {
        return Err(RkUsbError::LbaOutOfRange {
            start,
            count,
            flash_sectors,
        });
    }
    Ok(())
}

fn sector_count_for_bytes(len: usize) -> Result<u32, RkUsbError> {
    let sectors = len.div_ceil(STORAGE_SECTOR_SIZE);
    u32::try_from(sectors).map_err(|_| RkUsbError::LbaOverflow)
}

impl RkUsbType {
    /// Detect the Rockchip USB mode from a USB device descriptor.
    pub fn detect(desc: &rusb::DeviceDescriptor) -> Option<Self> {
        let pid = desc.product_id();
        let vid = desc.vendor_id();
        if RkDeviceType::from_pid_vid(pid, vid).is_some() {
            if desc.usb_version().sub_minor() & 0x01 == 0 {
                Some(Self::Maskrom)
            } else {
                Some(Self::Loader)
            }
        } else if vid == 0x2207 && (pid >> 8) > 0 {
            match desc.usb_version().sub_minor() & 0x01 {
                0 => Some(Self::Maskrom),
                1 => Some(Self::Loader),
                _ => Some(Self::Unknown), // Unreachable yet, need more information
            }
        } else if is_msc_device(pid, vid) {
            Some(Self::MSC)
        } else {
            None
        }
    }
}

pub struct RkDevice<T: rusb::UsbContext> {
    device: rusb::DeviceHandle<T>,
    bulk_in: u8,
    bulk_out: u8,
    flash_sectors: Option<u32>,
}

impl<T: rusb::UsbContext> RkDevice<T> {
    fn remaining_timeout(deadline: Instant) -> Result<Duration, RkUsbError> {
        deadline
            .checked_duration_since(Instant::now())
            .filter(|timeout| !timeout.is_zero())
            .ok_or(RkUsbError::Usb(rusb::Error::Timeout))
    }

    fn advance_lba_by_bytes(pos: u32, bytes: usize) -> Result<u32, RkUsbError> {
        let sectors =
            u32::try_from(bytes / STORAGE_SECTOR_SIZE).map_err(|_| RkUsbError::LbaOverflow)?;
        pos.checked_add(sectors).ok_or(RkUsbError::LbaOverflow)
    }

    fn cbw_transaction(
        &mut self,
        cbw: &usb::Cbw<usb::Cbwcb>,
        data_out: Option<&[u8]>,
        data_in: Option<&mut [u8]>,
        timeout: Duration,
    ) -> Result<usize, RkUsbError> {
        self.cbw_transaction_ex(cbw, data_out, data_in, timeout, true)
    }

    fn cbw_transaction_ex(
        &mut self,
        cbw: &usb::Cbw<usb::Cbwcb>,
        data_out: Option<&[u8]>,
        data_in: Option<&mut [u8]>,
        timeout: Duration,
        require_full_in: bool,
    ) -> Result<usize, RkUsbError> {
        let deadline = Instant::now() + timeout;

        let opcode = cbw.cb.oper_code;
        let cbw_tag = cbw.tag;
        let cbw_len = cbw.data_transfer_length;
        trace!("Sending CBW opcode={opcode:#04X} tag={cbw_tag:#010X} len={cbw_len}");
        let n = self.device.write_bulk(
            self.bulk_out,
            cbw.as_bytes(),
            Self::remaining_timeout(deadline)?,
        )?;
        if n != std::mem::size_of::<usb::Cbw<usb::Cbwcb>>() {
            return Err(RkUsbError::Usb(rusb::Error::Io));
        }

        if let Some(buf) = data_out {
            trace!("Writing data stage bytes={}", buf.len());
            let n =
                self.device
                    .write_bulk(self.bulk_out, buf, Self::remaining_timeout(deadline)?)?;
            if n != buf.len() {
                return Err(RkUsbError::Usb(rusb::Error::Io));
            }
        }

        let data_in_len = if let Some(buf) = data_in {
            trace!("Reading data stage bytes={}", buf.len());
            let n = self
                .device
                .read_bulk(self.bulk_in, buf, Self::remaining_timeout(deadline)?)?;
            let expected_min = cbw.data_transfer_length as usize;
            if n == 0 || n > buf.len() {
                return Err(RkUsbError::Usb(rusb::Error::Io));
            }
            if require_full_in && n < expected_min {
                return Err(RkUsbError::Usb(rusb::Error::Io));
            }
            n
        } else {
            0
        };

        let mut csw_buf = [0u8; std::mem::size_of::<usb::Csw>()];
        let n = self.device.read_bulk(
            self.bulk_in,
            &mut csw_buf,
            Self::remaining_timeout(deadline)?,
        )?;
        if n != csw_buf.len() {
            return Err(RkUsbError::InvalidCsw);
        }
        let csw = usb::Csw::try_read_from_bytes(&csw_buf).map_err(|_| RkUsbError::InvalidCsw)?;
        if csw.signature != CSW_SIGN {
            return Err(RkUsbError::InvalidCsw);
        }
        let csw_tag = csw.tag;
        if csw_tag != cbw_tag {
            return Err(RkUsbError::TagMismatch);
        }
        if csw.status != 0 {
            return Err(RkUsbError::CommandFailed(csw.status));
        }
        trace!("CSW validated tag={csw_tag:#010X}");
        Ok(data_in_len)
    }

    /// Open a Rockchip USB device handle and locate bulk endpoints.
    pub fn open(device: &rusb::Device<T>) -> Result<Self, RkUsbError> {
        debug!(
            "Opening USB device bus={} addr={}",
            device.bus_number(),
            device.address()
        );
        let handle = device.open()?;
        let config = device.active_config_descriptor()?;
        let interface = config
            .interfaces()
            .next()
            .ok_or(RkUsbError::Usb(rusb::Error::NotFound))?;
        let interface_desc = interface
            .descriptors()
            .next()
            .ok_or(RkUsbError::Usb(rusb::Error::NotFound))?;
        handle.set_active_configuration(config.number())?;
        handle.claim_interface(interface_desc.interface_number())?;
        let bulk_in = OnceCell::new();
        let bulk_out = OnceCell::new();
        for endpoint in interface_desc
            .endpoint_descriptors()
            .filter(|ep| ep.transfer_type() == rusb::TransferType::Bulk)
        {
            match endpoint.direction() {
                rusb::Direction::In => &bulk_in,
                rusb::Direction::Out => &bulk_out,
            }
            .set(endpoint.address())
            .map_err(|_| RkUsbError::DuplicateBulkEndpoint)?;
        }
        Ok(Self {
            device: handle,
            bulk_in: *bulk_in
                .get()
                .ok_or(RkUsbError::Usb(rusb::Error::NotFound))?,
            bulk_out: *bulk_out
                .get()
                .ok_or(RkUsbError::Usb(rusb::Error::NotFound))?,
            flash_sectors: None,
        })
    }

    fn flash_sector_count_cached(&mut self) -> Result<u32, RkUsbError> {
        if let Some(sectors) = self.flash_sectors {
            return Ok(sectors);
        }
        match self.read_storage_info() {
            Ok(info) => {
                let sectors = info.flash_size_sectors();
                self.flash_sectors = Some(sectors);
                Ok(sectors)
            }
            Err(err) => {
                debug!("Unable to read flash size for LBA range check: {err}");
                Ok(0)
            }
        }
    }

    fn ensure_lba_range(&mut self, pos: u32, count: u32) -> Result<(), RkUsbError> {
        let flash_sectors = self.flash_sector_count_cached()?;
        check_lba_range(pos, count, flash_sectors)
    }

    fn vendor_request(&mut self, dw_request: u16, data: &[u8]) -> Result<(), RkUsbError> {
        const CRC: Crc<u16> = Crc::<u16>::new(&CRC_16_IBM_3740);
        let crc16 = CRC.checksum(data);
        debug!(
            "Vendor request={dw_request:#06X} payload={} bytes crc16={crc16:#06X}",
            data.len()
        );
        let mut data = Vec::from(data);
        data.push((crc16 >> 8) as u8);
        data.push((crc16 & 0xFF) as u8);
        for chunk in data.chunks(4096) {
            let n = self
                .device
                .write_control(0x40, 0xC, 0, dw_request, chunk, USB_TIMEOUT)?;
            if n != chunk.len() {
                // No enough bytes written
                return Err(RkUsbError::Usb(rusb::Error::Io));
            }
            trace!("Vendor request chunk sent bytes={n}");
        }
        Ok(())
    }

    fn write_lba_raw(
        &mut self,
        pos: u32,
        data: &[u8],
        subcode: u8,
        timeout: Duration,
    ) -> Result<(), RkUsbError> {
        if data.is_empty() {
            debug!("Skipping empty LBA write at start_sector={pos}");
            return Ok(());
        }

        if !data.len().is_multiple_of(STORAGE_SECTOR_SIZE) {
            return Err(RkUsbError::Usb(rusb::Error::InvalidParam));
        }

        let sector_count = data.len() / STORAGE_SECTOR_SIZE;
        let sector_count_u16 =
            u16::try_from(sector_count).map_err(|_| RkUsbError::Usb(rusb::Error::InvalidParam))?;

        trace!("WRITE_LBA lba={pos:#010X} count={sector_count_u16:#06X} subcode={subcode:#04X}");

        let mut cbw = usb::Cbw::<usb::Cbwcb>::with_opcode(0x15); // WRITE_LBA
        cbw.data_transfer_length = data.len() as u32;
        cbw.cb.address = pos.to_be();
        cbw.cb.length = sector_count_u16.to_be();
        cbw.cb.reserved = subcode;
        self.cbw_transaction(&cbw, Some(data), None, timeout)?;

        Ok(())
    }

    fn read_lba_raw(
        &mut self,
        pos: u32,
        data: &mut [u8],
        subcode: u8,
        timeout: Duration,
    ) -> Result<(), RkUsbError> {
        if data.is_empty() {
            debug!("Skipping empty LBA read at start_sector={pos}");
            return Ok(());
        }

        if !data.len().is_multiple_of(STORAGE_SECTOR_SIZE) {
            return Err(RkUsbError::Usb(rusb::Error::InvalidParam));
        }

        let sector_count = data.len() / STORAGE_SECTOR_SIZE;
        let sector_count_u16 =
            u16::try_from(sector_count).map_err(|_| RkUsbError::Usb(rusb::Error::InvalidParam))?;

        trace!("READ_LBA pos={pos:#010X} count={sector_count_u16:#06X} subcode={subcode:#04X}");

        let mut cbw = usb::Cbw::<usb::Cbwcb>::with_opcode(0x14); // READ_LBA
        cbw.data_transfer_length = data.len() as u32;
        cbw.cb.address = pos.to_be();
        cbw.cb.length = sector_count_u16.to_be();
        cbw.cb.reserved = subcode;

        self.cbw_transaction(&cbw, None, Some(data), timeout)?;
        Ok(())
    }

    fn erase_lba_raw(
        &mut self,
        pos: u32,
        sector_count: u16,
        timeout: Duration,
    ) -> Result<(), RkUsbError> {
        if sector_count == 0 {
            debug!("Skipping empty LBA erase at start_sector={pos}");
            return Ok(());
        }

        trace!("ERASE_LBA pos={pos:#010X} count={sector_count:#06X}");

        let mut cbw = usb::Cbw::<usb::Cbwcb>::with_opcode(0x25); // ERASE_LBA
        cbw.cb.address = pos.to_be();
        cbw.cb.length = sector_count.to_be();
        self.cbw_transaction(&cbw, None, None, timeout)?;
        Ok(())
    }
}

impl<T: rusb::UsbContext> RkDevice<T> {
    /// Download boot entries from a parsed Rockchip boot image.
    pub fn download_boot(&mut self, boot_img: RkBootImage) -> Result<(), RkUsbError> {
        for (name, data, delay) in boot_img.iter_entries(RkBootEntryType::Entry471) {
            info!("Downloading {name} with request 0x0471");
            self.vendor_request(0x0471, data)?;
            sleep(delay);
        }
        for (name, data, delay) in boot_img.iter_entries(RkBootEntryType::Entry472) {
            info!("Downloading {name} with request 0x0472");
            self.vendor_request(0x0472, data)?;
            sleep(delay);
        }
        Ok(())
    }

    /// Reset the connected device with a specific reset subcode.
    pub fn reset_device(&mut self, subcode: u8) -> Result<(), RkUsbError> {
        info!("Resetting device with subcode={subcode:#04X}");
        let mut cbw = usb::Cbw::<usb::Cbwcb>::with_opcode(0xff); // DEVICE_RESET
        cbw.cb.reserved = subcode;
        self.cbw_transaction(&cbw, None, None, USB_TIMEOUT)?;
        Ok(())
    }

    /// Read device capability bytes.
    pub fn read_capability(&mut self, timeout: Duration) -> Result<[u8; 8], RkUsbError> {
        debug!("Reading device capability");
        let mut capability = [0u8; 8];
        let mut cbw = usb::Cbw::<usb::Cbwcb>::with_opcode(0xAA); // READ_CAPABILITY
        cbw.data_transfer_length = std::mem::size_of_val(&capability) as u32;
        self.cbw_transaction(&cbw, None, Some(&mut capability), timeout)?;
        Ok(capability)
    }

    /// Read storage information from device (opcode 0x1A), compatible with rkdeveloptool behavior.
    pub fn read_storage_info(&mut self) -> Result<RkFlashInfo, RkUsbError> {
        debug!("Reading flash info");
        let mut cbw = usb::Cbw::<usb::Cbwcb>::with_opcode(0x1A); // READ_FLASH_INFO
        cbw.data_transfer_length = std::mem::size_of::<RkFlashInfo>() as u32;

        let mut info_buf = [0u8; 512];
        let info_len = self.cbw_transaction(&cbw, None, Some(&mut info_buf), USB_TIMEOUT)?;
        let info_buf = info_buf
            .get(0..info_len)
            .ok_or(RkUsbError::InvalidFlashInfoLength(info_len))?;
        RkFlashInfo::try_read_from_bytes(info_buf)
            .map_err(|_| RkUsbError::InvalidFlashInfoLength(info_len))
    }

    /// Read current storage selection from device.
    ///
    /// Return value matches Rockchip storage code (for example, 1=EMMC, 2=SD, 9=SPINOR).
    /// Returns `None` when the device reports no active storage bit.
    pub fn read_storage(&mut self) -> Result<Option<u8>, RkUsbError> {
        debug!("Reading current storage type");
        let mut cbw = usb::Cbw::<usb::Cbwcb>::with_opcode(0x2B); // READ_STORAGE
        cbw.data_transfer_length = 4;
        let mut storage_bits_buf = [0u8; 4];
        self.cbw_transaction(&cbw, None, Some(&mut storage_bits_buf), USB_TIMEOUT)?;
        let storage_bits = u32::from_le_bytes(storage_bits_buf);
        let selected = (storage_bits != 0).then_some(storage_bits.trailing_zeros() as u8);
        debug!("Storage bitmap={storage_bits:#010X}, selected={selected:?}");
        Ok(selected)
    }

    /// Change device storage.
    pub fn switch_storage(&mut self, storage: u8) -> Result<(), RkUsbError> {
        info!("Switching storage to code={storage}");
        let mut cbw = usb::Cbw::<usb::Cbwcb>::with_opcode(0x2A); // CHANGE_STORAGE
        cbw.cb.reserved = storage;
        self.cbw_transaction(&cbw, None, None, USB_TIMEOUT)?;
        self.flash_sectors = None;
        Ok(())
    }

    /// Change device storage using a typed storage selector.
    pub fn switch_storage_type(&mut self, storage: RkStorageType) -> Result<(), RkUsbError> {
        self.switch_storage(storage as u8)
    }

    /// Write bytes to storage starting at the given sector.
    ///
    /// Even through this method named `write_lba`, the rkusb protocol runs with unit of sector (512 byte).
    /// So always consider position and length in sectors. The device's underlying implementation abstracts away the LBA size difference.
    ///
    /// Transfers are batched automatically. If the last chunk is not a whole block,
    /// a read-modify-write is used to preserve remaining bytes.
    ///
    /// The LBA range is checked against the current flash size before any write.
    /// `timeout` is applied to each USB transfer chunk, not the whole operation.
    ///
    /// # Arguments
    ///
    /// * `pos` - The index of the first sector to be write.
    /// * `data` - The input buffer for this operation.
    /// * `subcode` - `0` for "RWMETHOD_IMAGE" and `1` for "RWMETHOD_LBA". No idea what it means.
    /// * `timeout` - USB timeout for each transfer chunk.
    ///
    pub fn write_lba(
        &mut self,
        pos: u32,
        data: &[u8],
        subcode: u8,
        timeout: Duration,
    ) -> Result<(), RkUsbError> {
        self.write_lba_with_progress(pos, data, subcode, timeout, &mut |_, _| {})
    }

    /// Write bytes to storage and report `(bytes_done, bytes_total)` after each chunk.
    pub fn write_lba_with_progress(
        &mut self,
        pos: u32,
        data: &[u8],
        subcode: u8,
        timeout: Duration,
        progress: &mut dyn FnMut(u64, u64),
    ) -> Result<(), RkUsbError> {
        let total = data.len() as u64;
        progress(0, total);
        if data.is_empty() {
            debug!("Skipping empty LBA write at start_sector={pos}");
            return Ok(());
        }

        self.ensure_lba_range(pos, sector_count_for_bytes(data.len())?)?;

        let aligned_len = data.len() - (data.len() % STORAGE_SECTOR_SIZE);
        let (aligned, tail) = data.split_at(aligned_len);
        let mut next_pos = pos;
        let mut done = 0u64;

        for chunk in aligned.chunks(MAX_LBA_TRANSFER_BYTES) {
            self.write_lba_raw(next_pos, chunk, subcode, timeout)?;
            next_pos = Self::advance_lba_by_bytes(next_pos, chunk.len())?;
            done += chunk.len() as u64;
            progress(done, total);
        }

        if !tail.is_empty() {
            let mut sector = [0u8; STORAGE_SECTOR_SIZE];
            self.read_lba_raw(next_pos, &mut sector, subcode, timeout)?;
            sector[..tail.len()].copy_from_slice(tail);
            self.write_lba_raw(next_pos, &sector, subcode, timeout)?;
            progress(total, total);
        }

        Ok(())
    }

    /// Read bytes from storage starting at the given sector.
    ///
    /// Even through this method named `read_lba`, the rkusb protocol runs with unit of sector (512 byte).
    /// So always consider position and length in sectors. The device's underlying implementation abstracts away the LBA size difference.
    ///
    /// Transfers are batched automatically. If the output length is not a whole
    /// sector, the trailing bytes are satisfied from one extra 512-byte read.
    ///
    /// The LBA range is checked against the current flash size before any read.
    /// `timeout` is applied to each USB transfer chunk, not the whole operation.
    ///
    /// # Arguments
    ///
    /// * `pos` - The index of the first sector to be read.
    /// * `data` - The output buffer for this operation.
    /// * `subcode` - `0` for "RWMETHOD_IMAGE" and `1` for "RWMETHOD_LBA". No idea what it means.
    /// * `timeout` - USB timeout for each transfer chunk.
    ///
    pub fn read_lba(
        &mut self,
        pos: u32,
        data: &mut [u8],
        subcode: u8,
        timeout: Duration,
    ) -> Result<(), RkUsbError> {
        self.read_lba_with_progress(pos, data, subcode, timeout, &mut |_, _| {})
    }

    /// Read bytes from storage and report `(bytes_done, bytes_total)` after each chunk.
    pub fn read_lba_with_progress(
        &mut self,
        pos: u32,
        data: &mut [u8],
        subcode: u8,
        timeout: Duration,
        progress: &mut dyn FnMut(u64, u64),
    ) -> Result<(), RkUsbError> {
        let total = data.len() as u64;
        progress(0, total);
        if data.is_empty() {
            debug!("Skipping empty LBA read at start_sector={pos}");
            return Ok(());
        }

        self.ensure_lba_range(pos, sector_count_for_bytes(data.len())?)?;

        let aligned_len = data.len() - (data.len() % STORAGE_SECTOR_SIZE);
        let (aligned, tail) = data.split_at_mut(aligned_len);
        let mut next_pos = pos;
        let mut done = 0u64;

        for chunk in aligned.chunks_mut(MAX_LBA_TRANSFER_BYTES) {
            self.read_lba_raw(next_pos, chunk, subcode, timeout)?;
            next_pos = Self::advance_lba_by_bytes(next_pos, chunk.len())?;
            done += chunk.len() as u64;
            progress(done, total);
        }

        if !tail.is_empty() {
            let mut sector = [0u8; STORAGE_SECTOR_SIZE];
            self.read_lba_raw(next_pos, &mut sector, subcode, timeout)?;
            tail.copy_from_slice(&sector[..tail.len()]);
            progress(total, total);
        }

        Ok(())
    }

    /// Erase sectors from storage starting at the given LBA.
    ///
    /// Even through this method named `erase_lba`, the rkusb protocol runs with unit of sector (512 byte).
    /// So always consider position and counts in sectors. The device's underlying implementation abstracts away the LBA size difference.
    ///
    /// The range is split into device-sized commands automatically.
    ///
    /// The LBA range is checked against the current flash size before any erase.
    /// `timeout` is applied to each USB transfer chunk, not the whole operation.
    ///
    /// # Arguments
    ///
    /// * `pos` - The index of the first LBA to be erased.
    /// * `count` - The number of LBAs to be erased.
    /// * `timeout` - USB timeout for each transfer chunk.
    ///
    pub fn erase_lba(&mut self, pos: u32, count: u32, timeout: Duration) -> Result<(), RkUsbError> {
        self.erase_lba_with_progress(pos, count, timeout, &mut |_, _| {})
    }

    /// Erase sectors and report `(bytes_done, bytes_total)` after each chunk.
    pub fn erase_lba_with_progress(
        &mut self,
        pos: u32,
        count: u32,
        timeout: Duration,
        progress: &mut dyn FnMut(u64, u64),
    ) -> Result<(), RkUsbError> {
        let total = count as u64 * STORAGE_SECTOR_SIZE as u64;
        progress(0, total);
        if count == 0 {
            debug!("Skipping empty LBA erase at start_sector={pos}");
            return Ok(());
        }

        self.ensure_lba_range(pos, count)?;

        let mut next_pos = pos;
        let mut remaining = count;
        let mut done = 0u64;

        while remaining != 0 {
            let chunk_sectors = remaining.min(u16::MAX as u32);
            self.erase_lba_raw(next_pos, chunk_sectors as u16, timeout)?;
            next_pos = next_pos
                .checked_add(chunk_sectors)
                .ok_or(RkUsbError::LbaOverflow)?;
            remaining -= chunk_sectors;
            done += chunk_sectors as u64 * STORAGE_SECTOR_SIZE as u64;
            progress(done, total);
        }

        Ok(())
    }

    fn capability_bits(&mut self) -> Result<[u8; 8], RkUsbError> {
        self.read_capability(USB_TIMEOUT)
    }

    fn loader_supports_vendor_storage(&mut self) -> Result<bool, RkUsbError> {
        match self.capability_bits() {
            Ok(cap) => Ok((cap[0] & ((1 << 1) | (1 << 4))) != 0),
            Err(err) => {
                debug!("capability probe failed, continuing SN attempt: {err}");
                Ok(true)
            }
        }
    }

    fn loader_supports_otp(&mut self) -> Result<bool, RkUsbError> {
        match self.capability_bits() {
            Ok(cap) => Ok((cap[1] & (1 << 3)) != 0),
            Err(err) => {
                debug!("capability probe failed, continuing OTP attempt: {err}");
                Ok(true)
            }
        }
    }

    fn vendor_storage_cbw(
        opcode: u8,
        index: u16,
        backend: VendorBackend,
        len: u16,
    ) -> usb::Cbw<usb::Cbwcb> {
        let mut cbw = usb::Cbw::<usb::Cbwcb>::with_opcode(opcode);
        cbw.data_transfer_length = len as u32;
        // CDB address = index (BE16) || type (BE16), matching xrock vs_read/vs_write.
        cbw.cb.address = ((index as u32) << 16 | backend as u32).to_be();
        cbw.cb.length = len.to_be();
        cbw
    }

    /// Read a vendor-storage item (RKDevInfoWriteTool / xrock `vs`, opcode 0x27).
    pub fn read_vendor_storage(
        &mut self,
        index: u16,
        backend: VendorBackend,
        buf: &mut [u8],
    ) -> Result<usize, RkUsbError> {
        if buf.is_empty() || buf.len() > VENDOR_STORAGE_MAX {
            return Err(RkUsbError::VendorPayloadTooLarge {
                max: VENDOR_STORAGE_MAX,
            });
        }
        if !self.loader_supports_vendor_storage()? {
            return Err(RkUsbError::VendorStorageUnsupported);
        }
        let len = u16::try_from(buf.len()).map_err(|_| RkUsbError::VendorPayloadTooLarge {
            max: VENDOR_STORAGE_MAX,
        })?;
        debug!(
            "READ_VENDOR_STORAGE index={index} backend={} len={len}",
            backend.name()
        );
        let cbw = Self::vendor_storage_cbw(0x27, index, backend, len);
        self.cbw_transaction(&cbw, None, Some(buf), USB_TIMEOUT)
    }

    /// Write a vendor-storage item (RKDevInfoWriteTool / xrock `vs`, opcode 0x26).
    pub fn write_vendor_storage(
        &mut self,
        index: u16,
        backend: VendorBackend,
        data: &[u8],
    ) -> Result<(), RkUsbError> {
        if data.is_empty() || data.len() > VENDOR_STORAGE_MAX {
            return Err(RkUsbError::VendorPayloadTooLarge {
                max: VENDOR_STORAGE_MAX,
            });
        }
        if !self.loader_supports_vendor_storage()? {
            return Err(RkUsbError::VendorStorageUnsupported);
        }
        let len = u16::try_from(data.len()).map_err(|_| RkUsbError::VendorPayloadTooLarge {
            max: VENDOR_STORAGE_MAX,
        })?;
        debug!(
            "WRITE_VENDOR_STORAGE index={index} backend={} len={len}",
            backend.name()
        );
        let cbw = Self::vendor_storage_cbw(0x26, index, backend, len);
        self.cbw_transaction(&cbw, Some(data), None, USB_TIMEOUT)?;
        Ok(())
    }

    fn read_sn_via_lba(&mut self) -> Result<Option<String>, RkUsbError> {
        let mut buf = [0u8; STORAGE_SECTOR_SIZE];
        self.read_lba(VENDOR_SN_LBA, &mut buf, 0, USB_TIMEOUT)?;
        decode_serial_number(&buf)
    }

    fn read_sn_via_vs(&mut self, backend: VendorBackend) -> Result<Option<String>, RkUsbError> {
        let mut buf = [0u8; STORAGE_SECTOR_SIZE];
        let n = self.read_vendor_storage(VendorItemId::Sn as u16, backend, &mut buf)?;
        let data = &buf[..n.min(buf.len())];
        if let Ok(Some(sn)) = decode_serial_number(data) {
            return Ok(Some(sn));
        }
        let end = data.iter().position(|&b| b == 0).unwrap_or(data.len());
        let s = String::from_utf8_lossy(&data[..end]).into_owned();
        if s.trim().is_empty() {
            Ok(None)
        } else {
            Ok(Some(s))
        }
    }

    /// Read SN the way RKDevInfoWriteTool / xrock do in loader mode.
    ///
    /// Tries the special LBA `0xFFF00001` first, then vendor-storage opcode 0x27
    /// with `VENDOR_SN_ID` (1).
    pub fn read_sn(&mut self) -> Result<Option<String>, RkUsbError> {
        self.read_sn_with_backend(VendorBackend::Vendor)
    }

    pub fn read_sn_with_backend(
        &mut self,
        backend: VendorBackend,
    ) -> Result<Option<String>, RkUsbError> {
        if !self.loader_supports_vendor_storage()? {
            return Err(RkUsbError::VendorStorageUnsupported);
        }
        match self.read_sn_via_lba() {
            Ok(Some(sn)) => return Ok(Some(sn)),
            Ok(None) => {}
            Err(err) => debug!("SN LBA 0xFFF00001 read failed: {err}"),
        }
        self.read_sn_via_vs(backend)
    }

    /// Write SN via the special LBA *and* vendor-storage opcode, matching
    /// RKDevInfoWriteTool's loader write (SN + vendor item 1).
    pub fn write_sn(&mut self, sn: &str) -> Result<(), RkUsbError> {
        self.write_sn_with_backend(sn, VendorBackend::Vendor)
    }

    pub fn write_sn_with_backend(
        &mut self,
        sn: &str,
        backend: VendorBackend,
    ) -> Result<(), RkUsbError> {
        if !self.loader_supports_vendor_storage()? {
            return Err(RkUsbError::VendorStorageUnsupported);
        }
        let sector = encode_serial_number(sn)?;
        self.write_lba(VENDOR_SN_LBA, &sector, 0, USB_TIMEOUT)?;
        // Raw SN bytes on item 1 — same ID the kernel `vendor_storage` tool uses.
        if !sn.is_empty() {
            if let Err(err) =
                self.write_vendor_storage(VendorItemId::Sn as u16, backend, sn.as_bytes())
            {
                debug!("vendor-storage opcode SN write failed (LBA write succeeded): {err}");
            }
        }
        Ok(())
    }

    pub fn read_mac(
        &mut self,
        item: VendorItemId,
        backend: VendorBackend,
    ) -> Result<Option<[u8; 6]>, RkUsbError> {
        if !item.is_mac() {
            return Err(RkUsbError::Usb(rusb::Error::InvalidParam));
        }
        let mut buf = [0u8; STORAGE_SECTOR_SIZE];
        let n = self.read_vendor_storage(item as u16, backend, &mut buf)?;
        if n < 6 || buf[..6].iter().all(|&b| b == 0) {
            return Ok(None);
        }
        let mut mac = [0u8; 6];
        mac.copy_from_slice(&buf[..6]);
        Ok(Some(mac))
    }

    pub fn write_mac(
        &mut self,
        item: VendorItemId,
        backend: VendorBackend,
        mac: [u8; 6],
    ) -> Result<(), RkUsbError> {
        if !item.is_mac() {
            return Err(RkUsbError::Usb(rusb::Error::InvalidParam));
        }
        self.write_vendor_storage(item as u16, backend, &mac)
    }

    /// Read SoC chip info (opcode 0x1B). First four bytes are a LE ASCII tag
    /// such as `3588` / `3568`.
    pub fn read_chip_info(&mut self) -> Result<[u8; 16], RkUsbError> {
        debug!("Reading chip info");
        let mut buf = [0u8; 16];
        let mut cbw = usb::Cbw::<usb::Cbwcb>::with_opcode(0x1B); // READ_CHIP_INFO
        cbw.data_transfer_length = buf.len() as u32;
        self.cbw_transaction(&cbw, None, Some(&mut buf), USB_TIMEOUT)?;
        Ok(buf)
    }

    /// Dump chip OTP / eFuse bytes (xrock `otp`, opcode 0x2C).
    pub fn read_otp(&mut self, buf: &mut [u8]) -> Result<(), RkUsbError> {
        if buf.is_empty() {
            return Err(RkUsbError::InvalidOtpLength);
        }
        if !self.loader_supports_otp()? {
            return Err(RkUsbError::OtpUnsupported);
        }
        debug!("Reading OTP bytes={}", buf.len());
        let mut cbw = usb::Cbw::<usb::Cbwcb>::with_opcode(0x2C); // READ_OTP_CHIP
        cbw.data_transfer_length = buf.len() as u32;
        self.cbw_transaction(&cbw, None, Some(buf), USB_TIMEOUT)?;
        Ok(())
    }

    /// Pull the miniloader UART ring buffer over USB (opcode 0x28 READ_COM_LOG).
    ///
    /// This is the same text that would appear on the debug serial console,
    /// including `SecureMode = 0/1`.
    pub fn read_com_log(&mut self, buf: &mut [u8]) -> Result<usize, RkUsbError> {
        if buf.is_empty() {
            return Err(RkUsbError::InvalidOtpLength);
        }
        match self.capability_bits() {
            Ok(cap) if cap[0] & (1 << 5) == 0 => {
                debug!("capability does not advertise Read Com Log; trying anyway");
            }
            Err(err) => debug!("capability probe failed, trying COM LOG anyway: {err}"),
            _ => {}
        }
        debug!("Reading COM log bytes={}", buf.len());
        let mut cbw = usb::Cbw::<usb::Cbwcb>::with_opcode(0x28); // READ_COM_LOG
        cbw.data_transfer_length = buf.len() as u32;
        self.cbw_transaction_ex(&cbw, None, Some(buf), USB_TIMEOUT, false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serial_number_roundtrip() {
        let encoded = encode_serial_number("RK3588-TEST-001").unwrap();
        assert_eq!(&encoded[0..4], &1u32.to_le_bytes());
        assert_eq!(&encoded[4..8], &15u32.to_le_bytes());
        assert_eq!(
            decode_serial_number(&encoded).unwrap().as_deref(),
            Some("RK3588-TEST-001")
        );
    }

    #[test]
    fn serial_number_invalid_flag() {
        let mut buf = [0u8; STORAGE_SECTOR_SIZE];
        assert_eq!(decode_serial_number(&buf).unwrap(), None);
        buf[0] = 1;
        buf[4] = 3;
        buf[8..11].copy_from_slice(b"abc");
        assert_eq!(decode_serial_number(&buf).unwrap().as_deref(), Some("abc"));
    }

    #[test]
    fn serial_number_too_long() {
        let sn = "a".repeat(SN_MAX_LEN + 1);
        assert!(matches!(
            encode_serial_number(&sn),
            Err(RkUsbError::SnTooLong { .. })
        ));
    }

    #[test]
    fn lba_range_rejects_overflow() {
        let err = check_lba_range(100, 50, 120).unwrap_err();
        assert!(matches!(
            err,
            RkUsbError::LbaOutOfRange {
                start: 100,
                count: 50,
                flash_sectors: 120
            }
        ));
        check_lba_range(100, 20, 120).unwrap();
        check_lba_range(VENDOR_SN_LBA, 1, 120).unwrap();
        check_lba_range(0, 10, 0).unwrap();
    }

    #[test]
    fn storage_slug_known_and_unknown() {
        assert_eq!(storage_slug(1), "eMMC");
        assert_eq!(storage_name(9), "SPI NOR");
        assert_eq!(storage_slug(9), "SPINOR");
        assert_eq!(storage_slug(99), "storage99");
    }

    #[test]
    fn mac_parse_and_format() {
        assert_eq!(
            parse_mac("88:A9:A7:00:BC:64").unwrap(),
            [0x88, 0xA9, 0xA7, 0x00, 0xBC, 0x64]
        );
        assert_eq!(
            parse_mac("88a9a700bc64").unwrap(),
            [0x88, 0xA9, 0xA7, 0x00, 0xBC, 0x64]
        );
        assert_eq!(format_mac(&[0x88, 0xA9, 0xA7, 0x00, 0xBC, 0x64]), "88:A9:A7:00:BC:64");
        assert!(parse_mac("not-a-mac").is_err());
    }

    #[test]
    fn vendor_item_slug_roundtrip() {
        assert_eq!(VendorItemId::parse_slug("sn"), Some(VendorItemId::Sn));
        assert_eq!(VendorItemId::parse_slug("wifi-mac"), Some(VendorItemId::WifiMac));
        assert_eq!(VendorBackend::parse("rpmb"), Some(VendorBackend::Rpmb));
    }
}
