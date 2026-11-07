# Editable GPT JSON, version 1

The existing text table is a display report: it contains index/name/start/end/size/partition GUID but drops the type GUID, attribute flags, disk GUID and entry-array geometry. It cannot serve as a lossless editable metadata format. The standalone `rktools/src/partition_table.rs` model preserves those fields and rejects unknown JSON properties. It does not contain image paths or partition payloads.

## Shape

```json
{
  "format": "rkusb-gpt",
  "version": 1,
  "storage": 1,
  "flash_size_bytes": 67108864,
  "logical_block_size": 512,
  "disk_guid": "8d819d77-2df4-47bd-a6f0-b3f8b49c87fc",
  "first_usable_lba": 34,
  "last_usable_lba": 131038,
  "partition_entry_count": 128,
  "partition_entry_size": 128,
  "partitions": [
    {
      "index": 1,
      "name": "boot",
      "type_guid": "0fc63daf-8483-4772-8e79-3d69d8477de4",
      "part_guid": "526211ed-a27a-4c70-bbe0-90d769fc7fa3",
      "first_lba": 2048,
      "last_lba": 4095,
      "flags": 0
    }
  ]
}
```

All LBA values use GPT logical blocks (512 or 4096 bytes). Flash size is bytes. Rockusb transport addresses use 512-byte sectors; convert `first_lba * logical_block_size / 512` before calling direct LBA transfer APIs. End LBAs are inclusive. No redundant sector-count, human-readable size or byte-offset fields are persisted.

JSON integers are native unsigned Rust `u64` (indices/counts/version are `u32`, storage is `u8` or null). Rust serde_json preserves integer precision. Editors based on JavaScript must preserve integer text above 2^53-1; do not round-trip through floating-point numbers. `flags` preserves every attribute bit. GUIDs are nonzero hyphenated UUID strings, case-insensitive on input. Unknown but syntactically valid type GUIDs must be preserved by the GPT library.

## Validation before writes

- Require exact format/version and reject unknown fields, invalid types, negative/fractional/out-of-range integers and duplicate JSON object properties. Derived serde structs reject duplicate known properties.
- Compare storage selection, flash size, logical block size, disk GUID, usable range, entry count and entry size against a fresh table from the target. Geometry fields identify the target layout; only entries are editable. This is a same-device edit format, not a disk-cloning or resizing format.
- Require a supported block size, flash-size divisibility, nonzero entry count and 128-byte entries. Larger vendor entries cannot safely round-trip through the current library representation and must be refused.
- Compute entry-array reservation with checked arithmetic and ceiling division. Keep partitions between first/last usable LBAs, outside both GPT headers and entry arrays, and within flash capacity.
- Require unique one-based indices within entry count, nonzero unique partition GUIDs, nonzero type GUIDs, start <= end, checked byte-length arithmetic and no intersecting inclusive LBA ranges. Adjacent partitions are valid. Empty partition lists are representable.
- Names must contain no NUL and fit 36 UTF-16 code units. Empty or duplicate names are legal GPT data; use index or GUID to select these entries.

Validation returns the complete `BTreeMap<u32, gpt::partition::Partition>` only after every entry passes. Sorting by start LBA detects overlap independently of index or JSON order. Sparse indices remain intact. Export is deterministic in index order.

## Write boundary

The caller should show `diff` after validation, create a new timestamped JSON backup using `save_new`, then invoke the GPT writer. `save_new` uses exclusive creation and syncs the file before returning. A backup collision or disk error aborts rather than overwriting a previous backup. Timestamp names contain Unix seconds and nanoseconds and do not require a clock-formatting dependency.

The JSON backup preserves modeled metadata, not raw protective-MBR bytes, GPT checksums, vendor padding or invalid UTF-16 name bytes. GPT checksums must be recomputed by the library; do not accept them as user-supplied fields. Reopen and verify primary/backup GPT after writing. USB write/flush success cannot guarantee power-loss atomicity; failed writes require inspecting the target before retry.
