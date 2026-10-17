//! A64 译码器。
//!
//! 不依赖 capstone —— 自己解码。这是有意的：Python 版一半的 bug 来自 capstone 的
//! 操作数怪癖（`cmp` 只给 2 个操作数、条件码藏在 `ins.cc`、`adrp` 的 imm 是结果
//! 地址、`mem.disp` 分不清 offset/pre/post-index、`ARM64_REG_*` 枚举不连续、
//! X29/X30 叫 `fp`/`lr`）。自己解码就没有这一层。

use crate::splsim::mask::decode_bitmasks;

pub type R = u8; // 寄存器号 0..=30；31 由上下文决定是 SP 还是 XZR

/// 访存寻址方式
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Addr {
    /// [Xn, #imm]
    Offset(i64),
    /// [Xn, #imm]! —— 先加 imm 再访存，并写回 Xn
    Pre(i64),
    /// [Xn], #imm —— 先访存再加 imm 写回 Xn
    Post(i64),
    /// [Xn, Xm{,ext #amt}]
    Reg { ext: u8, sh: u8 },
}

/// 移位/扩展类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sh {
    Lsl,
    Lsr,
    Asr,
    Ror,
    /// 扩展：UXTB/UXTH/UXTW/UXTX/SXTB/SXTH/SXTW/SXTX，值 0..=7
    Ext(u8),
}

/// 解码结果。字段按指令类别取用，不适用的保持默认值。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct D {
    pub k: K,
    /// 32 位形式（W 寄存器 / 32 位运算）
    pub w32: bool,
    pub rd: R,
    pub rn: R,
    pub rm: R,
    pub ra: R,
    pub imm: i64,
    pub imm2: i64,
    /// 逻辑立即数的位掩码（已按 DecodeBitMasks 算好）
    pub bitmask: u64,
    pub sh: Sh,
    pub amt: u8,
    pub cond: u8,
    pub addr: Addr,
    /// 访存元素字节数
    pub size: u8,
    /// 访存是否符号扩展（ldrsb/ldrsh/ldrsw）
    pub sext: bool,
    /// 是否写标志位（adds/subs/ands/adc/sbc 的 S 变体）
    pub flags: bool,
    /// stp/ldp 的第二个寄存器
    pub rt2: R,
    /// 向量访存是 load 还是 store
    pub vload: bool,
    /// 向量访存是不是 pair（ldp/stp/ldnp/stnp）
    pub vpair: bool,
    /// 向量 pair 是不是非时序（ldnp/stnp）
    pub vnt: bool,
    /// 原始指令字（供助记符还原系统指令名等）
    pub raw: u32,
    /// ccmp/ccmn 的第二操作数是立即数(imm5)还是寄存器 Rm
    pub cc_imm: bool,
    /// 非缩放立即数形式（ldur/stur/ldurb/...）
    pub unscaled: bool,
}

impl Default for D {
    fn default() -> Self {
        D {
            k: K::Nop,
            w32: false,
            rd: 0,
            rn: 0,
            rm: 0,
            ra: 0,
            imm: 0,
            imm2: 0,
            bitmask: 0,
            sh: Sh::Lsl,
            amt: 0,
            cond: 0,
            addr: Addr::Offset(0),
            size: 4,
            sext: false,
            flags: false,
            rt2: 0,
            vload: false,
            vpair: false,
            vnt: false,
            raw: 0,
            unscaled: false,
            cc_imm: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum K {
    Nop,
    Hint,
    /// 未能解码
    Bad,

    Movz,
    Movk,
    Movn,
    Adr,
    Adrp,

    /// Rd = Rn OP bitmask
    LogImm,
    /// Rd = Rn OP (Rm 移位)
    LogSh,
    /// ands / tst（写标志）
    LogImmS,
    LogShS,

    AddImm,
    SubImm,
    AddSh,
    SubSh,
    AddExt,
    SubExt,
    AddCarry,
    SubCarry,
    Ccmp,
    Ccmn,
    Csel,
    Csinc,
    Csinv,
    Csneg,

    /// 位域：ubfm/sbfm/bfm（含 lsl/lsr/asr/ror 别名）
    Ubfm,
    Sbfm,
    Bfm,
    Extr,
    /// bfi / bfxil / ubfiz / sbfiz 已由上面三个覆盖，这里只留 rev 类
    Rev16,
    Rev32,
    Clz,
    Cls,
    Rbit,

    Mul,
    SMulH,
    UMulH,
    Madd,
    Msub,
    Smaddl,
    Umsubl,
    Umaddl,
    Smsubl,
    SDiv,
    UDiv,
    Lslv,
    Lsrv,
    Asrv,
    Rorv,

    Ldr,
    Str,
    Ldp,
    Stp,
    /// ldxr/stxr 等独占访存：单核仿真下当作普通访存 + 独占成功
    Ldxr,
    Stxr,
    /// ldr Xt, <label> —— PC 相对字面量
    LdrLit,

    B,
    Bl,
    Br,
    Blr,
    Ret,
    Bc,
    Cbz,
    Cbnz,
    Tbz,
    Tbnz,

    /// 向量 load/store：整数语义下是 no-op，但要吃掉地址计算
    VecLdSt,
    /// fmov 之类的 GPR<->FPR 搬运
    FmovGpr,
    /// svc/hvc/smc/eret/wfi/dc/ic/tlbi/prfm/dmb/dsb/isb
    Sys,
    Wfi,
}

/// 取 bits[hi:lo]。
///
/// 参数顺序是 **(v, lo, hi)** —— 全部调用处都按"从低位到高位"的习惯写，
/// 早先函数签名却是 (v, hi, lo)，于是 `b(v, 0, 4)` 里的 `hi - lo + 1` 直接
/// u32 下溢，位段全取错（表现为 bl 被认成 hint、ldr 被认成 cbnz）。
#[inline]
fn b(v: u32, lo: u32, hi: u32) -> u32 {
    debug_assert!(hi >= lo && hi < 32, "位段越界: bits[{hi}:{lo}]");
    (v >> lo) & ((1u32 << (hi - lo + 1)) - 1)
}
/// 取 bits[hi:lo] 并按其位宽做符号扩展。
#[inline]
fn sb(v: u32, lo: u32, hi: u32) -> i64 {
    let n = hi - lo + 1;
    let x = b(v, lo, hi) as i64;
    if x & (1 << (n - 1)) != 0 {
        x - (1 << n)
    } else {
        x
    }
}

/// 解码一条 32 位指令。`pc` 用于解析 PC 相对寻址（adr/adrp/分支）。
pub fn decode(v: u32, pc: u64) -> D {
    let mut d = D::default();
    d.raw = v;
    // ARM 顶层分组用 **bits[28:25]**（不是 bits[25:28]，那 4 位没有意义）
    let op0 = b(v, 25, 28) as u8;

    // ARM 顶层分组表，op0 = bits[28:25]（权威来源：A64 Index by Encoding）
    //   0000        Reserved / SME
    //   0010        SVE
    //   100x        Data Processing -- Immediate
    //   101x        Branches, Exception Generating and System instructions
    //   x101        Data Processing -- Register
    //   x111        Data Processing -- Scalar FP and Advanced SIMD
    //   x1x0        Loads and Stores
    match op0 {
        0b1000 | 0b1001 => dp_imm(v, &mut d, pc),
        0b1010 | 0b1011 => branch_sys(v, &mut d, pc),
        0b0101 | 0b1101 => dp_reg(v, &mut d, pc),
        0b0100 | 0b0110 | 0b1100 | 0b1110 => ld_st(v, &mut d, pc),
        // SIMD / FP / SVE：整数语义下当 no-op
        0b0111 | 0b1111 | 0b0010 => d.k = K::VecLdSt,
        0b0000 => d.k = K::Hint,
        _ => d.k = K::Bad,
    }
    d
}

/// 分支 / 异常生成 / 系统指令组：op0(bits[28:25]) = 101x。
///
/// 组内再按 bits[31:29] 分：
///   x00 → 无条件分支(立即数) B/BL
///   x01 → bit25=0 compare-and-branch，bit25=1 test-and-branch
///   010 → bits[25:24]=00 条件分支，其余 misc branch
///   110 → bit25=1 无条件分支(寄存器)，bit24=0 异常生成，否则系统指令/hints/barriers
fn branch_sys(v: u32, d: &mut D, pc: u64) {
    match b(v, 29, 31) {
        0b000 | 0b100 => {
            d.k = if b(v, 31, 31) == 1 { K::Bl } else { K::B };
            d.imm = pc.wrapping_add((sb(v, 0, 25) << 2) as u64) as i64;
        }
        0b001 | 0b101 => {
            // 被测寄存器放在 rd —— 这几条没有目的寄存器，解释器统一按 rd 取源。
            // 早先只写进 rt2，解释器读 rd 会拿到 0。
            d.rd = b(v, 0, 4) as R;
            d.rt2 = d.rd;
            // 两者的偏移字段宽度不同，早先都按 cbz 的 imm19 取，
            // 结果 tbz/tbnz 多读了 5 位（那 5 位是位号 b40），1172 条目标全错。
            //   cbz/cbnz : sf 011010 op imm19 Rt          → bits[23:5]
            //   tbz/tbnz : b5 011011 op b40 imm14 Rt      → bits[18:5]
            let off = if b(v, 25, 25) == 0 {
                d.k = if b(v, 24, 24) == 1 { K::Cbnz } else { K::Cbz };
                d.w32 = b(v, 31, 31) == 0;
                sb(v, 5, 23)
            } else {
                d.k = if b(v, 24, 24) == 1 { K::Tbnz } else { K::Tbz };
                d.amt = ((b(v, 31, 31) << 5) | b(v, 19, 23)) as u8;
                sb(v, 5, 18)
            };
            d.imm = pc.wrapping_add((off << 2) as u64) as i64;
        }
        0b010 => {
            if b(v, 24, 25) == 0 {
                d.k = K::Bc;
                d.cond = b(v, 0, 3) as u8;
                d.imm = pc.wrapping_add((sb(v, 5, 23) << 2) as u64) as i64;
            } else {
                d.k = K::Sys;
            }
        }
        0b110 => {
            if b(v, 25, 25) == 1 {
                // 无条件分支（寄存器）—— 目标寄存器一律放 rn（约定：rn 是第一个源操作数）
                match b(v, 21, 24) {
                    0b0000 => {
                        d.k = K::Br;
                        d.rn = b(v, 5, 9) as R;
                    }
                    0b0001 => {
                        d.k = K::Blr;
                        d.rn = b(v, 5, 9) as R;
                    }
                    0b0010 => {
                        d.k = K::Ret;
                        d.rn = b(v, 5, 9) as R;
                    }
                    _ => d.k = K::Sys,
                }
            } else if b(v, 24, 24) == 0 {
                d.k = K::Sys; // 异常生成 svc/hvc/smc/brk/hlt
            } else if v == 0xd503_205f {
                d.k = K::Wfi;
            } else {
                d.k = K::Sys; // msr/mrs/sys/dc/ic/tlbi/barriers/hints
                // mrs 会把系统寄存器写回 Rt，msr 会从 Rt 读，必须抽出来
                d.rd = b(v, 0, 4) as R;
            }
        }
        _ => d.k = K::Bad,
    }
}

/// 数据处理（立即数）：op0(bits[28:25]) = 100x，组内按 bits[25:23] 分。
fn dp_imm(v: u32, d: &mut D, pc: u64) {
    match b(v, 23, 25) {
        // pc-rel addressing: op immlo 10000 immhi Rd
        0b000 | 0b001 => {
            d.rd = b(v, 0, 4) as R;
            let immlo = b(v, 29, 30) as i64;
            let immhi = sb(v, 5, 23) as i64;
            let imm = (immhi << 2) | immlo;
            if b(v, 31, 31) == 1 {
                // adrp: 结果地址 = (PC & ~0xfff) + (imm << 12)
                d.k = K::Adrp;
                d.imm = ((pc & !0xfff) as i64).wrapping_add(imm << 12);
            } else {
                d.k = K::Adr;
                d.imm = pc.wrapping_add(imm as u64) as i64;
            }
        }
        // add/sub 立即数: sf op S 100010 sh imm12 Rn Rd
        0b010 | 0b011 => {
            d.w32 = b(v, 31, 31) == 0;
            d.flags = b(v, 29, 29) == 1;
            d.rn = b(v, 5, 9) as R;
            d.rd = b(v, 0, 4) as R;
            d.imm = b(v, 10, 21) as i64;
            if b(v, 22, 22) == 1 {
                d.imm <<= 12;
            }
            d.k = if b(v, 30, 30) == 1 { K::SubImm } else { K::AddImm };
        }
        // 逻辑立即数: sf opc 100100 N immr imms Rn Rd
        0b100 => {
            d.w32 = b(v, 31, 31) == 0;
            d.rn = b(v, 5, 9) as R;
            d.rd = b(v, 0, 4) as R;
            let bm = decode_bitmasks(b(v, 22, 22) == 1, b(v, 16, 21), b(v, 10, 15));
            d.bitmask = bm.unwrap_or(0);
            d.k = if bm.is_none() {
                K::Bad
            } else if b(v, 29, 30) == 3 {
                K::LogImmS
            } else {
                K::LogImm
            };
            d.imm = b(v, 29, 30) as i64; // 0=and 1=orr 2=eor 3=ands
        }
        // movz/movk/movn: sf opc 100101 hw imm16 Rd
        0b101 => {
            d.w32 = b(v, 31, 31) == 0;
            d.rd = b(v, 0, 4) as R;
            d.imm = b(v, 5, 20) as i64;
            d.amt = (b(v, 21, 22) * 16) as u8;
            d.k = match b(v, 29, 30) {
                0 => K::Movn,
                2 => K::Movz,
                _ => K::Movk,
            };
        }
        // 位域: sf opc 100110 N immr imms Rn Rd
        0b110 => {
            d.w32 = b(v, 31, 31) == 0;
            d.rn = b(v, 5, 9) as R;
            d.rd = b(v, 0, 4) as R;
            d.imm = b(v, 16, 21) as i64; // immr
            d.imm2 = b(v, 10, 15) as i64; // imms
            d.k = match b(v, 29, 30) {
                0 => K::Sbfm,
                1 => K::Bfm,
                _ => K::Ubfm,
            };
            // 移位别名要顺手把移位量填进 amt，否则译码结果不自洽：
            // `lsl x0,x0,#2`（0xd37ef400）的 amt 会留成 0。
            // 执行路径不读它（bitfield() 直接用 immr/imms），但对外暴露的字段不能是错的。
            let bits: i64 = if d.w32 { 32 } else { 64 };
            let (immr, imms) = (d.imm, d.imm2);
            if imms == bits - 1 {
                d.amt = immr as u8; // lsr / asr
            } else if immr == (imms + 1) % bits {
                d.amt = (bits - immr) as u8; // lsl
            }
        }
        // extract: sf op21 100111 N o0 Rm imms Rn Rd
        0b111 => {
            d.w32 = b(v, 31, 31) == 0;
            d.rn = b(v, 5, 9) as R;
            d.rm = b(v, 16, 20) as R;
            d.rd = b(v, 0, 4) as R;
            d.amt = b(v, 10, 15) as u8;
            d.k = K::Extr;
        }
        _ => d.k = K::Bad,
    }
}

/// 条件比较与选择：op0 = 10010/10011
#[allow(dead_code)] // 解码覆盖用；当前 SPL 未走到 CSEL 路径
fn cc_sel(v: u32, d: &mut D) {
    let op = b(v, 21, 31);
    if op == 0b1101010_0010 {
        // ccmp (imm): sf 1 0 11010010 Rn imm5 cond 1 0 nzcv
        d.k = K::Ccmp;
        d.w32 = b(v, 31, 31) == 0;
        d.rn = b(v, 5, 9) as R;
        d.imm = b(v, 16, 20) as i64;
        d.cond = b(v, 12, 15) as u8;
        d.imm2 = b(v, 0, 3) as i64; // 条件为假时要设的 nzcv
    } else if op == 0b1101010_0000 {
        // ccmn (imm)
        d.k = K::Ccmn;
        d.w32 = b(v, 31, 31) == 0;
        d.rn = b(v, 5, 9) as R;
        d.imm = b(v, 16, 20) as i64;
        d.cond = b(v, 12, 15) as u8;
        d.imm2 = b(v, 0, 3) as i64;
    } else if op == 0b1101010_0100 {
        // ccmp (reg): o2==0
        d.k = K::Ccmp;
        d.w32 = b(v, 31, 31) == 0;
        d.rn = b(v, 5, 9) as R;
        d.rm = b(v, 16, 20) as R;
        d.cond = b(v, 12, 15) as u8;
        d.imm2 = b(v, 0, 3) as i64;
    } else if op == 0b1101010_0110 {
        // ccmn (reg): o2==1
        d.k = K::Ccmn;
        d.w32 = b(v, 31, 31) == 0;
        d.rn = b(v, 5, 9) as R;
        d.rm = b(v, 16, 20) as R;
        d.cond = b(v, 12, 15) as u8;
        d.imm2 = b(v, 0, 3) as i64;
    } else if b(v, 21, 28) == 0b11010100 {
        // 条件选择: sf op S 11010100 Rm cond op2 Rn Rd
        d.w32 = b(v, 31, 31) == 0;
        d.rn = b(v, 5, 9) as R;
        d.rm = b(v, 16, 20) as R;
        d.rd = b(v, 0, 4) as R;
        d.cond = b(v, 12, 15) as u8;
        d.k = match b(v, 10, 11) {
            0b00 => K::Csel,
            0b01 => K::Csinc,
            0b10 => K::Csinv,
            _ => K::Csneg,
        };
    } else {
        d.k = K::Bad;
    }
}

fn dp_reg(v: u32, d: &mut D, pc: u64) {
    let _ = pc;
    // 组内按 bits[28:24] 分（权威来源：Data Processing -- Register）
    match b(v, 24, 28) {
        // ---- 逻辑（移位寄存器）: sf opc 01010 shift N Rm imm6 Rn Rd ----
        0b01010 => {
            d.w32 = b(v, 31, 31) == 0;
            d.rd = b(v, 0, 4) as R;
            d.rn = b(v, 5, 9) as R;
            d.rm = b(v, 16, 20) as R;
            d.amt = b(v, 10, 15) as u8;
            d.sh = match b(v, 22, 23) {
                0 => Sh::Lsl,
                1 => Sh::Lsr,
                2 => Sh::Asr,
                _ => Sh::Ror,
            };
            let neg = b(v, 21, 21);
            let opc = b(v, 29, 30);
            // 只有 opc==11 (ANDS/BICS) 才写标志；ORR 的 bit29 也是 1，
            // 早先用 `bit29==1` 判 flags，把 `mov x29, x30` 认成了 ands。
            d.flags = opc == 3;
            d.k = if d.flags { K::LogShS } else { K::LogSh };
            d.imm = ((opc << 1) | neg) as i64;
        }
        // ---- add/sub（移位寄存器 / 扩展寄存器）: sf op S 01011 ... ----
        0b01011 => {
            d.w32 = b(v, 31, 31) == 0;
            d.flags = b(v, 29, 29) == 1;
            d.rd = b(v, 0, 4) as R;
            d.rn = b(v, 5, 9) as R;
            d.rm = b(v, 16, 20) as R;
            if b(v, 21, 21) == 1 {
                // 扩展寄存器: opt(3)=xx1 Rm option S Rn Rd
                d.sh = Sh::Ext(b(v, 13, 15) as u8);
                d.amt = b(v, 10, 12) as u8;
                d.k = if b(v, 30, 30) == 1 { K::SubExt } else { K::AddExt };
            } else {
                d.sh = match b(v, 22, 23) {
                    0 => Sh::Lsl,
                    1 => Sh::Lsr,
                    2 => Sh::Asr,
                    _ => Sh::Lsl,
                };
                d.amt = b(v, 10, 15) as u8;
                d.k = if b(v, 30, 30) == 1 { K::SubSh } else { K::AddSh };
            }
        }
        // ---- 11010xxx：带进位加减 / 条件比较 / 条件选择 / 1源2源 ----
        0b11010 => match b(v, 21, 28) {
            // CCMN (imm): sf 0 1 11010000 Rn imm5 cond 0 0 nzcv
            // bits[28:21]==11010000 是 ADC/SBC（bits[15:10] 固定为 0）。
            // 注意：CCMN(imm) 不在这个分支 —— 它和 CCMP(imm) 一样是 11010010。
            0b11010000 => {
                d.w32 = b(v, 31, 31) == 0;
                d.flags = b(v, 29, 29) == 1;
                d.rd = b(v, 0, 4) as R;
                d.rn = b(v, 5, 9) as R;
                d.rm = b(v, 16, 20) as R;
                d.k = if b(v, 30, 30) == 1 { K::SubCarry } else { K::AddCarry };
            }
            // CCMP(imm) 与 CCMN(imm) 的 bits[28:21] 都是 11010010，
            // 靠 bits[30:29] 区分：10=CCMP, 01=CCMN（实测 0x3a4b1a64 是 ccmn）。
            0b11010010 => {
                // 实测：0x7a401824 bit30=1 是 ccmp，0x3a4b1a64 bit30=0 是 ccmn
                d.k = if b(v, 30, 30) == 1 { K::Ccmp } else { K::Ccmn };
                d.w32 = b(v, 31, 31) == 0;
                d.rn = b(v, 5, 9) as R;
                d.imm = b(v, 16, 20) as i64; // imm5
                d.cond = b(v, 12, 15) as u8;
                d.imm2 = b(v, 0, 3) as i64; // nzcv
                d.cc_imm = true;
            }
            // add/sub with carry: sf op S 11010000... 见上；这里是 11010000 已被占，
            // 带进位加减的 bits[28:21] 是 11010000 且 bits[15:10]=000000
            // —— 由下面的 0b11010000 分支里 Rm 字段区分不了，改在此处按 op 位判断。
            // 条件选择: sf 0 S 11010100 Rm cond op2 Rn Rd
            0b11010100 => {
                {
                    d.w32 = b(v, 31, 31) == 0;
                    d.rn = b(v, 5, 9) as R;
                    d.rm = b(v, 16, 20) as R;
                    d.rd = b(v, 0, 4) as R;
                    d.cond = b(v, 12, 15) as u8;
                    // 实测校准（0xda9f3000=csinv op=1/op2=00, 0x5a824662=csneg op=1/op2=01,
                    // 0x9a9f1400=csinc op=0/op2=01）：op2 为主，op 为辅。
                    d.k = match (b(v, 10, 11), b(v, 30, 30)) {
                        (0b00, 0) => K::Csel,
                        (0b00, _) => K::Csinv,
                        (0b01, 0) => K::Csinc,
                        (0b10, _) => K::Csinv,
                        _ => K::Csneg,
                    };
                }
            }
            0b11010110 => {
                if b(v, 30, 30) == 1 {
                    // 1-source: sf 1 0 11010110 Rn opcode2 opcode 00000 Rd
                    d.w32 = b(v, 31, 31) == 0;
                    d.rn = b(v, 5, 9) as R;
                    d.rd = b(v, 0, 4) as R;
                    d.k = match b(v, 10, 15) {
                        0b000000 => K::Rbit,
                        0b000001 => K::Rev16,
                        0b000010 => {
                            if d.w32 {
                                K::Rev32
                            } else {
                                K::Bad
                            }
                        }
                        0b000011 => K::Rev32,
                        0b000100 => K::Clz,
                        0b000101 => K::Cls,
                        _ => K::Bad,
                    };
                } else if b(v, 29, 29) == 1 && b(v, 10, 11) == 0 {
                    // CCMN (reg): sf 0 1 11010110 Rm cond 0 0 Rn Rd
                    d.k = K::Ccmn;
                    d.rn = b(v, 5, 9) as R;
                    d.rm = b(v, 16, 20) as R;
                    d.cond = b(v, 12, 15) as u8;
                    d.imm2 = b(v, 0, 3) as i64;
                    d.cc_imm = false;
                } else {
                    // 2-source: sf 0 S 11010110 Rm opcode 00000 Rd
                    d.w32 = b(v, 31, 31) == 0;
                    d.rd = b(v, 0, 4) as R;
                    d.rn = b(v, 5, 9) as R;
                    d.rm = b(v, 16, 20) as R;
                    d.k = match b(v, 10, 15) {
                        0b000010 => K::UDiv, // 注意：UDIV=000010, SDIV=000011（早先记反）
                        0b000011 => K::SDiv,
                        // 实测校准：LSLV=001000(8) … RORV=001011(11)
                        // （早先按 100000/100001/100010/100011 写，全错）
                        0b001000 => K::Lslv,
                        0b001001 => K::Lsrv,
                        0b001010 => K::Asrv,
                        0b001011 => K::Rorv,
                        _ => K::Bad,
                    };
                }
            }
            _ => d.k = K::Bad, // crypto 等
        },
        // ---- 三源: sf op31 11011 o0 Rm o0 Ra Rn Rd ----
        0b11011 => {
            d.w32 = b(v, 31, 31) == 0;
            d.rd = b(v, 0, 4) as R;
            d.rn = b(v, 5, 9) as R;
            d.ra = b(v, 10, 14) as R;
            d.rm = b(v, 16, 20) as R;
            let o0 = b(v, 15, 15);
            d.k = match (b(v, 21, 23), o0) {
                (0b000, 0) => K::Madd,
                (0b000, 1) => K::Msub,
                (0b001, 0) => K::Smaddl,
                (0b001, 1) => K::Smsubl,
                (0b010, 0) => K::SMulH,
                (0b101, 0) => K::Umaddl,
                (0b101, 1) => K::Umsubl,
                (0b110, 0) => K::UMulH,
                _ => K::Bad,
            };
        }
        _ => d.k = K::Bad, // crypto / 未分配
    }
}

fn ld_st(v: u32, d: &mut D, pc: u64) {
    let vb = b(v, 26, 26); // V: 1 = SIMD/FP
    let size = b(v, 30, 31); // bits[31:30]
    d.rt2 = b(v, 0, 4) as R;
    d.rd = b(v, 0, 4) as R;

    // ---- load/store pair: bits[29:27]=101 且 bit25=0 ----
    if b(v, 27, 29) == 0b101 && b(v, 25, 25) == 0 {
        let opc = b(v, 30, 31); // 00=32位 01=LDPSW 10=64位 11=128位(SIMD)
        let sc = if opc == 3 { 4u32 } else if opc == 2 { 3 } else { 2 };
        d.size = 1 << sc;
        d.sext = opc == 1;
        d.rt2 = b(v, 10, 14) as R;
        d.rn = b(v, 5, 9) as R;
        let imm7 = sb(v, 15, 21) << sc;
        d.addr = match b(v, 23, 25) {
            0b001 => Addr::Post(imm7),
            0b011 => Addr::Pre(imm7),
            _ => Addr::Offset(imm7),
        };
        d.vload = b(v, 22, 22) == 1;
        d.vpair = true;
        // ldnp/stnp 用 bits[25:23]==000（GPR pair 只用 001/010/011）。
        // 早先写成 ==010，把普通 ldp 和 ldnp 判反了。
        d.vnt = b(v, 23, 25) == 0b000;
        d.k = if vb == 1 {
            K::VecLdSt
        } else if d.vload {
            K::Ldp
        } else {
            K::Stp
        };
        return;
    }

    // ---- load/store exclusive: bits[29:24] = 001000 ----
    if b(v, 24, 29) == 0b001000 {
        d.rn = b(v, 5, 9) as R; // 基址
        d.size = 1 << size;
        d.addr = Addr::Offset(0);
        if b(v, 22, 22) == 0 {
            d.k = K::Stxr;
            // stxr Ws, Wt, [Xn]：Ws 是状态输出(bits[20:16])，Wt 是被存值(bits[4:0])
            // r2 打印时 Ws 在前，别把它当成被存的值。
            d.rm = b(v, 16, 20) as R;
        } else {
            d.k = K::Ldxr;
        }
        return;
    }

    // ---- load/store register literal: opc 011 V 00 imm19 Rt ----
    if b(v, 27, 29) == 0b011 && b(v, 24, 25) == 0b00 {
        let opc = b(v, 30, 31);
        d.size = if vb == 0 && opc == 0 { 4 } else { 8 };
        d.sext = vb == 0 && opc == 2;
        d.k = if vb == 1 {
            K::VecLdSt
        } else {
            K::LdrLit
        };
        d.vload = true;
        d.imm = pc.wrapping_add((sb(v, 5, 23) << 2) as u64) as i64;
        return;
    }

    // ---- load/store register: bits[29:27] = 111 ----
    if b(v, 27, 29) == 0b111 {
        let opc = b(v, 22, 23);
        d.rn = b(v, 5, 9) as R;
        // SIMD 128 位：V=1, size=00, opc=10
        d.size = if vb == 1 && size == 0 && opc == 2 {
            16
        } else {
            1 << size
        };
        if b(v, 24, 25) == 0b01 {
            // unsigned offset，比例是 2^size
            d.addr = Addr::Offset((b(v, 10, 21) as i64) << size);
        } else if b(v, 21, 21) == 1 && b(v, 10, 11) == 0b10 {
            // register offset: Rm{,option{,S}}
            d.rm = b(v, 16, 20) as R;
            d.addr = Addr::Reg {
                ext: b(v, 13, 15) as u8,
                sh: if b(v, 12, 12) == 1 { size as u8 } else { 0 },
            };
        } else {
            // imm9 形式：unscaled / post / unprivileged / pre
            let imm9 = sb(v, 12, 20);
            d.addr = match b(v, 10, 11) {
                0b01 => Addr::Post(imm9),
                0b11 => Addr::Pre(imm9),
                _ => {
                    d.unscaled = true;
                    Addr::Offset(imm9)
                }
            };
        }
        // 不能只看 opc 最低位：LDRSW 的 opc==10 也是 load。
        //   size<3: opc=00 STR, 01 LDR, 10 LDRSB/H(64位), 11 LDRSB/H(32位)
        //   size=3: opc=00 STR, 01 LDR
        let is_load = if vb == 1 {
            (opc & 1) == 1 // SIMD：opc 最低位就是 L（实测 0x3d801fa0 size=0 opc=2 是 str q0）
        } else if size == 3 {
            opc == 1
        } else {
            opc != 0
        };
        d.vload = is_load;
        d.sext = vb == 0 && ((size < 3 && opc >= 2) || (size == 2 && opc == 2));
        d.k = if vb == 1 {
            K::VecLdSt
        } else if is_load {
            K::Ldr
        } else {
            K::Str
        };
        return;
    }

    // 原子指令 / 未分配：SPL 里少量出现（ldsetb/ldeorh 等，多在数据区）
    d.k = K::Bad;
}

/// 系统指令的具体名字（mrs/msr/barriers）。dc/ic/tlbi 种类太多，统称 sys。
fn sys_name(d: &D) -> &'static str {
    let v = d.raw;
    if v & (1 << 21) != 0 {
        return "mrs";
    }
    // barriers: 1101 0101 0000 0011 0011 CRm op2 11111
    if v & 0xffff_f0e0 == 0xd503_30e0 && b(v, 0, 4) == 0x1f {
        return match b(v, 5, 7) {
            0b100 => "dsb",
            0b101 => "dmb",
            0b110 => "isb",
            0b010 => "clrex",
            _ => "sys",
        };
    }
    "msr"
}

// ---------------------------------------------------------------- 助记符格式化

/// 返回这条指令的 r2 风格助记符。
///
/// 只用于对拍（tests/r2_golden.tsv），不参与解释。别名（mov/cmp/tst/neg/...）
/// 按 r2 的输出还原，否则对不上。
pub fn mnemonic(d: &D) -> &'static str {
    match d.k {
        K::Nop => "nop",
        K::Hint => "hint",
        K::Bad => "invalid",

        // r2 把 movz/movn 都印成 mov（hw==0 时），movk 保留
        K::Movz | K::Movn => "mov",
        K::Movk => "movk",
        K::Lslv => "lsl",
        K::Lsrv => "lsr",
        K::Asrv => "asr",
        K::Rorv => "ror",
        K::Adr => "adr",
        K::Adrp => "adrp",

        K::LogImm => match d.imm {
            1 => {
                if d.rn == 31 {
                    "mov" // orr Xd, XZR, #imm
                } else {
                    "orr"
                }
            }
            2 => "eor",
            _ => "and",
        },
        K::LogImmS => {
            if d.rd == 31 { "tst" } else { "ands" }
        }
        K::LogSh => {
            // imm 低 1 位是 N(取反)，高位是逻辑运算种类
            let neg = d.imm & 1 == 1;
            match (d.imm >> 1, neg) {
                (0, true) => "bic",
                (1, true) => {
                    if d.rn == 31 {
                        "mvn" // orn Xd, XZR, Rm（实测 0x2a2003e0）
                    } else {
                        "orn"
                    }
                }
                (2, true) => "eon",
                (1, false) => {
                    // orr Rd, XZR, Rm 就是 mov
                    if d.rn == 31 {
                        "mov"
                    } else {
                        "orr"
                    }
                }
                (2, false) => "eor",
                _ => "and",
            }
        }
        K::LogShS => {
            if d.imm & 1 == 1 {
                "bics"
            } else if d.rd == 31 {
                "tst"
            } else {
                "ands"
            }
        }

        K::AddImm | K::AddSh | K::AddExt => {
            if d.flags {
                if d.rd == 31 {
                    "cmn"
                } else {
                    "adds"
                }
            } else if d.rn == 31 && d.imm == 0 {
                "mov" // mov Xd, SP
            } else {
                "add"
            }
        }
        K::SubImm | K::SubSh | K::SubExt => {
            let bare = d.sh == Sh::Lsl && d.amt == 0 && d.imm == 0;
            if d.flags {
                if d.rd == 31 {
                    "cmp"
                } else if d.rn == 31 && bare {
                    "negs"
                } else {
                    "subs"
                }
            } else if d.rn == 31 && bare {
                "neg"
            } else {
                "sub"
            }
        }
        K::AddCarry => {
            if d.flags {
                "adcs"
            } else {
                "adc"
            }
        }
        K::SubCarry => {
            if d.flags {
                if d.rd == 31 {
                    "sbcs"
                } else {
                    "sbc"
                }
            } else if d.rn == 31 {
                "ngc"
            } else {
                "sbc"
            }
        }
        K::Ccmp => "ccmp",
        K::Ccmn => "ccmn",
        K::Csel => "csel",
        // cset/csetm：csinc/csinv 且 Rn==Rm==31（实测 0x1a9f17e1 → cset w1, eq）。
        // cinc/cinv 的别名 r2 用得不一致（0x9a9f1400 明明是 csinc x0,x0,xzr,ne
        // 却不报 cinc），故不实现，统一报本名。
        K::Csinc => {
            if d.rn == 31 && d.rm == 31 {
                "cset"
            } else {
                "csinc"
            }
        }
        K::Csinv => {
            if d.rn == 31 && d.rm == 31 {
                "csetm"
            } else {
                "csinv"
            }
        }
        K::Csneg => "csneg",

        K::Ubfm => {
            let bits = if d.w32 { 32i64 } else { 64 };
            if d.imm2 == bits - 1 {
                "lsr"
            } else if d.imm == (d.imm2 + 1) % bits {
                "lsl"
            } else if d.imm2 < d.imm {
                "ubfiz"
            } else {
                "ubfx"
            }
        }
        K::Sbfm => {
            let bits = if d.w32 { 32i64 } else { 64 };
            if d.imm2 == bits - 1 {
                "asr"
            } else if d.imm == 0 && d.imm2 == 7 {
                "sxtb"
            } else if d.imm == 0 && d.imm2 == 15 {
                "sxth"
            } else if d.imm == 0 && d.imm2 == 31 && !d.w32 {
                "sxtw"
            } else if d.imm == 0 && d.imm2 == 31 && d.w32 {
                "sxth"
            } else if d.imm2 < d.imm {
                "sbfiz"
            } else {
                "sbfx"
            }
        }
        K::Bfm => {
            if d.imm2 + 1 == d.imm && d.imm != 0 {
                "bfxil"
            } else if d.imm == 0 {
                "bfi"
            } else {
                "bfm"
            }
        }
        K::Extr => {
            if d.rn == d.rm {
                "ror"
            } else {
                "extr"
            }
        }
        K::Rev16 => "rev16",
        K::Rev32 => "rev",
        K::Clz => "clz",
        K::Cls => "cls",
        K::Rbit => "rbit",

        K::Mul | K::Madd => {
            if d.ra == 31 {
                "mul"
            } else {
                "madd"
            }
        }
        K::Msub => {
            if d.ra == 31 {
                "mneg"
            } else {
                "msub"
            }
        }
        K::SMulH => "smulh",
        K::UMulH => "umulh",
        K::Smaddl => {
            if d.ra == 31 {
                "smull"
            } else {
                "smaddl"
            }
        }
        K::Smsubl => "smsubl",
        K::Umaddl => {
            if d.ra == 31 {
                "umull"
            } else {
                "umaddl"
            }
        }
        K::Umsubl => "umsubl",
        K::SDiv => "sdiv",
        K::UDiv => "udiv",

        K::LdrLit => "ldr",
        K::Ldr if d.unscaled => match (d.size, d.sext) {
            (1, true) => "ldursb",
            (1, false) => "ldurb",
            (2, true) => "ldursh",
            (2, false) => "ldurh",
            (4, true) => "ldursw",
            _ => "ldur",
        },
        K::Str if d.unscaled => match d.size {
            1 => "sturb",
            2 => "sturh",
            _ => "stur",
        },
        K::Ldr => match (d.size, d.sext) {
            (1, true) => "ldrsb",
            (1, false) => "ldrb",
            (2, true) => "ldrsh",
            (2, false) => "ldrh",
            (4, true) => "ldrsw",
            _ => "ldr",
        },
        K::Str => match d.size {
            1 => "strb",
            2 => "strh",
            _ => "str",
        },
        K::Ldp => {
            if d.vnt {
                "ldnp"
            } else if d.size == 4 && d.sext {
                "ldpsw"
            } else {
                "ldp"
            }
        }
        K::Stp => {
            if d.vnt {
                "stnp"
            } else {
                "stp"
            }
        }
        K::Ldxr => match d.size {
            1 => "ldxrb",
            2 => "ldxrh",
            _ => "ldxr",
        },
        K::Stxr => match d.size {
            1 => "stxrb",
            2 => "stxrh",
            _ => "stxr",
        },

        K::B => "b",
        K::Bl => "bl",
        K::Br => "br",
        K::Blr => "blr",
        K::Ret => "ret",
        K::Bc => match d.cond {
            0 => "b.eq", 1 => "b.ne", 2 => "b.cs", 3 => "b.cc", 4 => "b.mi", 5 => "b.pl",
            6 => "b.vs", 7 => "b.vc", 8 => "b.hi", 9 => "b.ls", 10 => "b.ge", 11 => "b.lt",
            12 => "b.gt", 13 => "b.le", 14 => "b.al", _ => "b.nv",
        },
        K::Cbz => "cbz",
        K::Cbnz => "cbnz",
        K::Tbz => "tbz",
        K::Tbnz => "tbnz",

        K::VecLdSt => {
            // r2 对 SIMD 访存也印 ldr/str/ldp/stp/ldnp/stnp，为了对拍一致跟着印
            if d.vnt {
                if d.vload { "ldnp" } else { "stnp" }
            } else if d.vpair {
                if d.vload { "ldp" } else { "stp" }
            } else if d.vload {
                "ldr"
            } else {
                "str"
            }
        }
        K::FmovGpr => "fmov",
        K::Sys => sys_name(d),
        K::Wfi => "wfi",
    }
}
