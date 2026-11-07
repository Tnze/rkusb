//! AArch64 解释器。
//!
//! 这里每一条语义都是 Python 版实测踩过坑之后定下来的，注释里写明了为什么，
//! 不要"顺手优化"掉：
//!
//! 1. **NZCV 的 V 标志**：减法 `((a^b)&(a^r))>>(bits-1)`，
//!    加法 `((~(a^b))&(a^r))>>(bits-1)`。Python 版 `set_nzcv_sub` 从来没算过 V（恒 0），
//!    于是所有有符号比较（`b.ge/b.lt/b.gt/b.le`，条件是 `N==V`）全错。
//! 2. **C 标志按 `bits` 位判，不是按 64 位** —— W 形式的进位在 bit32，不在 bit64。
//! 3. **变址寻址 `[xN, xM, ext #amt]` 必须把变址寄存器加进去。**
//!    Python 版只算了 `base+disp`，三份 SPL 里 837 条这种访存全错，
//!    包括 libfdt 跳表的下标读取（0x5c1c），直接导致 `fdt_next_tag` 永远选 case 0。
//! 4. **扩展寄存器移位** `add x0, x1, w0, sxtb 2`（libfdt 跳表 ×4）。
//! 5. **`adrp` 的立即数是页基址结果，`adr` 是 1 位粒度** —— 译码器已算好放在 `d.imm`。
//! 6. **`stp/ldp` 的 offset 形式是 `base + disp`**；pre/post-index 会写回基址寄存器，
//!    搞错会把 sp 写坏（Python 版把 `stp x1,x1,[x29,#0x50]` 误判成 pre-index）。
//! 7. **`movk` 的移位在第二个操作数**，中间值按 64 位算完再截断。
//! 8. **W 形式一律截 32 位。**

use crate::splsim::decode::{decode, Addr, D, K, Sh};
use crate::splsim::mem::{Mem, MemError};

/// 模拟停止的原因
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stop {
    /// 下一阶段（U-Boot）被搬进了 SPL 自己的代码区 —— 真机上 SPL 紧接着就跳过去。
    /// 继续模拟 SPL 的指令已经没有意义：它脚下的代码已经换成 U-Boot 了。
    Handoff(u64),
    /// 跳到 U-Boot 入口（成功）
    JumpUBoot(u64),
    /// 跳到 `hang()` 自旋（`b #0` 到自身）
    Hang(u64),
    /// `spl_ab_decrease_reset` 之类的主动复位
    Reset(u64),
    /// 步数用尽
    StepLimit,
    /// 地址越界 / 取指失败
    Fault(String),
    /// 被调函数返回到了哨兵地址，x0 是返回值
    Returned(i64),
}

/// 条件码
#[inline]
fn cond_holds(cc: u8, n: bool, z: bool, c: bool, v: bool) -> bool {
    let base = match cc >> 1 {
        0 => z,             // EQ / NE
        1 => c,             // CS / CC
        2 => n,             // MI / PL
        3 => v,             // VS / VC
        4 => c && !z,       // HI / LS
        5 => n == v,        // GE / LT
        6 => !z && n == v,  // GT / LE
        _ => true,          // AL / NV
    };
    // 低 1 位为 1 表示取反；AL(14)/NV(15) 恒真
    if cc == 0xf {
        true
    } else if cc & 1 == 1 {
        !base
    } else {
        base
    }
}

pub struct Cpu {
    /// x0..x30。31 号（xzr/sp）单独处理。
    pub x: [u64; 31],
    pub sp: u64,
    pub pc: u64,
    pub n: bool,
    pub z: bool,
    pub c: bool,
    pub v: bool,
    pub mem: Mem,
    pub steps: u64,
    pub max_steps: u64,
    pub stop: Option<Stop>,
    /// 观测到的跳转目标，用来判定是否交棒给 U-Boot
    pub jump_target: Option<u64>,
    /// 上一条指令的地址，用来检测自旋
    last_pc: u64,
    spin: u32,
    /// `info->read` 的桩地址。SPL 通过 `ldr x4,[x21,0x20]; blr x4` 调用它，
    /// 真机上那是读 flash 的函数；这里由模拟器接管。
    pub hook_read: u64,
    /// 桩被调用时从这里取数据（整份镜像）
    pub flash: Vec<u8>,
    /// flash[0] 对应的扇区号
    pub flash_base_sector: u64,
    /// read 桩被调用的次数
    pub read_calls: u64,
    /// printf 的桩地址：控制台没初始化，执行它会走进数据区
    pub hook_printf: u64,
    /// printf 被挂起的次数
    pub printf_calls: u64,
    /// 哨兵返回地址：被调函数 ret 到这里就算正常返回
    pub sentinel_ret: u64,
    /// verified-boot 门禁的桩地址。
    ///
    /// v8（反汇编实测，不再是"绕过"）：门禁函数（T20Pro 0x300 / RK3576 0x384）为
    ///   dev = uclass_get_device(0x24=misc);  if !dev -> 返回 1
    ///   misc_read(dev, 0x20, &byte, 1);      失败 -> 返回 1 ("Can't read verified-boot flag")
    ///   返回 (byte == 0xff)                   并打印 "## Verified-boot: %d"
    /// 驱动表（by-id 函数里 adrp 给出边界，entry 0x80、id 在 +8）静态扫描：
    /// T20Pro 0x33848..0x343c8、RK3576 0x3f0c8..0x3fcc8、T90Pro 0x3dd08 处
    /// 三份 SPL 均含 id 0x24 (misc) 驱动 => dev 非 NULL。
    /// byte 是 OTP/fuse，位于 patcher 触碰的 flash 区之外 => stock/patched 必然同值；
    /// stock 能启动（ground truth）=> 这些板上 byte != 0xff。`vb_flag_byte` 建模该字节。
    pub hook_vb_gate: u64,
    pub vb_gate_calls: u64,
    /// 门禁读到的 verified-boot fuse 字节（misc_read offset 0x20）；默认 0x00 = 与可启动板一致。
    pub vb_flag_byte: u8,
    /// hw_fit_calculate_hash 的桩地址
    pub hook_hash: u64,
    pub hash_calls: u64,
    /// 最近一次 hw_fit_calculate_hash 算出的摘要，供外部对拍
    pub last_digest: Option<[u8; 32]>,
    /// 诊断：打印桩入口时调用者栈上的候选长度
    pub hash_debug: bool,
    /// 最近执行过的 PC 环形缓冲（诊断控制流跑飞用）
    pub recent_pcs: Vec<u64>,
    recent_i: usize,
    /// 定点追踪还能打多少条（防止刷屏）
    pub trace_budget: u64,
    /// 从第几次哈希调用开始追踪（0/1 = 从第一次）
    pub trace_from_call: u64,
    /// memcpy 入口地址（SPL+0x7ca4）。只记录目的区间覆盖写监视区的那些调用。
    pub hook_memcpy: u64,
    /// (返回地址 x30, dst x0, src x1, len x2)
    pub memcpy_log: Vec<(u64, u64, u64, u64)>,
    /// 只有 dst 落在这个区间才记（用来盯"往 SPL 镜像里写"的 memcpy）
    pub memcpy_log_lo: u64,
    pub memcpy_log_hi: u64,
    pub memcpy_calls: u64,
    /// 写监视命中那一刻的 PC 环快照（回答"执行流是怎么走到这条写指令的"）
    /// SPL 自身代码区 [code_lo, code_hi)。写进这里就意味着交棒。
    pub code_lo: u64,
    pub code_hi: u64,
    pub watch_snap: Vec<Vec<u64>>,
    watch_seen: usize,
    /// 挂起 printf 时把 (格式串, 参数) 记下来 —— SPL 自己的诊断信息比反推强得多
    pub printf_log: Vec<String>,
    /// 记录 printf 时最多解析几个 % 转换
    pub printf_capture: bool,
    /// SPL 已经打印过 "Jumping to U-Boot"（完整成功的前置证据）
    pub saw_jumping: bool,
    /// U-Boot 首次盖进 SPL 代码区的地址（缓存语义开启点）
    pub handoff_addr: Option<u64>,
    /// SPL 的加载基址，仅用于把 x30 换算成 SPL 内偏移打印
    pub text_base: u64,
}

impl Cpu {
    pub fn new(mem: Mem, entry: u64) -> Self {
        Self {
            x: [0; 31],
            sp: 0,
            pc: entry,
            n: false,
            z: false,
            c: false,
            v: false,
            mem,
            steps: 0,
            max_steps: 400_000_000,
            stop: None,
            jump_target: None,
            last_pc: u64::MAX,
            spin: 0,
            hook_read: 0,
            flash: Vec::new(),
            flash_base_sector: 0,
            read_calls: 0,
            hook_printf: 0,
            printf_calls: 0,
            sentinel_ret: 0,
            hook_vb_gate: 0,
            vb_gate_calls: 0,
            vb_flag_byte: 0,
            hook_hash: 0,
            hash_calls: 0,
            last_digest: None,
            hash_debug: false,
            recent_pcs: vec![0; 48],
            recent_i: 0,
            trace_budget: 0,
            trace_from_call: 1,
            hook_memcpy: 0,
            memcpy_log: Vec::new(),
            memcpy_log_lo: 0,
            memcpy_log_hi: 0,
            memcpy_calls: 0,
            code_lo: 0,
            code_hi: 0,
            watch_snap: Vec::new(),
            watch_seen: 0,
            printf_log: Vec::new(),
            printf_capture: false,
            saw_jumping: false,
            handoff_addr: None,
            text_base: 0,
        }
    }

    // ------------------------------------------------------------ 寄存器

    /// 通用读：31 = xzr（读 0）
    #[inline]
    fn reg(&self, i: u8) -> u64 {
        if i >= 31 {
            0
        } else {
            self.x[i as usize]
        }
    }

    /// 写：31 = 丢弃
    #[inline]
    fn set_reg(&mut self, i: u8, v: u64) {
        if i < 31 {
            self.x[i as usize] = v;
        }
    }

    /// 供追踪打印用
    #[inline]
    pub fn reg_dbg(&self, i: u8) -> u64 {
        self.reg(i)
    }

    /// 读一个 NUL 结尾的 C 字符串（诊断打印用）。
    /// 只接受可打印 ASCII，否则返回占位符 —— 指针猜错时能一眼看出来，
    /// 而不是打出一串乱码让人以为是对的。
    pub fn cstr(&mut self, addr: u64) -> String {
        let mut s = String::new();
        for i in 0..128u64 {
            match self.mem.read_u8(addr + i) {
                Ok(0) => return s,
                Ok(b) if (0x20..0x7f).contains(&b) => s.push(b as char),
                _ => return format!("<非字符串 @{addr:#x}>"),
            }
        }
        s
    }

    /// 基址/栈指针语境：31 = sp
    #[inline]
    fn reg_sp(&self, i: u8) -> u64 {
        if i >= 31 {
            self.sp
        } else {
            self.x[i as usize]
        }
    }

    #[inline]
    fn set_reg_sp(&mut self, i: u8, v: u64) {
        if i >= 31 {
            self.sp = v;
        } else {
            self.x[i as usize] = v;
        }
    }

    /// W 形式截断到 32 位
    #[inline]
    fn trunc(&self, v: u64, w32: bool) -> u64 {
        if w32 {
            v & 0xffff_ffff
        } else {
            v
        }
    }

    // ------------------------------------------------------------ 标志

    /// 加法/带进位加法的标志。`bits` 是操作位宽（32 或 64）。
    fn flags_add(&mut self, a: u64, b: u64, r: u64, bits: u32, carry_in: bool) {
        let m = if bits == 32 { 0xffff_ffffu64 } else { u64::MAX };
        let (a, b, r) = (a & m, b & m, r & m);
        // C 按 bits 位判，不是按 64 位
        let full = a as u128 + b as u128 + carry_in as u128;
        self.c = full > m as u128;
        // V = (~(a^b)) & (a^r) 的最高位
        self.v = ((!(a ^ b)) & (a ^ r)) >> (bits - 1) & 1 == 1;
        self.n = r >> (bits - 1) & 1 == 1;
        self.z = r == 0;
    }

    /// 减法的标志
    fn flags_sub(&mut self, a: u64, b: u64, r: u64, bits: u32, borrow_in: bool) {
        let m = if bits == 32 { 0xffff_ffffu64 } else { u64::MAX };
        let (a, b, r) = (a & m, b & m, r & m);
        // 减法：C = 没有借位 = a >= b (+borrow_in)
        let full = a as i128 - b as i128 - borrow_in as i128;
        self.c = full >= 0;
        // V = ((a^b) & (a^r)) 的最高位
        self.v = (((a ^ b) & (a ^ r)) >> (bits - 1)) & 1 == 1;
        self.n = r >> (bits - 1) & 1 == 1;
        self.z = r == 0;
    }

    /// 逻辑运算只更新 N/Z（C 保持不变，V 清零 —— AArch64 的规定）
    fn flags_logic(&mut self, r: u64, bits: u32) {
        let m = if bits == 32 { 0xffff_ffffu64 } else { u64::MAX };
        let r = r & m;
        self.n = r >> (bits - 1) & 1 == 1;
        self.z = r == 0;
        self.v = false;
    }

    // ------------------------------------------------------------ 第二操作数

    /// 扩展寄存器：opt 0..7 = uxtb/uxth/uxtw/uxtx/sxtb/sxth/sxtw/sxtx
    fn extend(v: u64, opt: u8, w32: bool) -> u64 {
        match opt {
            0 => v & 0xff,                              // UXTB
            1 => v & 0xffff,                            // UXTH
            2 => v & 0xffff_ffff,                       // UXTW
            3 => v,                                     // UXTX
            4 => (v as u8) as i8 as i64 as u64,         // SXTB
            5 => (v as u16) as i16 as i64 as u64,       // SXTH
            6 => (v as u32) as i32 as i64 as u64,       // SXTW
            _ => {
                // SXTX：32 位形式下等于 UXTW
                if w32 {
                    v & 0xffff_ffff
                } else {
                    v
                }
            }
        }
    }

    /// 移位/扩展后的第二源操作数
    fn operand2(&self, d: &D) -> u64 {
        let v = self.reg(d.rm);
        let bits = if d.w32 { 32 } else { 64 };
        match d.sh {
            Sh::Lsl => v << (d.amt as u32),
            Sh::Lsr => v >> (d.amt as u32),
            Sh::Asr => ((v as i64) >> (d.amt as u32)) as u64,
            Sh::Ror => v.rotate_right((d.amt as u32) % bits),
            Sh::Ext(opt) => Self::extend(v, opt, d.w32) << (d.amt as u32),
        }
    }

    // ------------------------------------------------------------ 访存地址

    /// 算出访存地址，并返回要写回基址寄存器的值（offset 形式写回原值）
    fn resolve(&self, d: &D) -> (u64, u64) {
        let base = self.reg_sp(d.rn);
        match d.addr {
            Addr::Offset(disp) => (base.wrapping_add(disp as u64), base),
            Addr::Pre(disp) => {
                let a = base.wrapping_add(disp as u64);
                (a, a)
            }
            Addr::Post(disp) => (base, base.wrapping_add(disp as u64)),
            Addr::Reg { ext, sh } => {
                // 变址寄存器必须加进去 —— 漏掉它是 Python 版最贵的一个 bug
                let idx = Self::extend(self.reg(d.rm), ext, d.w32);
                (base.wrapping_add(idx << sh), base)
            }
        }
    }

    // ------------------------------------------------------------ 位域

    fn rotate_right(v: u64, r: u64, bits: u32) -> u64 {
        let r = (r % bits as u64) as u32;
        if r == 0 {
            v
        } else {
            (v >> r) | (v << (bits - r))
        }
    }

    /// UBFM / SBFM / BFM。
    ///
    /// 一律按 (immr, imms) 的两种情形直接定义，**不要**走"ROR 之后再 & DecodeBitMasks 掩码"
    /// 那条路。原因（这个坑真踩过）：逻辑立即数的 DecodeBitMasks 在 `imms == 全 1` 时是
    /// UNALLOCATED，会返回 None；可 `imms == bits-1` 恰恰就是 `lsr`/`asr` 的合法编码。
    /// 掩码一旦退化成全 1，`lsr x5, x2, #3` 就变成了 `ror x5, x2, #3`：
    /// 实测 SPL 的 memcpy 里循环上限 `(len>>3)<<3` 因此算成 0x3203c（奇数），
    /// 而计数器按 8 递增永远撞不上它，字循环一路写出天际，把 SPL 自己的代码覆盖成 0。
    ///
    ///   imms >= immr → 抽取（UBFX/SBFX/BFXIL）：取 Rn 第 immr 位起的 width 位，放到第 0 位
    ///   imms <  immr → 插入（UBFIZ/SBFIZ/BFI）：把 Rn 低 width 位左移 lsb = bits - immr
    fn bitfield(&self, d: &D, kind: u8) -> u64 {
        let bits = if d.w32 { 32u32 } else { 64 };
        let m = if d.w32 { 0xffff_ffffu64 } else { u64::MAX };
        let rn = self.reg(d.rn) & m;
        let immr = d.imm as u64;
        let imms = d.imm2 as u64;

        let extract = imms >= immr;
        let width = if extract { imms - immr + 1 } else { imms + 1 };
        let lsb = if extract { 0u64 } else { bits as u64 - immr };
        let wmask = if width >= 64 { u64::MAX } else { (1u64 << width) - 1 };

        // 先取出"未移位"的字段值，再一次性放到目标位置 —— 别在两步里各移一次。
        let raw = if extract { rn >> immr } else { rn };
        let field = ((raw & wmask) << lsb) & m;

        match kind {
            0 => field, // UBFM
            1 => {
                // SBFM：按 width 位符号扩展
                let signbit = 1u64 << (width - 1);
                if field & signbit != 0 {
                    (field | !wmask) & m
                } else {
                    field
                }
            }
            _ => {
                // BFM：把这一段插进 Rd，其余位保留
                (self.reg(d.rd) & !wmask & m) | field
            }
        }
    }

    // ------------------------------------------------------------ 主循环

    pub fn run(&mut self) -> Stop {
        while self.stop.is_none() {
            match self.step() {
                Ok(()) => {}
                Err(e) => {
                    self.stop = Some(Stop::Fault(format!("pc={:#x}: {e}", self.pc)));
                }
            }
        }
        self.stop.clone().unwrap()
    }

    pub fn step(&mut self) -> Result<(), MemError> {
        if self.steps >= self.max_steps {
            self.stop = Some(Stop::StepLimit);
            return Ok(());
        }
        self.steps += 1;
        self.mem.cur_pc = self.pc;
        self.mem.cur_regs = (self.reg_dbg(0), self.reg_dbg(1), self.reg_dbg(2), self.reg_dbg(3));

        // 自旋检测：`b #0` 到自身是 SPL 的 hang()
        if self.pc == self.last_pc {
            self.spin += 1;
            if self.spin > 8 {
                self.stop = Some(Stop::Hang(self.pc));
                return Ok(());
            }
        } else {
            self.spin = 0;
        }
        if self.hash_debug {
            self.recent_pcs[self.recent_i] = self.pc;
            self.recent_i = (self.recent_i + 1) % self.recent_pcs.len();
            // 定点追踪：SPL+0x4e90..0x4f30 这段（就是那个出不来的循环）
            let off = self.pc.wrapping_sub(self.text_base);
            if self.trace_budget > 0 && (0x7ca4..0x7d00).contains(&off) {
                self.trace_budget -= 1;
                let w = self.mem.read(self.pc, 4).unwrap_or(0) as u32;
                let d = decode(w, off);
                self.printf_log.push(format!(
                    "TRC SPL+{off:#x} {w:08x} {:<28} x0={:#x} x1={:#x} x2={:#x} x3={:#x} x4={:#x} x5={:#x} x6={:#x} sp={:#x}",
                    format!("{:?} imm={:#x} rm={} amt={}", d.k, d.imm, d.rm, d.amt), self.reg_dbg(0), self.reg_dbg(1), self.reg_dbg(2),
                    self.reg_dbg(3), self.reg_dbg(4), self.reg_dbg(5),
                    self.reg_dbg(6), self.sp
                ));
            }
        }
        self.last_pc = self.pc;

        // SPL 自己的代码区被写 = U-Boot 载荷开始盖住 SPL。真机 CPU 的 cache 里
        // 仍是旧 SPL 字节，SPL 照常执行到打印 "Jumping to U-Boot" 才跳转。
        // 这里开启缓存语义 overlay（SPL 区读旧字节、写真内存），继续模拟。
        if self.code_lo != self.code_hi {
            let last = self.mem.watch_log.last().copied();
            if let Some((_, a)) = last {
                if a >= self.code_lo && a < self.code_hi && !self.mem.ov_on {
                    self.mem.ov_on = true;
                    self.handoff_addr = Some(a);
                    self.printf_log.push(format!(
                        "(SPL+{:#x} 把载荷写入自身代码区 {a:#x}：开启缓存语义，SPL 继续执行)",
                        self.pc.wrapping_sub(self.text_base)
                    ));
                }
            }
        }

        // 写监视命中后（下一步进来时）把 PC 环快照下来
        if self.mem.watch_log.len() != self.watch_seen {
            self.watch_seen = self.mem.watch_log.len();
            if self.watch_snap.len() < 4 {
                let n = self.recent_pcs.len();
                let mut v = Vec::with_capacity(n);
                for k in 0..n {
                    let pc = self.recent_pcs[(self.recent_i + k) % n];
                    if pc != 0 {
                        v.push(pc.wrapping_sub(self.text_base));
                    }
                }
                self.watch_snap.push(v);
            }
        }

        // 返回到哨兵 → 被调函数正常结束
        if self.sentinel_ret != 0 && self.pc == self.sentinel_ret {
            self.stop = Some(Stop::Returned(self.reg(0) as i64));
            return Ok(());
        }

        // memcpy 入口：只在它的目的区间会覆盖被监视地址时记一笔
        if self.hook_memcpy != 0 && self.pc == self.hook_memcpy {
            let (dst, src, len) = (self.reg(0), self.reg(1), self.reg(2));
            // 只看"目的地址落在 SPL 镜像里"的调用 —— 正常启动流程里这种调用应该一次都没有，
            // 因为 SPL 代码/只读数据不该被 memcpy 覆盖。
            // 注意别用 dst+len 做区间判断：len 可能是垃圾大数，加完回绕反而判不出来。
            self.memcpy_calls += 1;
            if self.memcpy_log.len() < 64 {
                self.memcpy_log.push((self.reg(30), dst, src, len));
            }
            let _ = self.memcpy_log_lo;
            // 不接管，照常执行真 memcpy
        }

        // hw_fit_calculate_hash 桩。
        //
        // 参数契约来自 T20Pro SPL+0x4f20 的反汇编，并且**每一项都实测对上了**：
        //   x0 = 待哈希数据的指针
        //        依据：实测 x0=0x240000，与该镜像 `## Checking … @0x00240000` 的加载地址一致；
        //        且 0x4f3c `mov x22,x0` → 0x4fc8 `mov x2,x22` 原样传给 bl 0xf904。
        //   w1 = 数据长度
        //        依据：实测 w1=0x3203c，与 FIT 里 images/atf-1 的 data-size=0x3203c 完全相同。
        //        （注意 `## Checking atf-1 0x00040000` 打印的是 load=0x40000，不是 size，别搞混。）
        //   x2 = 算法名字符串（0x4f4c `mov x0,x2`; 0x4f54 `bl 0x7ad8` 与 "sha256" 比较）
        //   x3 = 摘要输出缓冲（0x4f50 `mov x23,x3`）
        //   x4 = &out_len（0x4f44 `mov x21,x4`）
        //
        // 复刻真实行为的两点细节：
        //   0x4f58 `cbnz w0,0x4fdc` → 算法名不等于 "sha256" 时 w19=-1，**摘要一个字都不写**；
        //   0x4f78 `mov w0,#0x20; str w0,[x21]` → *out_len=32 是**无条件**写的，失败路径也写。
        //     调用者 0x5468 `ldp w2,w0,[x29,#0x58]` + 0x546c `cmp w2,w0` 就是拿它和
        //     FIT 里 hash/value 的长度比，不等就打印 "Bad hash value len"。
        //     早期版本漏了这个写，SPL 当场就报了这句 —— 这条路径是被真实执行验证过的。
        //
        // 摘要本身用系统 libcrypto 算（见 sha256.rs），不是这里手写的。
        // 所以 SPL 自己那步 memcmp(算出的, hash/value) 依然是真裁判：
        // 镜像被改而 hash 没同步，这里照样会失败。
        if self.hook_hash != 0 && self.pc == self.hook_hash {
            self.hash_calls += 1;
            let data = self.reg(0);
            let size = self.reg(1) as u32 as u64;
            let algo_name = self.cstr(self.reg(2));
            let out_buf = self.reg(3);
            let out_len_ptr = self.reg(4);

            // *out_len = 32，无条件（对齐 0x4f7c）
            let _ = self.mem.write(out_len_ptr, 4, crate::splsim::sha256::SHA256_LEN as u64);

            let ret: i64 = if algo_name == "sha256" {
                match self.mem.read_bytes_live(data, size as usize) {
                    Ok(bytes) => {
                        let digest = crate::splsim::sha256::sha256(&bytes);
                        self.last_digest = Some(digest);
                        let _ = self.mem.write_bytes(out_buf, &digest);
                        0
                    }
                    Err(e) => {
                        // 数据读不出来 = 长度/指针不对，这本身就是有价值的失败信号
                        if self.hash_debug {
                            self.printf_log.push(format!(
                                "HASHSTUB 读取失败 data={data:#x} size={size:#x}: {e:?}"
                            ));
                        }
                        -1
                    }
                }
            } else {
                -1 // 对齐 0x4fdc：算法名不认
            };

            if self.hash_debug {
                // 调用者帧里的"期望摘要"（0x541c/0x5420 把 &out_len、&ptr 传给 bl 0x530c，
                // 由 fdt_getprop(fit, noffset, "value", &len) 填）。
                // 此时被调序言未执行，x29 还是调用者的帧指针。
                let fx29 = self.reg(29);
                let exp_ptr = self.mem.read(fx29 + 0x68, 8).unwrap_or(0);
                let exp_len = self.mem.read(fx29 + 0x5c, 4).unwrap_or(0);
                let exp = self
                    .mem
                    .read_bytes(exp_ptr, 32)
                    .map(|v| crate::splsim::sha256::hex(&v))
                    .unwrap_or_else(|_| "<读不到>".into());
                self.printf_log.push(format!(
                    "HASHSTUB data={data:#x} size={size:#x} algo=\"{algo_name}\" \
                     out={out_buf:#x} ret={ret}\n\
                     \x20        算出 = {}\n\
                     \x20        期望 = {exp}  (ptr={exp_ptr:#x} len={exp_len})  x29={fx29:#x}\n\
                     \x20        调用点 = SPL+{:#x}\n\
                     \x20        进桩前 PC = {}",
                    crate::splsim::sha256::hex(&self.last_digest.unwrap_or([0u8; 32])),
                    self.reg(30).wrapping_sub(self.text_base),
                    {
                        let n = self.recent_pcs.len();
                        let mut v = String::new();
                        for k in 0..n {
                            let pc = self.recent_pcs[(self.recent_i + k) % n];
                            if pc != 0 {
                                v.push_str(&format!("SPL+{:#x} ", pc.wrapping_sub(self.text_base)));
                            }
                        }
                        v
                    }
                ));
            }
            self.set_reg(0, ret as u64);
            self.pc = self.reg(30);
            return Ok(());
        }

        // verified-boot 门禁桩：按反汇编返回 (byte == 0xff)；默认 byte=0x00 => 0
        if self.hook_vb_gate != 0 && self.pc == self.hook_vb_gate {
            self.vb_gate_calls += 1;
            self.set_reg(0, u64::from(self.vb_flag_byte == 0xff));
            self.pc = self.reg(30);
            return Ok(());
        }

        // printf 桩：控制台没初始化，真执行会 blr 到空的输出函数指针然后跑进数据区。
        // 顺手把格式串和参数抓下来 —— SPL 自己的报错比反推强得多。
        if self.hook_printf != 0 && self.pc == self.hook_printf {
            self.printf_calls += 1;
            if self.printf_capture {
                let fmt_addr = self.reg(0);
                let mut fmt = String::new();
                if fmt_addr != 0 {
                    for i in 0..160u64 {
                        match self.mem.read_u8(fmt_addr + i) {
                            Ok(0) | Err(_) => break,
                            Ok(c) => fmt.push(c as char),
                        }
                    }
                }
                if fmt.contains("Jumping to U-Boot") {
                    self.saw_jumping = true;
                }
                // 按 % 转换的个数依次取 x1..x7
                let nargs = fmt.matches('%').filter(|_| true).count();
                let mut args: Vec<String> = Vec::new();
                for k in 0..nargs.min(7) {
                    let v = self.reg((k + 1) as u8);
                    // 既给十六进制也给可打印字符串（%s 的情况）
                    let as_str: String = (0..48)
                        .map(|i| self.mem.read_u8(v + i).unwrap_or(0))
                        .take_while(|&c| c != 0)
                        .map(|c| if (0x20..0x7f).contains(&c) { c as char } else { '?' })
                        .collect();
                    let printable = as_str.len() >= 3
                        && as_str.chars().filter(|c| *c == '?').count() == 0;
                    args.push(if printable {
                        // 不用嵌套引号的 format!，那样解析不了
                        let mut t = format!("{v:#x}=");
                        t.push('"');
                        t.push_str(&as_str);
                        t.push('"');
                        t
                    } else {
                        format!("{v:#x}")
                    });
                }
                // 带上调用点（printf 入口时 x30 还是返回地址），
                // 这样每条日志都能映射回 SPL 里的具体代码地址，不用靠字符串猜。
                self.printf_log.push(format!(
                    "{fmt} | {}  <SPL+{:#x}>",
                    args.join(", "),
                    self.reg(30).wrapping_sub(self.text_base)
                ));
            }
            self.pc = self.reg(30);
            return Ok(());
        }

        // info->read 桩：SPL 调它读 flash，这里由模拟器接管
        if self.hook_read != 0 && self.pc == self.hook_read {
            self.read_calls += 1;
            let sector = self.reg(1);
            let count = self.reg(2);
            let buf = self.reg(3);
            let nbytes = (count * 512) as usize;
            let off = sector
                .wrapping_sub(self.flash_base_sector)
                .wrapping_mul(512) as usize;
            let mut done = 0u64;
            if off < self.flash.len() {
                let avail = (self.flash.len() - off).min(nbytes);
                let data = self.flash[off..off + avail].to_vec();
                self.mem.load_region(buf, &data)?;
                done = (avail / 512) as u64;
            }
            self.set_reg(0, done); // 返回读到的扇区数
            self.pc = self.reg(30); // 相当于 ret
            return Ok(());
        }

        let word = self.mem.read(self.pc, 4)? as u32;
        let d = decode(word, self.pc);
        let next = self.pc.wrapping_add(4);
        self.exec(&d, next)?;
        Ok(())
    }

    fn exec(&mut self, d: &D, next: u64) -> Result<(), MemError> {
        let bits = if d.w32 { 32u32 } else { 64 };
        match d.k {
            K::Nop | K::Hint | K::VecLdSt | K::Sys | K::Wfi | K::Bad => {
                self.pc = next;
            }

            // ---------------- 分支 ----------------
            K::B => self.pc = d.imm as u64,
            K::Bl => {
                self.set_reg(30, next);
                self.pc = d.imm as u64;
            }
            K::Bc => {
                self.pc = if cond_holds(d.cond, self.n, self.z, self.c, self.v) {
                    d.imm as u64
                } else {
                    next
                }
            }
            K::Cbz => {
                let t = self.trunc(self.reg(d.rd), d.w32);
                self.pc = if t == 0 { d.imm as u64 } else { next };
            }
            K::Cbnz => {
                let t = self.trunc(self.reg(d.rd), d.w32);
                self.pc = if t != 0 { d.imm as u64 } else { next };
            }
            K::Tbz => {
                let t = self.reg(d.rd) >> (d.amt as u32) & 1;
                self.pc = if t == 0 { d.imm as u64 } else { next };
            }
            K::Tbnz => {
                let t = self.reg(d.rd) >> (d.amt as u32) & 1;
                self.pc = if t != 0 { d.imm as u64 } else { next };
            }
            K::Br => {
                let t = self.reg(d.rn);
                self.jump_target = Some(t);
                // "Jumping to U-Boot" 打印之后的间接跳转 = SPL 全流程终点。
                // 目标未必在 SPL 代码区：RK3588 系 FIT 的 firmware 节点是 atf-1
                // （BL31@0x40000），uboot 只是 loadable —— SPL 跳的是 ATF，
                // 之后由 ATF 再跳 U-Boot。打印本身就是"软件自洽走完"的证据。
                if self.saw_jumping {
                    self.stop = Some(Stop::JumpUBoot(t));
                    return Ok(());
                }
                self.pc = t;
            }
            K::Blr => {
                self.set_reg(30, next);
                let t = self.reg(d.rn);
                self.jump_target = Some(t);
                if self.saw_jumping {
                    self.stop = Some(Stop::JumpUBoot(t));
                    return Ok(());
                }
                self.pc = t;
            }
            K::Ret => {
                let t = if d.rn == 31 { self.reg(30) } else { self.reg(d.rn) };
                self.pc = t;
            }

            // ---------------- PC 相对 ----------------
            K::Adr | K::Adrp => {
                // 译码器已经把结果地址算好放在 imm
                self.set_reg(d.rd, d.imm as u64);
                self.pc = next;
            }

            // ---------------- 立即数搬移 ----------------
            K::Movz => {
                let v = (d.imm as u64) << (d.amt as u32);
                self.set_reg(d.rd, self.trunc(v, d.w32));
                self.pc = next;
            }
            K::Movn => {
                let v = !((d.imm as u64) << (d.amt as u32));
                self.set_reg(d.rd, self.trunc(v, d.w32));
                self.pc = next;
            }
            K::Movk => {
                // 中间值按 64 位算完再截断
                // d.amt 已是实际位移位数（译码器算过 hw*16），这里不要再乘
                let sh = d.amt as u32;
                let cur = self.reg(d.rd);
                let v = (cur & !(0xffffu64 << sh)) | ((d.imm as u64) << sh);
                self.set_reg(d.rd, self.trunc(v, d.w32));
                self.pc = next;
            }

            // ---------------- 逻辑（立即数） ----------------
            K::LogImm | K::LogImmS => {
                // 译码器已经算好掩码放在 d.bitmask，这里不要重算
                let mask = d.bitmask;
                let a = self.reg(d.rn);
                // d.imm 在 LogImm 里是 opc：1=orr 2=eor 其它=and
                let r = match d.imm {
                    1 => a | mask,
                    2 => a ^ mask,
                    _ => a & mask,
                };
                if d.k == K::LogImmS {
                    self.flags_logic(r, bits);
                }
                self.set_reg(d.rd, self.trunc(r, d.w32));
                self.pc = next;
            }

            // ---------------- 逻辑（移位寄存器） ----------------
            K::LogSh | K::LogShS => {
                let a = self.reg(d.rn);
                let b = self.operand2(d);
                let neg = d.imm & 1 == 1;
                let r = match (d.imm >> 1, neg) {
                    (0, true) => a & !b,  // BIC
                    (1, true) => a | !b,  // ORN
                    (2, true) => a ^ !b,  // EON
                    (1, false) => a | b,  // ORR
                    (2, false) => a ^ b,  // EOR
                    _ => a & b,           // AND
                };
                if d.k == K::LogShS {
                    self.flags_logic(r, bits);
                }
                self.set_reg(d.rd, self.trunc(r, d.w32));
                self.pc = next;
            }

            // ---------------- 加减（立即数） ----------------
            K::AddImm | K::SubImm => {
                let a = self.reg_sp(d.rn);
                let b = d.imm as u64;
                if d.k == K::AddImm {
                    let r = self.trunc(a.wrapping_add(b), d.w32);
                    if d.flags {
                        self.flags_add(a, b, r, bits, false);
                    }
                    self.set_reg(d.rd, r);
                } else {
                    let r = self.trunc(a.wrapping_sub(b), d.w32);
                    if d.flags {
                        self.flags_sub(a, b, r, bits, false);
                    }
                    self.set_reg(d.rd, r);
                }
                self.pc = next;
            }

            // ---------------- 加减（移位/扩展寄存器） ----------------
            K::AddSh | K::AddExt | K::SubSh | K::SubExt => {
                let a = self.reg(d.rn);
                let b = self.operand2(d);
                let is_sub = matches!(d.k, K::SubSh | K::SubExt);
                let r = if is_sub {
                    self.trunc(a.wrapping_sub(b), d.w32)
                } else {
                    self.trunc(a.wrapping_add(b), d.w32)
                };
                if d.flags {
                    if is_sub {
                        self.flags_sub(a, b, r, bits, false);
                    } else {
                        self.flags_add(a, b, r, bits, false);
                    }
                }
                self.set_reg(d.rd, r);
                self.pc = next;
            }

            // ---------------- 带进位加减 ----------------
            K::AddCarry | K::SubCarry => {
                let a = self.reg(d.rn);
                let b = self.reg(d.rm);
                let cin = self.c;
                let r = if d.k == K::AddCarry {
                    self.trunc(a.wrapping_add(b).wrapping_add(cin as u64), d.w32)
                } else {
                    self.trunc(a.wrapping_sub(b).wrapping_sub(!cin as u64), d.w32)
                };
                if d.flags {
                    if d.k == K::AddCarry {
                        self.flags_add(a, b, r, bits, cin);
                    } else {
                        self.flags_sub(a, b, r, bits, !cin);
                    }
                }
                self.set_reg(d.rd, r);
                self.pc = next;
            }

            // ---------------- 条件比较 ----------------
            K::Ccmp | K::Ccmn => {
                let (n_, z_, c_, v_) = (self.n, self.z, self.c, self.v);
                if cond_holds(d.cond, n_, z_, c_, v_) {
                    let a = self.reg(d.rn);
                    // 第二操作数是 imm5 还是 Rm 由译码器明确标出。
                    // 早先用 `d.rm == 0 && d.imm != 0` 这种启发式判断，
                    // 当 imm5 恰好为 0（如 `ccmp w0, 0, 4, ne`）时退化成
                    // `cmp w0, x0` —— 自己和自己比，Z 恒为 1，条件分支全错。
                    let b = if d.cc_imm {
                        d.imm as u64
                    } else {
                        self.reg(d.rm)
                    };
                    if d.k == K::Ccmp {
                        let r = self.trunc(a.wrapping_sub(b), d.w32);
                        self.flags_sub(a, b, r, bits, false);
                    } else {
                        let r = self.trunc(a.wrapping_add(b), d.w32);
                        self.flags_add(a, b, r, bits, false);
                    }
                } else {
                    let nzcv = d.imm2 as u8;
                    self.n = nzcv & 8 != 0;
                    self.z = nzcv & 4 != 0;
                    self.c = nzcv & 2 != 0;
                    self.v = nzcv & 1 != 0;
                }
                self.pc = next;
            }

            // ---------------- 条件选择 ----------------
            K::Csel | K::Csinc | K::Csinv | K::Csneg => {
                let taken = cond_holds(d.cond, self.n, self.z, self.c, self.v);
                let a = self.reg(d.rn);
                let b = self.reg(d.rm);
                let r = match d.k {
                    K::Csel => {
                        if taken {
                            a
                        } else {
                            b
                        }
                    }
                    K::Csinc => {
                        if taken {
                            a
                        } else {
                            b.wrapping_add(1)
                        }
                    }
                    K::Csinv => {
                        if taken {
                            a
                        } else {
                            !b
                        }
                    }
                    _ => {
                        if taken {
                            a
                        } else {
                            (!b).wrapping_add(1)
                        }
                    }
                };
                self.set_reg(d.rd, self.trunc(r, d.w32));
                self.pc = next;
            }

            // ---------------- 位域 ----------------
            K::Ubfm | K::Sbfm | K::Bfm => {
                let kind = match d.k {
                    K::Ubfm => 0,
                    K::Sbfm => 1,
                    _ => 2,
                };
                let r = self.bitfield(d, kind);
                self.set_reg(d.rd, self.trunc(r, d.w32));
                self.pc = next;
            }
            K::Extr => {
                let m = if d.w32 { 0xffff_ffffu64 } else { u64::MAX };
                let (hi, lo) = (self.reg(d.rn) & m, self.reg(d.rm) & m);
                let lsb = d.amt as u32;
                let r = if d.w32 {
                    ((hi << 32 | lo) >> lsb) & m
                } else {
                    let lsb = lsb as u64;
                    if lsb == 0 {
                        lo
                    } else {
                        (lo >> lsb) | (hi << (64 - lsb))
                    }
                };
                self.set_reg(d.rd, r);
                self.pc = next;
            }

            // ---------------- 单源 ----------------
            K::Rbit => {
                let v = self.reg(d.rn);
                let r = if d.w32 {
                    (v as u32).reverse_bits() as u64
                } else {
                    v.reverse_bits()
                };
                self.set_reg(d.rd, r);
                self.pc = next;
            }
            K::Rev16 => {
                let v = self.reg(d.rn);
                let r = if d.w32 {
                    (v as u32).swap_bytes() as u64
                } else {
                    v.swap_bytes()
                };
                // rev16 是按 16 位字节序反转
                let r = if d.w32 {
                    ((r & 0x00ff_00ff) >> 8) | ((r & 0xff00_ff00) << 8)
                } else {
                    ((r & 0x00ff_00ff_00ff_00ff) >> 8) | ((r & 0xff00_ff00_ff00_ff00) << 8)
                };
                self.set_reg(d.rd, self.trunc(r, d.w32));
                self.pc = next;
            }
            K::Rev32 => {
                let v = self.reg(d.rn);
                let r = if d.w32 {
                    (v as u32).swap_bytes() as u64
                } else {
                    ((v & 0xffff_ffff).swap_bytes() as u64) << 32
                        | ((v >> 32).swap_bytes() as u64)
                };
                self.set_reg(d.rd, r);
                self.pc = next;
            }
            K::Clz => {
                let v = if d.w32 {
                    self.reg(d.rn) as u32
                } else {
                    0
                };
                let r = if d.w32 {
                    v.leading_zeros() as u64
                } else {
                    self.reg(d.rn).leading_zeros() as u64
                };
                self.set_reg(d.rd, r);
                self.pc = next;
            }
            K::Cls => {
                let r = if d.w32 {
                    let v = self.reg(d.rn) as u32;
                    (v ^ (v >> 31).wrapping_neg()).leading_zeros() as u64 - 1
                } else {
                    let v = self.reg(d.rn);
                    (v ^ (v >> 63).wrapping_neg()).leading_zeros() as u64 - 1
                };
                self.set_reg(d.rd, r);
                self.pc = next;
            }

            // ---------------- 乘除 ----------------
            K::Madd | K::Mul => {
                let a = self.reg(d.rn);
                let b = self.reg(d.rm);
                let acc = self.reg(d.ra);
                let r = acc.wrapping_add(a.wrapping_mul(b));
                self.set_reg(d.rd, self.trunc(r, d.w32));
                self.pc = next;
            }
            K::Msub => {
                let a = self.reg(d.rn);
                let b = self.reg(d.rm);
                let acc = self.reg(d.ra);
                let r = acc.wrapping_sub(a.wrapping_mul(b));
                self.set_reg(d.rd, self.trunc(r, d.w32));
                self.pc = next;
            }
            K::Smaddl | K::Umaddl => {
                let p: u64 = if d.k == K::Smaddl {
                    (((self.reg(d.rn) as u32) as i32 as i64 as i128)
                        * ((self.reg(d.rm) as u32) as i32 as i64 as i128)) as u64
                } else {
                    (self.reg(d.rn) as u32) as u64 * (self.reg(d.rm) as u32) as u64
                };
                let r = self.reg(d.ra).wrapping_add(p as u64);
                self.set_reg(d.rd, r);
                self.pc = next;
            }
            K::Smsubl | K::Umsubl => {
                let p: u64 = if d.k == K::Smsubl {
                    (((self.reg(d.rn) as u32) as i32 as i64 as i128)
                        * ((self.reg(d.rm) as u32) as i32 as i64 as i128)) as u64
                } else {
                    (self.reg(d.rn) as u32) as u64 * (self.reg(d.rm) as u32) as u64
                };
                let r = self.reg(d.ra).wrapping_sub(p as u64);
                self.set_reg(d.rd, r);
                self.pc = next;
            }
            K::SMulH => {
                let a = self.reg(d.rn) as i64 as i128;
                let b = self.reg(d.rm) as i64 as i128;
                self.set_reg(d.rd, ((a * b) >> 64) as u64);
                self.pc = next;
            }
            K::UMulH => {
                let a = self.reg(d.rn) as u128;
                let b = self.reg(d.rm) as u128;
                self.set_reg(d.rd, ((a * b) >> 64) as u64);
                self.pc = next;
            }
            K::SDiv => {
                let a = self.trunc(self.reg(d.rn), d.w32);
                let b = self.trunc(self.reg(d.rm), d.w32);
                let r = if b == 0 {
                    0
                } else if d.w32 {
                    ((a as u32) as i32).wrapping_div((b as u32) as i32) as u32 as u64
                } else {
                    ((a as i64).wrapping_div(b as i64)) as u64
                };
                self.set_reg(d.rd, self.trunc(r, d.w32));
                self.pc = next;
            }
            K::UDiv => {
                let a = self.trunc(self.reg(d.rn), d.w32);
                let b = self.trunc(self.reg(d.rm), d.w32);
                let r = if b == 0 { 0 } else { a / b };
                self.set_reg(d.rd, self.trunc(r, d.w32));
                self.pc = next;
            }
            K::Lslv | K::Lsrv | K::Asrv | K::Rorv => {
                let v = self.trunc(self.reg(d.rn), d.w32);
                let sh = self.trunc(self.reg(d.rm), d.w32) % bits as u64;
                let r = match d.k {
                    K::Lslv => v << sh,
                    K::Lsrv => v >> sh,
                    K::Asrv => {
                        if d.w32 {
                            ((v as u32) as i32 >> sh) as u32 as u64
                        } else {
                            ((v as i64) >> sh) as u64
                        }
                    }
                    _ => Self::rotate_right(v, sh, bits),
                };
                self.set_reg(d.rd, self.trunc(r, d.w32));
                self.pc = next;
            }

            // ---------------- 访存 ----------------
            K::Ldr | K::LdrLit | K::Str => {
                let (addr, wb) = if d.k == K::LdrLit {
                    (d.imm as u64, self.reg_sp(d.rn))
                } else {
                    self.resolve(d)
                };
                if d.k == K::Str {
                    self.mem.write(addr, d.size as usize, self.reg(d.rd))?;
                } else {
                    let raw = self.mem.read(addr, d.size as usize)?;
                    let v = if d.sext {
                        // 符号扩展到 64 位（ldrsw / ldrsb / ldrsh）
                        let sh = 64 - d.size as u32 * 8;
                        ((raw << sh) as i64 >> sh) as u64
                    } else if d.size == 4 && d.w32 {
                        raw
                    } else {
                        raw
                    };
                    self.set_reg(d.rd, v);
                }
                // 只有 pre/post-index 才写回基址。offset 形式写回是无意义的，
                // 而且当 Rt == Rn（如 `ldr x0, [x0, #0x18]`，FDT/链表遍历里极常见）
                // 时会把刚加载的值覆盖成基址 —— 实测就是这个 bug 让
                // fdt_check_header 读到 last_comp_version = 0x1ff3c0 而返回 −10。
                // 只有 pre/post-index 才写回基址。Offset 和 Reg（寄存器变址）都不写回 ——
                // 漏掉 Reg 会让 `ldrb w0, [x0, w1, uxtw]`（Rt==Rn）的加载结果
                // 被基址覆盖，libfdt 跳表下标就读错了。
                if matches!(d.addr, Addr::Pre(_) | Addr::Post(_)) {
                    self.set_reg_sp(d.rn, wb);
                }
                self.pc = next;
            }
            K::Ldp | K::Stp => {
                let (addr, wb) = self.resolve(d);
                if d.k == K::Stp {
                    self.mem.write(addr, d.size as usize, self.reg(d.rd))?;
                    self.mem
                        .write(addr + d.size as u64, d.size as usize, self.reg(d.rt2))?;
                } else {
                    let a = self.mem.read(addr, d.size as usize)?;
                    let b = self.mem.read(addr + d.size as u64, d.size as usize)?;
                    let (a, b) = if d.sext {
                        let sh = 64 - d.size as u32 * 8;
                        (((a << sh) as i64 >> sh) as u64, ((b << sh) as i64 >> sh) as u64)
                    } else {
                        (a, b)
                    };
                    self.set_reg(d.rd, a);
                    self.set_reg(d.rt2, b);
                }
                // 只有 pre/post-index 才写回基址。Offset 和 Reg（寄存器变址）都不写回 ——
                // 漏掉 Reg 会让 `ldrb w0, [x0, w1, uxtw]`（Rt==Rn）的加载结果
                // 被基址覆盖，libfdt 跳表下标就读错了。
                if matches!(d.addr, Addr::Pre(_) | Addr::Post(_)) {
                    self.set_reg_sp(d.rn, wb);
                }
                self.pc = next;
            }
            K::Ldxr | K::Stxr => {
                let (addr, wb) = self.resolve(d);
                if d.k == K::Stxr {
                    self.mem.write(addr, d.size as usize, self.reg(d.rd))?;
                    self.set_reg(d.rm, 0); // 状态寄存器：0 = 成功
                } else {
                    let v = self.mem.read(addr, d.size as usize)?;
                    self.set_reg(d.rd, v);
                }
                // 只有 pre/post-index 才写回基址。Offset 和 Reg（寄存器变址）都不写回 ——
                // 漏掉 Reg 会让 `ldrb w0, [x0, w1, uxtw]`（Rt==Rn）的加载结果
                // 被基址覆盖，libfdt 跳表下标就读错了。
                if matches!(d.addr, Addr::Pre(_) | Addr::Post(_)) {
                    self.set_reg_sp(d.rn, wb);
                }
                self.pc = next;
            }
            K::FmovGpr => {
                self.pc = next;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 把指令序列装进内存并从 entry 跑 n 步
    fn run(words: &[u32], n: usize, entry: u64) -> Cpu {
        let mut mem = Mem::new();
        let code: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
        mem.load_region(entry, &code).unwrap();
        let mut cpu = Cpu::new(mem, entry);
        for _ in 0..n {
            if cpu.stop.is_some() {
                break;
            }
            cpu.step().unwrap();
        }
        cpu
    }

    const E: u64 = 0x20_0000;

    // 下面所有指令编码都不是手写的：要么由 rasm2 汇编生成、要么直接取自三份 SPL，
    // 并且每一条都用 radare2 反汇编回读确认过。
    // （手写编码错过 8 处，例如把 `mov sp,#0x400` 写成 0x910003fe，
    //   那其实是 `mov x30, sp`。）

    /// add x0, x1, 0x10 ; sub x2, x0, 4
    #[test]
    fn add_sub_immediate() {
        let c = run(&[0x91004020, 0xd1001002], 2, E);
        assert_eq!(c.x[0], 0x10);
        assert_eq!(c.x[2], 0x0c);
    }

    /// mov x0, 0x12340000 ; movk x0, 0x5678  → 0x12345678
    #[test]
    fn movz_movk_compose_constant() {
        let c = run(&[0xd2a24680, 0xf28acf00], 2, E);
        assert_eq!(c.x[0], 0x1234_5678);
    }

    /// mov x0, -1 ; add w1, w0, 1  → W 形式必须截 32 位，w1 = 0
    #[test]
    fn w_form_truncates() {
        let c = run(&[0x92800000, 0x11000401], 2, E);
        assert_eq!(c.x[0], u64::MAX);
        assert_eq!(c.x[1], 0);
    }

    /// 拼出 0x7fffffffffffffff 再 +1 → 有符号溢出，V 必须为 1。
    /// 序列：movz x0,0x7fff,lsl 48 ; movk x0,0xffff,lsl 32 ; movk x0,0xffff,lsl 16 ;
    ///       movk x0,0xffff ; adds x1,x0,1
    /// Python 版从来没算过 V，所有 b.ge/b.lt/b.gt/b.le 全错。
    #[test]
    fn overflow_flag_for_signed_compare() {
        let c = run(
            &[0xd2efffe0, 0xf2dfffe0, 0xf2bfffe0, 0xf29fffe0, 0xb1000401],
            5,
            E,
        );
        assert_eq!(c.x[0], 0x7fff_ffff_ffff_ffff);
        assert_eq!(c.x[1], 0x8000_0000_0000_0000);
        assert!(c.n, "N 应为 1");
        assert!(c.v, "V 应为 1（有符号溢出）");
        assert!(!c.z);
        // b.ge 的条件是 N==V，这里 N==V==true → 判定“大于等于”
        assert!(cond_holds(0xa, c.n, c.z, c.c, c.v), "GE 应成立");
        assert!(!cond_holds(0xb, c.n, c.z, c.c, c.v), "LT 不应成立");
    }

    /// 无符号比较：cmp + b.hi / b.ls
    #[test]
    fn unsigned_compare_flags() {
        let c = run(&[0xd28000a0, 0xf1000c1f], 2, E); // mov x0,5 ; cmp x0,3
        assert!(c.c, "5 > 3 无符号，C 应为 1（无借位）");
        assert!(!c.z);
        assert!(cond_holds(0x8, c.n, c.z, c.c, c.v), "HI 应成立");

        let c = run(&[0xd2800040, 0xf1000c1f], 2, E); // mov x0,2 ; cmp x0,3
        assert!(!c.c, "2 < 3 无符号，C 应为 0（有借位）");
        assert!(cond_holds(0x9, c.n, c.z, c.c, c.v), "LS 应成立");
    }

    /// 变址寻址必须把变址寄存器加进去 —— Python 版漏了，837 条访存全错
    #[test]
    fn indexed_addressing_adds_index_register() {
        // mov x1, 0x100 ; mov x2, 0x10 ; ldrb w0, [x1, w2, uxtw]
        let mut mem = Mem::new();
        mem.write_u8(0x110, 0xab).unwrap();
        mem.load_region(
            E,
            &[0xd2802001u32, 0xd2800202, 0x38624820]
                .iter()
                .flat_map(|w| w.to_le_bytes())
                .collect::<Vec<u8>>(),
        )
        .unwrap();
        let mut cpu = Cpu::new(mem, E);
        for _ in 0..3 {
            cpu.step().unwrap();
        }
        assert_eq!(
            cpu.x[0], 0xab,
            "应读 [0x100 + 0x10] = 0x110，而不是 [0x100]"
        );
    }

    /// stp 的 offset 形式不能写回基址（Python 版把 sp 写坏过）。
    /// 0xa9050be0 = `stp x0, x2, [sp, 0x50]`，取自真实 SPL。
    #[test]
    fn stp_offset_form_does_not_writeback() {
        let mut cpu = run(&[0xa9050be0], 1, E);
        cpu.sp = 0x300;
        cpu.x[0] = 0x1111;
        cpu.x[2] = 0x2222;
        cpu.pc = E;
        cpu.steps = 0;
        cpu.step().unwrap();
        assert_eq!(cpu.sp, 0x300, "offset 形式不能改基址");
        assert_eq!(cpu.mem.read(0x350, 8).unwrap(), 0x1111);
        assert_eq!(cpu.mem.read(0x358, 8).unwrap(), 0x2222);
    }

    /// pre-index 先写回再访存，post-index 先访存再写回。
    /// 0xa9bf7bfd = `stp x29, x30, [sp, -0x10]!`
    /// 0xa8c17bfd = `ldp x29, x30, [sp], 0x10`
    /// 两条都取自真实 SPL。
    #[test]
    fn pre_and_post_index_writeback() {
        let mut cpu = run(&[0xa9bf7bfd, 0xa8c17bfd], 0, E);
        cpu.sp = 0x400;
        cpu.x[29] = 0xaaaa;
        cpu.x[30] = 0xbbbb;
        cpu.x[0] = 0;

        cpu.step().unwrap(); // pre-index stp
        assert_eq!(cpu.sp, 0x3f0, "pre-index 应把 sp 减 0x10");
        assert_eq!(cpu.mem.read(0x3f0, 8).unwrap(), 0xaaaa, "数据应写在新的 sp");
        assert_eq!(cpu.mem.read(0x3f8, 8).unwrap(), 0xbbbb);

        cpu.x[29] = 0;
        cpu.x[30] = 0;
        cpu.step().unwrap(); // post-index ldp
        assert_eq!(cpu.x[29], 0xaaaa, "应从旧的 sp(0x3f0) 读");
        assert_eq!(cpu.x[30], 0xbbbb);
        assert_eq!(cpu.sp, 0x400, "post-index 读完才把 sp 加回 0x10");
    }

    /// bl 存返回地址，ret 跳回
    #[test]
    fn bl_and_ret_roundtrip() {
        // bl 8 ; mov x0,0xff ; mov x1,7 ; ret
        // 跑 3 步：bl → mov x1,7 → ret，此时 pc 刚跳回 E+4 还没执行那条指令
        let c = run(&[0x94000002, 0xd2801fe0, 0xd28000e1, 0xd65f03c0], 3, E);
        assert_eq!(c.x[30], E + 4, "bl 应把返回地址存进 x30");
        assert_eq!(c.x[1], 7);
        assert_eq!(c.pc, E + 4, "ret 应跳回 bl 之后");
    }

    /// 条件分支按标志走
    #[test]
    fn conditional_branch_takes_correct_path() {
        // mov x0,5 ; cmp x0,5 ; b.eq 0x14 ; mov x1,1 ; b 0x18 ; mov x1,2
        let c = run(
            &[0xd28000a0, 0xf100141f, 0x54000060, 0xd2800021, 0x14000002, 0xd2800041],
            4,
            E,
        );
        assert_eq!(c.x[1], 2, "相等时应跳到 0x14 执行 mov x1,2");
    }

    /// tbz：位为 1 时不跳
    #[test]
    fn tbz_bit_set_falls_through() {
        // mov x0,8 ; tbz w0,3,<跳过下一条> ; mov x1,0xaa
        let c = run(&[0xd2800100, 0x36180060, 0xd2801541], 3, E);
        assert_eq!(c.x[1], 0xaa, "bit3=1，tbz 不跳，应执行 mov x1,0xaa");
    }

    /// tbz：位为 0 时跳走
    #[test]
    fn tbz_bit_clear_takes_branch() {
        let c = run(&[0xd2800000, 0x36180060, 0xd2801541], 3, E);
        assert_eq!(c.x[1], 0, "bit3=0，tbz 应跳过 mov，x1 保持 0");
    }

    /// tbnz：位为 0 时不跳
    #[test]
    fn tbnz_bit_clear_falls_through() {
        let c = run(&[0xd2800000, 0x37180060, 0xd2801541], 3, E);
        assert_eq!(c.x[1], 0xaa);
    }

    /// tbnz：位为 1 时跳走
    #[test]
    fn tbnz_bit_set_takes_branch() {
        let c = run(&[0xd2800100, 0x37180060, 0xd2801541], 3, E);
        assert_eq!(c.x[1], 0);
    }

    /// adrp 给页基址。0x90000000 = `adrp x0, 0`（当前页）
    #[test]
    fn adrp_is_page_granular() {
        let mut mem = Mem::new();
        mem.load_region(0x20_0100, &0x90000000u32.to_le_bytes()).unwrap();
        let mut cpu = Cpu::new(mem, 0x20_0100);
        cpu.step().unwrap();
        assert_eq!(cpu.x[0], 0x20_0000, "adrp 必须对齐到 4K 页");
    }

    /// 自旋（SPL 的 hang()）要被检测出来，不能跑满步数。0x14000000 = `b .`
    #[test]
    fn detects_self_spin_hang() {
        let mut cpu = run(&[0x14000000], 0, E);
        cpu.max_steps = 100;
        assert_eq!(cpu.run(), Stop::Hang(E), "自旋应判为 hang");
    }

    /// 未初始化内存读 0：libfdt 靠这个终止遍历。
    /// mov x1,0x9000 ; ldr x0,[x1]
    #[test]
    fn loads_from_uninitialised_memory_read_zero() {
        let c = run(&[0xd2920001, 0xf9400020], 2, E);
        assert_eq!(c.x[0], 0);
    }
}

#[cfg(test)]
mod reg_tests {
    use super::*;
    use crate::splsim::mem::Mem;

    /// Rt == Rn 的加载：offset 形式不能写回基址，否则加载值被覆盖。
    /// 实测 bug：`ldr w0, [x0, #0x18]` 之后 x0 变成了基址而不是内存里的值，
    /// 导致 fdt_check_header 把 last_comp_version 读成 0x1ff3c0 而返回 −10。
    #[test]
    fn load_with_rt_equal_rn_is_not_clobbered() {
        // ldr w0, [x0, #0x18]  —— r2 验证过的真实编码（T20Pro SPL+0x5b04）
        let mut mem = Mem::new();
        mem.load_region(0x20_0000, &0xb9401800u32.to_le_bytes()).unwrap();
        // 在 0x1ff3c0 放一个 FDT 头，+0x18 处是 last_comp_version = 16（大端）
        mem.load_region(0x1ff3c0, &[0xd0, 0x0d, 0xfe, 0xed]).unwrap();
        mem.write(0x1ff3c0 + 0x18, 4, 0x1000_0000).unwrap();
        let mut cpu = Cpu::new(mem, 0x20_0000);
        cpu.x[0] = 0x1ff3c0;
        cpu.step().unwrap();
        assert_eq!(
            cpu.x[0], 0x1000_0000,
            "应读到内存里的 0x10000000，而不是被基址 0x1ff3c0 覆盖"
        );
    }

    /// pre-index 仍然必须写回基址。
    /// 编码取自真实 SPL：0xf84a8c41 = `ldr x1, [x2, 0xa8]!`（r2 已回读确认）
    /// （上一版先用我手写的 0xf8408400，r2 判为 invalid；
    ///   改用黄金表时又把已 byteswap 的字再 swap 一次，得到 0x418c4af8 也是 invalid。
    ///   同一个字节序陷阱这轮踩了三次。）
    #[test]
    fn pre_index_still_writes_back() {
        let mut mem = Mem::new();
        mem.load_region(0x20_0000, &0xf84a_8c41u32.to_le_bytes()).unwrap();
        mem.write(0x10a8, 8, 0xdead_beef).unwrap();
        let mut cpu = Cpu::new(mem, 0x20_0000);
        cpu.x[2] = 0x1000;
        cpu.step().unwrap();
        assert_eq!(cpu.x[1], 0xdead_beef, "应从 base+0xa8 读到值");
        assert_eq!(cpu.x[2], 0x10a8, "pre-index 应把基址加 0xa8 写回");
    }
}

#[cfg(test)]
mod reg_index_tests {
    use super::*;
    use crate::splsim::mem::Mem;

    /// 寄存器变址形式也不该写回基址。
    /// 真实编码 0x38614800 = `ldrb w0, [x0, w1, uxtw]`（T20Pro SPL+0x5c1c，
    /// libfdt 跳表的下标读取）。Rt == Rn，写回会把读到的下标覆盖成表基址，
    /// 于是跳表分发永远选错 case。
    #[test]
    fn reg_offset_load_with_rt_equal_rn_is_not_clobbered() {
        let mut mem = Mem::new();
        mem.load_region(0x20_0000, &0x3861_4800u32.to_le_bytes()).unwrap();
        // 跳表在 0x228e58，下标 5 处的值是 0x14
        mem.write_u8(0x228e58 + 5, 0x14).unwrap();
        let mut cpu = Cpu::new(mem, 0x20_0000);
        cpu.x[0] = 0x228e58;
        cpu.x[1] = 5;
        cpu.step().unwrap();
        assert_eq!(
            cpu.x[0], 0x14,
            "应读到跳表下标 0x14，而不是被表基址 0x228e58 覆盖"
        );
    }
}

#[cfg(test)]
mod ccmp_tests {
    use super::*;
    use crate::splsim::mem::Mem;

    fn run1(word: u32, x0: u64) -> Cpu {
        let mut mem = Mem::new();
        mem.load_region(0x20_0000, &word.to_le_bytes()).unwrap();
        let mut cpu = Cpu::new(mem, 0x20_0000);
        cpu.x[0] = x0;
        cpu.z = false;
        cpu.step().unwrap();
        cpu
    }

    /// `ccmp w0, 0, 4, ne` = 0x7a401804（T20Pro SPL+0x6e00，
    /// libfdt fdt_path_offset 里判断路径字符是否结束的那条）。
    ///
    /// 语义：若 ne 成立（即上一步 Z==0），则执行 cmp w0, #0；否则 NZCV = 4（Z=1）。
    /// imm5 是 **0** —— 早先的启发式在这种情况下退化成 cmp w0, x0，Z 恒为 1。
    #[test]
    fn ccmp_imm_zero_compares_against_zero_not_against_rn() {
        // x0 = 'c' (0x63)，预置 Z=0 使 ne 成立
        let c = run1(0x7a401804, 0x63);
        assert!(!c.z, "cmp 0x63, #0 应得 Z=0；若退化成 cmp w0,x0 就会是 Z=1");
        assert!(!c.n);
    }

    /// 同一个 imm5=0 的情况，但 x0 真的是 0 → Z 应为 1
    #[test]
    fn ccmp_imm_zero_sets_z_when_rn_is_zero() {
        let c = run1(0x7a401804, 0);
        assert!(c.z, "cmp 0, #0 应得 Z=1");
    }

    /// 条件不成立时应直接装载 nzcv=4（Z=1）
    #[test]
    fn ccmp_loads_nzcv_when_cond_fails() {
        // 预置 Z=1 → ne 不成立 → NZCV = 4 → Z=1, N=0
        let mut mem = Mem::new();
        mem.load_region(0x20_0000, &0x7a401804u32.to_le_bytes()).unwrap();
        let mut cpu = Cpu::new(mem, 0x20_0000);
        cpu.x[0] = 0x63;
        cpu.z = true; // 让 ne 不成立
        cpu.n = true;
        cpu.step().unwrap();
        assert!(cpu.z, "条件不成立应装载 nzcv=4 → Z=1");
        assert!(!cpu.n, "nzcv=4 → N=0");
    }

    /// SPL 里**真实的** memcmp —— T20Pro SPL+0x7d60..0x7d8c，12 条指令逐字节从镜像里抠出来。
    ///
    /// 为什么专门钉这一段：模拟 T20Pro 时 SPL 走进了 "Bad hash value" 分支，
    /// 而桩算出的摘要与 FIT 里 `hash/value` 存的值逐字节相同 ——
    /// 也就是 memcmp 在两个内容相同的缓冲上返回了非 0。
    /// 这不是镜像的问题，是模拟器的问题，所以用真代码把它固定成回归测试。
    ///
    /// 关键在于 `ldrb w3,[x6,x4]` / `ldrb w5,[x1,x4]`：变址寻址、无 extend、无 shift。
    fn run_memcmp(a: &[u8], b: &[u8], n: u64) -> i64 {
        const MEMCMP: [u32; 12] = [
            0xaa0003e6, 0xd2800004, 0xeb04005f, 0x54000061,
            0x52800000, 0x14000006, 0x386468c3, 0x38646825,
            0x91000484, 0x6b050060, 0x54ffff00, 0xd65f03c0,
        ];
        const CODE: u64 = 0x20_0000;
        const BA: u64 = 0x30_0000;
        const BB: u64 = 0x40_0000;
        const RET: u64 = 0x50_0000;
        let mut mem = Mem::new();
        let code: Vec<u8> = MEMCMP.iter().flat_map(|w| w.to_le_bytes()).collect();
        mem.load_region(CODE, &code).unwrap();
        mem.write_bytes(BA, a).unwrap();
        mem.write_bytes(BB, b).unwrap();
        let mut cpu = Cpu::new(mem, CODE);
        cpu.x[0] = BA;
        cpu.x[1] = BB;
        cpu.x[2] = n;
        cpu.x[30] = RET;
        cpu.sentinel_ret = RET;
        for _ in 0..10_000 {
            if cpu.stop.is_some() {
                break;
            }
            cpu.step().unwrap();
        }
        match cpu.stop {
            Some(Stop::Returned(v)) => v,
            other => panic!("memcmp 没有正常返回，stop={other:?} pc={:#x}", cpu.pc),
        }
    }

    #[test]
    fn real_spl_memcmp_equal_buffers_return_zero() {
        // 就是 atf-1 那个真实摘要
        let d = [
            0x54u8, 0xa1, 0x83, 0x7f, 0xe0, 0x3d, 0xde, 0x8e,
            0xd9, 0x3a, 0xaf, 0x93, 0xa8, 0x3d, 0xe6, 0x4a,
            0xf6, 0x5b, 0xbb, 0x8b, 0x52, 0xd9, 0xda, 0xcf,
            0x9a, 0xf1, 0xa7, 0x70, 0xd2, 0x70, 0x73, 0x4a,
        ];
        assert_eq!(run_memcmp(&d, &d, 32), 0, "相同内容必须返回 0");
    }

    #[test]
    fn real_spl_memcmp_detects_difference_at_every_offset() {
        let a = [0xaau8; 32];
        for i in 0..32usize {
            let mut b = a;
            b[i] = 0xbb;
            assert_ne!(run_memcmp(&a, &b, 32), 0, "第 {i} 字节不同却没查出来");
        }
    }

    #[test]
    fn real_spl_memcmp_short_and_empty() {
        assert_eq!(run_memcmp(&[1, 2, 3], &[1, 2, 3], 0), 0);
        assert_eq!(run_memcmp(&[1, 2, 3], &[1, 2, 3], 1), 0);
        assert_ne!(run_memcmp(&[1, 2, 3], &[1, 9, 3], 3), 0);
    }


    /// SPL+0x4ee8 的真实指令：`sbfiz x1, x1, #2, #32`（word 0x937e7c21，逐字节取自 T20Pro 镜像）。
    ///
    /// 为什么钉它：模拟 T20Pro 时 SPL 在 0x4ef0 的循环里出不来，PC 线性滑过 0x4f1c
    /// 撞进 hw_fit_calculate_hash 的入口 0x4f20，于是桩被垃圾参数反复触发。
    /// 那个循环的次数就是这条 sbfiz 算出来的 (len/4)<<2。
    ///
    /// SBFIZ Xd, Xn, #lsl, #width 的语义是：先把源截到 width 位，左移 lsl，
    /// 再按 width 位做**符号**扩展回填到 64 位。漏掉符号扩展/截断就会算错次数。
    fn run_sbfiz(word: u32, x1: u64) -> u64 {
        let mut mem = Mem::new();
        mem.load_region(0x20_0000, &word.to_le_bytes()).unwrap();
        let mut cpu = Cpu::new(mem, 0x20_0000);
        cpu.x[1] = x1;
        cpu.step().unwrap();
        cpu.x[1]
    }

    /// 跑一条真指令：把 src 放进 src_reg，执行，读回 rd。
    fn run_bf(word: u32, src: u64, src_reg: u8, rd: u8) -> u64 {
        let mut mem = Mem::new();
        mem.load_region(0x20_0000, &word.to_le_bytes()).unwrap();
        let mut cpu = Cpu::new(mem, 0x20_0000);
        cpu.x[src_reg as usize] = src;
        cpu.step().unwrap();
        cpu.reg_dbg(rd)
    }

    /// SPL 里真实的 memcpy 那两条 —— 就是这个 bug 让 SPL 把自己的代码覆盖了。
    ///
    /// `lsr x5, x2, #3`（0xd343fc45）+ `lsl x4, x5, #3`（0xd37df0a4）
    /// 一起算出字循环的上限 `(len>>3)<<3`。旧实现把 `lsr` 做成了 `ror`，
    /// len=0x3203c 时上限算成 0x3203c（奇数），计数器按 8 递增永远撞不上，
    /// memcpy 于是一路写穿，覆盖 SPL+0x3580 起的代码。
    #[test]
    fn memcpy_loop_bound_real_spl_words() {
        const LSR3: u32 = 0xd343fc45; // lsr x5, x2, #3
        const LSL3: u32 = 0xd37df0a4; // lsl x4, x5, #3

        // 就是 atf-1 的真实长度 0x3203c
        let len: u64 = 0x3203c;
        assert_eq!(run_bf(LSR3, len, 2, 5), 0x6407, "lsr 变成了 ror");
        // lsl 的源是 x5，这里单独验证：把 len>>3 放进 x2 再左移
        assert_eq!(run_bf(LSL3, 0x6407, 5, 4), 0x32038);

        // 上限必须是 8 的倍数，否则按 8 递增的计数器永远撞不上
        assert_eq!(run_bf(LSL3, run_bf(LSR3, len, 2, 5), 5, 4) % 8, 0);

        // lsr 必须清掉从低位绕上来的比特（ror 会保留）
        assert_eq!(run_bf(LSR3, 0xffff_ffff_ffff_ffff, 2, 5), 0x1fff_ffff_ffff_ffff);
        assert_eq!(run_bf(LSR3, 0x8000_0000_0000_0000, 2, 5), 0x1000_0000_0000_0000);
        assert_eq!(run_bf(LSR3, 7, 2, 5), 0);
    }

    /// UBFM/SBFM 的两种情形（抽取 / 插入）都要对。编码取自真实镜像。
    #[test]
    fn bitfield_extract_and_insert() {
        // lsr x5, x2, #3        抽取
        assert_eq!(run_bf(0xd343fc45, 0x1234_5678_9abc_def0, 2, 5), 0x0246_8acf_1357_9bde);
        // sxtw x2, w2 (SBFM immr=0 imms=31)  抽取 + 符号扩展
        const SXTW: u32 = 0x93407c42;
        assert_eq!(run_bf(SXTW, 0x0000_0000_8000_0000, 2, 2), 0xffff_ffff_8000_0000);
        assert_eq!(run_bf(SXTW, 0x0000_0000_7fff_ffff, 2, 2), 0x0000_0000_7fff_ffff);
        // sbfiz x1, x1, #2, #32  插入（源在 x1，单独构造）
        let mut mem = Mem::new();
        mem.load_region(0x20_0000, &0x937e7c21u32.to_le_bytes()).unwrap();
        let mut cpu = Cpu::new(mem, 0x20_0000);
        cpu.x[1] = 2;
        cpu.step().unwrap();
        assert_eq!(cpu.reg_dbg(1), 8);
    }

    #[test]
    fn sbfiz_real_spl_word() {
        const W: u32 = 0x937e7c21; // sbfiz x1, x1, #2, #32
        assert_eq!(run_sbfiz(W, 2), 8, "len/4=2 → 8 字节");
        assert_eq!(run_sbfiz(W, 1), 4);
        assert_eq!(run_sbfiz(W, 0), 0);
        assert_eq!(run_sbfiz(W, 8), 32);
        // 源先截到 32 位再左移：0x10000000 << 2 = 0x40000000，仍是正数
        assert_eq!(run_sbfiz(W, 0x1000_0000), 0x4000_0000);
        // 0x20000000 << 2 = 0x80000000 → 32 位看是负数 → 符号扩展成 0xffffffff80000000
        assert_eq!(run_sbfiz(W, 0x2000_0000), 0xffff_ffff_8000_0000);
        // 高 32 位必须先被截掉：0x1_00000002 → 低 32 位 2 → 8
        assert_eq!(run_sbfiz(W, 0x1_0000_0002), 8, "源没有截到 width 位");
        // -1 截成 32 位后 <<2 = 0xfffffffc → 符号扩展 = -4
        assert_eq!(run_sbfiz(W, 0xffff_ffff_ffff_ffff), 0xffff_ffff_ffff_fffc);
    }

}
