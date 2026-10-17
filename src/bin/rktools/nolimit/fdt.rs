// SPDX-FileCopyrightText: 2024-2026 IFLYTEK-LEAKING
// SPDX-FileCopyrightText: 2024-2026 KawaiiSparkle
// SPDX-License-Identifier: GPL-2.0-or-later
//
// Flattened Device Tree reader used by the nolimit engine.
// Port of the FDT class in rkusb_nolimit.py (same authors, GPL-2.0-or-later).
//
// Layout actually found in Rockchip uboot_a / uboot_b partitions:
//
//     0x000  fdt_header            40 B   (off_dt_struct / off_dt_strings are
//                                         written SWAPPED by Rockchip mkimage)
//     0x028  memreserve block      (scanned: entries until a (0,0) terminator)
//     0x048  struct block
//     ....   string table
//     ....   zero padding          -> totalsize
//
// The patcher never rewrites a property through a full re-serialisation; it
// overwrites the 32-byte hash/digest value in place (see hash::refresh), so
// this module only needs to PARSE reliably and expose property byte offsets.

use std::fmt;

pub const FDT_MAGIC: u32 = 0xd00d_feed;
pub const FDT_BEG: u32 = 1;
pub const FDT_END: u32 = 2;
pub const FDT_PROP: u32 = 3;
pub const FDT_NOP: u32 = 4;
pub const FDT_FEND: u32 = 9;
pub const FDT_HDR_LEN: usize = 40;

#[derive(Debug)]
pub struct FdtError(pub String);

impl fmt::Display for FdtError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "FDT error: {}", self.0)
    }
}

impl std::error::Error for FdtError {}

fn be32(b: &[u8], off: usize) -> Option<u32> {
    if off + 4 > b.len() {
        return None;
    }
    Some(u32::from_be_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]]))
}

/// End offset of the token stream starting at `start`, or None.
fn walk_end(buf: &[u8], start: usize) -> Option<usize> {
    let mut pos = start;
    loop {
        let t = be32(buf, pos)?;
        pos += 4;
        match t {
            FDT_BEG => {
                let e = buf[pos..].iter().position(|&b| b == 0)? + pos;
                pos = (e + 1 + 3) & !3;
            }
            FDT_PROP => {
                let ln = be32(buf, pos)? as usize;
                pos = (pos + 8 + ln + 3) & !3;
            }
            FDT_FEND => return Some(pos),
            FDT_END | FDT_NOP => {}
            _ => return None,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Node {
    pub name: String,
    pub props: Vec<(String, Vec<u8>)>,
    pub children: Vec<Node>,
}

impl Node {
    /// Find a child by slash-separated path, e.g. "images/uboot".
    pub fn find(&self, path: &str) -> Option<&Node> {
        let (first, rest) = match path.split_once('/') {
            Some((a, b)) => (a, b),
            None => (path, ""),
        };
        let hit = self.children.iter().find(|c| c.name == first)?;
        if rest.is_empty() { Some(hit) } else { hit.find(rest) }
    }

    pub fn get(&self, name: &str) -> Option<&[u8]> {
        self.props.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_slice())
    }

    pub fn u32_prop(&self, name: &str) -> Option<u32> {
        let v = self.get(name)?;
        if v.len() != 4 {
            return None;
        }
        Some(u32::from_be_bytes([v[0], v[1], v[2], v[3]]))
    }
}

pub struct Fdt<'a> {
    buf: &'a [u8],
    pub off: usize,
    /// totalsize from the header
    pub tsize: u32,
    /// off_memresv header field (what Rockchip writes where off_dt_struct
    /// would normally go, or vice versa - see `swapped`)
    pub offmem: u32,
    pub version: u32,
    pub last_comp: u32,
    /// offset of the struct block relative to `off` (after the memreserves)
    pub offstruct: usize,
    pub szstruct: u32,
    /// offset of the string table relative to `off`
    pub offstr: usize,
    pub szstr: u32,
    /// true when Rockchip's mkimage swapped off_dt_struct / off_dt_strings
    pub swapped: bool,
    pub root: Node,
}

impl<'a> Fdt<'a> {
    /// The raw dump bytes this FDT was parsed from.
    pub fn buf(&self) -> &'a [u8] {
        self.buf
    }
}

impl<'a> Fdt<'a> {
    /// Parse the FDT blob that starts at `off` in `buf`.
    pub fn parse(buf: &'a [u8], off: usize) -> Result<Self, FdtError> {
        if off + FDT_HDR_LEN > buf.len() {
            return Err(FdtError(format!("blob at 0x{off:x} truncated")));
        }
        let magic = be32(buf, off).unwrap();
        if magic != FDT_MAGIC {
            return Err(FdtError(format!("not an FDT: magic={magic:08x}")));
        }
        let tsize = be32(buf, off + 4).unwrap();
        let h2 = be32(buf, off + 8).unwrap();
        let h3 = be32(buf, off + 12).unwrap();
        let offmem = be32(buf, off + 16).unwrap();
        let version = be32(buf, off + 20).unwrap();
        let last_comp = be32(buf, off + 24).unwrap();
        let szstr = be32(buf, off + 32).unwrap();
        let szstruct = be32(buf, off + 36).unwrap();

        // memreserve block: entries until a (0,0) terminator; its length is
        // not in the header, so it has to be scanned for.
        let mut p = offmem as usize;
        loop {
            if off + p + 16 > buf.len() {
                return Err(FdtError("unterminated memreserve block".into()));
            }
            let a = u64::from_be_bytes(buf[off + p..off + p + 8].try_into().unwrap());
            let b = u64::from_be_bytes(buf[off + p + 8..off + p + 16].try_into().unwrap());
            if a == 0 && b == 0 {
                break;
            }
            p += 16;
        }
        p += 16; // the terminator is part of the block
        let offstruct = p;

        if walk_end(buf, off + offstruct).is_none() {
            return Err(FdtError(format!(
                "no FDT token stream at 0x{:x}",
                off + offstruct
            )));
        }
        let offstr = offstruct + szstruct as usize;

        // Rockchip's mkimage swaps these two header fields - remember that.
        let swapped = h2 == offstruct as u32 && h3 == offstr as u32;
        if !swapped && (h2 != offstr as u32 || h3 != offstruct as u32) {
            return Err(FdtError(format!(
                "unrecognised header layout: {h2:08x} {h3:08x}"
            )));
        }
        if off + offstr + szstr as usize > buf.len() {
            return Err(FdtError("string table out of range".into()));
        }

        let root = Self::parse_struct(buf, off, offstruct, offstr, szstr as usize)?;

        Ok(Self {
            buf,
            off,
            tsize,
            offmem,
            version,
            last_comp,
            offstruct,
            szstruct,
            offstr,
            szstr,
            swapped,
            root,
        })
    }

    fn strname(&self, no: usize) -> Result<String, FdtError> {
        let at = self.off + self.offstr + no;
        if at >= self.off + self.offstr + self.szstr as usize {
            return Err(FdtError(format!("string offset {no} out of table")));
        }
        let end = self.buf[at..]
            .iter()
            .position(|&b| b == 0)
            .ok_or_else(|| FdtError("unterminated string".into()))?
            + at;
        Ok(String::from_utf8_lossy(&self.buf[at..end]).into_owned())
    }

    fn parse_struct(
        buf: &[u8],
        base: usize,
        offstruct: usize,
        offstr: usize,
        szstr: usize,
    ) -> Result<Node, FdtError> {
        let strof = |no: usize| -> Result<String, FdtError> {
            let at = base + offstr + no;
            if no >= szstr || at >= buf.len() {
                return Err(FdtError(format!("string offset {no} out of table")));
            }
            let end = buf[at..]
                .iter()
                .position(|&b| b == 0)
                .ok_or_else(|| FdtError("unterminated string".into()))?
                + at;
            Ok(String::from_utf8_lossy(&buf[at..end]).into_owned())
        };

        let mut pos = base + offstruct;
        let mut stack: Vec<Node> = vec![Node::default()]; // wrapper: the root becomes its child
        loop {
            let t = be32(buf, pos)
                .ok_or_else(|| FdtError("token stream overruns buffer".into()))?;
            pos += 4;
            match t {
                FDT_BEG => {
                    let e = buf[pos..]
                        .iter()
                        .position(|&b| b == 0)
                        .ok_or_else(|| FdtError("unterminated node name".into()))?
                        + pos;
                    let name = String::from_utf8_lossy(&buf[pos..e]).into_owned();
                    pos = (e + 1 + 3) & !3;
                    stack.push(Node { name, ..Default::default() });
                }
                FDT_END => {
                    let node = stack.pop().unwrap();
                    stack
                        .last_mut()
                        .expect("FDT_END underflow")
                        .children
                        .push(node);
                }
                FDT_PROP => {
                    let ln = be32(buf, pos)
                        .ok_or_else(|| FdtError("prop header overruns buffer".into()))?
                        as usize;
                    let no = be32(buf, pos + 4)
                        .ok_or_else(|| FdtError("prop header overruns buffer".into()))?
                        as usize;
                    pos += 8;
                    if pos + ln > buf.len() {
                        return Err(FdtError("property value overruns buffer".into()));
                    }
                    let value = buf[pos..pos + ln].to_vec();
                    pos = (pos + ln + 3) & !3;
                    let name = strof(no)?;
                    stack
                        .last_mut()
                        .expect("property outside node")
                        .props
                        .push((name, value));
                }
                FDT_FEND => {
                    if stack.len() != 1 {
                        return Err(FdtError("unbalanced FDT structure".into()));
                    }
                    let mut wrapper = stack.pop().unwrap();
                    if wrapper.children.len() != 1 {
                        return Err(FdtError("expected exactly one root node".into()));
                    }
                    return Ok(wrapper.children.pop().unwrap());
                }
                FDT_NOP => {}
                _ => {
                    return Err(FdtError(format!("bad token {t} @0x{:x}", pos - 4)));
                }
            }
        }
    }

    /// Offsets (absolute in the dump) of FDT_PROP value bytes equal to
    /// `value` inside this FIT's struct block.
    ///
    /// Used for the surgical 32-byte hash/digest overwrite: rewriting a
    /// property through a full re-serialisation is not byte-identical, so the
    /// stored value is replaced where it sits, with nothing else moving.
    pub fn hash_prop_offsets(&self, value: &[u8]) -> Vec<usize> {
        let mut hits = Vec::new();
        if value.is_empty() {
            return hits;
        }
        let lo = self.off + self.offstruct;
        let hi = (lo + self.szstruct as usize).min(self.buf.len());
        if lo >= hi {
            return hits;
        }
        let mut o = lo;
        'outer: while o + value.len() <= hi {
            for i in 0..=(hi - o - value.len()) {
                if &self.buf[o + i..o + i + value.len()] != value {
                    continue;
                }
                let at = o + i;
                // the FDT_PROP header precedes the value: tag, len, nameoff
                if at >= 12 {
                    let tag = be32(self.buf, at - 12).unwrap_or(0);
                    let ln = be32(self.buf, at - 8).unwrap_or(0);
                    if tag == FDT_PROP && ln as usize == value.len() {
                        hits.push(at);
                        o = at + value.len();
                        continue 'outer;
                    }
                }
                o = at + 1;
            }
            break;
        }
        hits
    }
}
