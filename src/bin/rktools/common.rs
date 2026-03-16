use rkusb::RkUsbType;
use rusb::{Hotplug, HotplugBuilder, UsbContext};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum DeviceSelectionError {
    #[error("No suitable device found")]
    NoDeviceFound,
    #[error("Multiple devices found, please specify --bus and --addr")]
    MultipleDevicesFound,
    #[error("USB error: {0}")]
    Usb(#[from] rusb::Error),
}

struct DeviceFinder {
    bus: Option<u8>,
    addr: Option<u8>,
    usb_type: Option<RkUsbType>,
    found: Arc<Mutex<Option<rusb::Device<rusb::Context>>>>,
}

impl Hotplug<rusb::Context> for DeviceFinder {
    fn device_arrived(&mut self, device: rusb::Device<rusb::Context>) {
        if let Ok(desc) = device.device_descriptor() {
            if self.usb_type.is_some() && RkUsbType::detect(&desc) != self.usb_type {
                return;
            }
            assert!(self.bus.is_none() || device.bus_number() == self.bus.unwrap());
            assert!(self.addr.is_none() || device.address() == self.addr.unwrap());
            *self.found.lock().unwrap() = Some(device);
        }
    }

    fn device_left(&mut self, _device: rusb::Device<rusb::Context>) {}
}

fn is_device_matching(
    dev: &rusb::Device<rusb::Context>,
    bus: Option<u8>,
    addr: Option<u8>,
    rkusb_type: Option<RkUsbType>,
) -> bool {
    let Ok(desc) = dev.device_descriptor() else {
        return false;
    };
    let Some(typ) = RkUsbType::detect(&desc) else {
        return false;
    };
    rkusb_type.map_or(true, |t| t == typ)
        && bus.map_or(true, |b| dev.bus_number() == b)
        && addr.map_or(true, |a| dev.address() == a)
}

fn find_device_hotplug(
    usb_ctx: &rusb::Context,
    bus: Option<u8>,
    addr: Option<u8>,
    usb_type: Option<RkUsbType>,
    timeout: Duration,
) -> Result<rusb::Device<rusb::Context>, DeviceSelectionError> {
    let found = Arc::new(Mutex::new(None::<rusb::Device<rusb::Context>>));
    let finder = DeviceFinder {
        bus,
        addr,
        usb_type,
        found: Arc::clone(&found),
    };
    let _registration = HotplugBuilder::new().register(usb_ctx, Box::new(finder))?;

    let start = Instant::now();
    loop {
        let elapsed = start.elapsed();
        if elapsed >= timeout {
            break Err(DeviceSelectionError::Usb(rusb::Error::Timeout));
        }
        let remaining = timeout - elapsed;
        usb_ctx.handle_events(Some(remaining)).ok();
        if let Some(device) = found.lock().unwrap().take() {
            break Ok(device);
        }
    }
}

fn find_device_polling(
    usb_ctx: &rusb::Context,
    bus: Option<u8>,
    addr: Option<u8>,
    timeout: Duration,
) -> Result<rusb::Device<rusb::Context>, DeviceSelectionError> {
    let start = Instant::now();
    loop {
        let devices = usb_ctx.devices()?;
        let candidates: Vec<_> = devices
            .iter()
            .filter(|dev| is_device_matching(dev, bus, addr, None))
            .collect();

        match candidates.len() {
            0 => {
                if start.elapsed() >= timeout {
                    break Err(DeviceSelectionError::Usb(rusb::Error::Timeout));
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            1 => break Ok(candidates[0].clone()),
            _ => break Err(DeviceSelectionError::MultipleDevicesFound),
        }
    }
}

/// Find a device by bus number and address, optionally waiting with timeout
pub fn find_device(
    usb_ctx: &rusb::Context,
    bus: Option<u8>,
    addr: Option<u8>,
    timeout: Option<Duration>,
) -> Result<rusb::Device<rusb::Context>, DeviceSelectionError> {
    // First, check for already connected devices
    let devices = usb_ctx.devices()?;
    let candidates: Vec<_> = devices
        .iter()
        .filter(|dev| is_device_matching(dev, bus, addr, None))
        .collect();

    match candidates.len() {
        1 => return Ok(candidates[0].clone()),
        0 => {} // Continue to wait if timeout is set
        _ => return Err(DeviceSelectionError::MultipleDevicesFound),
    }

    // If no device found and no timeout, return error
    let Some(timeout) = timeout else {
        return Err(DeviceSelectionError::NoDeviceFound);
    };

    // Wait for new devices
    if rusb::has_hotplug() {
        find_device_hotplug(usb_ctx, bus, addr, None, timeout)
    } else {
        find_device_polling(usb_ctx, bus, addr, timeout)
    }
}
