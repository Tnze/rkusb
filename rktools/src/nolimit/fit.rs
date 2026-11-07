// SPDX-FileCopyrightText: 2024-2026 IFLYTEK-LEAKING
// SPDX-FileCopyrightText: 2024-2026 KawaiiSparkle
// SPDX-License-Identifier: GPL-2.0-or-later
//
// FIT discovery and U-Boot payload handling for the nolimit engine.
// Port of find_fits()/is_uboot_fit()/uboot_payload()/fit_images()/
// refresh_hash()/refresh_digest() in rkusb_nolimit.py.

use super::fdt::{Fdt, FdtError};
use flate2::read::GzDecoder;
use flate2::Compression;
use sha2::{Digest, Sha256};
use std::io::{Read, Write};

/// [(offset, totalsize)] of every Rockchip FIT FDT blob in `d`.
///
/// A Rockchip FIT FDT blob: sane totalsize, version 17, last_comp_version 16,
/// memreserve right after the 40-byte header.
pub fn find_fits(d: &[u8]) -> Vec<(usize, u32)> {
    let mut out = Vec::new();
    let mut o = 0usize;
    while let Some(i) = find_sub(d, &super::fdt::FDT_MAGIC.to_be_bytes(), o) {
        if i % 4 == 0 && i + super::fdt::FDT_HDR_LEN <= d.len() {
            let g = |off: usize| {
                u32::from_be_bytes(d[i + off..i + off + 4].try_into().unwrap())
            };
            let ts = g(4);
            let offmem = g(16);
            let ver = g(20);
            let last = g(24);
            if 0x400 < ts && ts < 0x400_0000 && ver == 17 && last == 16 && offmem == 40 {
                out.push((i, ts));
            }
        }
        o = i + 4;
    }
    out
}

/// A U-Boot FIT has /images/uboot.
pub fn is_uboot_fit(d: &[u8], off: usize) -> bool {
    Fdt::parse(d, off)
        .ok()
        .and_then(|f| f.root.find("images/uboot").map(|_| ()))
        .is_some()
}

#[derive(Debug, Clone)]
pub struct UbootPayload {
    pub comp: String,
    pub load: u32,
    /// byte offset of the payload inside the FIT blob
    pub pos: u32,
    /// stored (possibly compressed) size in the FIT
    pub size: u32,
    /// the DECOMPRESSED payload image
    pub img: Vec<u8>,
}

/// {comp, load, pos, size, img} of /images/uboot in the FIT at `off`.
pub fn uboot_payload(d: &[u8], off: usize) -> Result<UbootPayload, FdtError> {
    let f = Fdt::parse(d, off)?;
    let n = f
        .root
        .find("images/uboot")
        .ok_or_else(|| FdtError("no /images/uboot in this FIT".into()))?;
    let comp = n
        .get("compression")
        .map(|v| {
            let s: Vec<u8> = v.iter().copied().take_while(|&b| b != 0).collect();
            String::from_utf8_lossy(&s).into_owned()
        })
        .unwrap_or_else(|| "none".into());
    let pos = n.u32_prop("data-position").unwrap_or(0);
    let size = n.u32_prop("data-size").unwrap_or(0);
    let start = off + pos as usize;
    let end = start + size as usize;
    if end > d.len() {
        return Err(FdtError(format!(
            "uboot payload [0x{start:x},0x{end:x}) overruns the dump"
        )));
    }
    let raw = &d[start..end];
    let img = if comp == "gzip" {
        gzip_decompress(raw)?
    } else {
        raw.to_vec()
    };
    Ok(UbootPayload {
        comp,
        load: n.u32_prop("load").unwrap_or(0),
        pos,
        size,
        img,
    })
}

#[derive(Debug, Clone)]
pub struct FitImage {
    pub name: String,
    pub pos: u32,
    pub size: u32,
    pub comp: String,
    pub load: u32,
    /// index into the parent FDT root's children (the /images node children)
    pub idx: usize,
}

/// Iterate /images/* of a parsed FIT.
pub fn fit_images(fdt: &Fdt) -> Vec<FitImage> {
    let mut out = Vec::new();
    let Some(images) = fdt.root.find("images") else {
        return out;
    };
    for (idx, node) in images.children.iter().enumerate() {
        let Some(pos) = node.u32_prop("data-position") else {
            continue;
        };
        let comp = node
            .get("compression")
            .map(|v| {
                let s: Vec<u8> = v.iter().copied().take_while(|&b| b != 0).collect();
                String::from_utf8_lossy(&s).into_owned()
            })
            .unwrap_or_else(|| "none".into());
        out.push(FitImage {
            name: node.name.clone(),
            pos,
            size: node.u32_prop("data-size").unwrap_or(0),
            comp,
            load: node.u32_prop("load").unwrap_or(0),
            idx,
        });
    }
    out
}

/// Recompute and surgically rewrite /images/<image>/hash/value in place
/// (32 bytes, nothing moves).  Returns (new_dump, (prop_offset, old, new)).
pub fn refresh_hash(
    dump: &[u8],
    fit_off: usize,
    image: &str,
) -> Result<(Vec<u8>, usize), String> {
    let fdt = Fdt::parse(dump, fit_off).map_err(|e| e.to_string())?;
    let node = fdt
        .root
        .find("images")
        .and_then(|im| im.children.iter().find(|c| c.name == image))
        .ok_or_else(|| format!("no /images/{image} in FIT 0x{fit_off:x}"))?;
    let pos = node
        .u32_prop("data-position")
        .ok_or("image without data-position")?;
    let size = node.u32_prop("data-size").ok_or("image without data-size")?;
    let start = fit_off + pos as usize;
    let end = start + size as usize;
    if end > dump.len() {
        return Err(format!("payload range 0x{start:x}..0x{end:x} overruns dump"));
    }
    let new_hash: [u8; 32] = Sha256::digest(&dump[start..end]).into();
    let hnode = node.find("hash").ok_or("image without hash node")?;
    let old = hnode.get("value").ok_or("hash node without value")?;
    if old.len() != 32 {
        return Err(format!("hash/value is {} bytes, expected 32", old.len()));
    }
    let hits = fdt.hash_prop_offsets(old);
    if hits.len() != 1 {
        return Err(format!(
            "FIT 0x{fit_off:x}: {} candidate hash props, expected 1",
            hits.len()
        ));
    }
    let mut out = dump.to_vec();
    out[hits[0]..hits[0] + 32].copy_from_slice(&new_hash);
    Ok((out, hits[0]))
}

/// Rewrite /images/<image>/digest/value in place (nothing moves).
///
/// digest/value covers the DECOMPRESSED image; SPL checks it again after
/// image_decomp.  Boards without a digest node return Ok(None).
pub fn refresh_digest(
    dump: &[u8],
    fit_off: usize,
    decompressed: &[u8],
    image: &str,
) -> Result<Option<(Vec<u8>, usize)>, String> {
    let fdt = Fdt::parse(dump, fit_off).map_err(|e| e.to_string())?;
    let node = fdt
        .root
        .find("images")
        .and_then(|im| im.children.iter().find(|c| c.name == image))
        .ok_or_else(|| format!("no /images/{image} in FIT 0x{fit_off:x}"))?;
    let Some(dnode) = node.find("digest") else {
        return Ok(None);
    };
    let Some(old) = dnode.get("value") else {
        return Ok(None);
    };
    let algo = dnode
        .get("algo")
        .map(|v| {
            let s: Vec<u8> = v.iter().copied().take_while(|&b| b != 0).collect();
            String::from_utf8_lossy(&s).into_owned()
        })
        .unwrap_or_else(|| "sha256".into());
    if algo != "sha256" {
        return Err(format!(
            "FIT 0x{fit_off:x}: unsupported digest algo {algo:?}"
        ));
    }
    if old.len() != 32 {
        return Err(format!(
            "digest/value is {} bytes, expected 32",
            old.len()
        ));
    }
    let new: [u8; 32] = Sha256::digest(decompressed).into();
    let hits = fdt.hash_prop_offsets(old);
    if hits.len() != 1 {
        return Err(format!(
            "FIT 0x{fit_off:x}: {} candidate digest props, expected 1",
            hits.len()
        ));
    }
    let mut out = dump.to_vec();
    out[hits[0]..hits[0] + 32].copy_from_slice(&new);
    Ok(Some((out, hits[0])))
}

/// gzip-compress with mtime=0 (deterministic, like gzip.compress(..., mtime=0)).
/// flate2 does not expose mtime, so the 10-byte header is written by hand.
pub fn gzip_compress(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() / 2 + 64);
    // 1f 8b 08 00 <mtime=0> 00 ff   (XFL=0, OS=255 unknown)
    out.extend_from_slice(&[0x1f, 0x8b, 0x08, 0x00, 0, 0, 0, 0, 0x00, 0xff]);
    {
        let mut raw = flate2::write::DeflateEncoder::new(&mut out, Compression::best());
        raw.write_all(data).expect("deflate write");
        raw.finish().expect("deflate finish");
    }
    let crc = crc32fast::hash(data);
    out.extend_from_slice(&crc.to_le_bytes());
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out
}

pub fn gzip_decompress(data: &[u8]) -> Result<Vec<u8>, FdtError> {
    let mut dec = GzDecoder::new(data);
    let mut out = Vec::new();
    dec.read_to_end(&mut out)
        .map_err(|e| FdtError(format!("gzip decompress failed: {e}")))?;
    Ok(out)
}

pub fn find_sub(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() || from >= hay.len() {
        return None;
    }
    hay[from..]
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|i| i + from)
}
