use std::path::PathBuf;

use clap::Args as ClapArgs;

use crate::splsim::{self, SimOptions};

#[derive(ClapArgs)]
pub struct Args {
    /// Flash dump containing a supported RKNS IDBlock/SPL and U-Boot FIT
    dump: PathBuf,
    /// SPL link address (default 0x200000; RK3576 uses 0x40200000)
    #[arg(long, value_parser = parse_u64_hex, default_value = "0x200000")]
    text_base: u64,
    /// Step budget for the simulation
    #[arg(long, default_value_t = 400_000_000)]
    max_steps: u64,
    /// Verified-boot fuse byte (0xff demonstrates the locked path)
    #[arg(long, value_parser = parse_u8_hex, default_value_t = 0)]
    vb_flag: u8,
    /// Only simulate the FIT at this byte offset
    #[arg(long, value_parser = parse_u64_hex)]
    fit: Option<u64>,
}

#[cfg(test)]
mod tests {
    use clap::{CommandFactory, FromArgMatches};

    #[test]
    fn default_and_explicit_spl_addresses_match_documentation() {
        for (extra, expected) in [(None, 0x200000), (Some("0x40200000"), 0x40200000)] {
            let mut argv = vec!["rktools", "splsim", "dump.bin"];
            if let Some(value) = extra {
                argv.extend(["--text-base", value]);
            }
            let matches = crate::Cli::command().try_get_matches_from(argv).unwrap();
            let cli = crate::Cli::from_arg_matches(&matches).unwrap();
            let crate::Commands::Splsim(args) = cli.command else { panic!("wrong command") };
            assert_eq!(args.text_base, expected);
        }
    }
}

fn parse_u64_hex(s: &str) -> Result<u64, String> {
    u64::from_str_radix(s.trim_start_matches("0x").trim_start_matches("0X"), 16)
        .map_err(|_| format!("{s} is not hex"))
}

fn parse_u8_hex(s: &str) -> Result<u8, String> {
    u8::from_str_radix(s.trim_start_matches("0x").trim_start_matches("0X"), 16)
        .map_err(|_| format!("{s} is not hex"))
}

pub fn exec(args: &Args) -> Result<(), Box<dyn std::error::Error>> {
    let dump = std::fs::read(&args.dump)?;
    let (spl, ent, idb) = splsim::extract::extract_spl_owned(&dump)
        .map_err(|e| format!("SPL extraction failed: {e}"))?;
    println!("dump        : {}", args.dump.display());
    println!("RKNS        : {idb:#x}");
    println!(
        "SPL entry   : @0x{:x} sect=0x{:x} n=0x{:x} -> 0x{:x}..0x{:x} ({} bytes)",
        ent.rec_off,
        ent.sect,
        ent.nsect,
        ent.abs,
        ent.abs + ent.size,
        spl.len()
    );
    let loc = splsim::locate_fit_loader(&spl)
        .ok_or("could not locate spl_load_simple_fit via string anchors - unsupported SPL build")?;
    println!("fit loader  : SPL+0x{:x}", loc.loader);
    println!("printf      : SPL+0x{:x} (hooked)", loc.printf);
    println!("vb gate     : SPL+0x{:x} (modeled as fuse-aware)", loc.vb_gate);
    println!("hash fn     : SPL+0x{:x}", loc.hash_fn);

    let fits = splsim::scan_fits(&dump);
    if fits.is_empty() {
        return Err("no U-Boot FIT in this dump".into());
    }
    println!(
        "FIT candidates: {} -> {:?}",
        fits.len(),
        fits.iter().map(|f| format!("{f:#x}")).collect::<Vec<_>>()
    );

    let opts = SimOptions {
        text_base: args.text_base,
        max_steps: args.max_steps,
        vb_flag: args.vb_flag,
        quiet: false,
    };

    let mut any_ok = false;
    for &fit in &fits {
        if let Some(w) = args.fit {
            if fit as u64 != w {
                continue;
            }
        }
        print!("FIT @0x{fit:x} ... ");
        let v = splsim::simulate_fit(&spl, &dump, fit, &loc, &opts);
        match &v {
            splsim::Verdict::JumpedToUBoot { entry, steps } => {
                println!(
                    "OK: SPL printed 'Jumping to U-Boot' and jumped to {entry:#x} (steps={steps})"
                );
                any_ok = true;
            }
            splsim::Verdict::LoadedNoJump { steps } => {
                println!("WARN: spl_load_simple_fit returned 0 but no Jumping print (steps={steps})");
            }
            splsim::Verdict::LoadFailed { x0, steps } => {
                println!("FAIL: returned {x0} (steps={steps})");
            }
            splsim::Verdict::Stopped { reason, steps } => {
                println!("FAIL: stopped ({reason}, steps={steps})");
            }
            splsim::Verdict::AnchorsNotFound => {
                println!("SKIP: anchors not found");
            }
        }
    }

    if any_ok {
        println!("\nSPL -> U-Boot hand-off emulation PASSED");
        Ok(())
    } else {
        Err("SPL -> U-Boot hand-off emulation FAILED".into())
    }
}
