# rkusb

A Rust library and command-line toolkit for communicating with Rockchip devices over USB.

## Overview

`rkusb` is a Rust library and CLI tool for working with Rockchip devices over USB.

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

## Installation

Ensure you have [Rust](https://rustup.rs/) installed, then:

```sh
cargo install rkusb
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

> **Important:** Do **not** select the WinUSB or libusbK drivers — neither supports the `claim_interface` operation required by `rktools`.

Installing libusb-win32 will replace Rockchip's official kernel-space driver. If you later need to use Rockchip's official tools, open **Device Manager**, right-click the device, choose **Update driver**, and switch back to the official driver.

## CLI Usage

```
rktools [OPTIONS] <COMMAND>
```

Use `rktools --help` or `rktools <COMMAND> --help` to see all available options.

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
rktools rst --subcode 1  # power off
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
| `dump` | - | Batch-backup partitions per GPT (detects the 32 MiB 0xCC read wall) |
| `nolimit` | `nl` | Remove the rockusb 32 MiB read limit from a flash dump |

### Batch-backup partitions (dump)

```sh
# back up every partition (except userdata/cache/misc/metadata) into ./dump
rktools dump

# only specific partitions
rktools dump --partitions uboot_a,uboot_b,misc

# resume an interrupted backup; stop at the 32 MiB 0xCC wall (default)
rktools dump --resume
```

A device whose U-Boot still enforces the rockusb read limit fills every
sector past 32 MiB with `0xCC` and reports the read as successful. `dump`
detects that fill, truncates the affected partition file, and reports the
wall instead of silently producing garbage. Patch the U-Boot in a dump with
`rktools nl fix` (below), flash it back, and re-run the backup.

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
rktools nl limit uboot-payload.bin --base 0x00200000
```

Exit code is 0 only if every input was patched AND verified. The verified
image can be flashed back to `uboot_a`/`uboot_b` (see `storage partition`)
to lift the read limit on the device itself.

### List connected devices

```sh
rktools ls
```

This prints each matching USB device with its bus, address, VID:PID, and detected mode.

### Wait for a device

```sh
# wait forever until a supported device appears
rktools wait

# fail if nothing appears within 20 seconds
rktools wait 20s
```

This is useful in flashing scripts where the device may re-enumerate between Maskrom and Loader mode.

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
rktools storage partition table -o custom_table.txt
```

The command opens the currently selected storage as a 512-byte logical block device, reads the GPT, and prints the discovered partitions. It also writes a partition table file named from the current storage type and size reported by `st i`, for example `eMMC_29.12GiB_partition_table.txt`.

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
- The final partial sector is zero-padded automatically when needed.
- Read and write show a live progress bar (percentage, size, USB speed). `--timeout` is per USB chunk.

#### Dump all GPT partitions

```sh
# dump every GPT partition into eMMC_29.12GiB/ (name comes from st i)
rktools storage partition dump

# dump into a custom directory and skip huge/volatile partitions
rktools storage partition dump ./backup --exclude userdata,cache
```

This is the batch-read flow similar to spreadtrum_flash `r all` / `read_parts`:

- Creates an output directory named `<storage>_<size>` unless you pass one
- Writes `<storage>_<size>_partition_table.txt` first
- Reads each GPT partition to `<name>.img` with a per-partition progress bar

### Serial number and vendor storage (RKDevInfoWriteTool)

In loader mode RKDevInfoWriteTool writes SN / WiFi MAC / LAN MAC / BT MAC into vendor storage. The loader exposes that through:

1. Special LBA `0xFFF00001` for SN (`valid:u32le`, `len:u32le`, payload) — same as xrock `sn`
2. Opcodes `0x26` (WRITE) / `0x27` (READ) with item id + backend type — same as xrock `vs`

Item IDs match the kernel (`rk_vendor_storage.h`): `1=SN`, `2=WIFI MAC`, `3=LAN MAC`, `4=BT MAC`, `15=IMEI`. `--backend rpmb` selects eMMC RPMB (RKDevInfoWriteTool default on RK3588); `--backend vendor` uses the `vnvm` partition.

```sh
# SN (tries LBA 0xFFF00001, then vendor item 1)
rktools sn
rktools sn write RK3588-TEST-001
rktools sn --backend rpmb write RK3588-TEST-001

# dump SN + MACs
rktools vendor dump
rktools vendor dump --backend rpmb

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

### Upgrade loader by writing an IDBlock

```sh
# write the generated IDBlock to the default LBA 64
rktools upgrade-loader rk3588_spl_loader_v1.19.113.bin

# wait for the device and override the destination LBA
rktools upgrade-loader --wait 30s --lba 64 rk3588_spl_loader_v1.19.113.bin
```

`upgrade-loader` parses the Rockchip loader image, extracts `FlashBoot` and `FlashData`, and writes a generated IDBlock to storage. If the image also contains `FlashHead`, the tool will build the newer IDBlock format when the target device reports support for it.
