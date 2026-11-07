//! ARM 的 `DecodeBitMasks` —— 逻辑立即数 (`and/orr/eor/ands #imm`) 的位掩码编码。
//!
//! A64 的逻辑立即数不是直接的位模式，而是 "N:immr:imms" 三元组描述的
//! **可循环移位的基本位串**。这里照 ARM ARM 的伪代码实现，不做近似。
//!
//! 注意：capstone 直接给出最终立即数，所以 Python 版从没实现过这段；
//! Rust 版没有 capstone，必须自己算。

/// 把 `N:immr:imms` 解码成 64 位 `wmask`。
///
/// `n==true` 表示 64 位形式（element size = 64）。
/// 返回 `None` 表示该编码是 UNALLOCATED。
pub fn decode_bitmasks(n: bool, immr: u32, imms: u32) -> Option<u64> {
    // ARM 伪代码：len = HighestSetBit(immN : NOT(imms))
    // 注意 NOT 是对 imms 的 6 位取反，即 `(!imms) & 0x3f`。
    // （曾经误以为该写成 `imms ^ 0x3f`，那是错的 —— 用 SPL 里 228 个真实
    //   编码对拍过，`(!imms)&0x3f` 191/191 全对。）
    let not_imms = (!imms) & 0x3f;
    let nbit = if n { 1u32 << 6 } else { 0 };
    let len = highest_set_bit((not_imms | nbit) as u64);
    if len < 1 {
        return None;
    }
    let esize = 1u32 << len;
    if esize > 64 {
        return None;
    }
    let levels = (esize - 1) as u64;
    // N=1 时 imms 的高位必须为 0，否则 unallocated
    if n && (imms as u64 & !levels) != 0 {
        return None;
    }
    let s = imms as u64 & levels;
    let r = immr as u64 & levels;
    // 逻辑立即数下 imms 低位全 1 是保留编码（会得到无用的全 1）
    if s == levels {
        return None;
    }

    // welem = ZeroExtend('1':Zeros(S), esize)  —— 低 S+1 位是 1
    let welem = (1u64 << (s + 1)) - 1;
    let wmask = replicate(welem, esize);
    Some(wmask.rotate_right(r as u32))
}

/// ARM `Replicate(x, n)`：把 x 的低 esize 位重复填满 64 位。
///
/// 必须是**先重复再截断**，不能在重复之后再 & 掩码 —— 那样会把重复出来的
/// 位全截掉，`#0xfffffffc` 这种掩码会算错。
fn replicate(pattern: u64, esize: u32) -> u64 {
    debug_assert!(esize.is_power_of_two() && esize <= 64);
    let mut out = pattern & mask(esize);
    let mut size = esize;
    while size < 64 {
        out |= out << size;
        size *= 2;
    }
    out
}

/// 低 `bits` 位的掩码；bits>=64 时全 1。
pub fn mask(bits: u32) -> u64 {
    if bits >= 64 {
        u64::MAX
    } else {
        (1u64 << bits) - 1
    }
}

/// 最高置位位号（0 起），x==0 返回 -1。
pub fn highest_set_bit(x: u64) -> i32 {
    if x == 0 {
        -1
    } else {
        63 - x.leading_zeros() as i32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
fn masks_from_real_encodings() {
        // 参数与期望值都取自 tests/logimm_golden.json —— 那是从 SPL 二进制里
        // 扫出的真实编码 + radare2 6.2.2 的反汇编结果。
        // 不要手写这些数字：immr/imms 靠眼睛从 hex 里抽，已经连续抽错三次。
        let w = |r: u32, s: u32| decode_bitmasks(false, r, s).unwrap();
        assert_eq!(w(30, 30) & 0xffff_ffff, 0xfffffffd);
        assert_eq!(w(16, 15) & 0xffff_ffff, 0xffff0000);
        assert_eq!(w( 0, 15) & 0xffff_ffff, 0x0000ffff);
        assert_eq!(w(10, 34) & 0xffff_ffff, 0x01c001c0);
        assert_eq!(w( 3, 32) & 0xffff_ffff, 0x20002000);
        assert_eq!(w( 0, 33) & 0xffff_ffff, 0x00030003);
        assert_eq!(w(13, 32) & 0xffff_ffff, 0x00080008);
        assert_eq!(w( 0,  1) & 0xffff_ffff, 0x00000003);
        assert_eq!(w( 0,  0) & 0xffff_ffff, 0x00000001);
        assert_eq!(w(30,  0) & 0xffff_ffff, 0x00000004);
        assert_eq!(w(19, 30) & 0xffff_ffff, 0xffffefff);
        assert_eq!(w( 0,  4) & 0xffff_ffff, 0x0000001f);
        assert_eq!(w( 0,  5) & 0xffff_ffff, 0x0000003f);
        assert_eq!(w(26, 25) & 0xffff_ffff, 0xffffffc0);
    }

        #[test]
    fn reserved() {
        // imms 低位全 1 是逻辑立即数的保留编码
        assert_eq!(decode_bitmasks(false, 0, 0x3f), None);
        assert_eq!(decode_bitmasks(true, 0, 0x3f), None);
    }
}
