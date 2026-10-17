//! 稀疏物理内存。
//!
//! 两条硬约束，都是 Python 版实测踩出来的：
//!
//! 1. **未初始化的内存必须读 0，不能读 0xFF。**
//!    libfdt 用"读到 0 就停"的方式遍历字符串块/结构块，填 0xFF 会让遍历不终止。
//! 2. **地址要按 40 位物理地址检查。**
//!    Python 的任意精度整数把一个下溢悄悄藏住了：`buf = x19 - bl_len - size`
//!    算出过 `0xFFFFFFFFFFEC69C0` 这种值还被写进了报告，实测正确值是
//!    `0x00000000400C6BC0`。这里显式用 wrapping 运算 + 40 位断言，不让它再溜过去。

use std::collections::HashMap;

/// 页大小。4096 是 SPL 里 FIT 对齐用的值（`FIT_ALIGN`），也正好是常见页大小。
const PAGE_BITS: u32 = 12;
const PAGE_SIZE: usize = 1 << PAGE_BITS;
const PAGE_MASK: u64 = (PAGE_SIZE as u64) - 1;

/// AArch64 物理地址宽度（SPL 阶段用的是 40 位 PA）。
pub const PA_BITS: u32 = 40;
pub const PA_MASK: u64 = (1u64 << PA_BITS) - 1;

#[derive(Debug)]
pub enum MemError {
    /// 地址超出 40 位物理地址空间
    BadAddr(u64),
}

impl std::fmt::Display for MemError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MemError::BadAddr(a) => write!(f, "地址 {a:#x} 超出 40 位物理地址空间"),
        }
    }
}

/// 按页分配的稀疏内存。
///
/// 用"页表 + 整页零初始化"而不是 `HashMap<u64, u8>`：单次启动要跑约 2000 万步，
/// 逐字节的哈希查找会成为瓶颈。
#[derive(Default)]
pub struct Mem {
    pages: HashMap<u64, Box<[u8; PAGE_SIZE]>>,
    /// 写过的字节数，用来观察模拟到底动了多少内存
    pub touched: u64,
    /// 写监视点区间 [watch_lo, watch_hi)，(0,0) = 关闭
    pub watch_lo: u64,
    pub watch_hi: u64,
    /// Cpu 每步把当前 PC 放这里，于是监视日志能定位到指令
    pub cur_pc: u64,
    /// (写入时的 PC, 被写地址)
    pub watch_log: Vec<(u64, u64)>,
    /// Cpu 每步把 x0..x3 放这里，命中时一并记录
    pub cur_regs: (u64, u64, u64, u64),
    /// (PC, 被写地址, x0, x1, x2, x3)
    pub watch_detail: Vec<(u64, u64, u64, u64, u64, u64)>,
    /// 缓存语义 overlay：SPL 把压缩的 U-Boot 解压/搬到自己代码区后，真机 CPU 的
    /// I/D cache 里仍是旧 SPL 字节，所以 SPL 继续正常执行到最终跳转。ov_on=true
    /// 之后，[ov_base, ov_base+ov.len()) 的所有**读**（取指+加载）返回原始 SPL
    /// 字节；**写**永远落到真实内存。唯一绕过 overlay 的是
    /// hw_fit_calculate_hash 桩（read_bytes_live）——它必须对真实载荷算摘要，
    /// 否则 digest 校验就形同虚设。
    pub ov_base: u64,
    pub ov: Vec<u8>,
    pub ov_on: bool,
}

impl Mem {
    pub fn new() -> Self {
        Self::default()
    }

    #[inline]
    fn check(a: u64) -> Result<(), MemError> {
        if a & !PA_MASK != 0 {
            Err(MemError::BadAddr(a))
        } else {
            Ok(())
        }
    }

    /// 取（或按需零初始化）一页
    fn page(&mut self, a: u64) -> &mut [u8; PAGE_SIZE] {
        let idx = a >> PAGE_BITS;
        self.pages
            .entry(idx)
            .or_insert_with(|| Box::new([0u8; PAGE_SIZE]))
    }

    pub fn read_u8(&mut self, a: u64) -> Result<u8, MemError> {
        Self::check(a)?;
        if self.ov_on {
            let d = a.wrapping_sub(self.ov_base);
            if d < self.ov.len() as u64 {
                return Ok(self.ov[d as usize]);
            }
        }
        let off = (a & PAGE_MASK) as usize;
        Ok(self.page(a)[off])
    }

    /// 绕过 overlay 读真实内存：hash 桩专用（对真实载荷算摘要）
    pub fn read_u8_live(&mut self, a: u64) -> Result<u8, MemError> {
        Self::check(a)?;
        let off = (a & PAGE_MASK) as usize;
        Ok(self.page(a)[off])
    }

    pub fn read_bytes_live(&mut self, a: u64, n: usize) -> Result<Vec<u8>, MemError> {
        let mut out = vec![0u8; n];
        for (i, b) in out.iter_mut().enumerate() {
            *b = self.read_u8_live(a.wrapping_add(i as u64))?;
        }
        Ok(out)
    }

    pub fn write_u8(&mut self, a: u64, v: u8) -> Result<(), MemError> {
        Self::check(a)?;
        self.hit_watch(a);
        let off = (a & PAGE_MASK) as usize;
        self.page(a)[off] = v;
        self.touched += 1;
        Ok(())
    }

    #[inline]
    fn hit_watch(&mut self, a: u64) {
        if self.watch_lo != self.watch_hi && a >= self.watch_lo && a < self.watch_hi {
            if self.watch_log.len() < 4096 {
                self.watch_log.push((self.cur_pc, a));
                let (r0, r1, r2, r3) = self.cur_regs;
                self.watch_detail.push((self.cur_pc, a, r0, r1, r2, r3));
            }
        }
    }

    /// 小端读。跨页时逐字节组装（SPL 的数据结构基本都自然对齐，但栈上不一定）。
    pub fn read(&mut self, a: u64, n: usize) -> Result<u64, MemError> {
        let mut v: u64 = 0;
        for i in 0..n {
            v |= (self.read_u8(a.wrapping_add(i as u64))? as u64) << (8 * i);
        }
        Ok(v)
    }

    /// 小端写。
    pub fn write(&mut self, a: u64, n: usize, v: u64) -> Result<(), MemError> {
        for i in 0..n {
            self.write_u8(a.wrapping_add(i as u64), ((v >> (8 * i)) & 0xff) as u8)?;
        }
        Ok(())
    }

    /// 批量读成字节串（FIT/镜像搬运用）
    pub fn read_bytes(&mut self, a: u64, n: usize) -> Result<Vec<u8>, MemError> {
        let mut out = vec![0u8; n];
        for (i, b) in out.iter_mut().enumerate() {
            *b = self.read_u8(a.wrapping_add(i as u64))?;
        }
        Ok(out)
    }

    /// 批量写（把 FIT 镜像搬进内存用）
    pub fn write_bytes(&mut self, a: u64, data: &[u8]) -> Result<(), MemError> {
        for (i, &b) in data.iter().enumerate() {
            self.write_u8(a.wrapping_add(i as u64), b)?;
        }
        Ok(())
    }

    /// 整页写入（搬镜像时比逐字节快得多）
    pub fn load_region(&mut self, a: u64, data: &[u8]) -> Result<(), MemError> {
        Self::check(a)?;
        if self.watch_lo != self.watch_hi && !data.is_empty() {
            let end = a.wrapping_add(data.len() as u64);
            if a < self.watch_hi && end > self.watch_lo {
                self.hit_watch(a);
            }
        }
        Self::check(a.wrapping_add(data.len() as u64))?;
        let mut off = 0usize;
        while off < data.len() {
            let addr = a.wrapping_add(off as u64);
            let po = (addr & PAGE_MASK) as usize;
            let chunk = (PAGE_SIZE - po).min(data.len() - off);
            let pg = self.page(addr);
            pg[po..po + chunk].copy_from_slice(&data[off..off + chunk]);
            off += chunk;
        }
        self.touched += data.len() as u64;
        Ok(())
    }

    pub fn is_allocated(&self, a: u64) -> bool {
        self.pages.contains_key(&(a >> PAGE_BITS))
    }

    pub fn page_count(&self) -> usize {
        self.pages.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uninitialised_reads_zero_not_ff() {
        let mut m = Mem::new();
        // 这是硬约束：libfdt 靠"读到 0 就停"终止遍历，读 0xFF 会跑飞
        assert_eq!(m.read_u8(0x1234_5678).unwrap(), 0);
        assert_eq!(m.read(0x0600_1000, 8).unwrap(), 0);
    }

    #[test]
    fn roundtrip_all_widths() {
        let mut m = Mem::new();
        m.write(0x2000_0000, 4, 0xdead_beef).unwrap();
        assert_eq!(m.read(0x2000_0000, 4).unwrap(), 0xdead_beef);
        // 小端序：低字节在低地址
        assert_eq!(m.read_u8(0x2000_0000).unwrap(), 0xef);
        assert_eq!(m.read_u8(0x2000_0003).unwrap(), 0xde);

        m.write(0x4020_0000, 8, 0x0123_4567_89ab_cdef).unwrap();
        assert_eq!(m.read(0x4020_0000, 8).unwrap(), 0x0123_4567_89ab_cdef);
    }

    #[test]
    fn crosses_page_boundary() {
        let mut m = Mem::new();
        let a = 0x1000_0ffc; // 距页尾 4 字节
        m.write(a, 8, 0x1122_3344_5566_7788).unwrap();
        assert_eq!(m.read(a, 8).unwrap(), 0x1122_3344_5566_7788);
        assert_eq!(m.page_count(), 2);
    }

    #[test]
    fn load_region_bulk() {
        let mut m = Mem::new();
        let data: Vec<u8> = (0..5000u32).map(|i| (i & 0xff) as u8).collect();
        m.load_region(0x2000_0000, &data).unwrap();
        assert_eq!(m.read_bytes(0x2000_0000, 5000).unwrap(), data);
        // 5000 字节跨 2 页
        assert_eq!(m.page_count(), 2);
    }

    /// 40 位 PA 检查：Python 版就是在这里让下溢溜过去的
    #[test]
    fn rejects_out_of_range_pa() {
        let mut m = Mem::new();
        // 实测出现过的错误值
        assert!(m.read_u8(0xFFFF_FFFF_FFEC_69C0).is_err());
        // 正确的值要能读写
        assert!(m.write_u8(0x0000_0000_400C_6BC0, 1).is_ok());
        // 边界
        assert!(m.read_u8(PA_MASK).is_ok());
        assert!(m.read_u8(PA_MASK + 1).is_err());
    }

    /// wrapping 运算：`x19 - bl_len - size` 这类计算下溢后必须被 PA 检查拦下
    #[test]
    fn underflow_is_caught_not_silently_wrapped() {
        let x19: u64 = 0x1FFFC0;
        let bl_len: u64 = 512;
        let size: u64 = 0x400000;
        let buf = x19.wrapping_sub(bl_len).wrapping_sub(size);
        // 下溢了 —— 高位全 1
        assert_ne!(buf & !PA_MASK, 0, "这个值应该被识别为越界");
        assert!(Mem::check(buf).is_err());
    }
}
