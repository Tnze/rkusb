// SPDX-FileCopyrightText: 2024-2026 IFLYTEK-LEAKING
// SPDX-FileCopyrightText: 2024-2026 KawaiiSparkle
// SPDX-License-Identifier: GPL-2.0-or-later
//
// `rktools nolimit`: remove the Rockchip rockusb 32 MiB read limit from a
// flash dump.  Rust port of rkusb_nolimit.py (PolyForm-NC -> GPL-2.0-or-later
// by the same authors) so it can ship inside the rkusb toolkit.

pub mod arm64;
pub mod classify;
pub mod engine;
pub mod fdt;
pub mod fit;
pub mod gpt;

use clap::Subcommand;
use std::path::PathBuf;

#[derive(Subcommand)]
pub enum NolimitCommands {
    /// classify + trim + patch + verify (the default)
    Fix {
        /// input dumps (default: every .bin/.img in --in)
        dumps: Vec<PathBuf>,
        /// directory to scan when no dumps are given
        #[arg(long, default_value = "in")]
        indir: PathBuf,
        /// where to write the patched images
        #[arg(long, default_value = "out")]
        outdir: PathBuf,
        /// patch only the FIT the SPL boots first
        #[arg(long)]
        only_primary: bool,
        #[arg(long)]
        no_verify: bool,
        #[arg(long)]
        dry_run: bool,
    },
    /// identify a dump, change nothing
    Classify { dump: PathBuf },
    /// cut a repeated-block garbage tail off
    Trim {
        dump: PathBuf,
        #[arg(short, long)]
        output: PathBuf,
    },
    /// locate the 32MB guard in a payload/FIT
    Limit {
        /// U-Boot payload (raw or gzipped), or a dump with --fit-off
        payload: PathBuf,
        /// link address; default: read from FIT /images/uboot
        #[arg(long)]
        base: Option<u32>,
        /// treat the file as a dump and read the FIT at this offset
        #[arg(long)]
        fit_off: Option<usize>,
    },
    /// patch one dump (no verify pass)
    Patch {
        dump: PathBuf,
        #[arg(short, long)]
        output: PathBuf,
        #[arg(long)]
        only_primary: bool,
        #[arg(long)]
        dry_run: bool,
    },
    /// re-check a patched dump
    Verify {
        /// patched dump
        patched: PathBuf,
        /// original dump (optional, only used for reporting)
        orig: Option<PathBuf>,
    },
}

impl std::fmt::Display for NolimitCommands {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NolimitCommands::Fix { .. } => write!(f, "fix"),
            NolimitCommands::Classify { .. } => write!(f, "classify"),
            NolimitCommands::Trim { .. } => write!(f, "trim"),
            NolimitCommands::Limit { .. } => write!(f, "limit"),
            NolimitCommands::Patch { .. } => write!(f, "patch"),
            NolimitCommands::Verify { .. } => write!(f, "verify"),
        }
    }
}

pub fn exec(cmd: &NolimitCommands) -> Result<(), Box<dyn std::error::Error>> {
    match cmd {
        NolimitCommands::Classify { dump } => {
            let d = std::fs::read(dump)?;
            classify::classify(&d, true);
            Ok(())
        }
        NolimitCommands::Trim { dump, output } => {
            let d = std::fs::read(dump)?;
            match engine::repeat_run_start(&d, 0x10000, 3) {
                Some((start, len)) => println!(
                    "  repeat-run garbage : starts 0x{start:x}, {len} bytes ({:.1} MiB)",
                    len as f64 / 1048576.0
                ),
                None => println!("  repeat-run garbage : none"),
            }
            let n = engine::trim_len(&d);
            std::fs::write(output, &d[..n])?;
            println!(
                "trim {}: wrote {} : {} bytes ({:.2} MiB), dropped {} bytes of tail",
                dump.display(),
                output.display(),
                n,
                n as f64 / 1048576.0,
                d.len() - n
            );
            Ok(())
        }
        NolimitCommands::Limit { payload, base, fit_off } => {
            let d = std::fs::read(payload)?;
            let (img, base) = match fit_off {
                Some(off) => {
                    let pl = fit::uboot_payload(&d, *off)?;
                    (pl.img, pl.load)
                }
                None => {
                    let b = base.ok_or("limit: pass --base or --fit-off")?;
                    (d.clone(), b)
                }
            };
            let hits = arm64::analyse(&img, base);
            if hits.is_empty() {
                println!("  no rkusb_read_sector 32MB guard found in this image");
                return Err("guard not found".into());
            }
            for h in hits {
                println!("  rkusb_read_sector guard found:");
                println!("    add x1, x4, x1          ; blkstart = start + ums->start_sector");
                println!("    add x4, x1, x2          ; blkstart + blkcnt");
                println!(
                    "    cmp x4, #0x10, lsl #12  ; {} sectors = 32 MiB   @0x{:x}",
                    arm64::RKUSB_LIMIT_SECTORS,
                    h.cmp_va
                );
                if h.patched {
                    println!("    b #0                    ; @0x{:x}  PATCHED", h.guard);
                } else {
                    let cond = h
                        .cond
                        .map(|c| arm64::COND[c as usize])
                        .unwrap_or("?");
                    println!(
                        "    b.{cond} #0x{:x}              ; @0x{:x}  <== PATCH SITE",
                        h.blk_dread.unwrap_or(0),
                        h.guard
                    );
                }
                if let Some(t) = h.blk_dread {
                    println!("    normal path @0x{t:x}: add x0, x0, #0x28 -> blk_dread");
                }
            }
            Ok(())
        }
        NolimitCommands::Patch { dump, output, only_primary, dry_run } => {
            let d = std::fs::read(dump)?;
            let patched = engine::patch_dump(&d, *only_primary, true)?;
            if *dry_run {
                println!("\n--dry-run: not writing {}", output.display());
                return Ok(());
            }
            std::fs::write(output, &patched)?;
            println!(
                "\nwrote {}  ({} bytes, md5 {})",
                output.display(),
                patched.len(),
                md5_hex(&patched)
            );
            Ok(())
        }
        NolimitCommands::Verify { patched, orig } => {
            let p = std::fs::read(patched)?;
            if let Some(o) = orig {
                let od = std::fs::read(o)?;
                println!(
                    "    size {} -> {}",
                    od.len(),
                    p.len()
                );
            }
            let r = engine::verify(&p, true);
            if !r.ok {
                return Err("verification FAILED".into());
            }
            println!("  {} checks passed", r.checks);
            // SPL -> U-Boot hand-off emulation (rockchip_splsim, GPL-3.0-or-later)
            println!("\n  --- SPL hand-off emulation ---");
            match crate::splsim::verify_dump(&p, &Default::default()) {
                Ok(v) if v.is_success() => {
                    println!("  SPL printed 'Jumping to U-Boot' - image is bootable");
                    Ok(())
                }
                Ok(crate::splsim::Verdict::AnchorsNotFound) => {
                    println!("  SPL anchors not found (unsupported SPL build) - skipped");
                    Ok(())
                }
                Ok(v) => Err(format!("hand-off emulation failed: {v:?}").into()),
                Err(e) if e.contains("no U-Boot FIT") => {
                    println!("  not a full dump (no FIT) - simulation skipped");
                    Ok(())
                }
                Err(e) => Err(format!("hand-off emulation error: {e}").into()),
            }
        }
        NolimitCommands::Fix { dumps, indir, outdir, only_primary, no_verify, dry_run } => {
            let files: Vec<PathBuf> = if !dumps.is_empty() {
                dumps.clone()
            } else {
                let mut v: Vec<PathBuf> = std::fs::read_dir(indir)?
                    .filter_map(|e| e.ok())
                    .map(|e| e.path())
                    .filter(|p| {
                        matches!(
                            p.extension().and_then(|e| e.to_str()),
                            Some("bin") | Some("img")
                        )
                    })
                    .collect();
                v.sort();
                v
            };
            if files.is_empty() {
                return Err(format!("no dumps found in {}", indir.display()).into());
            }
            std::fs::create_dir_all(outdir)?;
            let mut failed: Vec<String> = Vec::new();
            for path in &files {
                let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                    continue;
                };
                let stem = path
                    .file_stem()
                    .and_then(|n| n.to_str())
                    .unwrap_or("dump");
                let dest = outdir.join(format!("{stem}_nolimit.bin"));
                println!();
                println!("{}", "#".repeat(76));
                println!("# {name}");
                println!("{}", "#".repeat(76));
                let d = std::fs::read(path)?;
                let trim = engine::needs_trim(&d);
                if trim {
                    println!("[rktools nolimit] repeated-block garbage tail detected, will trim");
                }
                let res = engine::patch_dump(&d, *only_primary, true);
                match res {
                    Err(e) => {
                        println!("[rktools nolimit] FAILED: {e}");
                        failed.push(name.to_string());
                        continue;
                    }
                    Ok(mut patched) => {
                        if trim {
                            let n = engine::trim_len(&patched);
                            patched.truncate(n);
                        }
                        if *dry_run {
                            println!("\n--dry-run: not writing {}", dest.display());
                            continue;
                        }
                        std::fs::write(&dest, &patched)?;
                        println!(
                            "\nwrote {}  ({} bytes, md5 {})",
                            dest.display(),
                            patched.len(),
                            md5_hex(&patched)
                        );
                        if !*no_verify {
                            let r = engine::verify(&patched, false);
                            if !r.ok {
                                println!("[rktools nolimit] FAILED: verification");
                                failed.push(format!("{name} (verification)"));
                                continue;
                            }
                            println!("  verification: {} checks passed", r.checks);
                        }
                    }
                }
            }
            println!();
            println!("{}", "#".repeat(76));
            if !failed.is_empty() {
                println!("# {} input(s) failed: {}", failed.len(), failed.join(", "));
                return Err(format!("{} input(s) failed", failed.len()).into());
            }
            println!("# all {} input(s) patched -> {}", files.len(), outdir.display());
            Ok(())
        }
    }
}

fn md5_hex(d: &[u8]) -> String {
    use md5::Digest;
    hex::encode(md5::Md5::digest(d))
}
