# RKNS Header Parsing Rules

This document describes the Rockchip RKNS header format used by the newer
IDBlock layout on RK35-family parts such as RK356x and RK3588. It is written
as an implementation-oriented reference for a Rust tool which needs to:

- locate an RKNS header inside a full flash image or a standalone `idblock.bin`
- dump the parsed header fields
- enumerate embedded stage images
- verify header and payload hashes when possible
- identify which embedded image is likely to be SPL

The focus here is the header format visible in the current workspace and the
verified layout seen in the user's SPI NOR dump.

## Scope

This document covers the newer Rockchip header variant, called `header0_info_v2`
in U-Boot, with magic `RKNS`.

It does not attempt to fully document the older RC4-based Rockchip header used
by older SoCs such as RK30xx, RK32xx, RK33xx. A dumper should distinguish the
two formats by magic and parse them separately.

## Source of Truth in This Workspace

Primary code references:

- `misc/u-boot/tools/rkcommon.c`
- `misc/u-boot/scripts/spl.sh`
- `misc/rkbin/RKBOOT/RK3588MINIALL.ini`
- `misc/u-boot/arch/arm/include/asm/arch-rockchip/boot0.h`

Relevant observations already verified against the user's samples:

- `workspace/idblock.bin` is a self-consistent standalone RKNS IDBlock sample
- `unpack/SPI_FLASH_BACKUP.img` contains an RKNS IDBlock starting at `0x8000`
- the GPT partition table exists in the same flash image, but the IDBlock area
  is outside all GPT-defined partitions

## Format Identification

### New header, RKNS

- Magic bytes: `52 4b 4e 53`
- ASCII: `RKNS`
- Little-endian `u32`: `0x534e4b52`

In the current U-Boot tree this corresponds to `RK_MAGIC_V2`.

### Old header, non-RKNS

- Little-endian `u32`: `0x0ff0aa55`

This is the older header format and should not be parsed with the RKNS rules.

## Medium-Level Placement

The RKNS header is an IDBlock header. It is usually found:

- at offset `0x0000` inside a standalone `idblock.bin`
- at offset `0x8000` inside a raw boot medium image, because Rockchip places
  the boot header at sector 64 and Rockchip sectors here are 512-byte sectors

For the verified SPI NOR sample in this workspace:

- image file: `unpack/SPI_FLASH_BACKUP.img`
- IDBlock base offset: `0x8000`

This means that every payload offset stored in the RKNS header must be treated
as relative to `idblock_base`, not relative to the start of the whole flash
image.

## GPT Interaction

In the verified SPI NOR sample, GPT exists and starts normally near the front
of the device:

- protective MBR area near `0x0000`
- GPT header at `0x0200`
- GPT entry array at `0x0400`

The first defined partitions in the sample are:

- `vnvm`: `0x180000` to `0x1bffff`
- `misc`: `0x200000` to `0x5fffff`
- `uboot_a`: `0x800000` to `0xbfffff`
- `uboot_b`: `0xc00000` to `0xffffff`

The RKNS IDBlock at `0x8000` is therefore in the pre-partition boot region and
is not itself described by GPT.

## Packed C Definitions

The following definitions are taken from the visible U-Boot source and are the
best available definition for the RKNS layout in this workspace.

```c
struct image_entry {
    uint32_t size_and_off;
    uint32_t address;
    uint32_t flag;
    uint32_t counter;
    uint8_t reserved[8];
    uint8_t hash[64];
};

struct header0_info_v2 {
    uint32_t magic;
    uint8_t reserved[4];
    uint32_t size_and_nimage;
    uint32_t boot_flag;
    uint8_t reserved1[104];
    struct image_entry images[4];
    uint8_t reserved2[1064];
    uint8_t hash[512];
};
```

Important consequences:

- the RKNS header is exactly `0x800` bytes
- the `images` array starts at offset `0x78`
- each `image_entry` is exactly `88` bytes
- the header reserves space for at most 4 image entries

## On-Disk Layout of RKNS Header

All multi-byte integer fields are little-endian.

| Offset | Size | Field | Meaning |
| --- | ---: | --- | --- |
| `0x000` | 4 | `magic` | must be `RKNS` |
| `0x004` | 4 | `reserved` | currently zero in observed images |
| `0x008` | 4 | `size_and_nimage` | upper 16 bits = image count, lower 16 bits = header hash offset in 4-byte words |
| `0x00c` | 4 | `boot_flag` | low nibble is hash type |
| `0x010` | `0x68` | `reserved1` | currently zero in observed images |
| `0x078` | `4 * 88` | `images[4]` | image descriptors |
| `0x1d8` | `0x428` | `reserved2` | currently zero in observed images |
| `0x600` | `0x200` | `hash` area | header hash or signature blob |

The values above are derived from the packed struct layout and match the actual
hexdump of the verified samples.

## Image Entry Layout

Each entry is 88 bytes and begins at:

```text
entry_offset = 0x78 + image_index * 88
```

Image entry layout:

| Relative Offset | Size | Field | Meaning |
| --- | ---: | --- | --- |
| `+0x00` | 4 | `size_and_off` | upper 16 bits = payload size in 512-byte sectors, lower 16 bits = payload start offset in 512-byte sectors relative to IDBlock base |
| `+0x04` | 4 | `address` | usually `0xffffffff`; do not treat this as the CPU entry address |
| `+0x08` | 4 | `flag` | currently zero in observed images |
| `+0x0c` | 4 | `counter` | usually 1-based sequence number |
| `+0x10` | 8 | `reserved` | currently zero in observed images |
| `+0x18` | 64 | `hash` | digest or signature blob for this payload |

### `size_and_off` decoding

Let:

```text
sector_count = (size_and_off >> 16) & 0xffff
sector_off   = size_and_off & 0xffff
```

Then:

```text
payload_size_bytes  = sector_count * 512
payload_rel_offset  = sector_off * 512
payload_abs_offset  = idblock_base + payload_rel_offset
payload_abs_end     = payload_abs_offset + payload_size_bytes - 1
```

This is the single most important rule for locating embedded stages.

## Header-Level Field Semantics

### `size_and_nimage`

This packs two values:

```text
nimage          = (size_and_nimage >> 16) & 0xffff
hash_word_off   = size_and_nimage & 0xffff
hash_byte_off   = hash_word_off * 4
```

For the verified RK3588 samples:

- `size_and_nimage = 0x00020180`
- `nimage = 2`
- `hash_byte_off = 0x180 * 4 = 0x600`

### `boot_flag`

Visible code comments define the low nibble as hash type:

- `0`: no hash
- `1`: SHA-256
- `2`: SHA-512

For robust parsing use:

```text
hash_type = boot_flag & 0x0f
```

The visible U-Boot code for RKNS generation writes SHA-256.

## Hash Verification Rules

### Header hash

The visible generator computes the header digest over the prefix of the header
which ends right before the `hash` field.

For the common RK3588 case:

```text
header_hash_input = header[0 .. hash_byte_off)
```

If `hash_type == 1`, compare the computed SHA-256 digest against the first 32
bytes beginning at `header[hash_byte_off]`.

In the verified samples:

- the header hash area starts at `0x600`
- the first 32 bytes at `0x600` match SHA-256 of `header[0..0x600)`

### Per-image hash

The visible generator computes a digest over the exact payload bytes for each
image and stores it in `image_entry.hash`.

For `hash_type == 1`:

- compare SHA-256 of the payload against the first 32 bytes of the entry hash
- ignore the remaining 32 bytes of the 64-byte field unless future images use
  them for signatures or SHA-512

For `hash_type == 2`:

- compare SHA-512 against all 64 bytes

### Important observed caveat

`workspace/idblock.bin` is fully self-consistent:

- header hash matches
- image 0 hash matches
- image 1 hash matches

`unpack/SPI_FLASH_BACKUP.img` is only partially self-consistent:

- header hash matches
- image payload hashes do not match the payload bytes currently present in the
  dump

A dumper should therefore:

- always parse and print the structure even if image hashes fail
- treat image hash failure as a warning, not a structural parse failure
- report header hash status and per-image hash status separately

## Relationship to Rockchip Build Flow

In this workspace the path used by `make.sh --spl` is:

1. `misc/u-boot/make.sh` dispatches `--spl` and `--tpl` to `pack_spl_loader_image`
2. that function invokes `misc/u-boot/scripts/spl.sh`
3. `spl.sh` rewrites the loader `.ini` so that:
   - `FlashData` points to the first stage blob
   - `FlashBoot` points to the second stage blob

The relevant mapping in `spl.sh` is:

- `FlashData=.\/tmp\/tpl.bin`
- `FlashBoot=.\/tmp\/u-boot-spl.bin`

For the stock RK3588 loader configuration in this workspace, the `.ini` says:

- `FlashData=bin/rk35/rk3588_ddr_lp4_2112MHz_lp5_2400MHz_v1.19.bin`
- `FlashBoot=bin/rk35/rk3588_spl_v1.13.bin`

Therefore, in practice on RK3588:

- image 0 is the init stage, often DDR init blob or custom TPL-like first stage
- image 1 is the boot stage, usually SPL

When implementing a generic dumper, label them conservatively as:

- `image0`, role `init` or `FlashData`
- `image1`, role `boot` or `FlashBoot`

If the tool is specifically told it is handling an RK3588 loader packed via the
same path as this workspace, it is reasonable to additionally label image 1 as
the likely SPL payload.

## Worked Example: `unpack/SPI_FLASH_BACKUP.img`

This section records the verified values for the user's full SPI NOR dump.

### IDBlock base

- file: `unpack/SPI_FLASH_BACKUP.img`
- IDBlock base offset: `0x8000`

### Header fields

- magic: `RKNS`
- `size_and_nimage = 0x00020180`
- `nimage = 2`
- header hash offset = `0x600`
- `boot_flag = 0x00000001`, so hash type is SHA-256

### Image table

Image 0 entry:

- `size_and_off = 0x00980004`
- sector offset = `0x0004`
- sector count = `0x0098`
- relative payload offset = `0x0800`
- absolute flash offset = `0x8800`
- payload size = `0x13000`
- flash range = `0x8800 .. 0x1b7ff`

Image 1 entry:

- `size_and_off = 0x01f8009c`
- sector offset = `0x009c`
- sector count = `0x01f8`
- relative payload offset = `0x13800`
- absolute flash offset = `0x1b800`
- payload size = `0x3f000`
- flash range = `0x1b800 .. 0x5a7ff`

### SPL identification in this sample

Because the RK3588 pack path maps `FlashBoot` to the second payload, the likely
SPL stage in this sample is:

- absolute flash offset `0x1b800`
- size `0x3f000`

### First instruction words in the sample

Image 0 starts with these first 32-bit little-endian words:

```text
0x14000001 0xa9bf13e0 0xa9bf7bfd 0x58000164
```

The first word `0x14000001` is the common Rockchip boot0-hook branch-to-next
stub. In that specific case, the first meaningful instruction stream begins at
payload offset `+4`.

Image 1 starts with these first 32-bit little-endian words:

```text
0xf941e260 0x52800027 0xb90003e1 0x52800306
```

This payload does not begin with the branch stub and appears to begin directly
with ordinary AArch64 instructions.

## Entry Point Semantics

There are three different ideas that can be confused with each other:

1. the payload's location in flash
2. the first executable word inside that payload
3. the runtime address in SRAM or DRAM where BootROM loads and jumps to it

The RKNS header only gives reliable information about item 1.

For item 2, a dumper may use a heuristic:

- if the first word is `0x14000001`, annotate that the first effective code may
  begin at payload offset `+4`
- otherwise annotate that the payload appears to start directly with code at
  offset `+0`

For item 3, the RKNS header is not authoritative. The `address` field in the
image entry is usually `0xffffffff` and is not a reliable runtime entry
address. To recover the actual runtime load address and entry address you need
the corresponding ELF, map file, or SoC-specific BootROM behavior.

## Recommended Rust Parsing Model

Do not directly reinterpret the header as a packed Rust struct and then borrow
unaligned fields from it. A safe and portable dumper should parse from raw byte
slices.

Recommended constants:

```rust
const RKNS_MAGIC: [u8; 4] = *b"RKNS";
const RKNS_HEADER_SIZE: usize = 0x800;
const RKNS_IMAGE_ENTRY_OFFSET: usize = 0x78;
const RKNS_IMAGE_ENTRY_SIZE: usize = 88;
const RKNS_MAX_IMAGES: usize = 4;
const RK_SECTOR_SIZE: usize = 512;
```

Suggested logical model:

```rust
struct RknsHeader {
    magic: [u8; 4],
    size_and_nimage: u32,
    boot_flag: u32,
    nimage: u16,
    hash_word_off: u16,
    hash_byte_off: usize,
    hash_type: HashType,
    images: Vec<RknsImage>,
    header_hash_ok: Option<bool>,
}

struct RknsImage {
    index: usize,
    size_and_off: u32,
    sector_count: u16,
    sector_off: u16,
    flash_offset: u64,
    payload_size: u64,
    address: u32,
    flag: u32,
    counter: u32,
    digest_kind: HashType,
    digest_ok: Option<bool>,
    first_word_le: Option<u32>,
    starts_with_boot0_stub: bool,
}
```

Suggested enum:

```rust
enum HashType {
    None,
    Sha256,
    Sha512,
    Unknown(u32),
}
```

## Recommended Parsing Algorithm

1. Accept both a standalone IDBlock file and a full flash image.
2. Either:
   - require the caller to provide `idblock_base`, or
   - search for `RKNS` at expected offsets such as `0x0` and `0x8000`.
3. Read exactly `0x800` bytes from `idblock_base`.
4. Verify magic is `RKNS`.
5. Parse `size_and_nimage`, `boot_flag`, and derive `nimage`, `hash_type`, and
   `hash_byte_off`.
6. For each image slot `0..4`:
   - parse the entry at `0x78 + index * 88`
   - stop at `nimage`, or keep parsing all 4 slots and mark empty ones
7. For each non-empty entry:
   - compute `flash_offset` and `payload_size`
   - read the payload bytes from the original image
   - compute and compare the payload digest when the hash type is known
   - record the first 32-bit word for entry-point heuristics
8. Verify the header hash independently.
9. Print warnings instead of aborting when hashes do not match.

## Recommended Dump Output

At a minimum, the tool should display:

- IDBlock base offset
- magic
- image count
- hash type
- header hash offset and status
- one row per image containing:
  - image index
  - sector offset
  - sector count
  - absolute flash offset
  - payload size
  - address field
  - counter
  - digest status
  - first word
  - boot0-stub heuristic result

Optional but useful output:

- hexdump of the first 32 or 64 bytes of each payload
- label hint such as `init` or `boot`
- probable `SPL` label for image 1 when parsing RK3588-style pack output

## Minimal Structural Sanity Checks

A parser should reject the image as structurally invalid if any of these fail:

- file is too small to contain `idblock_base + 0x800`
- magic is not `RKNS`
- `hash_byte_off` is beyond `0x800`
- any referenced payload range extends beyond the file end

A parser should warn, but not necessarily reject, if any of these fail:

- header hash mismatch
- payload hash mismatch
- image count is larger than 4
- address field is not `0xffffffff`
- unexpected non-zero reserved bytes

## Old vs New Header Selection Rule

If the first 4 bytes at the candidate IDBlock base are:

- `RKNS`, use the rules in this document
- `0x0ff0aa55` as little-endian `u32`, use the older Rockchip header parser

For RK3588 in this workspace, the SoC is configured to use the new IDB header,
so the expected parser is always the RKNS parser.

## Practical Summary

For the user's RK3588 SPI NOR dump, a dumper implementing these rules should be
able to report:

- IDBlock at `0x8000`
- image 0 at `0x8800`, size `0x13000`
- image 1 at `0x1b800`, size `0x3f000`
- image 1 is the likely SPL payload

That is the most important end result needed to locate SPL inside the full SPI
flash image.