// SPDX-FileCopyrightText: 2024-2026 IFLYTEK-LEAKING
// SPDX-FileCopyrightText: 2024-2026 KawaiiSparkle
// SPDX-License-Identifier: GPL-2.0-or-later
//
// Locate and neutralise the Rockchip rockusb 32 MiB read-limit guard in an
// AArch64 U-Boot payload.  Port of analyse()/neutralise() and the decoding
// helpers in rkusb_nolimit.py (same authors, GPL-2.0-or-later).
//
// WHY: Rockchip's U-Boot caps how far rockusb (the USB-download gadget that
// RKDevTool / upgrade_tool talk to) is allowed to read:
//
//     include/rockusb.h:129
//         #define RKUSB_READ_LIMIT_ADDR   (32 * 2048)     /* 65536 sectors */
//
//     cmd/rockusb.c  rkusb_read_sector()
//         if ((blkstart + blkcnt) > RKUSB_READ_LIMIT_ADDR) {
//                 memset(buf, 0xcc, blkcnt * SECTOR_SIZE);
//                 return blkcnt;                          /* lies: "success" */
//         }
//
// So every sector at or beyond 32 MiB comes back as 0xCC and the read is
// *reported as successful*.  The fix is the one-line change from the RV1106
// U-Boot: make the guard impossible to take.  In the compiled image that is a
// single branch instruction rewritten in place to `b <blk_dread path>`
// (same length, so XIP images keep working).
//
// No disassembler needed - every pattern below is a fixed AArch64 encoding
// the compiler had no freedom in:
//
//   ldr w4, [x0, #0x18]     b9401804   ums->start_sector (struct ums:
//                                      read_sector@0x00 write_sector@0x08
//                                      erase_sector@0x10 start_sector@0x14
//                                      num_sectors@0x18 name@0x20
//                                      block_dev@0x28, sizeof 0xd0)
//   add x1, x4, x1          8b010081   blkstart = start + ums->start_sector
//   add x4, x1, x2          8b020024   blkstart + blkcnt
//   cmp x4, #0x10, lsl #12  f140409f   vs 0x10000 sectors == 32 MiB
//   b.ls  <blk_dread path>  54000149   <-- THE PATCH SITE (cond ls = 9 pairs
//                                      with the source's unsigned >)
//   mov w1, #0xcc           52801981   the memset fill value
//   add x0, x0, #0x28       9100a000   &ums->block_dev, on the normal path
//   lsl x2, x2, #9          d377d842   blkcnt * SECTOR_SIZE
//
// Encoding notes, because both are easy to get wrong:
//   * `cmp xN, #imm, lsl #s` is SUBS XZR:
//         sf<<31 | 11101001 | sh<<22 | imm12<<10 | Rn<<5 | 11111
//     `sh` is ONE bit (bit 22): 0 = no shift, 1 = `lsl #12`.  Treating it as
//     two bits silently yields a 4096x-too-small limit.
//   * `b.<cond>` is 01010100 | imm19<<5 | 0 | cond, so the low nibble is the
//     condition and bits 23:5 are a signed offset in instructions.

pub const RKUSB_LIMIT_SECTORS: u32 = 32 * 2048; // RKUSB_READ_LIMIT_ADDR, 65536 sectors = 32 MiB

pub const LDR_START_SECTOR: u32 = 0xB940_1804;
pub const ADD_BLKSTART: u32 = 0x8B01_0081;
pub const ADD_BLKEND: u32 = 0x8B02_0024;
pub const CMP_32MB: u32 = 0xF140_409F;
pub const MOV_0XCC: u32 = 0x5280_1981;
pub const ADD_BLOCK_DEV: u32 = 0x9100_A000;
pub const LSL9: u32 = 0xD377_D842;
/// `b +0x28` -> blk_dread.  THE correct fix; a plain `b #0` bricks the board.
pub const BRANCH_ALWAYS: u32 = 0x1400_000A;
/// the stock guard branch, used by the selftest
pub const B_LS: u32 = 0x5400_0149;

pub const COND: [&str; 16] = [
    "eq", "ne", "cs", "cc", "mi", "pl", "vs", "vc",
    "hi", "ls", "ge", "lt", "gt", "le", "al", "nv",
];

/// (cond, byte_delta) for a `b.<cond>`, else None.
pub fn decode_bcond(w: u32) -> Option<(u8, i32)> {
    if (w & 0xFF00_0010) != 0x5400_0000 {
        return None;
    }
    let mut imm = ((w >> 5) & 0x7_FFFF) as i32;
    if imm & (1 << 18) != 0 {
        imm -= 1 << 19;
    }
    Some(((w & 0xF) as u8, imm * 4))
}

/// byte_delta for an unconditional `b`, else None.
pub fn decode_b(w: u32) -> Option<i32> {
    if (w & 0xFC00_0000) != 0x1400_0000 {
        return None;
    }
    let mut imm = (w & 0x03FF_FFFF) as i32;
    if imm & (1 << 25) != 0 {
        imm -= 1 << 26;
    }
    Some(imm * 4)
}

/// The compared value if w is `cmp xN, #imm12{, lsl #12}`, else None.
pub fn cmp32mb_imm(w: u32) -> Option<u32> {
    if (w & 0x7F80_001F) != 0x7100_001F {
        return None;
    }
    Some(((w >> 10) & 0xFFF) << if (w >> 22) & 1 != 0 { 12 } else { 0 })
}

#[inline]
fn word(img: &[u8], off: usize) -> Option<u32> {
    if off + 4 > img.len() {
        return None;
    }
    Some(u32::from_le_bytes([img[off], img[off + 1], img[off + 2], img[off + 3]]))
}

#[derive(Debug, Clone)]
pub struct GuardHit {
    /// approximate VA of the rkusb_read_sector entry
    pub fn_va: u32,
    /// VA of the `cmp x4, #32MiB` instruction
    pub cmp_va: u32,
    /// VA of the branch to rewrite (THE patch site)
    pub guard: u32,
    /// true when the guard is already `b +0x28`
    pub patched: bool,
    pub cond: Option<u8>,
    /// VA of the normal path (blk_dread), when resolvable
    pub blk_dread: Option<u32>,
}

/// Find the guard in an AArch64 U-Boot payload.
///
/// `img`: the (decompressed) payload bytes; `base`: the link address the
/// payload was linked at.  Returns every hit (usually one).
///
/// The anchor is the run `add x1,x4,x1` / `add x4,x1,x2` / `cmp x4,#32MiB` /
/// `b.<cond>` - those four must be contiguous.  The `ldr w4,[x0,#0x18]` that
/// reads ums->start_sector and the `mov w1,#0xcc` fill are confirmed inside a
/// window instead, because the scheduler may interleave a spill between them
/// (RK3576 puts `str x19,[sp,#0x10]` right after the ldr; T30Pro/T90Pro do
/// not).  Requiring six contiguous words finds nothing at all on the RK3576
/// image.
pub fn analyse(img: &[u8], base: u32) -> Vec<GuardHit> {
    const W: usize = 16; // words to look around the anchor
    let mut hits = Vec::new();
    if img.len() < (W + 12) * 4 + 4 {
        return hits;
    }
    for off in (0..img.len() - (W + 12) * 4).step_by(4) {
        if word(img, off) != Some(ADD_BLKSTART)
            || word(img, off + 4) != Some(ADD_BLKEND)
            || word(img, off + 8) != Some(CMP_32MB)
        {
            continue;
        }
        let guard_w = match word(img, off + 12) {
            Some(w) => w,
            None => continue,
        };
        let decoded = decode_bcond(guard_w);
        let patched = guard_w == BRANCH_ALWAYS;
        let bad = guard_w == 0x1400_0000; // a previous tool's bricking b#0
        if decoded.is_none() && !(patched || bad) {
            continue;
        }
        let lo = off.saturating_sub(W * 4);
        let hi = (off + (W + 12) * 4).min(img.len());
        let mut has_ldr = false;
        let mut has_mov = false;
        let mut w = lo;
        while w + 4 <= hi {
            match word(img, w) {
                Some(LDR_START_SECTOR) => has_ldr = true,
                Some(MOV_0XCC) => has_mov = true,
                _ => {}
            }
            w += 4;
        }
        if !(has_ldr && has_mov) {
            continue;
        }
        let (cond, delta) = decoded.unwrap_or((0, 0));
        let tgt_i = off as i64 + 12 + delta as i64;
        let ok_tgt = tgt_i >= 0
            && (tgt_i as usize) + 4 <= img.len()
            && word(img, tgt_i as usize) == Some(ADD_BLOCK_DEV);
        hits.push(GuardHit {
            fn_va: (off as u32).wrapping_sub(4 * 4).wrapping_add(base),
            cmp_va: (off as u32 + 8).wrapping_add(base),
            guard: (off as u32 + 12).wrapping_add(base),
            patched,
            cond: if decoded.is_some() { Some(cond) } else { None },
            blk_dread: if ok_tgt { Some((tgt_i as u32) + base) } else { None },
        });
    }
    hits
}

/// Rewrite the guard branch to unconditionally jump to the original blk_dread
/// target.  Returns (new_payload, report).
///
/// The stock `b.<cond>` target is decoded from the instruction itself; the
/// broken `b #0` target cannot be self-decoded, but the blk_dread entry
/// (`add x0,x0,#0x28`) always sits at guard+0x28, so it is recovered from
/// that anchor and verified before writing.
pub fn neutralise(img: &[u8], base: u32, guard_va: u32) -> Result<(Vec<u8>, String), String> {
    let off = (guard_va - base) as usize;
    let w = word(img, off).ok_or_else(|| format!("guard VA 0x{guard_va:x} out of payload"))?;
    if w == BRANCH_ALWAYS {
        return Ok((img.to_vec(), "already correctly patched (b blk_dread)".into()));
    }
    let delta: i32 = if let Some((_cond, d)) = decode_bcond(w) {
        d // stock: use the original b.<cond> target
    } else if w == 0x1400_0000 {
        0x28 // broken b#0: target recovered from the blk_dread anchor
    } else {
        return Err(format!(
            "0x{guard_va:x} holds 0x{w:08x}, not patchable"
        ));
    };
    // verify before writing: the target must be add x0,x0,#0x28 (blk_dread entry)
    let tgt = (off as i64 + delta as i64) as usize;
    if word(img, tgt) != Some(ADD_BLOCK_DEV) {
        return Err(format!(
            "blk_dread anchor missing at 0x{:x}",
            (guard_va as i64 + delta as i64)
        ));
    }
    let word_new: u32 = 0x1400_0000 | (((delta >> 2) as u32) & 0x03ff_ffff);
    let mut b = img.to_vec();
    b[off..off + 4].copy_from_slice(&word_new.to_le_bytes());
    Ok((
        b,
        format!(
            "guard -> b +{delta:#x} (0x{word_new:08x}), was 0x{w:08x}"
        ),
    ))
}
