//! Editable GPT metadata, deliberately independent of USB transport and CLI parsing.
use gpt::{GptDisk, partition::Partition};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashSet},
    error::Error,
    fs::OpenOptions,
    io::{Read, Seek, Write},
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

type Result<T> = std::result::Result<T, Box<dyn Error>>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PartitionTable {
    pub format: String,
    pub version: u32,
    pub storage: Option<u8>,
    pub flash_size_bytes: u64,
    pub logical_block_size: u64,
    pub disk_guid: String,
    pub first_usable_lba: u64,
    pub last_usable_lba: u64,
    pub partition_entry_count: u32,
    pub partition_entry_size: u32,
    pub partitions: Vec<PartitionEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PartitionEntry {
    pub index: u32,
    pub name: String,
    pub type_guid: String,
    pub part_guid: String,
    pub first_lba: u64,
    pub last_lba: u64,
    pub flags: u64,
}

fn invalid(message: impl Into<String>) -> Box<dyn Error> {
    std::io::Error::new(std::io::ErrorKind::InvalidData, message.into()).into()
}

fn guid(text: &str, field: &str) -> Result<Uuid> {
    let value = Uuid::parse_str(text).map_err(|_| invalid(format!("{field}: invalid GUID")))?;
    if value.is_nil()
        || text.len() != 36
        || !text.eq_ignore_ascii_case(&value.hyphenated().to_string())
    {
        return Err(invalid(format!(
            "{field}: expected a nonzero hyphenated GUID"
        )));
    }
    Ok(value)
}

impl PartitionTable {
    pub fn from_disk<D: Read + Write + Seek>(
        disk: &GptDisk<D>,
        storage: Option<u8>,
        flash_bytes: u64,
    ) -> Result<Self> {
        let header = disk.primary_header().or_else(|_| disk.backup_header())?;
        Ok(Self {
            format: "rkusb-gpt".into(),
            version: 1,
            storage,
            flash_size_bytes: flash_bytes,
            logical_block_size: u64::from(*disk.logical_block_size()),
            disk_guid: disk.guid().to_string(),
            first_usable_lba: header.first_usable,
            last_usable_lba: header.last_usable,
            partition_entry_count: header.num_parts,
            partition_entry_size: header.part_size,
            partitions: disk
                .partitions()
                .iter()
                .map(|(&index, p)| PartitionEntry {
                    index,
                    name: p.name.clone(),
                    type_guid: p.part_type_guid.guid.to_string(),
                    part_guid: p.part_guid.to_string(),
                    first_lba: p.first_lba,
                    last_lba: p.last_lba,
                    flags: p.flags,
                })
                .collect(),
        })
    }

    /// Validate the entire candidate before any disk write. Geometry describes the target, not edits.
    pub fn validate_against(&self, current: &Self) -> Result<BTreeMap<u32, Partition>> {
        if self.format != "rkusb-gpt" || self.version != 1 {
            return Err(invalid("unsupported partition table format/version"));
        }
        if self.storage != current.storage
            || self.flash_size_bytes != current.flash_size_bytes
            || self.logical_block_size != current.logical_block_size
            || self.first_usable_lba != current.first_usable_lba
            || self.last_usable_lba != current.last_usable_lba
            || self.partition_entry_count != current.partition_entry_count
            || self.partition_entry_size != current.partition_entry_size
        {
            return Err(invalid(
                "storage or GPT geometry differs from the current device",
            ));
        }
        if guid(&self.disk_guid, "disk_guid")? != guid(&current.disk_guid, "current.disk_guid")? {
            return Err(invalid("disk GUID differs from the current device"));
        }
        if !matches!(self.logical_block_size, 512 | 4096)
            || self.flash_size_bytes % self.logical_block_size != 0
        {
            return Err(invalid(
                "invalid logical block size or unaligned flash size",
            ));
        }
        // gpt 4.1 only writes the standard 128-byte entry representation.
        if self.partition_entry_size != 128 || self.partition_entry_count == 0 {
            return Err(invalid(
                "only nonempty arrays of 128-byte GPT entries are supported",
            ));
        }
        let blocks = self.flash_size_bytes / self.logical_block_size;
        let array_bytes = u64::from(self.partition_entry_count)
            .checked_mul(128)
            .ok_or_else(|| invalid("entry array overflow"))?;
        let array_blocks = array_bytes.div_ceil(self.logical_block_size);
        let minimum_first = 2u64
            .checked_add(array_blocks)
            .ok_or_else(|| invalid("GPT geometry overflow"))?;
        let maximum_last = blocks
            .checked_sub(array_blocks + 2)
            .ok_or_else(|| invalid("flash is too small for GPT"))?;
        if self.first_usable_lba < minimum_first
            || self.last_usable_lba > maximum_last
            || self.first_usable_lba > self.last_usable_lba
        {
            return Err(invalid(
                "usable range intersects reserved GPT metadata or exceeds flash",
            ));
        }
        let mut result = BTreeMap::new();
        let mut guids = HashSet::new();
        let mut ranges = Vec::new();
        for entry in &self.partitions {
            let label = format!("partition #{}", entry.index);
            if entry.index == 0
                || entry.index > self.partition_entry_count
                || result.contains_key(&entry.index)
            {
                return Err(invalid(format!("{label}: duplicate or out-of-range index")));
            }
            let part_guid = guid(&entry.part_guid, &format!("{label}.part_guid"))?;
            if !guids.insert(part_guid) {
                return Err(invalid(format!("{label}: duplicate partition GUID")));
            }
            let type_uuid = guid(&entry.type_guid, &format!("{label}.type_guid"))?;
            if entry.name.contains('\0') || entry.name.encode_utf16().count() > 36 {
                return Err(invalid(format!(
                    "{label}: name must fit 36 UTF-16 code units and contain no NUL"
                )));
            }
            if entry.first_lba > entry.last_lba
                || entry.first_lba < self.first_usable_lba
                || entry.last_lba > self.last_usable_lba
            {
                return Err(invalid(format!(
                    "{label}: invalid or out-of-bounds inclusive LBA range"
                )));
            }
            let bytes = entry
                .last_lba
                .checked_sub(entry.first_lba)
                .and_then(|n| n.checked_add(1))
                .and_then(|n| n.checked_mul(self.logical_block_size));
            if bytes.is_none() {
                return Err(invalid(format!(
                    "{label}: partition byte size overflows u64"
                )));
            }
            ranges.push((entry.first_lba, entry.last_lba, entry.index));
            result.insert(
                entry.index,
                Partition {
                    part_type_guid: type_uuid.to_string().parse().map_err(|_| {
                        invalid(format!("{label}: unsupported partition type GUID"))
                    })?,
                    part_guid,
                    first_lba: entry.first_lba,
                    last_lba: entry.last_lba,
                    flags: entry.flags,
                    name: entry.name.clone(),
                },
            );
        }
        ranges.sort_unstable();
        for pair in ranges.windows(2) {
            if pair[1].0 <= pair[0].1 {
                return Err(invalid(format!(
                    "partitions #{} and #{} overlap",
                    pair[0].2, pair[1].2
                )));
            }
        }
        Ok(result)
    }

    pub fn diff(&self, current: &Self) -> String {
        let old: BTreeMap<_, _> = current.partitions.iter().map(|p| (p.index, p)).collect();
        let new: BTreeMap<_, _> = self.partitions.iter().map(|p| (p.index, p)).collect();
        let describe = |p: &PartitionEntry| {
            format!(
                "name={:?}, LBA {}..={}, type={}, GUID={}, flags=0x{:016x}",
                p.name, p.first_lba, p.last_lba, p.type_guid, p.part_guid, p.flags
            )
        };
        let mut output = String::new();
        for (index, before) in &old {
            if !new.contains_key(index) {
                output.push_str(&format!("REMOVE #{index}: {}\n", describe(before)));
            }
        }
        for (index, after) in &new {
            match old.get(index) {
                None => output.push_str(&format!("ADD #{index}: {}\n", describe(after))),
                Some(before) if *before != *after => output.push_str(&format!(
                    "CHANGE #{index}:\n  before {}\n  after  {}\n",
                    describe(before),
                    describe(after)
                )),
                _ => {}
            }
        }
        if output.is_empty() {
            output.push_str("No partition changes.\n");
        }
        output
    }

    /// Refuse overwrite and flush a complete JSON backup before the caller performs a device write.
    pub fn save_new(&self, path: &Path) -> Result<()> {
        let mut data = serde_json::to_vec_pretty(self)?;
        data.push(b'\n');
        let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
        file.write_all(&data)?;
        file.sync_all()?;
        Ok(())
    }
}

pub fn timestamp_filename(backup: bool) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let suffix = if backup { "backup" } else { "table" };
    format!(
        "partition_{suffix}_{}_{:09}.json",
        now.as_secs(),
        now.subsec_nanos()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sample() -> PartitionTable {
        PartitionTable {
            format: "rkusb-gpt".into(),
            version: 1,
            storage: Some(1),
            flash_size_bytes: 64 * 1024 * 1024,
            logical_block_size: 512,
            disk_guid: "8d819d77-2df4-47bd-a6f0-b3f8b49c87fc".into(),
            first_usable_lba: 34,
            last_usable_lba: 131038,
            partition_entry_count: 128,
            partition_entry_size: 128,
            partitions: vec![PartitionEntry {
                index: 3,
                name: "boot".into(),
                type_guid: "12345678-1234-1234-1234-123456789abc".into(),
                part_guid: "526211ed-a27a-4c70-bbe0-90d769fc7fa3".into(),
                first_lba: 2048,
                last_lba: 4095,
                flags: u64::MAX,
            }],
        }
    }
    #[test]
    fn json_preserves_sparse_index_unknown_type_and_all_flags() {
        let table = sample();
        let parsed: PartitionTable =
            serde_json::from_str(&serde_json::to_string(&table).unwrap()).unwrap();
        assert_eq!(parsed, table);
        let parts = parsed.validate_against(&table).unwrap();
        assert_eq!(parts[&3].flags, u64::MAX);
        assert_eq!(
            parts[&3].part_type_guid.guid.to_string(),
            table.partitions[0].type_guid
        );
    }
    #[test]
    fn rejects_overlap_duplicates_reversed_ranges_and_wrong_target() {
        let table = sample();
        let mut bad = table.clone();
        bad.partitions[0].last_lba = 2047;
        assert!(bad.validate_against(&table).is_err());
        bad = table.clone();
        bad.partitions.push(bad.partitions[0].clone());
        assert!(bad.validate_against(&table).is_err());
        bad.partitions[1].index = 4;
        bad.partitions[1].part_guid = "626211ed-a27a-4c70-bbe0-90d769fc7fa3".into();
        assert!(bad.validate_against(&table).is_err());
        bad.partitions[1].first_lba = 4096;
        bad.partitions[1].last_lba = 8191;
        assert!(bad.validate_against(&table).is_ok());
        bad.storage = Some(2);
        assert!(bad.validate_against(&table).is_err());
    }
    #[test]
    fn utf16_limit_counts_surrogate_pairs_and_rejects_nul() {
        let table = sample();
        let mut edited = table.clone();
        edited.partitions[0].name = "😀".repeat(18);
        assert!(edited.validate_against(&table).is_ok());
        edited.partitions[0].name.push('a');
        assert!(edited.validate_against(&table).is_err());
        edited.partitions[0].name = String::from("boot") + &char::from(0).to_string();
        assert!(edited.validate_against(&table).is_err());
    }
    #[test]
    fn save_new_refuses_overwrite_and_preserves_original() {
        let table = sample();
        let path = std::env::temp_dir().join(format!(
            "rkusb-test-{}-{}",
            std::process::id(),
            timestamp_filename(true)
        ));
        table.save_new(&path).unwrap();
        let original = std::fs::read(&path).unwrap();
        let mut edited = table.clone();
        edited.partitions.clear();
        assert!(edited.save_new(&path).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), original);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn rejects_outside_indices_duplicate_guids_schema_and_geometry() {
        let table = sample();
        let mut bad = table.clone();
        bad.partitions[0].first_lba = 0;
        assert!(bad.validate_against(&table).is_err());
        bad = table.clone();
        bad.partitions[0].last_lba = u64::MAX;
        assert!(bad.validate_against(&table).is_err());
        bad = table.clone();
        bad.partitions[0].index = 0;
        assert!(bad.validate_against(&table).is_err());
        bad.partitions[0].index = 129;
        assert!(bad.validate_against(&table).is_err());
        bad = table.clone();
        bad.partitions.push(bad.partitions[0].clone());
        bad.partitions[1].index = 4;
        bad.partitions[1].first_lba = 4096;
        bad.partitions[1].last_lba = 8191;
        assert!(bad.validate_against(&table).is_err());
        bad = table.clone();
        bad.version = 2;
        assert!(bad.validate_against(&table).is_err());
        bad = table.clone();
        bad.logical_block_size = 4096;
        assert!(bad.validate_against(&table).is_err());
        bad = table.clone();
        bad.partitions[0].part_guid = Uuid::nil().to_string();
        assert!(bad.validate_against(&table).is_err());
    }
    #[test]
    fn rejects_reserved_space_unknown_fields_and_duplicate_properties() {
        let mut table = sample();
        table.first_usable_lba = 1;
        assert!(table.validate_against(&table).is_err());
        let json = serde_json::to_string(&sample()).unwrap();
        let unknown = json.replacen('{', "{\"unknown\":1,", 1);
        assert!(serde_json::from_str::<PartitionTable>(&unknown).is_err());
        let duplicate = json.replacen('{', "{\"version\":1,", 1);
        assert!(serde_json::from_str::<PartitionTable>(&duplicate).is_err());
    }
}
