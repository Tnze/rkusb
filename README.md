# rkusb

English | [简体中文](README.zh-CN.md)

A Rust library and command-line toolkit for communicating with Rockchip devices over USB.

## Overview

This workspace contains `rkusb` (the Rust USB protocol/image library) and `rktools` (the CLI). This README describes the current checkout; published crates may have a different command set.

It supports common Maskrom and Loader workflows such as downloading a loader, reading and writing storage, switching storage media, parsing Rockchip images, and writing IDBlock data.

## Features

- List devices
- Download a bootloader in Maskrom mode
- Reset devices
- Read, write, and erase raw LBAs with live USB speed and a progress bar
- LBA range checks against the current flash size (no more whole-command timeout)
- Wait for device enumeration
- Query and switch storage media
- Read GPT partitions, dump all partitions, and auto-save a partition table file
- Read and write the device serial number (xrock `sn`)
- Dump chip OTP / eFuse (xrock `otp`)
- Parse Rockchip image files
- Generate and write IDBlock data from a loader image
- Read/write vendor items (SN, MAC addresses, IMEI, or numeric IDs)
- Export editable timestamped GPT JSON and apply changes with validation, backup and primary/backup verification
- Flash update.img entries to existing GPT partitions (experimental)
- Experimental 32 MiB read-wall detection in top-level dump (currently blocked by a GPT parsing issue; see below)
- Classify, trim, patch and verify dumps with nolimit
- Emulate supported SPL-to-U-Boot hand-offs offline with splsim

## Installation

Install a current stable [Rust](https://rustup.rs/) toolchain supporting edition 2024. From the repository root:

```sh
cargo +stable build --release -p rktools
cargo +stable install --path rktools
```

The build produces `target/release/rktools` (`rktools.exe` on Windows); installation places it in Cargo’s bin directory. Installing the published `rkusb` crate does not install this checkout’s CLI. To check the workspace or generate library API documentation:

```sh
cargo +stable test --workspace
cargo +stable doc -p rkusb --no-deps
```

On Linux you may also need `libusb` development headers:

```sh
# Debian / Ubuntu
sudo apt install libusb-1.0-0-dev

# Fedora / RHEL
sudo dnf install libusb1-devel
```

On Linux you need either `udev` rules granting access to Rockchip USB devices, or run `rktools` as root.

### Windows

On Windows, you must install the **libusb-win32** driver for the Rockchip USB gadget device. The recommended way is to use [Zadig](https://zadig.akeo.ie/): select the Rockchip device from the list and choose the **libusb-win32** driver.

The libusb-win32 setup is the working configuration used for this fork. Other driver/backend combinations have not been validated here; this is not a general claim that WinUSB or libusbK cannot claim USB interfaces.

Installing libusb-win32 will replace Rockchip's official kernel-space driver. If you later need to use Rockchip's official tools, open **Device Manager**, right-click the device, choose **Update driver**, and switch back to the official driver.

## CLI Usage

```
rktools [OPTIONS] <COMMAND>
```

Use `rktools --help` or `rktools <COMMAND> --help` to see all available options.

Examples use illustrative input filenames, partition names, GUIDs and bus/address values; replace them with existing files and values from your own device. Firmware inputs are not bundled with this repository. USB commands require a compatible connected device, the appropriate Maskrom/Loader mode and working USB access. Offline `nolimit`/`splsim` examples require a supported firmware build, not an arbitrary `.bin` file. Argument parsing is regression-tested against both READMEs, but this is not an end-to-end hardware compatibility guarantee.

### Device selection, logging and timeouts

Use `rktools -v` for info logs, `-vv` for debug and `-vvv` for trace. These logging flags are global. Device commands generally accept `--bus`, `--addr` and `--wait`; consult the specific command help. Without selectors, multiple matching devices produce an error. Bus/address values can change when a device re-enumerates.

Place a command family's options before its nested subcommand; they are not global:

```sh
rktools storage --bus 1 --addr 2 --timeout 10s partition read --name boot boot.img
rktools vendor --backend rpmb read sn
rktools lba --wait 30s read 0 128 header.bin
```

`--wait` bounds waiting for enumeration; it does not bound a complete read or write. Transfer timeouts apply to individual USB phases/chunks, with bounded retries for selected transient LBA transfer failures. A sufficiently long individual stall can still fail. Raw LBA and partition transfers show percentage, transferred bytes and a recent USB transfer rate; the rate is host-observed throughput, not a bus analyzer measurement.

Errors returned by commands exit with status 1; Clap usage errors normally exit with status 2. Some experimental code paths have incomplete failure reporting, so also inspect their output. Successful exit alone does not certify flashed firmware or filesystem integrity.

### Download a bootloader image (Maskrom mode)

```sh
# basic usage
rktools db rk3588_spl_loader_v1.19.113.bin

# wait up to 30 s for the device to appear first
rktools db --wait 30s rk3588_spl_loader_v1.19.113.bin
```

### Reset a device

```sh
rktools rst              # normal reboot
rktools rst 1            # pass reset subcode 1 (meaning depends on firmware)
```

### Command summary

| Command | Aliases | Description |
|---|---|---|
| `list` | `ls` | Enumerate connected Rockchip USB devices |
| `download-boot` | `db` | Download a loader image to a device in Maskrom mode |
| `info` | - | Detect and inspect Rockchip image files |
| `reset` | `rst` | Reset or power off a connected device |
| `lba` | - | Read, write, or erase raw sectors by LBA (progress + range check) |
| `wait` | - | Wait for a Rockchip device to appear |
| `storage` | `st` | Query storage, switch media, print flash info, and access GPT partitions |
| `upgrade-loader` | `ul` | Generate and write an IDBlock from a Rockchip loader image |
| `sn` | `serial` | Read or write the device serial number |
| `otp` | - | Dump chip OTP / eFuse |
| `vendor` | `vs` | Read/write vendor storage (SN, MAC, IMEI) like RKDevInfoWriteTool |
| `upgrade` | - | Upgrade a device by flashing an `update.img` |
| `dump` | - | Experimental batch backup / 0xCC read-wall detection; known GPT parsing issue below |
| `nolimit` | `nl` | Remove the rockusb 32 MiB read limit from a flash dump |
| `splsim` | `sim` | Emulate a supported SPL loading U-Boot FIT images from a dump |

### Edit and apply a GPT partition table

```sh
# Export: partition_table_<current UTC timestamp>.json
rktools storage partition table
# Optional explicit path or legacy display format
rktools storage partition table --output layout.json
rktools storage partition table --format txt
# Edit first_lba/last_lba/name/etc. in the exported JSON, then preview
rktools storage partition apply layout.json
# Validate, save a mandatory timestamped backup, write and verify both GPT copies
rktools storage partition apply layout.json --yes --backup-dir backups
```

`apply` defaults to dry-run and prints added, deleted and changed entries. It checks
geometry, storage identity, GUIDs, partition ranges, overlap, entry indexes and UTF-16
name lengths before any write. The exported disk geometry is a guard against
applying a table to the wrong device; do not change it to bypass validation.
Existing table export and metadata backup files are never overwritten. `storage partition dump` also saves the JSON; image reads/dumps can truncate existing image files, so use a fresh destination directory.

**Changing a partition range does not move data, resize a filesystem or format a
partition.** Back up partition contents before changing layout, restore/recreate
filesystems as needed, and reboot after applying the table. The JSON backup can be
applied with the same command to restore the previous table. GPT writes cannot be
atomic over USB; keep the backup if a device disconnects during the write.

See [the JSON format reference](docs/partition-table-json.md) for editable fields
and exact validation rules.

### Batch-backup partitions (dump, experimental)

**Known issue in this checkout:** the top-level `dump` command passes a leading zero sector to its GPT parser and fails with `GPT header not found at LBA 1` after reading a normal GPT. Use `storage partition dump` for the currently usable batch-read path; that path does not implement resume or 0xCC-wall detection.

Until this parsing defect is fixed, do not use the top-level `dump` as a backup path. Its intended options include `--partitions`, `--resume`, `--lite` and `--all`; use the `storage partition dump` examples below for batch reads.

Some limited U-Boot builds fill sectors past 32 MiB with `0xCC` while reporting the read as successful. The top-level `dump` implementation is intended to detect this pattern; it is not a universal test of all firmware read restrictions. `dump`
checks for that fill and reports the wall. It does not truncate an existing file’s old tail at the wall; use fresh output files. Resume trusts file length without verifying device identity or existing contents, so only resume files from the same unchanged source. Patch the U-Boot in a dump with
`rktools nl fix` (below), flash it back, and re-run the backup.

The top-level `dump` parser assumes 512-byte GPT blocks, does not validate GPT CRCs, and does not recover from the backup GPT. `--lite` skips names ending in `_b`; it does not detect the currently active slot. `--all` includes the usual skip list; `--allow-cc-wall` disables wall stopping and can save filler bytes as data.

### Remove the 32 MiB rockusb read limit (nolimit)

```sh
# identify a dump, change nothing
rktools nl classify dump.bin

# patch every U-Boot FIT copy: guard -> b blk_dread, hash+digest refreshed
# (rktools nl fix = classify + trim + patch + verify)
rktools nl fix dump.bin --outdir out

# re-check a patched dump
rktools nl verify out/dump_nolimit.bin

# just locate the guard in a U-Boot payload
rktools nl limit uboot-payload.bin --base 2097152
```

The default `fix` pipeline patches and verifies each input; `--no-verify` skips verification and `--dry-run` previews changes. Successful command completion returns 0; do not treat a dry-run or skipped verification as proof of a verified image. The verified
U-Boot partition image can be flashed back to its matching `uboot_a`/`uboot_b` partition (see `storage partition`). A patched whole-device dump remains a whole-device layout: extract the exact partition range first; never write the entire dump into a U-Boot partition.

Additional nolimit operations:

```sh
# scan .bin/.img inputs from a directory
rktools nl fix --indir in --outdir out
# preview, or restrict patching to the primary FIT
rktools nl fix dump.bin --dry-run
rktools nl fix dump.bin --only-primary --outdir out
# standalone tail trimming and patching (patch does not verify)
rktools nl trim dump.bin --output trimmed.bin
rktools nl patch trimmed.bin --output patched.bin
rktools nl verify patched.bin dump.bin
# inspect a FIT embedded at a byte offset
rktools nl limit dump.bin --fit-off 8388608
```

`nolimit` operates on local files; it does not flash the device automatically. Output paths can overwrite existing files. `fix` verification checks patched structures/hashes/guard changes; standalone `verify` also attempts SPL simulation, but unsupported anchors can be skipped. Check that FIT candidates and actual checks are present: success on an unsupported or empty candidate set is not boot validation. Neither path certifies a vendor cryptographic signature or hardware bootability. Its patterns support particular firmware builds, not every Rockchip loader. Preserve the original dump and inspect verification output before flashing.

### Offline SPL simulation (splsim)

```sh
rktools splsim dump.bin
rktools sim dump.bin --text-base 0x40200000 --max-steps 400000000
rktools sim dump.bin --fit 0x800000 --vb-flag 0xff
```

The input must be a dump containing a supported RKNS IDBlock/SPL and U-Boot FIT images. The CLI extracts SPL, locates loader functions through anchors and emulates each selected FIT candidate. Output distinguishes an observed U-Boot hand-off, a loader return without a jump, a failure and an unsupported build. At least one successful hand-off is required for command success; it does not mean every candidate succeeded.

`--text-base` defaults to `0x200000`; RK3576 commonly needs `0x40200000`. `--fit` selects a byte offset. Text-base, FIT offset and fuse-byte options parse hexadecimal values, even without a prefix; `--max-steps` is a decimal instruction budget. The fuse byte and helper hooks are simulation models, not measurements of hardware secure-boot state. This is not a full SoC emulator or proof of boot compatibility. The CLI accepts dumps containing RKNS IDBlock data; it does not provide a bare-SPL `--spl` option.

### List connected devices

```sh
rktools ls
```

This prints each matching USB device with its bus, address and detected mode. Use `--maskrom`, `--loader` or `--msc` to filter modes; `-l` means Loader filtering, not a long listing. VID:PID is not printed.

### Wait for a device

```sh
# wait forever until a supported device appears
rktools wait

# fail if nothing appears within 20 seconds
rktools wait --timeout 20s
```

This is intended for flashing scripts where the device re-enumerates between Maskrom and Loader mode. The current hotplug wait implementation has incomplete filtering: unrelated USB arrival events may be selected, and mismatched bus/address events can trigger an assertion. Do not rely on it as a robust unattended device-identity check.

### Inspect Rockchip image files

```sh
# inspect a Rockchip loader image
rktools info rk3588_spl_loader_v1.19.113.bin

# inspect a Rockchip firmware bundle
rktools info update.img
```

The command prints the file size and a parsed debug view of the detected Rockchip image header.

### Raw LBA operations

```sh
# read 0x400 sectors starting at sector 0x2000
rktools lba read 0x2000 0x400 boot.img

# write a file to storage starting at sector 0x4000
rktools lba write 0x4000 rootfs.img

# erase 256 sectors starting at sector 0x8000
rktools lba erase 0x8000 256
```

Notes:

- `begin_sector` and `sector_count` accept decimal and `0x`-prefixed hexadecimal values.
- `lba write` automatically pads the last partial sector with zeros.
- Before each read/write/erase the tool checks the requested LBA range against the flash size from `storage info`. Transfers that would run past the end of the medium are rejected.
- `--timeout` is the USB timeout **per transfer chunk**. The whole operation is bounded by the requested range size, not a fixed wall-clock budget. A live progress bar shows percentage, transferred size, and USB speed.

### Storage operations

Aliases are `st sl` for `storage select`, `st i` for `storage info`, and `table/ls`, `read/r`, `write/w`, `dump/rall` below `storage partition`.

The `storage` command family works on the currently selected storage medium and supports device selection with `--bus`, `--addr`, and `--wait` just like `lba` and `upgrade-loader`.

#### Query or switch current storage

```sh
# query current storage selection
rktools storage select

# switch to eMMC
rktools storage select 1

# switch to SPI NOR
rktools storage select 9

# switch to NVMe
rktools storage select 11
```

Known storage codes:

- `1` = eMMC
- `2` = SD
- `9` = SPI NOR
- `11` = NVMe

#### Read flash information

```sh
rktools storage info
```

This prints the parsed flash/storage information structure returned by the device.

#### Print GPT partition table

```sh
rktools storage partition table
rktools storage partition table --no-save
rktools storage partition table --format txt -o custom_table.txt
```

GPT operations require an existing readable primary GPT with 128-byte entries. Apply additionally requires matching healthy primary and backup copies, standard GPT 1.0 headers, and unchanged device geometry; it is not a damaged-GPT repair tool.

The command uses the logical block size reported by the current storage (512 or 4096 bytes), reads the GPT, and prints editable JSON by default. It also saves `partition_table_<Unix seconds>_<nanoseconds>.json`. Use `--format txt` for the display-only text report. Outputs use exclusive creation and refuse to overwrite an existing file.

#### Read a GPT partition to a file

```sh
# select by GPT partition name
rktools storage partition read --name boot boot.img

# select by partition index
rktools storage partition read --index 4 rootfs.img

# select by partition GUID
rktools storage partition read --guid 01234567-89ab-cdef-0123-456789abcdef misc.img
```

#### Write a file to a GPT partition

```sh
# write by name
rktools storage partition write --name boot boot.img

# write by index
rktools storage partition write --index 4 rootfs.img
```

Partition write behavior:

- The target partition can be selected by exactly one of `--name`, `--guid`, or `--index`.
- The input file must fit inside the selected partition.
- The final partial 512-byte transport sector is zero-padded automatically when needed. A smaller image leaves the rest of the partition unchanged.
- Writes treat input as raw bytes; they do not automatically expand Android sparse images or format a filesystem. There is no automatic payload readback verification.
- Read and write show a live progress bar (percentage, size, USB speed). `--timeout` is per USB chunk.

#### Dump all GPT partitions

```sh
# dump every GPT partition into eMMC_29.12GiB/ (name comes from st i)
rktools storage partition dump

# dump into a custom directory and skip huge/volatile partitions
rktools storage partition dump ./backup --exclude userdata,cache
```

This is the batch-read flow similar to spreadtrum_flash `r all` / `read_parts`. Unlike the top-level experimental `dump`, it saves `.img` files and JSON, has no resume or read-wall detection, and reads all partitions unless excluded:

- Creates an output directory named `<storage>_<size>` unless you pass one
- Writes `partition_table_<Unix seconds>_<nanoseconds>.json` first
- Reads each GPT partition to `<name>.img` with a per-partition progress bar
- Stops on the first error; a failed read may leave a pre-sized, incomplete image. File size alone does not prove a complete backup.

### Serial number and vendor storage (RKDevInfoWriteTool)

In loader mode RKDevInfoWriteTool writes SN / WiFi MAC / LAN MAC / BT MAC into vendor storage. The loader exposes that through:

1. Special LBA `0xFFF00001` for SN (`valid:u32le`, `len:u32le`, payload) — same as xrock `sn`
2. Opcodes `0x26` (WRITE) / `0x27` (READ) with item id + backend type — same as xrock `vs`

Item IDs follow the Rockchip kernel vendor-storage interface (`rk_vendor_storage.h` in the device kernel source, not a file bundled here): `1=SN`, `2=WIFI MAC`, `3=LAN MAC`, `4=BT MAC`, `15=IMEI`. `--backend rpmb` selects eMMC RPMB (RKDevInfoWriteTool default on RK3588); `--backend vendor` uses the `vnvm` partition. `rktools vendor` and `rktools sn` default to the `vendor` backend, not RPMB.

```sh
# SN (tries LBA 0xFFF00001, then vendor item 1)
rktools sn
rktools sn write RK3588-TEST-001
rktools sn --backend rpmb write RK3588-TEST-001

# dump SN + MACs
rktools vendor dump
rktools vendor --backend rpmb dump

# individual items
rktools vendor read sn
rktools vendor write wifi-mac 88:A9:A7:00:BC:64
rktools vendor write lan-mac 88A9A700BC95
rktools vendor write bt-mac 88:A9:A7:00:BC:66
rktools vendor read imei
```

OTP dump uses opcode `0x2C` and is decoded against the Linux/U-Boot NS OTP map for the SoC (CPUID, cpu-code, leakage bins). Secure Boot key hash lives in *secure* OTP and is usually **not** in this dump.

```sh
rktools otp                  # 128-byte dump + named fields (auto map from chip info)
rktools otp 256
rktools otp --map rk3588
rktools otp --raw            # hex only
```

### Flash an update.img (experimental upgrade)

```sh
rktools upgrade --wait 30s update.img
```

This command parses RKFW/RKAF firmware, requires a `parameter` entry and matches firmware entries to the device's existing GPT partition names. It skips `package-file` and `parameter`, handles `super` as a sparse image, and copies other entries as regular images. It does not create or reconcile the GPT from the parameter file; use the explicit JSON workflow for metadata changes.

This upstream feature remains partial: parameter CRC checking and partition-layout validation are TODOs; Maskrom loader download reopens the old bus/address rather than reliably tracking re-enumeration. It lacks partition-operation progress reporting and does not read back flashed payloads for verification. Partition writes also do not automatically verify payloads; GPT apply verifies only GPT metadata. The current regular-image path caps copying at the partition length instead of rejecting oversized images, so check sizes beforehand. Its `--timeout` defaults to 300s but is not propagated consistently through all I/O paths. Prefer explicit partition writes for controlled workflows and do not assume this is a complete vendor-tool replacement.

### Upgrade loader by writing an IDBlock

```sh
# write the generated IDBlock to the default LBA 64
rktools upgrade-loader rk3588_spl_loader_v1.19.113.bin

# wait for the device and override the destination LBA
rktools upgrade-loader --wait 30s --lba 64 rk3588_spl_loader_v1.19.113.bin
```

`upgrade-loader` parses the Rockchip loader image, extracts `FlashBoot` and `FlashData`, and writes a generated IDBlock to storage. If the image also contains `FlashHead`, the tool will build the newer IDBlock format when the target device reports support for it.
