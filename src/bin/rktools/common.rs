use rkusb::RkUsbType;
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

/// Select a device by bus number and address
pub fn select_device_by_bus_addr<T: rusb::UsbContext>(
    usb_ctx: T,
    bus: Option<u8>,
    addr: Option<u8>,
) -> Result<rusb::Device<T>, DeviceSelectionError> {
    let devices = usb_ctx.devices()?;

    let matching_devices: Vec<_> = devices
        .iter()
        .filter_map(|dev| {
            dev.device_descriptor()
                .ok()
                .and_then(|desc| RkUsbType::detect(&desc))
                .map(|_| dev)
        })
        .collect();

    let candidates: Vec<_> = if let (Some(b), Some(a)) = (bus, addr) {
        matching_devices
            .into_iter()
            .filter(|dev| dev.bus_number() == b && dev.address() == a)
            .collect()
    } else {
        matching_devices
    };

    match candidates.len() {
        0 => Err(DeviceSelectionError::NoDeviceFound),
        1 => Ok(candidates[0].clone()),
        _ => Err(DeviceSelectionError::MultipleDevicesFound),
    }
}
