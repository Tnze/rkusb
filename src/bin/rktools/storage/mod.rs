use humansize::{BINARY, SizeFormatter};
use rkusb::RkDevice;
use std::{
    fmt,
    io::{self, Read, Seek, SeekFrom, Write},
    ops::{Deref, DerefMut},
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

impl<T: rusb::UsbContext> Deref for RkBlockDevice<'_, T> {
    type Target = RkDevice<T>;

    fn deref(&self) -> &Self::Target {
        self.rkdev
    }
}

impl<T: rusb::UsbContext> DerefMut for RkBlockDevice<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.rkdev
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
        let read_len = buf.len();
        let end_pos = self
            .pos
            .checked_add(read_len as u64)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "read position overflow"))?;
        let sector_size = SECTOR_SIZE as usize;
        let mut pos = self.pos;
        let mut remaining = buf;

        // 1) Handle first unaligned sector.
        let offset_in_sector = (pos % SECTOR_SIZE) as usize;
        if offset_in_sector != 0 {
            let start_sector = pos / SECTOR_SIZE;
            let lba = u32::try_from(start_sector)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "LBA out of range"))?;

            let mut tmp = [0u8; SECTOR_SIZE as usize];
            self.rkdev
                .read_lba(lba, &mut tmp, self.subcode, self.timeout)
                .map_err(io::Error::other)?;

            let readable = (sector_size - offset_in_sector).min(remaining.len());
            remaining[..readable]
                .copy_from_slice(&tmp[offset_in_sector..offset_in_sector + readable]);
            pos = pos.checked_add(readable as u64).ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "read position overflow")
            })?;
            remaining = &mut remaining[readable..];
        }

        // 2) Handle middle full sectors directly.
        let aligned_len = (remaining.len() / sector_size) * sector_size;
        if aligned_len != 0 {
            let start_sector = pos / SECTOR_SIZE;
            let lba = u32::try_from(start_sector)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "LBA out of range"))?;
            let (aligned, tail) = remaining.split_at_mut(aligned_len);
            self.rkdev
                .read_lba(lba, aligned, self.subcode, self.timeout)
                .map_err(io::Error::other)?;
            pos = pos.checked_add(aligned_len as u64).ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "read position overflow")
            })?;
            remaining = tail;
        }

        // 3) Handle last partial sector.
        if !remaining.is_empty() {
            let start_sector = pos / SECTOR_SIZE;
            let lba = u32::try_from(start_sector)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "LBA out of range"))?;

            let mut tmp = [0u8; SECTOR_SIZE as usize];
            self.rkdev
                .read_lba(lba, &mut tmp, self.subcode, self.timeout)
                .map_err(io::Error::other)?;
            remaining.copy_from_slice(&tmp[..remaining.len()]);
        }

        self.pos = end_pos;
        Ok(read_len)
    }
}

impl<T: rusb::UsbContext> Write for RkBlockDevice<'_, T> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let end_pos = self.pos.checked_add(buf.len() as u64).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "write position overflow")
        })?;
        let sector_size = SECTOR_SIZE as usize;
        let mut pos = self.pos;
        let mut remaining = buf;

        // 1) Handle first unaligned sector with read-modify-write.
        let offset_in_sector = (pos % SECTOR_SIZE) as usize;
        if offset_in_sector != 0 {
            let start_sector = pos / SECTOR_SIZE;
            let lba = u32::try_from(start_sector)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "LBA out of range"))?;

            let mut tmp = [0u8; SECTOR_SIZE as usize];
            self.rkdev
                .read_lba(lba, &mut tmp, self.subcode, self.timeout)
                .map_err(io::Error::other)?;

            let writable = (sector_size - offset_in_sector).min(remaining.len());
            tmp[offset_in_sector..offset_in_sector + writable]
                .copy_from_slice(&remaining[..writable]);

            self.rkdev
                .write_lba(lba, &tmp, self.subcode, self.timeout)
                .map_err(io::Error::other)?;
            pos = pos.checked_add(writable as u64).ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "write position overflow")
            })?;
            remaining = &remaining[writable..];
        }

        // 2) Handle middle full sectors directly.
        let aligned_len = (remaining.len() / sector_size) * sector_size;
        if aligned_len != 0 {
            let start_sector = pos / SECTOR_SIZE;
            let lba = u32::try_from(start_sector)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "LBA out of range"))?;
            let (aligned, tail) = remaining.split_at(aligned_len);
            self.rkdev
                .write_lba(lba, aligned, self.subcode, self.timeout)
                .map_err(io::Error::other)?;
            pos = pos.checked_add(aligned_len as u64).ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "write position overflow")
            })?;
            remaining = tail;
        }

        // 3) Handle last partial sector with read-modify-write.
        if !remaining.is_empty() {
            let start_sector = pos / SECTOR_SIZE;
            let lba = u32::try_from(start_sector)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "LBA out of range"))?;

            let mut tmp = [0u8; SECTOR_SIZE as usize];
            self.rkdev
                .read_lba(lba, &mut tmp, self.subcode, self.timeout)
                .map_err(io::Error::other)?;
            tmp[..remaining.len()].copy_from_slice(remaining);

            self.rkdev
                .write_lba(lba, &tmp, self.subcode, self.timeout)
                .map_err(io::Error::other)?;
        }

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
