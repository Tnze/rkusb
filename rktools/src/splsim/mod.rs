// SPDX-FileCopyrightText: 2024-2026 IFLYTEK-LEAKING
// SPDX-License-Identifier: GPL-3.0-or-later
//
// Vendored from rockchip_splsim (https://github.com/iflytek-leaking/rockchip_splsim)
// by the same authors.  decode/emu/mem/mask/sha256/extract are verbatim copies
// of the upstream src/ modules; the orchestration below is a library-shaped
// port of upstream src/main.rs (anchor location, FIT scan, two-stage
// simulation, verdict) so `rktools` can drive it in-process.

pub mod decode;
pub mod emu;
pub mod extract;
pub mod mask;
pub mod mem;
pub mod sha256;

use emu::{Cpu, Stop};
use mem::Mem;

/// SPL 里定位 FIT 的那句日志
const ANCHOR: &[u8] = b"Trying fit image at 0x%lx sector\n";
/// verified-boot 门禁函数里打印的那句。定位它就能自动找到门禁函数。
const VB_ANCHOR: &[u8] = b"## Verified-boot: %d\n";
/// hw_fit_calculate_hash 里打印的那句。
const HASH_ANCHOR: &[u8] = b"Can't find expected crypto device\n";

/// `info->read` 的桩地址（选一个 SPL 不会用到的低地址）
const HOOK_READ: u64 = 0x0000_0000_0000_0100;
/// 放 spl_load_info 的地方
const INFO_ADDR: u64 = 0x0600_0000;
/// 调用者预先读好 FIT 头放这里（SPL 自己不读，见 0x380c `ldr w0,[x28]`）
const HDR_BUF: u64 = 0x0600_1000;
/// 哨兵返回地址：spl_load_simple_fit ret 到这里就算它返回了
const SENTINEL_RET: u64 = 0x0000_0000_0000_0200;
/// 栈
const STACK_TOP: u64 = 0x0620_0000;

fn find_str(b: &[u8], needle: &[u8]) -> Option<usize> {
    b.windows(needle.len()).position(|w| w == needle)
}

/// 函数序言：`stp x29, x30, [sp, #-N]!`
fn is_prologue(w: u32) -> bool {
    w & 0xffc0_7fff == 0xa980_7bfd
}

/// 用字符串锚点定位一个函数的入口（找 adrp+add 引用，再往前走到函数序言）
fn locate_by_anchor(spl: &[u8], needle: &[u8]) -> Option<usize> {
    use decode::{decode, K};
    let soff = find_str(spl, needle)?;
    let page = (soff & !0xfff) as u64;
    let off_in_page = (soff & 0xfff) as u64;
    let mut refs: Vec<usize> = Vec::new();
    let mut adrp_reg: Option<u8> = None;
    let mut a = 0usize;
    while a + 4 <= spl.len() {
        let w = u32::from_le_bytes([spl[a], spl[a + 1], spl[a + 2], spl[a + 3]]);
        let d = decode(w, a as u64);
        match d.k {
            K::Adrp => {
                if (d.imm as u64) & !0xfff == page {
                    adrp_reg = Some(d.rd);
                }
            }
            K::AddImm => {
                if let Some(r) = adrp_reg {
                    if d.rn == r && d.imm as u64 == off_in_page {
                        refs.push(a);
                    }
                }
            }
            _ => {}
        }
        a += 4;
    }
    for r in refs {
        let mut p = r;
        while p >= 4 {
            p -= 4;
            let w = u32::from_le_bytes([spl[p], spl[p + 1], spl[p + 2], spl[p + 3]]);
            if is_prologue(w) {
                return Some(p);
            }
        }
    }
    None
}

/// 收尾模式：厂商老 SPL = 内联版（add x0,x29,#imm2 → blr [x0+0x18]）；
/// 公版新 SPL = 分离版（mov x0,xRm → bl jump_to_image_no_args）
#[derive(Clone, Copy, Debug)]
enum TailMode {
    X29Add(u32),
    Reg(u8),
}

struct JumpTail {
    ldr_va: u64,
    print_va: u64,
    imm_entry: u32,
    ldr_rn: u8,
    mode: TailMode,
}

/// 找 "Jumping to U-Boot" 打印的收尾（两种编译形态都支持，全部从二进制扫出）
fn find_jump_tail(spl: &[u8], base: u64) -> Option<JumpTail> {
    let s = find_str(spl, b"Jumping to U-Boot")?;
    let sva = base + s as u64;
    let rd4 = |o: usize| u32::from_le_bytes(spl[o..o + 4].try_into().unwrap());
    let mut last_adrp = [None::<u64>; 32];
    let mut add_va = None;
    for off in (0..spl.len().saturating_sub(3)).step_by(4) {
        let w = rd4(off);
        if w & 0x9f00_0000 == 0x9000_0000 {
            let rd = (w & 31) as usize;
            let immlo = ((w >> 29) & 3) as i64;
            let immhi = ((w >> 5) & 0x7_ffff) as i64;
            let mut imm = (immhi << 2) | immlo;
            if imm & 0x10_0000 != 0 {
                imm -= 0x20_0000;
            }
            last_adrp[rd] =
                Some(((base + off as u64) & !0xfff).wrapping_add((imm << 12) as u64));
        } else if w & 0xff80_0000 == 0x9100_0000 {
            let rn = ((w >> 5) & 31) as usize;
            let imm12 = ((w >> 10) & 0xfff) as u64;
            if last_adrp[rn].map(|p| p + imm12) == Some(sva) {
                add_va = Some(base + off as u64);
            }
        }
    }
    let add_va = add_va?;
    let poff = (add_va - base) as usize + 4;
    if poff + 4 > spl.len() || rd4(poff) >> 26 != 0b100101 {
        return None; // add 的下一条必须是 bl printf
    }
    let print_va = add_va + 4;
    let mut ldr = None;
    for k in 1..=8u64 {
        let off = (add_va - base) as usize - 4 * k as usize;
        let w = rd4(off);
        if w & 0xffc0_03ff == 0xf940_03a1 {
            ldr = Some((base + off as u64, ((w >> 10) & 0xfff) * 8, 29u8));
            break;
        }
        if w & 0xffc0_03ff == 0xf940_03e1 {
            ldr = Some((base + off as u64, ((w >> 10) & 0xfff) * 8, 31u8));
            break;
        }
    }
    let (ldr_va, imm_entry, ldr_rn) = ldr?;
    for k in 1..=8u64 {
        let off = (print_va - base) as usize + 4 * k as usize;
        if off + 4 > spl.len() {
            break;
        }
        let w = rd4(off);
        // 模式 A：add x0,x29,#imm2（随后 bl → ldr x0,[x0,#0x18]; blr x0）
        if w & 0xffc0_03ff == 0x9100_03a0 {
            let imm_x0 = (w >> 10) & 0xfff;
            if imm_x0 != 0 {
                return Some(JumpTail {
                    ldr_va,
                    print_va,
                    imm_entry,
                    ldr_rn,
                    mode: TailMode::X29Add(imm_x0),
                });
            }
        }
        // 模式 B：mov x0,xRm（随后 bl jump_to_image_no_args）
        if w & 0xffe0_ffe0 == 0xaa00_03e0 {
            let rm = ((w >> 16) & 31) as u8;
            let rd = (w & 31) as u8;
            if rd == 0 && rm != 0 {
                return Some(JumpTail {
                    ldr_va,
                    print_va,
                    imm_entry,
                    ldr_rn,
                    mode: TailMode::Reg(rm),
                });
            }
        }
    }
    None
}

/// 定位结果
pub struct Located {
    /// spl_load_simple_fit 的入口（SPL 内偏移）
    pub loader: usize,
    /// printf 的入口（锚点字符串后面紧跟的那条 bl 的目标）
    pub printf: usize,
    /// verified-boot 门禁函数的入口
    pub vb_gate: usize,
    /// hw_fit_calculate_hash 的入口
    pub hash_fn: usize,
}

/// 用字符串锚点自动定位 spl_load_simple_fit 的入口和 printf
pub fn locate_fit_loader(spl: &[u8]) -> Option<Located> {
    use decode::{decode, K};
    let soff = find_str(spl, ANCHOR)?;
    let page = (soff & !0xfff) as u64;
    let off_in_page = (soff & 0xfff) as u64;
    let mut refs: Vec<usize> = Vec::new();
    let mut adrp_reg: Option<(u8, u64)> = None;
    let mut a = 0usize;
    while a + 4 <= spl.len() {
        let w = u32::from_le_bytes([spl[a], spl[a + 1], spl[a + 2], spl[a + 3]]);
        let d = decode(w, a as u64);
        match d.k {
            K::Adrp => {
                if (d.imm as u64) & !0xfff == page {
                    adrp_reg = Some((d.rd, d.imm as u64));
                }
            }
            K::AddImm => {
                if let Some((r, _)) = adrp_reg {
                    if d.rn == r && d.imm as u64 == off_in_page {
                        refs.push(a);
                    }
                }
            }
            _ => {}
        }
        a += 4;
    }
    let mut printf = 0usize;
    for &r in &refs {
        if r + 8 <= spl.len() {
            let w = u32::from_le_bytes([
                spl[r + 4],
                spl[r + 5],
                spl[r + 6],
                spl[r + 7],
            ]);
            let d = decode(w, (r + 4) as u64);
            if d.k == K::Bl {
                printf = d.imm as usize;
                break;
            }
        }
    }
    let vb_gate = locate_by_anchor(spl, VB_ANCHOR).unwrap_or(0);
    let hash_fn = locate_by_anchor(spl, HASH_ANCHOR).unwrap_or(0);
    for r in refs {
        let mut p = r;
        while p >= 4 {
            p -= 4;
            let w = u32::from_le_bytes([spl[p], spl[p + 1], spl[p + 2], spl[p + 3]]);
            if is_prologue(w) {
                return Some(Located {
                    loader: p,
                    printf,
                    vb_gate,
                    hash_fn,
                });
            }
        }
    }
    None
}

/// 一个 U-Boot FIT 候选（在整份 dump 里）
#[derive(Debug, Clone, Copy)]
pub struct FitCandidate {
    pub off: usize,
}

/// 扫描 dump 里所有 U-Boot FIT（/configurations 下有 firmware+loadables 属性名）。
///
/// 只凭 d00dfeed + 头合法性会筛进 SPL 内嵌控制 DTB；SPL 主里判据是
/// 字符串块同时含 "firmware" 和 "loadables"（属性名，不是节点名）。
pub fn scan_fits(dump: &[u8]) -> Vec<usize> {
    let be32 = |b: &[u8], i: usize| u32::from_be_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]);
    let mut fits = Vec::new();
    let mut o = 0usize;
    while o + 0x28 <= dump.len() {
        if &dump[o..o + 4] == &[0xd0, 0x0d, 0xfe, 0xed] {
            let ts = be32(dump, o + 4) as usize;
            let off_struct = be32(dump, o + 8) as usize;
            let off_strings = be32(dump, o + 0xc) as usize;
            let ver = be32(dump, o + 0x14);
            if ts > 0x100
                && ts < 0x400_0000
                && ver >= 16
                && off_struct < ts
                && off_strings < ts
                && off_struct <= off_strings
            {
                let s_start = o + off_strings;
                let s_len = be32(dump, o + 0x20) as usize;
                let s_end = s_start.saturating_add(s_len).min(dump.len());
                let block = if s_start < s_end { &dump[s_start..s_end] } else { &[] };
                let is_fit = block.windows(8).any(|w| w == b"firmware")
                    && block.windows(9).any(|w| w == b"loadables");
                if is_fit {
                    fits.push(o);
                }
            }
        }
        o += 4;
    }
    fits
}

/// 一次仿真的结论
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// SPL 打印了 "Jumping to U-Boot" 并跳转 —— 软件自洽校验完整通过
    JumpedToUBoot { entry: u64, steps: u64 },
    /// spl_load_simple_fit 返回 0 但没捕到 Jumping 打印
    LoadedNoJump { steps: u64 },
    /// spl_load_simple_fit 返回非 0
    LoadFailed { x0: i64, steps: u64 },
    /// 其它停止原因
    Stopped { reason: String, steps: u64 },
    /// 锚点没定位到（这份 SPL 的编译形态不支持）
    AnchorsNotFound,
}

impl Verdict {
    pub fn is_success(&self) -> bool {
        matches!(self, Verdict::JumpedToUBoot { .. })
    }
}

pub struct SimOptions {
    pub text_base: u64,
    pub max_steps: u64,
    /// vb fuse 字节（0x00 = 与可启动板一致）
    pub vb_flag: u8,
    pub quiet: bool,
}

impl Default for SimOptions {
    fn default() -> Self {
        Self {
            text_base: 0x20_0000,
            max_steps: 400_000_000,
            vb_flag: 0,
            quiet: true,
        }
    }
}

/// 对一个 FIT 跑完整的两阶段仿真（stage-1 spl_load_simple_fit + stage-2 spl_invoke 尾巴）。
pub fn simulate_fit(
    spl: &[u8],
    dump: &[u8],
    fit_off: usize,
    loc: &Located,
    opts: &SimOptions,
) -> Verdict {
    let text_base = opts.text_base;
    let sector = (fit_off / extract::SECTOR) as u64;
    let mut mem = Mem::new();
    mem.load_region(text_base, spl).expect("load SPL");
    for p in (0..0x1000).step_by(8) {
        mem.write(STACK_TOP - 0x1000 + p as u64, 8, 0).expect("stack");
    }
    mem.write(INFO_ADDR + 0x10, 4, extract::SECTOR as u64).expect("info bl_len");
    mem.write(INFO_ADDR + 0x18, 8, 0).expect("info +0x18");
    mem.write(INFO_ADDR + 0x20, 8, HOOK_READ).expect("info read hook");
    let hdr = &dump[fit_off..(fit_off + 0x200).min(dump.len())];
    mem.load_region(HDR_BUF, hdr).expect("FIT header");

    let mut cpu = Cpu::new(mem, text_base + loc.loader as u64);
    cpu.max_steps = opts.max_steps;
    cpu.mem.ov_base = text_base;
    cpu.mem.ov = spl.to_vec();
    cpu.hook_read = HOOK_READ;
    cpu.hook_printf = text_base + loc.printf as u64;
    cpu.hook_vb_gate = text_base + loc.vb_gate as u64;
    cpu.vb_flag_byte = opts.vb_flag;
    cpu.hook_hash = text_base + loc.hash_fn as u64;
    cpu.mem.watch_lo = text_base;
    cpu.mem.watch_hi = text_base + spl.len() as u64;
    cpu.code_lo = text_base;
    cpu.code_hi = text_base + spl.len() as u64;
    cpu.printf_capture = true;
    cpu.text_base = text_base;
    cpu.flash = dump.to_vec();
    cpu.flash_base_sector = 0;
    cpu.sp = STACK_TOP;
    cpu.x[0] = 0x0610_0000; // entry_pointp 输出缓冲
    cpu.x[1] = INFO_ADDR;
    cpu.x[2] = sector;
    cpu.x[3] = HDR_BUF;
    cpu.x[30] = SENTINEL_RET;
    cpu.sentinel_ret = SENTINEL_RET;

    let mut stop = cpu.run();

    // stage-2: spl_load_simple_fit 返回 0 后接 spl_invoke 尾巴
    if matches!(stop, Stop::Returned(0)) {
        if let Some(t) = find_jump_tail(spl, text_base) {
            let simg = 0x0610_0000u64;
            let base_reg = simg + 0x18 - t.imm_entry as u64;
            if t.ldr_rn == 31 {
                cpu.sp = base_reg;
                cpu.x[29] = base_reg;
            } else {
                cpu.x[29] = base_reg;
                cpu.sp = base_reg.wrapping_sub(0x400);
            }
            if let TailMode::Reg(rm) = t.mode {
                cpu.x[rm as usize] = simg;
            }
            cpu.x[30] = t.print_va + 4;
            cpu.pc = t.ldr_va;
            cpu.stop = None;
            stop = cpu.run();
        }
    }

    let steps = cpu.steps;
    match stop {
        Stop::JumpUBoot(entry) => {
            if cpu.saw_jumping {
                Verdict::JumpedToUBoot { entry, steps }
            } else {
                Verdict::LoadedNoJump { steps }
            }
        }
        Stop::Handoff(_) => Verdict::JumpedToUBoot { entry: cpu.jump_target.unwrap_or(0), steps },
        Stop::Returned(0) => Verdict::LoadedNoJump { steps },
        Stop::Returned(v) => Verdict::LoadFailed { x0: v, steps },
        Stop::Hang(a) => Verdict::Stopped { reason: format!("hang() at {a:#x}"), steps },
        Stop::Reset(a) => Verdict::Stopped { reason: format!("reset at {a:#x}"), steps },
        Stop::StepLimit => Verdict::Stopped { reason: "step limit".into(), steps },
        Stop::Fault(m) => Verdict::Stopped { reason: m, steps },
    }
}

/// 端到端：提取 SPL → 定位锚点 → 对每个 FIT 仿真，返回 (verdict, fit_off)。
/// 任何 FIT 完整通过即返回 success。
pub fn verify_dump(dump: &[u8], opts: &SimOptions) -> Result<Verdict, String> {
    let (spl, _ent, _idb) = extract::extract_spl_owned(dump)
        .map_err(|e| format!("SPL extraction failed: {e}"))?;
    let Some(loc) = locate_fit_loader(&spl) else {
        return Ok(Verdict::AnchorsNotFound);
    };
    let fits = scan_fits(dump);
    if fits.is_empty() {
        return Err("no U-Boot FIT in this dump".into());
    }
    let mut last = None;
    for &fit in &fits {
        let v = simulate_fit(&spl, dump, fit, &loc, opts);
        if v.is_success() {
            return Ok(v);
        }
        last = Some(v);
    }
    Ok(last.expect("fits nonempty"))
}
