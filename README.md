# rkusb

A Rust library and command-line toolkit for communicating with Rockchip devices over USB.

## Overview

`rkusb` provides both a reusable Rust library crate and a ready-to-use CLI binary (`rktools`) for interacting with Rockchip SoC-based devices in Maskrom and Loader USB modes. It implements the Rockchip USB protocol used to download boot images, perform raw LBA storage operations, upgrade loaders, and more — all from a host PC over a standard USB cable.

The project aims to be a clean, memory-safe, cross-platform replacement for the official C++ tool [rkdeveloptool](https://github.com/rockchip-linux/rkdeveloptool), while also exposing its functionality as a first-class Rust library for integration into other tools and workflows.

## Features

- **List devices** — enumerate all connected Rockchip USB devices and display their bus/address and mode (Maskrom / Loader / MSC).
- **Download boot** — send a Rockchip `.bin` loader image to a device in Maskrom mode via the vendor USB control protocol (requests `0x0471` / `0x0472`).
- **Reset device** — issue a device reset with a configurable subcode (normal reboot or power-off).
- **LBA operations** — read, write, and erase raw storage sectors by logical block address, with chunked transfer and configurable timeouts.
- **Wait for device** — block until a Rockchip device appears on the bus, with an optional timeout — useful in scripted flashing pipelines.
- **Storage selection** — query and switch the active storage medium (eMMC, SD card, SPI-NOR flash).
- **Upgrade loader** — construct and write an IDBlock from a standard Rockchip loader image directly to storage, supporting both legacy and new (FlashHead) IDBlock formats with optional RC4 encryption.
- **File info** — inspect Rockchip image files (`.bin`, firmware) and print header metadata.

## Supported Devices

The following Rockchip SoC families are recognised automatically by USB VID/PID.
Most of these chips have **not been tested** — the primary development and test platform is **RK3588S2**.

| Family | Modes |
|--------|-------|
| RK27xx | Maskrom, Loader |
| RK28xx / RK281x | Maskrom, Loader |
| RK NANO / SMART / PANDA / CAYMAN / CROWN | Maskrom, Loader |
| RK29xx / RK292x | Maskrom, Loader |
| RK30xx / RK30B | Maskrom, Loader |
| RK31xx | Maskrom, Loader |
| RK32xx | Maskrom, Loader |
| Generic `0x2207` vendor devices | Maskrom, Loader |
| MSC devices | MSC |

## Advantages over rkdeveloptool (C++ original)

| | **rkusb** (this project) | **rkdeveloptool** (C++ original) |
|---|---|---|
| **Language** | Rust | C++ |
| **Memory safety** | Guaranteed by the Rust compiler — no buffer overflows, use-after-free, or data races | Manual memory management; historically had memory-safety issues |
| **Error handling** | Typed errors with `thiserror`; every failure path is explicit | Mix of return codes, exceptions, and silent failures |
| **Cross-platform** | Works on Linux, macOS, and Windows via the `rusb`/`libusb` abstraction | Primarily Linux; Windows support requires separate tooling |
| **Library crate** | Fully usable as a Rust library — import `rkusb` in your own project | Monolithic application; not designed for library use |
| **CLI ergonomics** | Built with `clap` — rich `--help`, subcommands, aliases, type-safe argument parsing | Ad-hoc argument parsing |
| **Scripting support** | `wait` subcommand and per-operation timeouts make it suitable for automated flashing scripts | Limited scripting support |
| **Installation** | `cargo install rkusb` — single static binary, no runtime dependencies beyond `libusb` | Must build from source; depends on system libraries |

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
