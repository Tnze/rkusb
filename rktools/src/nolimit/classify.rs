// SPDX-FileCopyrightText: 2024-2026 IFLYTEK-LEAKING
// SPDX-FileCopyrightText: 2024-2026 KawaiiSparkle
// SPDX-License-Identifier: GPL-2.0-or-later
//
// Dump classification for the nolimit engine.  Port of classify() in
// rkusb_nolimit.py (same authors, GPL-2.0-or-later): identify a dump and
// locate the patchable guard(s) in every U-Boot FIT copy.

use super::arm64::{self, GuardHit};
use super::fit::{find_fits, is_uboot_fit, uboot_payload};
use super::gpt::{parse_gpt, read_ab, Gpt};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// eMMC first 32 MiB, uboot_a/uboot_b, gzip payload
    Emmc32,
    /// SPINOR 8/16 MiB, GPT matches where the FITs are
    SpiOk,
    /// SPINOR whose GPT does NOT match reality -> patch by real offset
    SpiBad,
    /// SPINOR holding no U-Boot at all (SPI+NVMe)
    NoUboot,
    /// no GPT at all
    NoGpt,
}

impl Verdict {
    pub fn kind(&self) -> &'static str {
        match self {
            Verdict::Emmc32 => "emmc32",
            Verdict::SpiOk => "spi_ok",
            Verdict::SpiBad => "spi_bad",
            Verdict::NoUboot => "NO_UBOOT",
            Verdict::NoGpt => "NO_GPT",
        }
    }

    pub fn note(&self) -> &'static str {
        match self {
            Verdict::Emmc32 => {
                "eMMC dump cut at 32 MiB: uboot_a/uboot_b FITs present (gzip payload expected)"
            }
            Verdict::SpiOk => "GPT matches reality",
            Verdict::SpiBad => {
                "GPT does NOT match reality - FIT sits outside its GPT partition;                  patch by the actual FIT offset and verify on the device before flashing"
            }
            Verdict::NoUboot => {
                "U-Boot is not on this chip. This looks like a SPI+NVMe / SPI+eMMC design:                  the SPI only holds idblock + misc + AvbABData. You need a dump of the other                  storage (NVMe / eMMC) to patch rockusb."
            }
            Verdict::NoGpt => "no GPT found in this dump",
        }
    }
}

#[derive(Debug, Clone)]
pub struct LimitInfo {
    /// FIT offset in the dump
    pub off: usize,
    pub primary: bool,
    /// "patched" | "ACTIVE" | "absent"
    pub state: &'static str,
    pub guard: Option<GuardHit>,
}

#[derive(Debug, Clone)]
pub struct ClassifyInfo {
    pub fits: Vec<usize>,
    pub primary: Option<usize>,
    pub limits: Vec<LimitInfo>,
    pub verdict: Verdict,
    pub ab: Option<super::gpt::AbInfo>,
    pub has_gpt: bool,
}

fn classify_verdict(d: &[u8], g: Option<&Gpt>, fits: &[usize]) -> Verdict {
    let gpt_present = g.is_some();
    if fits.is_empty() {
        return if gpt_present { Verdict::NoUboot } else { Verdict::NoGpt };
    }
    let Some(g) = g else {
        // FITs without any GPT: still patchable by offset
        return Verdict::SpiOk;
    };
    let mut starts = Vec::new();
    for nm in ["uboot", "uboot_a", "uboot_b"] {
        if let Some(p) = g.part(nm) {
            starts.push(p.off as usize);
        }
    }
    let on_target = fits.iter().any(|f| starts.contains(f));
    let emmc_like = d.len() <= 64 * 1024 * 1024 && starts.len() >= 2;
    if on_target {
        if emmc_like {
            Verdict::Emmc32
        } else {
            Verdict::SpiOk
        }
    } else {
        Verdict::SpiBad
    }
}

/// Identify a dump and locate the patchable guard(s).
pub fn classify(d: &[u8], verbose: bool) -> ClassifyInfo {
    let g = parse_gpt(d);
    let fits: Vec<usize> = find_fits(d)
        .into_iter()
        .map(|(off, _ts)| off)
        .filter(|&off| is_uboot_fit(d, off))
        .collect();

    let ab = g.as_ref().and_then(|g| read_ab(d, g));

    if verbose {
        let names: Vec<&str> = g
            .as_ref()
            .map(|g| g.parts.iter().map(|p| p.name.as_str()).collect())
            .unwrap_or_default();
        println!(
            "  {}   ({} bytes = {:.2} MiB)",
            "dump",
            d.len(),
            d.len() as f64 / 1048576.0
        );
        println!(
            "  GPT partitions      : {}",
            if names.is_empty() { "none".to_string() } else { names.join(", ") }
        );
        if let Some(ab) = &ab {
            println!(
                "\n  --- A/B metadata @0x{:x}  crc 0x{:08x} {} ---",
                ab.off,
                ab.crc,
                if ab.crc_ok { "OK" } else { "BAD" }
            );
            for (i, s) in ab.slots.iter().enumerate() {
                println!(
                    "    slot _{} priority={:<3} tries={:<2} successful={} bootable={}",
                    ['a', 'b'][i],
                    s.priority,
                    s.tries,
                    s.successful,
                    s.bootable
                );
            }
            println!("    => active slot _{}  ({})", ['a', 'b'][ab.index], ab.why);
        }
        println!("\n  --- U-Boot FIT search ---");
        if fits.is_empty() {
            println!("    no FIT containing /images/uboot anywhere in this dump");
        } else {
            println!(
                "    FITs at           : {}",
                fits.iter()
                    .map(|f| format!("0x{f:x}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
    }

    // primary selection: A/B active slot first, then uboot, then lowest FIT
    let mut primary: Option<usize> = None;
    if !fits.is_empty() {
        if let (Some(ab), Some(g)) = (&ab, &g) {
            let nm = format!("uboot_{}", ['a', 'b'][ab.index]);
            if let Some(p) = g.part(&nm) {
                if fits.contains(&(p.off as usize)) {
                    primary = Some(p.off as usize);
                    if verbose {
                        println!(
                            "\n  primary FIT         : 0x{:x}  ({}, per A/B metadata)",
                            primary.unwrap(),
                            nm
                        );
                    }
                }
            }
        }
        if primary.is_none() {
            if let Some(g) = &g {
                if let Some(p) = g.part("uboot") {
                    if fits.contains(&(p.off as usize)) {
                        primary = Some(p.off as usize);
                        if verbose {
                            println!(
                                "\n  primary FIT         : 0x{:x}  (no A/B, partition start)",
                                primary.unwrap()
                            );
                        }
                    }
                }
            }
        }
        if primary.is_none() {
            primary = Some(fits[0]);
            if verbose {
                println!(
                    "\n  primary FIT         : 0x{:x}  (lowest FIT found)",
                    fits[0]
                );
            }
        }
    }

    let mut limits = Vec::new();
    for &off in &fits {
        let tag = if Some(off) == primary { "PRIMARY" } else { "copy" };
        match uboot_payload(d, off) {
            Err(e) => {
                if verbose {
                    println!("    FIT 0x{off:x}: cannot read /images/uboot ({e})");
                }
                limits.push(LimitInfo {
                    off,
                    primary: Some(off) == primary,
                    state: "absent",
                    guard: None,
                });
            }
            Ok(pl) => {
                let hits = arm64::analyse(&pl.img, pl.load);
                if hits.is_empty() {
                    if verbose {
                        println!("    FIT 0x{off:x} ({tag}): no 32MB limit code found");
                    }
                    limits.push(LimitInfo {
                        off,
                        primary: Some(off) == primary,
                        state: "absent",
                        guard: None,
                    });
                } else {
                    let h = hits[0].clone();
                    let st = if h.patched { "patched" } else { "ACTIVE" };
                    if verbose {
                        println!(
                            "    FIT 0x{off:x} ({tag}): rkusb_read_sector=0x{:x} guard=0x{:x} -> {st}",
                            h.fn_va, h.guard
                        );
                    }
                    limits.push(LimitInfo {
                        off,
                        primary: Some(off) == primary,
                        state: st,
                        guard: Some(h),
                    });
                }
            }
        }
    }

    let verdict = classify_verdict(d, g.as_ref(), &fits);
    if verbose {
        println!("\n  >>> {}: {}", verdict.kind(), verdict.note());
    }

    ClassifyInfo {
        fits,
        primary,
        limits,
        verdict,
        ab,
        has_gpt: g.is_some(),
    }
}
