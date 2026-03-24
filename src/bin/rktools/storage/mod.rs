use humansize::{BINARY, SizeFormatter};
use rkusb::RkDevice;
use std::{
    fmt,
    io::{self, Read, Seek, SeekFrom, Write},
    time::Duration,
};

pub(crate) const SECTOR_SIZE: u64 = 512;
pub(crate) const DEFAULT_LBA_SUBCODE: u8 = 0;
pub(crate) const DEFAULT_IO_TIMEOUT: Duration = Duration::from_secs(30);

pub(crate) struct RkBlockDevice<'a, T: rusb::UsbContext> {
    rkdev: &'a mut RkDevice<T>,
    pos: u64,
    disk_size_bytes: u64,
    subcode: u8,
    timeout: Duration,
}

impl<'a, T: rusb::UsbContext> RkBlockDevice<'a, T> {
    pub(crate) fn new(
        rkdev: &'a mut RkDevice<T>,
        disk_size_bytes: u64,
        subcode: u8,
        timeout: Duration,
    ) -> Self {
        Self {
            rkdev,
            pos: 0,
            disk_size_bytes,
            subcode,
            timeout,
        }
    }
}

impl<'a, T: rusb::UsbContext> TryFrom<&'a mut RkDevice<T>> for RkBlockDevice<'a, T> {
    type Error = io::Error;

    fn try_from(rkdev: &'a mut RkDevice<T>) -> Result<Self, Self::Error> {
        let info = rkdev.read_storage_info().map_err(io::Error::other)?;
        let disk_size_bytes = (info.flash_size as u64)
            .checked_mul(SECTOR_SIZE)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "disk size overflow"))?;

        Ok(Self::new(
            rkdev,
            disk_size_bytes,
            DEFAULT_LBA_SUBCODE,
            DEFAULT_IO_TIMEOUT,
        ))
    }
}

impl<T: rusb::UsbContext> fmt::Debug for RkBlockDevice<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let size = format_args!("{}", SizeFormatter::new(self.disk_size_bytes, BINARY));
        f.debug_struct("RkBlockDevice")
            .field("pos", &self.pos)
            .field("size", &size)
            .field("subcode", &self.subcode)
            .field("timeout", &self.timeout)
            .finish()
    }
}

impl<T: rusb::UsbContext> Read for RkBlockDevice<'_, T> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }

        // Fast path: aligned position and aligned length can read directly.
        let aligned_pos = self.pos.is_multiple_of(SECTOR_SIZE);
        let aligned_len = buf.len().is_multiple_of(SECTOR_SIZE as usize);
        if aligned_pos && aligned_len {
            let start_sector = self.pos / SECTOR_SIZE;
            let lba = u32::try_from(start_sector)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "LBA out of range"))?;
            self.rkdev
                .read_lba(lba, buf, self.subcode, self.timeout)
                .map_err(io::Error::other)?;
            self.pos = self.pos.checked_add(buf.len() as u64).ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "read position overflow")
            })?;
            return Ok(buf.len());
        }

        let start_sector = self.pos / SECTOR_SIZE;
        let offset_in_sector = (self.pos % SECTOR_SIZE) as usize;
        let end_pos = self
            .pos
            .checked_add(buf.len() as u64)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "read position overflow"))?;
        let total_span = offset_in_sector + buf.len();
        let sector_count = total_span.div_ceil(SECTOR_SIZE as usize);

        let lba = u32::try_from(start_sector)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "LBA out of range"))?;
        let mut tmp = vec![0u8; sector_count * SECTOR_SIZE as usize];
        self.rkdev
            .read_lba(lba, &mut tmp, self.subcode, self.timeout)
            .map_err(io::Error::other)?;

        buf.copy_from_slice(&tmp[offset_in_sector..offset_in_sector + buf.len()]);
        self.pos = end_pos;
        Ok(buf.len())
    }
}

impl<T: rusb::UsbContext> Write for RkBlockDevice<'_, T> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }

        // Fast path: aligned position and aligned length can write directly.
        let aligned_pos = self.pos.is_multiple_of(SECTOR_SIZE);
        let aligned_len = buf.len().is_multiple_of(SECTOR_SIZE as usize);
        if aligned_pos && aligned_len {
            let start_sector = self.pos / SECTOR_SIZE;
            let lba = u32::try_from(start_sector)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "LBA out of range"))?;
            self.rkdev
                .write_lba(lba, buf, self.subcode, self.timeout)
                .map_err(io::Error::other)?;
            self.pos = self.pos.checked_add(buf.len() as u64).ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "write position overflow")
            })?;
            return Ok(buf.len());
        }

        let start_sector = self.pos / SECTOR_SIZE;
        let offset_in_sector = (self.pos % SECTOR_SIZE) as usize;
        let end_pos = self.pos.checked_add(buf.len() as u64).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "write position overflow")
        })?;
        let end_sector = end_pos.saturating_sub(1) / SECTOR_SIZE;
        let sector_count = (end_sector - start_sector + 1) as usize;

        let lba = u32::try_from(start_sector)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "LBA out of range"))?;
        let mut tmp = vec![0u8; sector_count * SECTOR_SIZE as usize];

        let head_partial = offset_in_sector != 0;
        let tail_partial = !end_pos.is_multiple_of(SECTOR_SIZE);
        if head_partial || tail_partial {
            self.rkdev
                .read_lba(lba, &mut tmp, self.subcode, self.timeout)
                .map_err(|e| io::Error::other(e.to_string()))?;
        }

        tmp[offset_in_sector..offset_in_sector + buf.len()].copy_from_slice(buf);

        self.rkdev
            .write_lba(lba, &tmp, self.subcode, self.timeout)
            .map_err(|e| io::Error::other(e.to_string()))?;

        self.pos = end_pos;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<T: rusb::UsbContext> Seek for RkBlockDevice<'_, T> {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let new_pos = match pos {
            SeekFrom::Start(x) => Some(x),
            SeekFrom::End(x) => self.disk_size_bytes.checked_add_signed(x),
            SeekFrom::Current(x) => self.pos.checked_add_signed(x),
        };

        self.pos = new_pos
            .filter(|x| *x < self.disk_size_bytes)
            .ok_or(io::Error::new(io::ErrorKind::InvalidInput, "out of range"))?;
        Ok(self.pos)
    }
}
