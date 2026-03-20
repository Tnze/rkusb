use std::{cell::OnceCell, thread::sleep, time::Duration};

use crc::{CRC_16_IBM_3740, Crc};
use log::{debug, info, trace};
use thiserror::Error;
use zerocopy::TryFromBytes;

use crate::image::{BootImage, RkBootEntryType};

const USB_TIMEOUT: Duration = Duration::from_secs(5);
const STORAGE_SECTOR_SIZE: usize = 512;

#[derive(Error, Debug)]
pub enum RkUsbError {
    #[error("USB error: {0}")]
    Usb(#[from] rusb::Error),
    #[error("Duplicate bulk endpoint detected in USB interface descriptor")]
    DuplicateBulkEndpoint,
    #[error("CBW/CSW tag mismatch")]
    TagMismatch,
    #[error("Command failed with status {0}")]
    CommandFailed(u8),
    #[error("Invalid CSW data")]
    InvalidCsw,
}

pub mod image;
mod usb;

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
    Sd = 2,
    SpiNor = 9,
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
}

impl<T: rusb::UsbContext> RkDevice<T> {
    fn cbw_transaction(
        &mut self,
        cbw: &usb::Cbw<usb::Cbwcb>,
        data_out: Option<&[u8]>,
        data_in: Option<&mut [u8]>,
    ) -> Result<(), RkUsbError> {
        let opcode = cbw.cb.oper_code;
        let cbw_tag = cbw.tag;
        let cbw_len = cbw.data_transfer_length;
        trace!("Sending CBW opcode={opcode:#04X} tag={cbw_tag:#010X} len={cbw_len}");
        let n = self
            .device
            .write_bulk(self.bulk_out, cbw.as_bytes(), USB_TIMEOUT)?;
        if n != std::mem::size_of::<usb::Cbw<usb::Cbwcb>>() {
            return Err(RkUsbError::Usb(rusb::Error::Io));
        }

        if let Some(buf) = data_out {
            trace!("Writing data stage bytes={}", buf.len());
            let n = self.device.write_bulk(self.bulk_out, buf, USB_TIMEOUT)?;
            if n != buf.len() {
                return Err(RkUsbError::Usb(rusb::Error::Io));
            }
        }

        if let Some(buf) = data_in {
            trace!("Reading data stage bytes={}", buf.len());
            let n = self.device.read_bulk(self.bulk_in, buf, USB_TIMEOUT)?;
            if n != buf.len() {
                return Err(RkUsbError::Usb(rusb::Error::Io));
            }
        }

        let mut csw_buf = [0u8; std::mem::size_of::<usb::Csw>()];
        let n = self
            .device
            .read_bulk(self.bulk_in, &mut csw_buf, USB_TIMEOUT)?;
        if n != csw_buf.len() {
            return Err(RkUsbError::InvalidCsw);
        }

        let csw = usb::Csw::try_read_from_bytes(&csw_buf).map_err(|_| RkUsbError::InvalidCsw)?;
        let csw_tag = csw.tag;
        if csw_tag != cbw_tag {
            return Err(RkUsbError::TagMismatch);
        }
        if csw.status != 0 {
            return Err(RkUsbError::CommandFailed(csw.status));
        }
        trace!("CSW validated tag={csw_tag:#010X}");
        Ok(())
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
        })
    }

    fn device_request(&mut self, dw_request: u16, data: &[u8]) -> Result<(), RkUsbError> {
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

    /// Download boot entries from a parsed Rockchip boot image.
    pub fn download_boot(&mut self, boot_img: BootImage) -> Result<(), RkUsbError> {
        for (name, data, delay) in boot_img.iter_entries(RkBootEntryType::Entry471) {
            info!("Downloading {name} with request 0x0471");
            self.device_request(0x0471, data)?;
            sleep(delay);
        }
        for (name, data, delay) in boot_img.iter_entries(RkBootEntryType::Entry472) {
            info!("Downloading {name} with request 0x0472");
            self.device_request(0x0472, data)?;
            sleep(delay);
        }
        Ok(())
    }

    /// Reset the connected device with a specific reset subcode.
    pub fn reset_device(&mut self, subcode: u8) -> Result<(), RkUsbError> {
        info!("Resetting device with subcode={subcode:#04X}");
        let mut cbw = usb::Cbw::<usb::Cbwcb>::with_opcode(0xff); // DEVICE_RESET
        cbw.cb.reserved = subcode;
        self.cbw_transaction(&cbw, None, None)
    }

    /// Write a contiguous sector-aligned buffer to storage starting at the given LBA.
    pub fn write_lba(&mut self, pos: u32, data: &[u8], subcode: u8) -> Result<(), RkUsbError> {
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
        self.cbw_transaction(&cbw, Some(data), None)?;

        Ok(())
    }

    /// Read a contiguous range of sectors from storage starting at the given LBA.
    pub fn read_lba(&mut self, pos: u32, data: &mut [u8], subcode: u8) -> Result<(), RkUsbError> {
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

        self.cbw_transaction(&cbw, None, Some(data))
    }

    /// Erase a contiguous range of sectors from storage starting at the given LBA.
    pub fn erase_lba(&mut self, pos: u32, sector_count: u16) -> Result<(), RkUsbError> {
        if sector_count == 0 {
            debug!("Skipping empty LBA erase at start_sector={pos}");
            return Ok(());
        }

        trace!("ERASE_LBA pos={pos:#010X} count={sector_count:#06X}");

        let mut cbw = usb::Cbw::<usb::Cbwcb>::with_opcode(0x25); // ERASE_LBA
        cbw.cb.address = pos.to_be();
        cbw.cb.length = sector_count.to_be();
        self.cbw_transaction(&cbw, None, None)
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
        self.cbw_transaction(&cbw, None, Some(&mut storage_bits_buf))?;
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
        self.cbw_transaction(&cbw, None, None)
    }

    /// Change device storage using a typed storage selector.
    pub fn switch_storage_type(&mut self, storage: RkStorageType) -> Result<(), RkUsbError> {
        self.switch_storage(storage as u8)
    }
}
