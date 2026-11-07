//! Ordered GPT metadata updates. Partition payloads and LBA0 are never rewritten.
use gpt::{
    GptConfig, GptDisk,
    disk::LogicalBlockSize,
    header::Header,
    partition::{self, Partition},
};
use std::{
    collections::BTreeMap,
    error::Error,
    fmt::Debug,
    io::{self, Read, Seek, SeekFrom, Write},
};

type Result<T> = std::result::Result<T, Box<dyn Error>>;
fn invalid(message: &str) -> Box<dyn Error> {
    io::Error::new(io::ErrorKind::InvalidData, message).into()
}

/// Apply a validated partition map, retaining its explicit one-based slot numbers.
/// Requires two healthy, matching GPT copies. Writes and verifies the backup before
/// touching the primary. A failed write can leave one old and one new copy; it is
/// deliberately not followed by an automatic rollback. Device flush durability is
/// determined by the underlying device (USB flash adapters may have no sync command).
pub(crate) fn apply<D: Read + Write + Seek + Debug>(
    disk: GptDisk<D>,
    partitions: BTreeMap<u32, Partition>,
) -> Result<D> {
    let lb = *disk.logical_block_size();
    let mut device = disk.take_device();
    // Re-read rather than trusting cached headers or an optionally read-only backup.
    let current = GptConfig::new()
        .logical_block_size(lb)
        .only_valid_headers(true)
        .open_from_device(&mut device)?;
    let mut primary = current.primary_header()?.clone();
    let mut backup = current.backup_header()?.clone();
    drop(current);
    let bytes = device.seek(SeekFrom::End(0))?;
    let block = u64::from(lb);
    if bytes < block * 2
        || bytes % block != 0
        || primary.current_lba != 1
        || backup.current_lba != bytes / block - 1
        || primary.backup_lba != backup.current_lba
        || backup.backup_lba != primary.current_lba
        || primary.disk_guid != backup.disk_guid
        || primary.first_usable != backup.first_usable
        || primary.last_usable != backup.last_usable
        || primary.num_parts != backup.num_parts
        || primary.part_size != 128
        || backup.part_size != 128
        || primary.header_size_le != 92
        || backup.header_size_le != 92
        || primary.revision != (1, 0)
        || backup.revision != (1, 0)
        || primary.reserved != 0
        || backup.reserved != 0
    {
        return Err(invalid("unsupported or inconsistent GPT header geometry"));
    }
    let table_blocks = (u64::from(primary.num_parts) * 128).div_ceil(block);
    if primary.num_parts == 0
        || primary.first_usable > primary.last_usable
        || primary.part_start < 2
        || primary
            .part_start
            .checked_add(table_blocks)
            .is_none_or(|end| end > primary.first_usable)
        || backup.part_start <= primary.last_usable
        || backup
            .part_start
            .checked_add(table_blocks)
            .is_none_or(|end| end > backup.current_lba)
    {
        return Err(invalid("GPT arrays overlap headers or usable sectors"));
    }
    let old_primary = partition::file_read_partitions(&mut device, &primary, lb)?;
    let old_backup = partition::file_read_partitions(&mut device, &backup, lb)?;
    if old_primary != old_backup {
        return Err(invalid("primary and backup partition arrays disagree"));
    }
    let mut ranges = Vec::new();
    let mut guids = std::collections::HashSet::new();
    for (&index, p) in &partitions {
        if index == 0
            || index > primary.num_parts
            || !p.is_used()
            || p.part_guid.is_nil()
            || !guids.insert(p.part_guid)
            || p.first_lba < primary.first_usable
            || p.last_lba > primary.last_usable
            || p.first_lba > p.last_lba
            || p.name.contains('\0')
            || p.name.encode_utf16().count() > 36
        {
            return Err(invalid("invalid partition slot, GUID, range, or name"));
        }
        ranges.push((p.first_lba, p.last_lba));
    }
    ranges.sort_unstable();
    if ranges.windows(2).any(|pair| pair[0].1 >= pair[1].0) {
        return Err(invalid("partition ranges overlap"));
    }
    // Preserve boot code, disk signature, protective/hybrid records and 4K padding.
    let mut lba0 = vec![0; block as usize];
    device.seek(SeekFrom::Start(0))?;
    device.read_exact(&mut lba0)?;
    write_table(&mut device, &backup, lb, &partitions)?;
    backup.write_backup(&mut device, lb)?;
    device.flush()?;
    verify_copy(&mut device, &backup, lb, &partitions)?;
    write_table(&mut device, &primary, lb, &partitions)?;
    primary.write_primary(&mut device, lb)?;
    device.flush()?;
    verify_copy(&mut device, &primary, lb, &partitions)?;
    verify_copy(&mut device, &backup, lb, &partitions)?;
    let mut actual_lba0 = vec![0; lba0.len()];
    device.seek(SeekFrom::Start(0))?;
    device.read_exact(&mut actual_lba0)?;
    if actual_lba0 != lba0 {
        return Err(invalid("LBA0 changed while applying GPT"));
    }
    Ok(device)
}

fn write_table<D: Read + Write + Seek + Debug>(
    device: &mut D,
    header: &Header,
    lb: LogicalBlockSize,
    parts: &BTreeMap<u32, Partition>,
) -> Result<()> {
    // Zero individual gaps in bounded memory, including entries deleted by this edit.
    let empty = Partition::zero();
    for index in 1..=header.num_parts {
        parts.get(&index).unwrap_or(&empty).write_to_device(
            device,
            u64::from(index - 1),
            header.part_start,
            lb,
            header.part_size,
        )?;
    }
    Ok(())
}

fn verify_copy<D: Read + Seek>(
    device: &mut D,
    header: &Header,
    lb: LogicalBlockSize,
    expected: &BTreeMap<u32, Partition>,
) -> Result<()> {
    let mut raw = [0; 92];
    device.seek(SeekFrom::Start(
        header
            .current_lba
            .checked_mul(u64::from(lb))
            .ok_or_else(|| invalid("header offset overflow"))?,
    ))?;
    device.read_exact(&mut raw)?;
    let stored_crc = u32::from_le_bytes(raw[16..20].try_into()?);
    raw[16..20].fill(0);
    if &raw[..8] != b"EFI PART" || stored_crc != header.crc32 || crc32fast::hash(&raw) != stored_crc
    {
        return Err(invalid("GPT header read-back CRC mismatch"));
    }
    // This reader verifies the complete on-disk array CRC, including unused slots.
    if partition::file_read_partitions(device, header, lb)? != *expected {
        return Err(invalid(
            "GPT partition array read-back differs from requested map",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    fn fixture(lb: LogicalBlockSize) -> GptDisk<Cursor<Vec<u8>>> {
        let mut disk = GptConfig::new()
            .writable(true)
            .logical_block_size(lb)
            .create_from_device(Cursor::new(vec![0; 4 * 1024 * 1024]), None)
            .unwrap();
        disk.add_partition("old", 4096, gpt::partition_types::BASIC, 0, None)
            .unwrap();
        disk.write_inplace().unwrap();
        disk
    }
    #[test]
    fn preserves_sparse_slots_guids_lba0_and_payload_for_both_block_sizes() {
        for lb in [LogicalBlockSize::Lb512, LogicalBlockSize::Lb4096] {
            let mut disk = fixture(lb);
            let guid = *disk.guid();
            let mut p = disk.partitions()[&1].clone();
            let offset = p.first_lba * u64::from(lb);
            disk.device_mut().seek(SeekFrom::Start(offset)).unwrap();
            disk.device_mut().write_all(b"payload").unwrap();
            disk.device_mut().get_mut()[0..512].fill(0x5a);
            p.name = "edited".into();
            let expected = BTreeMap::from([(3, p)]);
            let mut dev = apply(disk, expected.clone()).unwrap();
            assert_eq!(&dev.get_ref()[..512], &[0x5a; 512]);
            assert_eq!(
                &dev.get_ref()[offset as usize..offset as usize + 7],
                b"payload"
            );
            let reopened = GptConfig::new()
                .logical_block_size(lb)
                .only_valid_headers(true)
                .open_from_device(&mut dev)
                .unwrap();
            assert_eq!(*reopened.guid(), guid);
            assert_eq!(reopened.partitions(), &expected);
            let bh = reopened.backup_header().unwrap().clone();
            drop(reopened);
            assert_eq!(
                partition::file_read_partitions(&mut dev, &bh, lb).unwrap(),
                expected
            );
        }
    }
    #[test]
    fn rejects_invalid_index_and_overlap() {
        let disk = fixture(LogicalBlockSize::Lb512);
        let p = disk.partitions()[&1].clone();
        assert!(apply(disk, BTreeMap::from([(0, p)])).is_err());
        let disk = fixture(LogicalBlockSize::Lb512);
        let p = disk.partitions()[&1].clone();
        let mut q = p.clone();
        q.part_guid = *disk.guid();
        assert!(apply(disk, BTreeMap::from([(1, p), (2, q)])).is_err());
    }
    #[test]
    fn rejects_corrupted_backup_array() {
        let mut disk = fixture(LogicalBlockSize::Lb512);
        let offset = disk.backup_header().unwrap().part_start * 512;
        disk.device_mut().get_mut()[offset as usize] ^= 1;
        let parts = disk.partitions().clone();
        assert!(apply(disk, parts).is_err());
    }
    #[derive(Debug)]
    struct FailingDevice {
        inner: std::rc::Rc<std::cell::RefCell<Cursor<Vec<u8>>>>,
        fail_from: u64,
    }
    impl Read for FailingDevice {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.inner.borrow_mut().read(buf)
        }
    }
    impl Seek for FailingDevice {
        fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
            self.inner.borrow_mut().seek(pos)
        }
    }
    impl Write for FailingDevice {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            let mut inner = self.inner.borrow_mut();
            if inner.position() >= self.fail_from {
                return Err(io::Error::other("injected backup write failure"));
            }
            inner.write(buf)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    #[test]
    fn failed_backup_write_leaves_primary_copy_untouched() {
        let disk = fixture(LogicalBlockSize::Lb512);
        let mut parts = disk.partitions().clone();
        parts.get_mut(&1).unwrap().name = "new".into();
        let backup_start = disk.backup_header().unwrap().part_start * 512;
        let inner = std::rc::Rc::new(std::cell::RefCell::new(disk.take_device()));
        let before = inner.borrow().get_ref()[..34 * 512].to_vec();
        let device = FailingDevice {
            inner: inner.clone(),
            fail_from: backup_start + 128,
        };
        let disk = GptConfig::new().open_from_device(device).unwrap();
        assert!(apply(disk, parts).is_err());
        assert_eq!(&inner.borrow().get_ref()[..34 * 512], before.as_slice());
    }
    #[test]
    fn deletion_clears_both_arrays() {
        let disk = fixture(LogicalBlockSize::Lb512);
        let mut dev = apply(disk, BTreeMap::new()).unwrap();
        let reopened = GptConfig::new()
            .only_valid_headers(true)
            .open_from_device(&mut dev)
            .unwrap();
        assert!(reopened.partitions().is_empty());
        let backup = reopened.backup_header().unwrap().clone();
        drop(reopened);
        assert!(
            partition::file_read_partitions(&mut dev, &backup, LogicalBlockSize::Lb512)
                .unwrap()
                .is_empty()
        );
    }
}
