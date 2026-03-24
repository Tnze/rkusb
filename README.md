# rkusb

A Rust library and command-line toolkit for communicating with Rockchip devices over USB.

## Overview

`rkusb` is a Rust library and CLI tool for working with Rockchip devices over USB.

It supports common Maskrom and Loader workflows such as downloading a loader, reading and writing storage, switching storage media, parsing Rockchip images, and writing IDBlock data.

## Features

- List devices
- Download a bootloader in Maskrom mode
- Reset devices
- Read, write, and erase raw LBAs
- Wait for device enumeration
- Query and switch storage media
- Read GPT partitions and transfer partition contents
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
| `lba` | - | Read, write, or erase raw sectors by LBA |
| `wait` | - | Wait for a Rockchip device to appear |
| `storage` | `st` | Query storage, switch media, print flash info, and access GPT partitions |
| `upgrade-loader` | `ul` | Generate and write an IDBlock from a Rockchip loader image |

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
- `--timeout` applies to the full command, not each individual USB transfer.

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
```

The command opens the currently selected storage as a 512-byte logical block device, reads the GPT, and prints the discovered partitions.

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

### Upgrade loader by writing an IDBlock

```sh
# write the generated IDBlock to the default LBA 64
rktools upgrade-loader rk3588_spl_loader_v1.19.113.bin

# wait for the device and override the destination LBA
rktools upgrade-loader --wait 30s --lba 64 rk3588_spl_loader_v1.19.113.bin
```

`upgrade-loader` parses the Rockchip loader image, extracts `FlashBoot` and `FlashData`, and writes a generated IDBlock to storage. If the image also contains `FlashHead`, the tool will build the newer IDBlock format when the target device reports support for it.
