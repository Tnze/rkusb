use std::{thread::sleep, time::Duration};

use crc::{CRC_16_KERMIT, Crc};
use thiserror::Error;

use crate::image::{BootImage, RkBootEntryType};

pub mod image;

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
    match (pid, vid) {
        (0x3203, 0x071B)
        | (0x3205, 0x071B)
        | (0x2910, 0x0BB4)
        | (0x0000, 0x2207)
        | (0x0010, 0x2207) => true,
        _ => false,
    }
}

#[derive(Debug)]
pub enum RkUsbType {
    Unknown = 0x00,
    Maskrom = 0x01,
    Loader = 0x02,
    MSC = 0x04,
}

impl RkUsbType {
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
}

#[derive(Error, Debug)]
pub enum DownloadBootErr {
    #[error("Request 0x471 error: {0}")]
    Download471Err(rusb::Error),
    #[error("Request 0x472 error: {0}")]
    Download472Err(rusb::Error),
}

impl<T: rusb::UsbContext> RkDevice<T> {
    pub fn open(device: &rusb::Device<T>) -> rusb::Result<Self> {
        let intf_num = device
            .active_config_descriptor()?
            .interfaces()
            .flat_map(|intf| intf.descriptors())
            .find(|alt| {
                alt.class_code() == 0xFF && alt.sub_class_code() == 6 && alt.protocol_code() == 5
            })
            .map(|x| x.interface_number());
        let device = device.open()?;
        if let Some(intf_num) = intf_num {
            device.claim_interface(intf_num)?;
        }
        Ok(Self { device })
    }

    fn device_request(&mut self, dw_request: u16, data: &[u8]) -> rusb::Result<()> {
        let crc16 = Crc::<u16>::new(&CRC_16_KERMIT).checksum(data);
        let mut data = Vec::from(data);
        data.push((crc16 >> 8) as u8);
        data.push((crc16 & 0xFF) as u8);
        // Crc::new(algorithm)
        for (i, chunk) in data.chunks(4096).enumerate() {
            println!("Writting [{i}] chunk");
            let n = self.device.write_control(
                0x40,
                0xC,
                0,
                dw_request,
                chunk,
                Duration::from_secs(5),
            )?;
            if n != chunk.len() {
                panic!("Transfer failed: {n}");
            }
            println!("Written {n} bytes");
        }
        Ok(())
    }

    pub fn download_boot(&mut self, boot_img: BootImage) -> Result<(), DownloadBootErr> {
        for (name, data, delay) in boot_img.iter_entries(RkBootEntryType::Entry471) {
            println!("Writing {name}");
            self.device_request(0x0471, data)
                .map_err(DownloadBootErr::Download471Err)?;
            sleep(delay);
        }
        for (name, data, delay) in boot_img.iter_entries(RkBootEntryType::Entry472) {
            println!("Writing {name}");
            self.device_request(0x0472, data)
                .map_err(DownloadBootErr::Download472Err)?;
            sleep(delay);
        }
        Ok(())
    }
}
