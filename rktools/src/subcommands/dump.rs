use std::fs::OpenOptions;
use std::io::{Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::time::Duration;

use clap::Args as ClapArgs;

use crate::common;
use crate::nolimit::engine;
use crate::progress::ProgressBar;
use rkusb::RkDevice;

#[derive(ClapArgs)]
pub struct Args {
    /// Output directory (one <partition>.bin per partition)
    #[arg(default_value = "dump")]
    outdir: PathBuf,
    #[arg(long, help = "Bus number of the device")]
    bus: Option<u8>,
    #[arg(long, help = "Address of the device")]
    addr: Option<u8>,
    #[arg(
        long,
        value_parser = humantime::parse_duration,
        help = "Wait for device with timeout (e.g., 30s, 1m)"
    )]
    wait: Option<Duration>,
    #[arg(
        long,
        value_parser = humantime::parse_duration,
        default_value = "5s",
        help = "USB stall timeout: a chunk that makes no progress for this long is retried"
    )]
    timeout: Duration,
    /// Only back up these partitions (by name); default: all except the skip list
    #[arg(long, value_delimiter = ',')]
    partitions: Vec<String>,
    /// Also back up usually-skipped partitions (userdata, cache, ...)
    #[arg(long)]
    all: bool,
    /// Skip the inactive A/B slot suffixes (_b) when both slots exist
    #[arg(long)]
    lite: bool,
    /// Resume: keep existing files and read only the missing tail
    #[arg(long)]
    resume: bool,
    /// Do NOT stop on a detected 0xCC read wall (dangerous: output will
    /// contain the device's 0xCC fill past 32 MiB)
    #[arg(long)]
    allow_cc_wall: bool,
}

const SKIP_PARTITIONS: &[&str] = &["userdata", "cache", "misc", "metadata"];

pub fn exec(usb_ctx: rusb::Context, args: &Args) -> Result<(), Box<dyn std::error::Error>> {
    let selected_device = common::find_device(&usb_ctx, args.bus, args.addr, args.wait)?;
    let mut rkdev = RkDevice::open(&selected_device)?;
    let flash_info = rkdev.read_storage_info()?;
    let flash_sectors = flash_info.flash_size_sectors();
    let flash_bytes = flash_info.flash_size_bytes();
    println!(
        "Flash: {} sectors ({}), LBA size {}",
        flash_sectors,
        humansize::format_size(flash_bytes, humansize::BINARY),
        flash_info.lba_size()
    );

    // Read the GPT from the device (first 34 sectors are usually enough for
    // the header + a few hundred entries; re-read with more if truncated).
    let mut gpt_buf = vec![0u8; 34 * 512];
    rkdev.read_lba(1, &mut gpt_buf[512..], 0, args.timeout)?;
    let mut nentries = 0u32;
    if gpt_buf[512 + 72..512 + 76] != [0u8; 4] {
        // probe: entry count lives at header offset 80 (LBA1 + 0x50)
        nentries = u32::from_le_bytes(gpt_buf[512 + 80..512 + 84].try_into()?);
    }
    let need_sectors = if nentries > 0 {
        // entries LBA + n*esz/512, bounded by the disk
        let est = 2 + (nentries as usize * 128).div_ceil(512);
        est.min((flash_sectors as usize).max(34))
    } else {
        34
    };
    gpt_buf.resize(need_sectors * 512, 0);
    rkdev.read_lba(1, &mut gpt_buf[512..], 0, args.timeout)?;

    // The device returns sector 0 at offset 512 (we read from LBA 1), so the
    // GPT header sits at buffer offset 0. Re-align for the parser.
    let mut aligned = vec![0u8; 512 + gpt_buf.len()];
    aligned[512..].copy_from_slice(&gpt_buf);
    // parse: sector 0 placeholder + the sectors we read starting at LBA1
    let gpt_view = &aligned[512..];
    let parts = parse_partition_entries(gpt_view)?;

    if parts.is_empty() {
        return Err("no GPT partition entries found - cannot dump".into());
    }
    println!(
        "GPT: {} partitions{}",
        parts.len(),
        if args.lite { " (lite: skipping inactive slot _b)" } else { "" }
    );

    std::fs::create_dir_all(&args.outdir)?;
    let mut failures: Vec<String> = Vec::new();

    for (name, start, sectors) in &parts {
        let base = name.trim_end_matches(['\0', ' ']);
        if args.lite && base.ends_with("_b") {
            println!("skip {base} (lite mode)");
            continue;
        }
        if !args.all && !args.partitions.is_empty() && !args.partitions.iter().any(|p| p == base) {
            continue;
        }
        if !args.all && args.partitions.is_empty() && SKIP_PARTITIONS.contains(&base) {
            println!("skip {base} (default skip list, use --all or --partitions)");
            continue;
        }
        let out_path = args.outdir.join(format!("{base}.bin"));
        let mut start = *start;
        let mut sectors = *sectors;
        let mut existing: u64 = 0;
        if args.resume && out_path.exists() {
            let len = out_path.metadata()?.len();
            existing = (len / 512).min(sectors as u64);
            start += existing as u32;
            sectors -= existing as u32;
            if sectors == 0 {
                println!("done {base} (resume: complete)");
                continue;
            }
            println!("resume {base}: {} sectors already read", existing);
        }
        println!(
            "read {base}: LBA {start:#x} + {sectors} sectors ({})",
            humansize::format_size(sectors as u64 * 512, humansize::BINARY)
        );
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .open(&out_path)?;
        file.seek(SeekFrom::Start(existing * 512))?;

        let mut wall: Option<engine::CcWall> = None;
        let chunk_sectors = 1024usize.min(sectors.max(1) as usize); // 512 KiB chunks
        let mut remaining = sectors as usize;
        let mut abs_off = existing * 512;
        let total_bytes = existing * 512 + sectors as u64 * 512;
        let mut bar = ProgressBar::new(format!("read {base}"), total_bytes);
        bar.set(abs_off, total_bytes);
        while remaining > 0 {
            let n = chunk_sectors.min(remaining);
            let mut buf = vec![0u8; n * 512];
            if let Err(e) = rkdev.read_lba(start, &mut buf, 0, args.timeout) {
                failures.push(format!("{base}: read LBA {start:#x}: {e}"));
                break;
            }
            // 0xCC wall check: once we are at/past 32 MiB and the whole chunk
            // is 0xCC, the device hit the read limit - stop and report.
            if wall.is_none() && !args.allow_cc_wall && start >= 32 * 2048 {
                if let Some(w) = engine::find_cc_wall(&buf, 512 * 16) {
                    wall = Some(engine::CcWall {
                        start: abs_off + w.start,
                        len: w.len,
                    });
                    file.write_all(&buf[..w.start as usize])?;
                    failures.push(format!(
                        "{base}: 0xCC READ WALL at 0x{:x} (device refuses reads past 32 MiB). \
Output truncated to {}. Patch U-Boot first: rktools nl fix",
                        wall.as_ref().unwrap().start,
                        humansize::format_size(wall.as_ref().unwrap().start, humansize::BINARY),
                    ));
                    break;
                }
            }
            file.write_all(&buf)?;
            abs_off += buf.len() as u64;
            start += n as u32;
            remaining -= n;
            bar.set(abs_off, total_bytes);
        }
        if remaining == 0 {
            bar.finish();
        }
    }

    if failures.is_empty() {
        println!("\ndump complete -> {}", args.outdir.display());
        Ok(())
    } else {
        for f in &failures {
            eprintln!("FAIL: {f}");
        }
        Err(format!("{} partition(s) failed", failures.len()).into())
    }
}

/// Parse GPT partition entries from a buffer that starts at LBA 1 (the GPT
/// header).  Returns (name, start_lba, sectors) triples.
fn parse_partition_entries(buf: &[u8]) -> Result<Vec<(String, u32, u32)>, Box<dyn std::error::Error>> {
    if buf.len() < 92 || &buf[0..8] != b"EFI PART" {
        return Err("GPT header not found at LBA 1".into());
    }
    let ent_lba = u64::from_le_bytes(buf[72..80].try_into()?);
    let n = u32::from_le_bytes(buf[80..84].try_into()?);
    let esz = u32::from_le_bytes(buf[84..88].try_into()?);
    if esz == 0 || n == 0 {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for i in 0..n as usize {
        let off = ((ent_lba - 1) as usize) * 512 + i * esz as usize;
        if off + esz as usize > buf.len() {
            break;
        }
        let e = &buf[off..off + esz as usize];
        if e[..16].iter().all(|&b| b == 0) {
            continue;
        }
        let st = u64::from_le_bytes(e[32..40].try_into()?);
        let en = u64::from_le_bytes(e[40..48].try_into()?);
        let name_u16: Vec<u16> = e[56..128]
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        let mut name = String::from_utf16_lossy(&name_u16);
        while name.ends_with('\u{0}') {
            name.pop();
        }
        if en >= st && st > 0 && st <= u32::MAX as u64 && (en - st + 1) <= u32::MAX as u64 {
            out.push((name, st as u32, (en - st + 1) as u32));
        }
    }
    Ok(out)
}
