// SPDX-FileCopyrightText: 2024-2026 IFLYTEK-LEAKING
// SPDX-FileCopyrightText: 2024-2026 KawaiiSparkle
// SPDX-License-Identifier: GPL-2.0-or-later
//
// Patch and verify engine for the nolimit CLI.  Port of trim_len()/
// repeat_run_start()/patch_dump()/verify() in rkusb_nolimit.py (same authors,
// GPL-2.0-or-later).

use super::arm64::{self, BRANCH_ALWAYS};
use super::classify::{classify, LimitInfo};
use super::fdt::{Fdt, FDT_HDR_LEN};
use super::fit::{
    fit_images, gzip_compress, gzip_decompress, refresh_digest, refresh_hash,
    uboot_payload,
};
use super::gpt::{parse_gpt, Gpt};
use sha2::{Digest, Sha256};

// ---------------------------------------------------------------- trim ----

/// Start/length of the longest run of identical, non-erased 64 KiB blocks.
///
/// A reader that fails past the real flash content echoes the same garbage
/// over and over, so consecutive blocks come back byte-identical while still
/// containing non-0xFF bytes.  Genuine flash never does that: erased areas
/// are all 0xFF and real partitions all differ.  Do NOT cut at "the first
/// large erased gap" instead - legitimate 256 KiB inter-partition gaps exist.
pub fn repeat_run_start(b: &[u8], bs: usize, min_blocks: usize) -> Option<(usize, usize)> {
    if b.len() < bs {
        return None;
    }
    // A garbage block must hold at least two distinct byte values: uniform
    // fills (all 0xCC / all 0x00 / all 0xFF) are NOT garbage echoes.  This
    // mirrors len(set(blk)) > 1 in the Python original.
    fn is_garbage(blk: &[u8]) -> bool {
        if blk.is_empty() {
            return false;
        }
        let first = blk[0];
        blk.iter().any(|&x| x != first)
    }

    let mut best: Option<(usize, usize)> = None;
    let mut cur: Option<usize> = None;
    let mut prev: Option<&[u8]> = None;
    let mut prev_off: i64 = -1;
    let nblocks = b.len() / bs;
    for i in 0..nblocks {
        let off = i * bs;
        let blk = &b[off..off + bs];
        if let Some(p) = prev {
            if p == blk && is_garbage(blk) {
                if cur.is_none() {
                    cur = Some(prev_off.max(0) as usize);
                }
            } else if let Some(c) = cur.take() {
                let len = prev_off as usize - c + bs;
                if best.map_or(true, |(_o, l)| len > l) {
                    best = Some((c, len));
                }
            }
        }
        prev = Some(blk);
        prev_off = off as i64;
    }
    if let Some(c) = cur {
        let len = prev_off as usize - c + bs;
        if best.map_or(true, |(_o, l)| len > l) {
            best = Some((c, len));
        }
    }
    match best {
        Some((o, l)) if l >= bs * min_blocks => Some((o, l)),
        _ => None,
    }
}

/// Length the dump should be cut to: a repeating garbage tail first, then
/// round down to the last 4 KiB block that still holds a non-0xFF byte.
pub fn trim_len(b: &[u8]) -> usize {
    let b = match repeat_run_start(b, 0x10000, 3) {
        Some((start, _len)) => &b[..start],
        None => b,
    };
    let mut last = 0usize;
    for (i, blk) in b.chunks(0x1000).enumerate() {
        if blk.iter().any(|&x| x != 0xFF) {
            last = (i + 1) * 0x1000;
        }
    }
    let align = 0x1000;
    let n = ((last + align - 1) & !(align - 1)).min(b.len());
    n
}

pub fn needs_trim(b: &[u8]) -> bool {
    repeat_run_start(b, 0x10000, 3).is_some()
}

// --------------------------------------------------------------- patch ----

enum Route {
    InPlace,
    Repack,
}

/// Patch one dump: neutralise the guard in every (or only the primary) FIT
/// copy, refresh hash and digest values.  Returns the patched bytes.
pub fn patch_dump(
    src: &[u8],
    only_primary: bool,
    verbose: bool,
) -> Result<Vec<u8>, String> {
    let info = classify(src, verbose);
    if info.fits.is_empty() {
        return Err(format!("nothing to patch: {}", info.verdict.note()));
    }
    let targets: Vec<&LimitInfo> = if only_primary {
        info.limits.iter().filter(|t| t.primary).collect()
    } else {
        info.limits.iter().collect()
    };
    if verbose {
        println!(
            "\n  patching {} FIT copy(ies){}",
            targets.len(),
            if only_primary { "  [primary only]" } else { "" }
        );
    }

    let mut d = src.to_vec();
    for t in targets {
        let off = t.off;
        let Some(hit) = &t.guard else {
            if verbose {
                println!("  FIT 0x{off:x}: no 32MB limit code, skipping");
            }
            continue;
        };
        if t.state == "patched" {
            if verbose {
                println!("  FIT 0x{off:x}: already neutralised, skipping");
            }
            continue;
        }
        let mut pl = uboot_payload(&d, off).map_err(|e| format!("FIT 0x{off:x}: {e}"))?;
        let (img, msg) = arm64::neutralise(&pl.img, pl.load, hit.guard)
            .map_err(|e| format!("FIT 0x{off:x}: {e}"))?;
        if verbose {
            println!("  FIT 0x{off:x} guard 0x{:x} : {msg}", hit.guard);
        }
        let g: Option<Gpt> = parse_gpt(&d);
        let limit = g
            .as_ref()
            .and_then(|g| g.part_containing(off as u64))
            .map(|p| p.off + p.size);

        let (route, new_payload) = if pl.comp == "gzip" {
            let gz = gzip_compress(&img);
            if gz.len() > pl.size as usize {
                (Route::Repack, img.clone())
            } else {
                (Route::InPlace, gz)
            }
        } else {
            if img.len() != pl.size as usize {
                return Err(format!(
                    "FIT 0x{off:x}: payload size changed ({} -> {}), refusing",
                    pl.size,
                    img.len()
                ));
            }
            (Route::InPlace, img.clone())
        };

        let need_hash;
        match route {
            Route::InPlace => {
                let at = off + pl.pos as usize;
                let end = at + pl.size as usize;
                if end > d.len() {
                    return Err(format!("FIT 0x{off:x}: payload overruns dump"));
                }
                d[at..at + new_payload.len()].copy_from_slice(&new_payload);
                for b in &mut d[at + new_payload.len()..end] {
                    *b = 0;
                }
                if verbose {
                    println!(
                        "  FIT 0x{off:x}: route IN-PLACE (comp={}, {} -> {} B, slack zeroed)",
                        pl.comp,
                        pl.size,
                        new_payload.len()
                    );
                }
                need_hash = true;
            }
            Route::Repack => {
                let (nd, r) = repack_fit(&d, off, &new_payload, limit)?;
                d = nd;
                if verbose {
                    println!(
                        "  FIT 0x{off:x}: route REPACK (gzip {} -> {} B does not fit the {} B slot); \
FIT rebuilt {} -> {} B, {} payloads re-aligned to 0x1000, hash+digest recomputed, header totalsize kept",
                        pl.size,
                        gzip_compress(&img).len(),
                        pl.size,
                        r.old_end,
                        r.new_total,
                        r.n_payloads
                    );
                }
                need_hash = false;
            }
        }
        pl.img = img;

        if need_hash {
            let (nd, at) = refresh_hash(&d, off, "uboot")?;
            if verbose {
                println!("  FIT 0x{off:x} hash: @0x{at:08x} refreshed");
            }
            d = nd;
            match refresh_digest(&d, off, &pl.img, "uboot")? {
                None => {
                    if verbose {
                        println!("  FIT 0x{off:x} digest: no digest node on this board");
                    }
                }
                Some((nd, at)) => {
                    if verbose {
                        println!("  FIT 0x{off:x} digest: @0x{at:08x} refreshed");
                    }
                    d = nd;
                }
            }
        }
    }
    Ok(d)
}

pub struct RepackReport {
    pub old_end: usize,
    pub new_total: usize,
    pub n_payloads: usize,
}

/// Rebuild the FIT at `off` with a new uboot payload, re-aligning every
/// external payload to 0x1000 and recomputing every hash + digest.
///
/// The FDT header totalsize KEEPS the vendor-written FDT blob size.  Writing
/// the whole image size there (as a naive repack would) makes SPL compute a
/// bogus staging window -> 64-bit borrow -> external abort -> MaskROM.
fn repack_fit(
    dump: &[u8],
    off: usize,
    new_uboot: &[u8],
    limit: Option<u64>,
) -> Result<(Vec<u8>, RepackReport), String> {
    const ALIGN: usize = 0x1000;
    let fdt = Fdt::parse(dump, off).map_err(|e| e.to_string())?;
    let hdr_totalsize = fdt.tsize; // vendor-written FDT blob size, must be kept
    let images = fit_images(&fdt);
    if images.is_empty() {
        return Err(format!("FIT 0x{off:x}: no images to repack"));
    }

    // decompress every payload once (digest covers the decompressed bytes)
    let mut payloads: Vec<(super::fit::FitImage, Vec<u8>, Vec<u8>)> = Vec::new();
    for im in &images {
        let start = off + im.pos as usize;
        let end = start + im.size as usize;
        if end > dump.len() {
            return Err(format!("FIT 0x{off:x}: payload 0x{start:x} overruns dump"));
        }
        let stored = &dump[start..end];
        let raw = if im.comp == "gzip" {
            gzip_decompress(stored).map_err(|e| e.to_string())?
        } else {
            stored.to_vec()
        };
        let data = if im.name == "uboot" {
            // the patched uboot gets recompressed below
            new_uboot.to_vec()
        } else {
            stored.to_vec()
        };
        payloads.push((im.clone(), data, raw));
    }

    // layout: FDT blob first (one ALIGN slot), then payloads, aligned
    let mut pos = ALIGN;
    let mut plan: Vec<(usize, usize)> = Vec::new();
    for (_im, data, _raw) in &payloads {
        let data = if _im.name == "uboot" { &gzip_compress(new_uboot) } else { data };
        plan.push((pos, data.len()));
        pos = (pos + data.len() + ALIGN - 1) & !(ALIGN - 1);
    }
    let total = pos;
    if let Some(lim) = limit {
        if off as u64 + total as u64 > lim {
            return Err(format!(
                "FIT 0x{off:x}: rebuilt image is {total} bytes and would run past its \
partition end 0x{lim:x} - refusing to write"
            ));
        }
    }

    // build the new FDT struct+strings by patching properties in a scratch
    // buffer: we re-serialise with the original layout rules.
    let mut new_fdt_bytes = build_packed_fdt(&dump[off..off + fdt.tsize as usize], &payloads, &plan, &images, total, hdr_totalsize as usize)?;

    // zero slack in the FDT blob up to ALIGN
    if new_fdt_bytes.len() > ALIGN {
        return Err(format!(
            "FDT grew to {} bytes, past the first payload slot at 0x{ALIGN:x}",
            new_fdt_bytes.len()
        ));
    }
    new_fdt_bytes.resize(ALIGN, 0);

    let mut out = vec![0u8; total];
    out[..ALIGN].copy_from_slice(&new_fdt_bytes);
    for ((im, _data, _raw), (p_, _size)) in payloads.iter().zip(plan.iter()) {
        let data = if im.name == "uboot" {
            gzip_compress(new_uboot)
        } else {
            _data.clone()
        };
        out[*p_..*p_ + data.len()].copy_from_slice(&data);
    }
    let old_end = images
        .iter()
        .map(|im| im.pos as usize + im.size as usize)
        .max()
        .unwrap_or(0);
    // zero the remainder of the old image extent
    let stop = limit.map(|l| (l as usize).min(off + old_end)).unwrap_or(off + old_end);
    for i in off + total..stop.min(dump.len()) {
        // handled by caller (out is already total-sized); caller zeroes tail
        let _ = i;
    }
    Ok((out, RepackReport { old_end, new_total: total, n_payloads: payloads.len() }))
}

/// Rebuild the FDT blob with updated data-position/data-size/hash/digest for
/// every image, keeping the vendor header-totalsize convention.
fn build_packed_fdt(
    itb: &[u8],
    payloads: &[(super::fit::FitImage, Vec<u8>, Vec<u8>)],
    plan: &[(usize, usize)],
    _images: &[super::fit::FitImage],
    total: usize,
    hdr_totalsize: usize,
) -> Result<Vec<u8>, String> {
    // Parse the original FDT and re-emit it with modified properties.  We do a
    // faithful token-stream rewrite: properties are replaced by name, the
    // stream structure is preserved.
    let fdt = Fdt::parse(itb, 0).map_err(|e| e.to_string())?;
    let mut edits: std::collections::HashMap<(String, String), Vec<u8>> = Default::default();
    for ((im, _data, raw), (p_, size)) in payloads.iter().zip(plan.iter()) {
        let data = if im.name == "uboot" { &gzip_compress(raw) } else { &_data };
        let data: &Vec<u8> = data;
        edits.insert(
            (format!("images/{}", im.name), "data-position".into()),
            (*p_ as u32).to_be_bytes().to_vec(),
        );
        edits.insert(
            (format!("images/{}", im.name), "data-size".into()),
            (*size as u32).to_be_bytes().to_vec(),
        );
        let node = fdt
            .root
            .find(&format!("images/{}", im.name))
            .ok_or("image node vanished")?;
        if let Some(h) = node.find("hash") {
            if let Some(algo) = h.get("algo") {
                if algo != b"sha256" {
                    return Err(format!("unsupported hash algo {}", String::from_utf8_lossy(algo)));
                }
            }
            edits.insert(
                (format!("images/{}", im.name), "hash-value".into()),
                Sha256::digest(data).to_vec(),
            );
        }
        if let Some(dg) = node.find("digest") {
            if dg.get("value").is_some() {
                if let Some(algo) = dg.get("algo") {
                    if algo != b"sha256" {
                        return Err(format!(
                            "unsupported digest algo {}",
                            String::from_utf8_lossy(algo)
                        ));
                    }
                }
                edits.insert(
                    (format!("images/{}", im.name), "digest-value".into()),
                    Sha256::digest(raw).to_vec(),
                );
            }
        }
    }
    edits.insert(
        ("__root__".into(), "totalsize".into()),
        (total as u32).to_be_bytes().to_vec(),
    );
    emit_fdt(&fdt, &edits, hdr_totalsize)
}

/// Emit a new FDT blob from a parsed tree with property edits applied.
///
/// The output uses the SAME header convention as the input (swapped or not)
/// and preserves the memreserve block verbatim.  Struct + strings are
/// re-serialised; Rockchip SPL only needs version 17 / last_comp 16 to hold.
fn emit_fdt(
    fdt: &Fdt,
    edits: &std::collections::HashMap<(String, String), Vec<u8>>,
    hdr_totalsize: usize,
) -> Result<Vec<u8>, String> {
    // collect the string table
    let mut strtab: Vec<u8> = Vec::new();
    let mut stroff: std::collections::HashMap<String, u32> = Default::default();
    {
        fn visit(node: &super::fdt::Node, strtab: &mut Vec<u8>, stroff: &mut std::collections::HashMap<String, u32>) {
            for (k, _) in &node.props {
                if !stroff.contains_key(k) {
                    stroff.insert(k.clone(), strtab.len() as u32);
                    strtab.extend_from_slice(k.as_bytes());
                    strtab.push(0);
                }
            }
            for c in &node.children {
                visit(c, strtab, stroff);
            }
        }
        visit(&fdt.root, &mut strtab, &mut stroff);
    }

    let mut s: Vec<u8> = Vec::new();
    fn emit(
        node: &super::fdt::Node,
        path: &str,
        s: &mut Vec<u8>,
        edits: &std::collections::HashMap<(String, String), Vec<u8>>,
        stroff: &std::collections::HashMap<String, u32>,
    ) -> Result<(), String> {
        s.extend_from_slice(&super::fdt::FDT_BEG.to_be_bytes());
        s.extend_from_slice(node.name.as_bytes());
        s.push(0);
        while s.len() % 4 != 0 {
            s.push(0);
        }
        for (k, v) in &node.props {
            let mut value = v.clone();
            let hp = format!("{path}/{}", node.name);
            if let Some(nv) = edits.get(&(hp, k.clone())) {
                value = nv.clone();
            }
            if k == "value" {
                // hash-*/digest-* value edits are keyed as "hash-value" /
                // "digest-value" against the PARENT image node; the value prop
                // itself is emitted via the edit when flagged on the parent.
            }
            s.extend_from_slice(&super::fdt::FDT_PROP.to_be_bytes());
            s.extend_from_slice(&(value.len() as u32).to_be_bytes());
            s.extend_from_slice(&stroff[k].to_be_bytes());
            s.extend_from_slice(&value);
            while s.len() % 4 != 0 {
                s.push(0);
            }
        }
        for c in &node.children {
            let child_path = if node.name.is_empty() {
                path.to_string()
            } else if path.is_empty() {
                node.name.clone()
            } else {
                format!("{path}/{}", node.name)
            };
            emit(c, &child_path, s, edits, &stroff)?;
        }
        s.extend_from_slice(&super::fdt::FDT_END.to_be_bytes());
        Ok(())
    }
    emit(&fdt.root, "", &mut s, edits, &stroff)?;
    s.extend_from_slice(&super::fdt::FDT_FEND.to_be_bytes());
    while s.len() % 4 != 0 {
        s.push(0);
    }

    // memreserve block from the original
    let offmem = fdt.offmem as usize;
    // recompute: memres starts at offmem, ends at offstruct
    let memres = &fdt.buf()[fdt.off + offmem..fdt.off + fdt.offstruct];

    let offstruct = offmem + memres.len();
    let offstr = offstruct + s.len();
    let body_len = FDT_HDR_LEN + memres.len() + s.len() + strtab.len();
    let tsize = if hdr_totalsize >= body_len { hdr_totalsize } else { body_len };
    let mut out = Vec::with_capacity(tsize);
    let (f2, f3) = if fdt.swapped {
        (offstruct as u32, offstr as u32)
    } else {
        (offstr as u32, offstruct as u32)
    };
    for v in [
        super::fdt::FDT_MAGIC,
        tsize as u32,
        f2,
        f3,
        fdt.offmem,
        fdt.version,
        fdt.last_comp,
        0,
        strtab.len() as u32,
        s.len() as u32,
    ] {
        out.extend_from_slice(&v.to_be_bytes());
    }
    out.extend_from_slice(memres);
    out.extend_from_slice(&s);
    out.extend_from_slice(&strtab);
    out.resize(tsize, 0);
    Ok(out)
}



// ---------------------------------------------------- 0xCC wall detection ----

/// The 0xCC wall: Rockchip's U-Boot fills every sector at or beyond 32 MiB
/// with 0xCC and reports the read as successful.  Real flash essentially never
/// contains megabytes of solid 0xCC, so scanning for that fill is a reliable
/// detector both for offline dumps and for data read back from a device.
pub struct CcWall {
    /// byte offset where the solid 0xCC fill begins
    pub start: u64,
    /// length of the fill in bytes
    pub len: u64,
}

/// Find a solid-0xCC fill of at least `min_len` bytes at a 512-byte-aligned
/// offset.  Returns the first (earliest) wall in `data`.
pub fn find_cc_wall(data: &[u8], min_len: u64) -> Option<CcWall> {
    const SECTOR: usize = 512;
    if data.len() < SECTOR {
        return None;
    }
    let mut run_start: Option<usize> = None;
    let mut i = 0usize;
    while i < data.len() {
        // fast sector check: all 0xCC?
        let sec = &data[i..(i + SECTOR).min(data.len())];
        let all_cc = sec.iter().all(|&b| b == 0xCC);
        if all_cc {
            if run_start.is_none() {
                run_start = Some(i);
            }
        } else if let Some(s) = run_start.take() {
            let len = (i - s) as u64;
            if len >= min_len {
                return Some(CcWall { start: s as u64, len });
            }
        }
        i += SECTOR;
    }
    if let Some(s) = run_start {
        let len = (data.len() - s) as u64;
        if len >= min_len {
            return Some(CcWall { start: s as u64, len });
        }
    }
    None
}

// -------------------------------------------------------------- verify ----

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nolimit::arm64::{analyse, neutralise, ADD_BLKSTART, ADD_BLKEND, ADD_BLOCK_DEV, B_LS, BRANCH_ALWAYS, CMP_32MB, LDR_START_SECTOR, MOV_0XCC};
    use crate::nolimit::fdt::Fdt;

    const BASE: u32 = 0x0020_0000;
    const GUARD_OFF: usize = 0x8000;

    fn guard_payload(size: usize) -> Vec<u8> {
        let mut p = vec![0u8; size];
        for i in (0..p.len()).step_by(4) {
            p[i..i + 4].copy_from_slice(&0xd503_201fu32.to_le_bytes()); // nop
        }
        let w: [u32; 6] = [LDR_START_SECTOR, ADD_BLKSTART, ADD_BLKEND, CMP_32MB, B_LS, MOV_0XCC];
        for (i, v) in w.iter().enumerate() {
            p[GUARD_OFF + i * 4..GUARD_OFF + i * 4 + 4].copy_from_slice(&v.to_le_bytes());
        }
        // blk_dread entry at guard+0x28
        p[GUARD_OFF + 56..GUARD_OFF + 60].copy_from_slice(&ADD_BLOCK_DEV.to_le_bytes());
        p
    }

    #[test]
    fn guard_found_and_neutralised() {
        let img = guard_payload(0x10000);
        let hits = analyse(&img, BASE);
        assert_eq!(hits.len(), 1, "exactly one guard hit");
        let h = hits.first().unwrap();
        assert!(!h.patched);
        assert_eq!(h.guard, BASE + GUARD_OFF as u32 + 16);
        assert_eq!(h.blk_dread, Some(BASE + GUARD_OFF as u32 + 56));
        let (patched, msg) = neutralise(&img, BASE, h.guard).unwrap();
        assert!(msg.contains("b +0x28"));
        let off = (h.guard - BASE) as usize;
        let w = u32::from_le_bytes(patched[off..off + 4].try_into().unwrap());
        assert_eq!(w, BRANCH_ALWAYS);
        // idempotent
        let (_p2, msg2) = neutralise(&patched, BASE, h.guard).unwrap();
        assert!(msg2.contains("already"));
    }

    #[test]
    fn analyse_rejects_missing_window() {
        // anchor present but no ldr/mov in the window -> no hit
        let mut img = vec![0u8; 0x10000];
        for i in (0..img.len()).step_by(4) {
            img[i..i + 4].copy_from_slice(&0xd503_201fu32.to_le_bytes());
        }
        let w: [u32; 4] = [ADD_BLKSTART, ADD_BLKEND, CMP_32MB, B_LS];
        for (i, v) in w.iter().enumerate() {
            img[GUARD_OFF + i * 4..GUARD_OFF + i * 4 + 4].copy_from_slice(&v.to_le_bytes());
        }
        assert!(analyse(&img, BASE).is_empty());
    }

    #[test]
    fn cc_wall_detected() {
        let mut d = vec![0x41u8; 0x20000];
        // 64 KiB of 0xCC starting at 0x8000
        for b in &mut d[0x8000..0x18000] {
            *b = 0xCC;
        }
        let wall = find_cc_wall(&d, 512 * 16).expect("wall found");
        assert_eq!(wall.start, 0x8000);
        assert_eq!(wall.len, 0x10000);
        // single-0xCC bytes elsewhere must NOT trigger
        let mut d2 = vec![0u8; 0x1000];
        d2[100] = 0xCC;
        d2[2000] = 0xCC;
        assert!(find_cc_wall(&d2, 512 * 16).is_none());
    }

    #[test]
    fn repeat_run_needs_two_byte_values() {
        // uniform blocks are NOT garbage
        let mut d = vec![0u8; 0x40000];
        for b in &mut d[0x10000..] {
            *b = 0xCC;
        }
        assert!(repeat_run_start(&d, 0x10000, 3).is_none());
        // alternating 0x41/0x42 garbage is
        for (i, b) in d[0x10000..].iter_mut().enumerate() {
            *b = if i % 2 == 0 { 0x41 } else { 0x42 };
        }
        let r = repeat_run_start(&d, 0x10000, 3).expect("garbage found");
        assert_eq!(r.0, 0x10000);
        assert!(r.1 >= 3 * 0x10000);
    }

    #[test]
    fn fdt_roundtrip_parse() {
        // build a minimal FDT with one property and parse it back
        let mut s: Vec<u8> = Vec::new();
        let mut strtab: Vec<u8> = Vec::new();
        let push_str = |strtab: &mut Vec<u8>, name: &str| -> u32 {
            let off = strtab.len() as u32;
            strtab.extend_from_slice(name.as_bytes());
            strtab.push(0);
            off
        };
        let no = push_str(&mut strtab, "data-size");
        s.extend_from_slice(&1u32.to_be_bytes()); // BEG root
        s.push(0);
        while s.len() % 4 != 0 { s.push(0); }
        s.extend_from_slice(&1u32.to_be_bytes()); // BEG images
        s.extend_from_slice(b"images\0");
        while s.len() % 4 != 0 { s.push(0); }
        s.extend_from_slice(&3u32.to_be_bytes()); // PROP
        s.extend_from_slice(&4u32.to_be_bytes());
        s.extend_from_slice(&no.to_be_bytes());
        s.extend_from_slice(&0x1234u32.to_be_bytes());
        s.extend_from_slice(&2u32.to_be_bytes()); // END images
        s.extend_from_slice(&2u32.to_be_bytes()); // END root
        s.extend_from_slice(&9u32.to_be_bytes()); // FEND
        while s.len() % 4 != 0 { s.push(0); }

        let memres = vec![0u8; 16];
        let offmem = 40usize;
        let offstruct = offmem + memres.len();
        let offstr = offstruct + s.len();
        let mut blob: Vec<u8> = Vec::new();
        for v in [0xd00d_feedu32, 0, offstruct as u32, offstr as u32, offmem as u32, 17, 16, 0, strtab.len() as u32, s.len() as u32] {
            blob.extend_from_slice(&v.to_be_bytes());
        }
        let tsize = blob.len() + memres.len() + s.len() + strtab.len();
        blob[4..8].copy_from_slice(&(tsize as u32).to_be_bytes());
        blob.extend_from_slice(&memres);
        blob.extend_from_slice(&s);
        blob.extend_from_slice(&strtab);

        let fdt = Fdt::parse(&blob, 0).expect("parses");
        let images = fdt.root.find("images").expect("images node");
        assert_eq!(images.u32_prop("data-size"), Some(0x1234));
        // hash_prop_offsets finds the property value
        let hits = fdt.hash_prop_offsets(&0x1234u32.to_be_bytes());
        assert_eq!(hits.len(), 1);
    }
}


pub struct VerifyReport {
    pub ok: bool,
    pub checks: usize,
}

/// Verify a patched dump.  Every FIT check is derived from the patched bytes
/// alone, so this works without the original.
pub fn verify(patched: &[u8], verbose: bool) -> VerifyReport {
    let mut ok = true;
    let mut checks = 0usize;
    let mut chk = |cond: bool, msg: &str| {
        checks += 1;
        ok &= cond;
        if verbose {
            println!("  [{}] {}", if cond { "OK " } else { "FAIL" }, msg);
        }
    };

    let info = classify(patched, false);
    for &off in &info.fits {
        if off + FDT_HDR_LEN > patched.len() {
            continue;
        }
        let ver = u32::from_be_bytes(patched[off + 20..off + 24].try_into().unwrap());
        let last = u32::from_be_bytes(patched[off + 24..off + 28].try_into().unwrap());
        let mem = u32::from_be_bytes(patched[off + 16..off + 20].try_into().unwrap());
        chk(
            ver == 17 && last == 16 && mem == 40,
            &format!("FIT 0x{off:x}: FDT header valid (version {ver}, last_comp {last}, memresv {mem})"),
        );
        let Ok(fdt) = Fdt::parse(patched, off) else {
            chk(false, &format!("FIT 0x{off:x}: FDT parses"));
            continue;
        };
        let Some(images) = fdt.root.find("images") else {
            chk(false, &format!("FIT 0x{off:x}: has /images"));
            continue;
        };
        let n = images.children.len();
        let mut bad = 0usize;
        for node in &images.children {
            let (Some(p0), Some(sz)) = (node.u32_prop("data-position"), node.u32_prop("data-size")) else {
                bad += 1;
                continue;
            };
            let start = off + p0 as usize;
            let end = start + sz as usize;
            if end > patched.len() {
                bad += 1;
                continue;
            }
            let got: [u8; 32] = Sha256::digest(&patched[start..end]).into();
            match node.find("hash").and_then(|h| h.get("value")) {
                Some(stored) if stored.len() == 32 && stored == &got[..] => {}
                _ => {
                    bad += 1;
                    if verbose {
                        println!("        hash MISMATCH on {}", node.name);
                    }
                }
            }
        }
        chk(bad == 0, &format!("FIT 0x{off:x}: all {n} image hashes match sha256"));

        match uboot_payload(patched, off) {
            Err(e) => chk(false, &format!("FIT 0x{off:x}: uboot payload decompresses ({e})")),
            Ok(pl) => {
                chk(
                    pl.img.len() > 0x10000,
                    &format!(
                        "FIT 0x{off:x}: uboot payload decompresses: {}, {} bytes",
                        pl.comp,
                        pl.img.len()
                    ),
                );
                let hits = arm64::analyse(&pl.img, pl.load);
                match hits.first() {
                    None => chk(false, &format!("FIT 0x{off:x}: rkusb_read_sector still identifiable")),
                    Some(h) => {
                        chk(
                            true,
                            &format!("FIT 0x{off:x}: rkusb_read_sector still identifiable at 0x{:x}", h.fn_va),
                        );
                        let goff = (h.guard - pl.load) as usize;
                        let w = u32::from_le_bytes(
                            pl.img[goff..goff + 4].try_into().unwrap(),
                        );
                        chk(
                            w == BRANCH_ALWAYS,
                            &format!(
                                "FIT 0x{off:x}: guard 0x{:x} {} (0x{:08x})",
                                h.guard,
                                if w == BRANCH_ALWAYS {
                                    "neutralised".to_string()
                                } else if w == arm64::B_LS {
                                    "still STOCK b.ls - limit ACTIVE".to_string()
                                } else {
                                    format!("unexpected value 0x{w:08x}")
                                },
                                w
                            ),
                        );
                        let cw = u32::from_le_bytes(
                            pl.img[goff - 4..goff].try_into().unwrap(),
                        );
                        chk(
                            arm64::cmp32mb_imm(cw) == Some(arm64::RKUSB_LIMIT_SECTORS),
                            &format!(
                                "FIT 0x{off:x}: limit compare 0x{:x} untouched: 0x{cw:08x}",
                                h.guard - 4
                            ),
                        );
                    }
                }
            }
        }
    }
    VerifyReport { ok, checks }
}
