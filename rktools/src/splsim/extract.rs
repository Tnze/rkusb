//! 从整份 dump 里自提取 SPL。
//!
//! **不写死任何单板规则** —— 靠 RKNS 的 loader-entry 记录自己找。
//! 每条记录 16 字节：`u16 sect_off | u16 nsect | u32 0xffffffff | u32 index`。
//! 扫 `0xffffffff` 这个标记而不是假定表偏移，这样不同 RKNS 版本都适用
//! （这几份 dump 的表偏移就不一样：T90Pro/T30Pro 在 0x10000，T20Pro/RK3576 在 0x8000）。
//!
//! 扇区号是**相对 idblock 起点**的，不是相对镜像起点 —— 这点搞错会 carve 出垃圾。

use std::fs;
use std::path::Path;

/// 一条 RKNS loader-entry 记录
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// 记录在镜像里的偏移
    pub rec_off: usize,
    /// 相对 idblock 的扇区号
    pub sect: u16,
    /// 扇区数
    pub nsect: u16,
    /// 记录里的 index 字段
    pub index: u32,
    /// 换算出的绝对偏移
    pub abs: usize,
    /// 字节数
    pub size: usize,
}

#[derive(Debug)]
pub enum ExtractError {
    Io(std::io::Error),
    /// 在给定偏移找不到 RKNS 魔数
    NoRkns(usize),
    /// idblock 里一条 loader-entry 都没扫到
    NoEntries,
    /// 有 entry 但没有一个像 SPL
    NoSpl,
}

impl std::fmt::Display for ExtractError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExtractError::Io(e) => write!(f, "读文件失败: {e}"),
            ExtractError::NoRkns(o) => write!(f, "偏移 {o:#x} 处不是 RKNS 魔数"),
            ExtractError::NoEntries => write!(f, "idblock 里没扫到 loader-entry 记录"),
            ExtractError::NoSpl => write!(f, "有 loader-entry，但没有一个含 \"U-Boot SPL\""),
        }
    }
}

pub const SECTOR: usize = 512;

/// 在 idblock 起始后的前 `scan` 字节里扫 loader-entry 记录
pub fn scan_entries(b: &[u8], idb: usize, scan: usize) -> Vec<Entry> {
    let mut out = Vec::new();
    let end = (idb + scan).min(b.len().saturating_sub(16));
    let mut o = idb;
    while o < end {
        // 标记字段在 o+4..o+8
        if u32::from_le_bytes([b[o + 4], b[o + 5], b[o + 6], b[o + 7]]) == 0xffff_ffff {
            let sect = u16::from_le_bytes([b[o], b[o + 1]]);
            let nsect = u16::from_le_bytes([b[o + 2], b[o + 3]]);
            let index = u32::from_le_bytes([b[o + 8], b[o + 9], b[o + 10], b[o + 11]]);
            // 和 Python 版一样的合理性过滤，避免把数据区的巧合当成记录
            if nsect > 0 && nsect < 0x2000 && (sect as usize) < 0x20000 {
                out.push(Entry {
                    rec_off: o,
                    sect,
                    nsect,
                    index,
                    abs: idb + sect as usize * SECTOR,
                    size: nsect as usize * SECTOR,
                });
            }
        }
        o += 4;
    }
    out
}

/// 判断一段字节像不像 SPL
pub fn looks_like_spl(b: &[u8]) -> bool {
    b.windows(12).any(|w| w == b"U-Boot SPL 2")
}

/// 从镜像字节里提取 SPL。返回 (SPL 字节, 命中的 entry)
pub fn extract_spl(b: &[u8], idb: usize) -> Result<(Vec<u8>, Entry), ExtractError> {
    if b.get(idb..idb + 4) != Some(b"RKNS".as_ref()) {
        return Err(ExtractError::NoRkns(idb));
    }
    let ents = scan_entries(b, idb, 0x2000);
    if ents.is_empty() {
        return Err(ExtractError::NoEntries);
    }
    for e in &ents {
        if e.abs + e.size <= b.len() {
            let slice = &b[e.abs..e.abs + e.size];
            if looks_like_spl(slice) {
                return Ok((slice.to_vec(), e.clone()));
            }
        }
    }
    Err(ExtractError::NoSpl)
}

/// 在 idblock 之后自动找 RKNS 魔数（省得每次手写偏移）
pub fn find_rkns(b: &[u8], limit: usize) -> Option<usize> {
    b[..limit.min(b.len())]
        .windows(4)
        .position(|w| w == b"RKNS")
}

/// 从已读入内存的整份 dump 提取 SPL（自动定位 RKNS）
pub fn extract_spl_owned(b: &[u8]) -> Result<(Vec<u8>, Entry, usize), ExtractError> {
    let idb = find_rkns(b, 0x20000).ok_or(ExtractError::NoRkns(0))?;
    let (spl, e) = extract_spl(b, idb)?;
    Ok((spl, e, idb))
}

/// 便捷入口：读文件 + 自动定位 RKNS + 提取
pub fn extract_from_file<P: AsRef<Path>>(p: P) -> Result<(Vec<u8>, Entry, usize), ExtractError> {
    let b = fs::read(p).map_err(ExtractError::Io)?;
    let idb = find_rkns(&b, 0x20000).ok_or(ExtractError::NoRkns(0))?;
    let (spl, e) = extract_spl(&b, idb)?;
    Ok((spl, e, idb))
}

#[cfg(all(test, feature = "splsim-bench-dirs"))]
mod tests {
    use super::*;

    // 用 CARGO_MANIFEST_DIR 相对定位：splsim 在 rk/splsim，fixed/spl 在 rk/ 下。
    // 这样整个树搬到 /code 或别处都不用改。
    const FIXED: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../fixed");
    const SPLDIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../spl");

    /// 这四份是之前用 Python 版 carve 出来、并且已经用 radare2 反汇编分析过的。
    /// Rust 版必须逐字节复现 —— 这是对提取逻辑的差分校验。
    fn cases() -> Vec<(&'static str, &'static str, usize, &'static str)> {
        vec![
            (
                "T20Pro",
                "SPINOR_RK3588_SPI+NVMe_T20Pro_A12_无双槽位_CTG3_nolimit.bin",
                0x8000,
                "T20Pro.bin",
            ),
            (
                "T30Pro",
                "SPINOR_RK3588_SPI+NVMe_T30Pro_A12_无双槽位_SU30Pro_nolimit.bin",
                0x10000,
                "T30Pro.bin",
            ),
            (
                "T90Pro",
                "SPINOR_RK3588_SPI+NVMe_T90Pro_A15_双槽位_ME30_nolimit.bin",
                0x10000,
                "T90Pro.bin",
            ),
            (
                "RK3576",
                "RK3576_eMMC_only_dump_A15_双槽位_SS30_nolimit.bin",
                0x8000,
                "RK3576.bin",
            ),
        ]
    }

    #[test]
    fn rkns_magic_at_expected_offsets() {
        if fs::metadata(FIXED).is_err() {
            eprintln!("skip: ../fixed dumps not in this tree");
            return;
        }
        for (k, f, idb, _) in cases() {
            let b = fs::read(format!("{FIXED}/{f}")).unwrap();
            assert_eq!(&b[idb..idb + 4], b"RKNS", "{k} 在 {idb:#x} 处应有 RKNS");
        }
    }

    /// 自动定位要和已知偏移一致
    #[test]
    fn auto_locate_matches_known_offsets() {
        if fs::metadata(FIXED).is_err() {
            eprintln!("skip: ../fixed dumps not in this tree");
            return;
        }
        for (k, f, idb, _) in cases() {
            let b = fs::read(format!("{FIXED}/{f}")).unwrap();
            assert_eq!(find_rkns(&b, 0x20000), Some(idb), "{k} 自动定位应为 {idb:#x}");
        }
    }

    /// 核心：逐字节复现 Python 版 carve 出来的 SPL
    #[test]
    fn reproduces_known_good_carvings() {
        if fs::metadata(FIXED).is_err() || fs::metadata(SPLDIR).is_err() {
            eprintln!("skip: ../fixed or ../spl not in this tree");
            return;
        }
        let mut checked = 0;
        for (k, f, idb, want_file) in cases() {
            let dump = fs::read(format!("{FIXED}/{f}")).unwrap();
            let (spl, ent) = extract_spl(&dump, idb).unwrap_or_else(|e| {
                panic!("{k}: 提取失败 {e}")
            });
            let want = fs::read(format!("{SPLDIR}/{want_file}")).unwrap();
            assert_eq!(
                spl.len(),
                want.len(),
                "{k}: 长度 {} != 已知良好值 {}",
                spl.len(),
                want.len()
            );
            assert_eq!(
                spl, want,
                "{k}: 提取结果与 {want_file} 不一致（entry@{:#x} sect={:#x} n={:#x}）",
                ent.rec_off, ent.sect, ent.nsect
            );
            checked += 1;
        }
        assert_eq!(checked, 4, "应校验 4 份");
    }

    /// 扇区号是相对 idblock 起点的，不是相对镜像起点。
    /// （早先想用"按镜像起点算就扫不到 SPL"来证，但那不成立：T20Pro 的
    ///   idb=0x8000，错算出的窗口 0x12800..0x48000 正好覆盖真实 SPL 区间
    ///   0x1a800..，照样能扫到字符串。所以直接断言换算关系。）
    #[test]
    fn sector_offset_is_idblock_relative() {
        if fs::metadata(FIXED).is_err() {
            eprintln!("skip: ../fixed dumps not in this tree");
            return;
        }
        for (k, f, idb, _) in cases() {
            let b = fs::read(format!("{FIXED}/{f}")).unwrap();
            let ents = scan_entries(&b, idb, 0x2000);
            let e = ents
                .iter()
                .find(|e| e.abs + e.size <= b.len() && looks_like_spl(&b[e.abs..e.abs + e.size]))
                .unwrap_or_else(|| panic!("{k}: 应找到 SPL 那条 entry"));
            assert_eq!(
                e.abs,
                idb + e.sect as usize * SECTOR,
                "{k}: abs 必须是 idb + sect*512"
            );
            // idb != 0，所以两种算法的结果必然不同 —— 用错就一定 carve 偏
            assert_ne!(
                e.abs,
                e.sect as usize * SECTOR,
                "{k}: 若 abs == sect*512，这条测试就检验不出相对性了"
            );
        }
    }

    /// 找不到 RKNS 时要报错，不能静默返回垃圾
    #[test]
    fn rejects_non_rkns_image() {
        let junk = vec![0u8; 0x10000];
        assert!(matches!(
            extract_spl(&junk, 0x8000),
            Err(ExtractError::NoRkns(0x8000))
        ));
    }
}
