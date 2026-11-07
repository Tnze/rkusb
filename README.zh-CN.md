# rkusb

[English](README.md) | 简体中文

用于通过 USB 与瑞芯微（Rockchip）设备通信的 Rust 库和命令行工具集。

## 概述

本工作区包含 `rkusb`（Rust USB 协议与镜像库）和 `rktools`（命令行工具）。本 README 描述的是当前检出的代码；已发布 crate 的命令集可能有所不同。

支持常见的 Maskrom 和 Loader 工作流程，包括下载 loader、读写存储、切换存储介质、解析 Rockchip 镜像以及写入 IDBlock 数据。

## 功能

- 列出设备
- 在 Maskrom 模式下下载引导加载程序
- 重置设备
- 读取、写入和擦除原始 LBA，并显示实时 USB 速度和进度条
- 根据当前闪存容量检查 LBA 范围（不再使用整条命令的超时限制）
- 等待设备枚举
- 查询和切换存储介质
- 读取 GPT 分区、导出所有分区，并自动保存分区表文件
- 读写设备序列号（xrock `sn`）
- 导出芯片 OTP / eFuse（xrock `otp`）
- 解析 Rockchip 镜像文件
- 根据 loader 镜像生成并写入 IDBlock 数据
- 读写厂商存储项（SN、MAC 地址、IMEI 或数字 ID）
- 导出带时间戳、可编辑的 GPT JSON，并在校验、备份以及主备分区表验证的保护下应用更改
- 将 update.img 中的条目刷写到现有 GPT 分区（实验性功能）
- 在顶层 dump 命令中实验性检测 32 MiB 读取屏障（目前受 GPT 解析问题影响，详见下文）
- 使用 nolimit 对转储文件进行分类、裁剪、修补和验证
- 使用 splsim 离线模拟受支持的 SPL 向 U-Boot 移交控制权的过程

## 安装

安装支持 Rust 2024 edition 的最新稳定版 [Rust](https://rustup.rs/) 工具链。在仓库根目录执行：

```sh
cargo +stable build --release -p rktools
cargo +stable install --path rktools
```

构建生成 `target/release/rktools`（Windows 下为 `rktools.exe`）；安装后，程序位于 Cargo 的 bin 目录。安装已发布的 `rkusb` crate 不会安装当前检出代码中的 CLI。要检查整个工作区或生成库的 API 文档，可执行：

```sh
cargo +stable test --workspace
cargo +stable doc -p rkusb --no-deps
```

在 Linux 上，可能还需要安装 `libusb` 开发头文件：

```sh
# Debian / Ubuntu
sudo apt install libusb-1.0-0-dev

# Fedora / RHEL
sudo dnf install libusb1-devel
```

在 Linux 上，需要配置允许访问 Rockchip USB 设备的 `udev` 规则，或以 root 身份运行 `rktools`。

### Windows

在 Windows 上，必须为 Rockchip USB gadget 设备安装 **libusb-win32** 驱动。推荐使用 [Zadig](https://zadig.akeo.ie/)：在列表中选择 Rockchip 设备，然后选择 **libusb-win32** 驱动。

libusb-win32 是本分支实际使用并确认可用的配置。这里尚未验证其他驱动与后端组合；这并不意味着 WinUSB 或 libusbK 普遍无法取得 USB 接口的控制权。

安装 libusb-win32 会替换 Rockchip 官方的内核态驱动。若之后需要使用 Rockchip 官方工具，请打开**设备管理器**，右键单击设备，选择**更新驱动程序**，然后切换回官方驱动。

## CLI 用法

```
rktools [OPTIONS] <COMMAND>
```

使用 `rktools --help` 或 `rktools <COMMAND> --help` 查看所有可用选项。

示例中的输入文件名、分区名、GUID 和总线号/地址均为示意，请替换为实际存在的文件及自己设备上的值。仓库不附带示例固件。USB 命令要求连接兼容设备、处于相应的 Maskrom/Loader 模式，并已配置 USB 访问权限。离线 `nolimit`/`splsim` 示例要求受支持的固件构建，不能用于任意 `.bin` 文件。两版 README 的参数解析均纳入回归测试，但这不等于完成了端到端真机兼容性验证。

### 设备选择、日志与超时

使用 `rktools -v` 输出 info 日志，`-vv` 输出 debug 日志，`-vvv` 输出 trace 日志。这些日志选项是全局选项。设备操作命令通常接受 `--bus`、`--addr` 和 `--wait`；具体请查阅各命令的帮助。不指定设备选择条件时，如果有多个匹配设备，命令会报错。设备重新枚举后，总线号和地址可能发生变化。

命令组的选项应放在其嵌套子命令之前；这些选项并非全局选项：

```sh
rktools storage --bus 1 --addr 2 --timeout 10s partition read --name boot boot.img
rktools vendor --backend rpmb read sn
rktools lba --wait 30s read 0 128 header.bin
```

`--wait` 限制的是等待设备枚举的时间，并不限制整个读写操作的耗时。传输超时针对单个 USB 阶段或数据块；对于部分暂时性的 LBA 传输故障，工具会进行次数有限的重试。单次停滞时间足够长时，操作仍可能失败。原始 LBA 和分区传输会显示百分比、已传输字节数以及近期 USB 传输速率；该速率是主机观测到的吞吐量，并非总线分析仪的测量结果。

命令返回错误时退出状态为 1；Clap 用法错误通常以状态 2 退出。部分实验性代码路径的失败报告尚不完整，因此还应检查输出内容。仅凭成功退出，不能确认已刷写固件或文件系统的完整性。

### 下载引导加载程序镜像（Maskrom 模式）

```sh
# 基本用法
rktools db rk3588_spl_loader_v1.19.113.bin

# 先等待设备出现，最多等待 30 秒
rktools db --wait 30s rk3588_spl_loader_v1.19.113.bin
```

### 重置设备

```sh
rktools rst              # 正常重启
rktools rst 1            # 传入重置子码 1（含义取决于固件）
```

### 命令一览

| 命令 | 别名 | 说明 |
|---|---|---|
| `list` | `ls` | 枚举已连接的 Rockchip USB 设备 |
| `download-boot` | `db` | 向处于 Maskrom 模式的设备下载 loader 镜像 |
| `info` | - | 检测并查看 Rockchip 镜像文件 |
| `reset` | `rst` | 重置已连接的设备或将其关机 |
| `lba` | - | 按 LBA 读取、写入或擦除原始扇区（进度显示与范围检查） |
| `wait` | - | 等待 Rockchip 设备出现 |
| `storage` | `st` | 查询存储、切换介质、显示闪存信息以及访问 GPT 分区 |
| `upgrade-loader` | `ul` | 根据 Rockchip loader 镜像生成并写入 IDBlock |
| `sn` | `serial` | 读取或写入设备序列号 |
| `otp` | - | 导出芯片 OTP / eFuse |
| `vendor` | `vs` | 类似 RKDevInfoWriteTool，读写厂商存储（SN、MAC、IMEI） |
| `upgrade` | - | 通过刷写 `update.img` 升级设备 |
| `dump` | - | 实验性批量备份 / 0xCC 读取屏障检测；下文列有已知的 GPT 解析问题 |
| `nolimit` | `nl` | 从闪存转储文件中移除 rockusb 的 32 MiB 读取限制 |
| `splsim` | `sim` | 模拟受支持的 SPL 从转储文件加载 U-Boot FIT 镜像 |

### 编辑并应用 GPT 分区表

```sh
# 导出：partition_table_<当前 UTC 时间戳>.json
rktools storage partition table
# 可选：显式指定路径，或使用旧版显示格式
rktools storage partition table --output layout.json
rktools storage partition table --format txt
# 编辑导出的 JSON 中的 first_lba/last_lba/name 等字段，然后预览
rktools storage partition apply layout.json
# 校验、强制保存带时间戳的备份，然后写入并验证两份 GPT
rktools storage partition apply layout.json --yes --backup-dir backups
```

`apply` 默认只做预演（dry-run），并列出新增、删除和更改的条目。在任何写入发生前，会检查存储几何信息、存储标识、GUID、分区范围、重叠、条目索引以及 UTF-16 名称长度。导出的磁盘几何信息用于防止将分区表应用到错误设备；不要修改它来绕过校验。已有的分区表导出文件和元数据备份文件绝不会被覆盖。`storage partition dump` 也会保存 JSON；镜像读取或转储可能会截断已有镜像文件，因此请使用新的目标目录。

**更改分区范围不会移动数据、调整文件系统大小，也不会格式化分区。** 更改布局前请备份分区内容，按需恢复或重新创建文件系统，并在应用分区表后重启。可使用相同命令应用 JSON 备份，以恢复之前的分区表。通过 USB 写入 GPT 无法保证原子性；若设备在写入期间断开连接，请保留备份。

有关可编辑字段和确切的校验规则，请参阅 [JSON 格式参考（英文）](docs/partition-table-json.md)。

### 批量备份分区（dump，实验性功能）

**当前检出代码的已知问题：** 顶层 `dump` 命令向 GPT 解析器传入了一个前置的全零扇区，因此成功读取普通 GPT 后也会报错 `GPT header not found at LBA 1`。当前可用的批量读取路径是 `storage partition dump`；该路径不支持断点续传，也不检测 0xCC 读取屏障。

在此解析缺陷修复前，请勿将顶层 `dump` 用作备份路径。其预期选项包括 `--partitions`、`--resume`、`--lite` 和 `--all`；批量读取请使用下方的 `storage partition dump` 示例。

部分受限的 U-Boot 构建会将 32 MiB 之后的扇区填充为 `0xCC`，同时仍报告读取成功。顶层 `dump` 的实现旨在检测这种模式；它并不是适用于所有固件读取限制的通用检测方法。`dump` 会检查这种填充并报告读取屏障。遇到屏障时，它不会截去已有文件中原先残留的尾部；请使用新的输出文件。断点续传仅依赖文件长度，不会验证设备标识或已有内容，因此只应继续备份来自同一未变更源的文件。使用 `rktools nl fix`（见下文）修补转储文件中的 U-Boot，将其刷回设备后，再重新备份。

顶层 `dump` 解析器假定 GPT 块大小为 512 字节，不校验 GPT CRC，也不会尝试从备份 GPT 恢复。`--lite` 跳过名称以 `_b` 结尾的分区；它不会检测当前活动槽位。`--all` 会包含通常被跳过的分区；`--allow-cc-wall` 禁用遇到读取屏障时停止的行为，因此可能会将填充字节保存为数据。

### 移除 rockusb 的 32 MiB 读取限制（nolimit）

```sh
# 识别转储文件，不做任何修改
rktools nl classify dump.bin

# 修补每份 U-Boot FIT 副本：将保护逻辑改为 b blk_dread，并更新哈希与摘要
# （rktools nl fix = 分类 + 裁剪 + 修补 + 验证）
rktools nl fix dump.bin --outdir out

# 重新检查已修补的转储文件
rktools nl verify out/dump_nolimit.bin

# 仅定位 U-Boot 载荷中的保护逻辑
rktools nl limit uboot-payload.bin --base 2097152
```

默认的 `fix` 流程会修补并验证每个输入；`--no-verify` 跳过验证，`--dry-run` 则预览更改。命令成功完成时返回 0；不要将预演或跳过验证的结果视为镜像已通过验证的证据。通过验证的 U-Boot 分区镜像可刷回与其匹配的 `uboot_a`/`uboot_b` 分区（参见 `storage partition`）。修补后的整机转储仍保留整机布局：应先提取精确的分区范围，绝不要将整个转储文件写入 U-Boot 分区。

其他 nolimit 操作：

```sh
# 扫描目录中的 .bin/.img 输入文件
rktools nl fix --indir in --outdir out
# 预览，或将修补范围限制为主 FIT
rktools nl fix dump.bin --dry-run
rktools nl fix dump.bin --only-primary --outdir out
# 单独裁剪尾部和修补（patch 不执行验证）
rktools nl trim dump.bin --output trimmed.bin
rktools nl patch trimmed.bin --output patched.bin
rktools nl verify patched.bin dump.bin
# 检查嵌入在指定字节偏移处的 FIT
rktools nl limit dump.bin --fit-off 8388608
```

`nolimit` 操作本地文件，不会自动刷写设备。输出路径可能覆盖已有文件。`fix` 的验证会检查修补后的结构、哈希及保护逻辑的变化；独立的 `verify` 还会尝试 SPL 模拟，但可能跳过不受支持的锚点。请确认输出中确实包含 FIT 候选和实际执行的检查：对不受支持或空候选集返回成功，并不代表通过了启动验证。两种路径都不能认证厂商的加密签名，也不能保证硬件能够启动。其模式仅支持特定固件构建，并非适用于所有 Rockchip loader。请保留原始转储，并在刷写前检查验证输出。

### 离线 SPL 模拟（splsim）

```sh
rktools splsim dump.bin
rktools sim dump.bin --text-base 0x40200000 --max-steps 400000000
rktools sim dump.bin --fit 0x800000 --vb-flag 0xff
```

输入必须是包含受支持的 RKNS IDBlock/SPL 和 U-Boot FIT 镜像的转储文件。CLI 会提取 SPL，通过锚点定位 loader 函数，并模拟每个选中的 FIT 候选。输出会区分观测到的 U-Boot 控制权移交、loader 未跳转即返回、失败以及不受支持的构建。至少有一次成功移交控制权，命令才会成功；这并不意味着每个候选都成功。

`--text-base` 默认为 `0x200000`；RK3576 通常需要 `0x40200000`。`--fit` 选择字节偏移。代码基址、FIT 偏移和熔丝字节选项即使不带前缀，也按十六进制解析；`--max-steps` 是十进制的指令执行预算。熔丝字节和辅助函数钩子属于模拟模型，并非对硬件安全启动状态的测量。这不是完整的 SoC 模拟器，也不能证明启动兼容性。CLI 接受包含 RKNS IDBlock 数据的转储文件，不提供读取裸 SPL 的 `--spl` 选项。

### 列出已连接的设备

```sh
rktools ls
```

输出每个匹配的 USB 设备的总线号、地址和检测到的模式。使用 `--maskrom`、`--loader` 或 `--msc` 按模式筛选；`-l` 表示筛选 Loader 模式，并非详细列表。输出不包含 VID:PID。

### 等待设备

```sh
# 一直等待，直到受支持的设备出现
rktools wait

# 如果 20 秒内未出现设备，则失败
rktools wait --timeout 20s
```

此命令用于刷写脚本中设备在 Maskrom 与 Loader 模式切换时重新枚举的场景。当前热插拔等待实现的筛选不完整：可能选中无关 USB 设备的到达事件，总线号/地址不匹配的事件可能触发断言。不要将其作为可靠的无人值守设备身份校验。

### 查看 Rockchip 镜像文件

```sh
# 查看 Rockchip loader 镜像
rktools info rk3588_spl_loader_v1.19.113.bin

# 查看 Rockchip 固件包
rktools info update.img
```

该命令输出文件大小，以及检测到的 Rockchip 镜像头解析后的调试视图。

### 原始 LBA 操作

```sh
# 从扇区 0x2000 开始读取 0x400 个扇区
rktools lba read 0x2000 0x400 boot.img

# 从扇区 0x4000 开始将文件写入存储
rktools lba write 0x4000 rootfs.img

# 从扇区 0x8000 开始擦除 256 个扇区
rktools lba erase 0x8000 256
```

说明：

- `begin_sector` 和 `sector_count` 接受十进制以及带 `0x` 前缀的十六进制值。
- `lba write` 会自动将最后一个不完整扇区补零。
- 每次读取、写入或擦除前，工具都会根据 `storage info` 返回的闪存容量检查请求的 LBA 范围，拒绝超出介质末尾的传输。
- `--timeout` 是**每个传输块**的 USB 超时。整个操作的范围由请求的数据量限定，而非固定的总耗时预算。实时进度条显示百分比、已传输大小和 USB 速度。

### 存储操作

`storage select` 的别名为 `st sl`，`storage info` 的别名为 `st i`；`storage partition` 下支持 `table/ls`、`read/r`、`write/w` 和 `dump/rall`。

`storage` 命令组操作当前选中的存储介质，与 `lba` 和 `upgrade-loader` 一样，可通过 `--bus`、`--addr` 和 `--wait` 选择设备。

#### 查询或切换当前存储

```sh
# 查询当前选中的存储
rktools storage select

# 切换到 eMMC
rktools storage select 1

# 切换到 SPI NOR
rktools storage select 9

# 切换到 NVMe
rktools storage select 11
```

已知存储代码：

- `1` = eMMC
- `2` = SD
- `9` = SPI NOR
- `11` = NVMe

#### 读取闪存信息

```sh
rktools storage info
```

输出设备返回的闪存/存储信息结构的解析结果。

#### 输出 GPT 分区表

```sh
rktools storage partition table
rktools storage partition table --no-save
rktools storage partition table --format txt -o custom_table.txt
```

GPT 操作要求设备已有可读取的主 GPT，且分区条目大小为 128 字节。应用分区表还要求主备副本均健康且一致、使用标准 GPT 1.0 头，并且设备几何信息未发生变化；此功能不是损坏 GPT 的修复工具。

该命令使用当前存储报告的逻辑块大小（512 或 4096 字节）读取 GPT，默认输出可编辑的 JSON，同时保存 `partition_table_<Unix seconds>_<nanoseconds>.json`。使用 `--format txt` 可输出仅供查看的文本报告。输出文件以独占创建方式写入，若文件已存在则拒绝覆盖。

#### 将 GPT 分区读取到文件

```sh
# 按 GPT 分区名称选择
rktools storage partition read --name boot boot.img

# 按分区索引选择
rktools storage partition read --index 4 rootfs.img

# 按分区 GUID 选择
rktools storage partition read --guid 01234567-89ab-cdef-0123-456789abcdef misc.img
```

#### 将文件写入 GPT 分区

```sh
# 按名称写入
rktools storage partition write --name boot boot.img

# 按索引写入
rktools storage partition write --index 4 rootfs.img
```

分区写入行为：

- 必须且只能使用 `--name`、`--guid` 或 `--index` 中的一项选择目标分区。
- 输入文件大小不得超过所选分区容量。
- 必要时，会自动将最后一个不完整的 512 字节传输扇区补零。镜像小于分区时，分区剩余部分保持不变。
- 写入时将输入视为原始字节；不会自动展开 Android sparse 镜像，也不会格式化文件系统。不会自动读回验证载荷。
- 读写都会显示实时进度条（百分比、大小、USB 速度）。`--timeout` 针对每个 USB 数据块。

#### 导出所有 GPT 分区

```sh
# 将每个 GPT 分区导出到 eMMC_29.12GiB/（名称来自 st i）
rktools storage partition dump

# 导出到自定义目录，并跳过体积较大或内容易变的分区
rktools storage partition dump ./backup --exclude userdata,cache
```

此批量读取流程类似于 spreadtrum_flash 的 `r all` / `read_parts`。与顶层实验性 `dump` 不同，它保存 `.img` 文件和 JSON，不支持断点续传或读取屏障检测，且默认读取所有分区，除非显式排除：

- 未指定输出目录时，创建名为 `<storage>_<size>` 的目录
- 首先写入 `partition_table_<Unix seconds>_<nanoseconds>.json`
- 将每个 GPT 分区读取到 `<name>.img`，并逐分区显示进度条
- 遇到第一个错误即停止；读取失败可能留下已预分配大小但内容不完整的镜像。仅凭文件大小无法证明备份完整。

### 序列号与厂商存储（RKDevInfoWriteTool）

在 Loader 模式下，RKDevInfoWriteTool 会将 SN / WiFi MAC / LAN MAC / BT MAC 写入厂商存储。loader 通过以下接口提供访问：

1. 用于 SN 的特殊 LBA `0xFFF00001`（`valid:u32le`、`len:u32le`、载荷）——与 xrock `sn` 相同
2. 操作码 `0x26`（WRITE）/ `0x27`（READ），配合存储项 ID 和后端类型——与 xrock `vs` 相同

存储项 ID 遵循 Rockchip 内核的厂商存储接口（`rk_vendor_storage.h` 位于设备内核源码中，并非本仓库附带文件）：`1=SN`、`2=WIFI MAC`、`3=LAN MAC`、`4=BT MAC`、`15=IMEI`。`--backend rpmb` 选择 eMMC RPMB（RKDevInfoWriteTool 在 RK3588 上的默认后端）；`--backend vendor` 使用 `vnvm` 分区。`rktools vendor` 和 `rktools sn` 默认使用 `vendor` 后端，而非 RPMB。

```sh
# SN（先尝试 LBA 0xFFF00001，再尝试厂商存储项 1）
rktools sn
rktools sn write RK3588-TEST-001
rktools sn --backend rpmb write RK3588-TEST-001

# 导出 SN 和各 MAC 地址
rktools vendor dump
rktools vendor --backend rpmb dump

# 单独访问存储项
rktools vendor read sn
rktools vendor write wifi-mac 88:A9:A7:00:BC:64
rktools vendor write lan-mac 88A9A700BC95
rktools vendor write bt-mac 88:A9:A7:00:BC:66
rktools vendor read imei
```

OTP 导出使用操作码 `0x2C`，并根据该 SoC 的 Linux/U-Boot NS OTP 映射进行解码（CPUID、cpu-code、漏电分档）。安全启动密钥哈希位于*安全* OTP 中，通常**不包含**在此导出结果中。

```sh
rktools otp                  # 导出 128 字节并显示命名字段（根据芯片信息自动选择映射）
rktools otp 256
rktools otp --map rk3588
rktools otp --raw            # 仅输出十六进制
```

### 刷写 update.img（实验性升级功能）

```sh
rktools upgrade --wait 30s update.img
```

此命令解析 RKFW/RKAF 固件，要求存在 `parameter` 条目，并按设备现有的 GPT 分区名称匹配固件条目。它跳过 `package-file` 和 `parameter`，将 `super` 作为 sparse 镜像处理，将其他条目作为普通镜像复制。它不会根据 parameter 文件创建或协调 GPT；元数据更改请使用显式的 JSON 工作流程。

这一上游功能仍不完整：parameter CRC 检查和分区布局校验尚待实现；在 Maskrom 模式下载 loader 后，会重新打开原先的总线号/地址，无法可靠跟踪重新枚举。它缺少分区操作的进度显示，也不会读回校验已刷写的载荷。分区写入同样不会自动验证载荷；GPT apply 只验证 GPT 元数据。当前普通镜像路径会在达到分区长度时停止复制，而不是拒绝过大的镜像，因此请事先检查大小。其 `--timeout` 默认为 300s，但未一致地传递到所有 I/O 路径。对于需要可控操作的工作流程，建议显式写入分区，不要将此功能视为厂商工具的完整替代品。

### 通过写入 IDBlock 升级 loader

```sh
# 将生成的 IDBlock 写入默认 LBA 64
rktools upgrade-loader rk3588_spl_loader_v1.19.113.bin

# 等待设备并覆盖目标 LBA
rktools upgrade-loader --wait 30s --lba 64 rk3588_spl_loader_v1.19.113.bin
```

`upgrade-loader` 解析 Rockchip loader 镜像，提取 `FlashBoot` 和 `FlashData`，并将生成的 IDBlock 写入存储。如果镜像还包含 `FlashHead`，且目标设备报告支持新格式，工具会构建较新的 IDBlock 格式。
