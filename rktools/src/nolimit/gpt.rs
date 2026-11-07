// SPDX-FileCopyrightText: 2024-2026 IFLYTEK-LEAKING
// SPDX-FileCopyrightText: 2024-2026 KawaiiSparkle
// SPDX-License-Identifier: GPL-2.0-or-later
//
// GPT parsing and AvbAB (A/B metadata) decoding for the nolimit engine.
// Port of parse_gpt()/part()/part_containing()/read_ab() in rkusb_nolimit.py
// (same authors, GPL-2.0-or-later).

pub const SECTOR: usize = 512;
pub const AVB_AB_MAGIC: [u8; 4] = [0x00, b'A', b'B', b'0'];
/// sectors; include/spl_ab.h
pub const AB_METADATA_OFFSET: u64 = 4;
pub const AVB_AB_MAX_PRIORITY: u8 = 15;
pub const AVB_AB_MAX_TRIES: u8 = 7;

#[derive(Debug, Clone)]
pub struct Partition {
    pub idx: usize,
    pub name: String,
    pub start: u64,
    pub end: u64,
    pub sectors: u64,
    pub guid: String,
    /// byte offset on the storage
    pub off: u64,
    /// byte size
    pub size: u64,
}

#[derive(Debug, Clone, Default)]
pub struct Gpt {
    pub ent_lba: u64,
    pub n: u32,
    pub esz: u32,
    pub disk_guid: String,
    pub parts: Vec<Partition>,
}

impl Gpt {
    pub fn part(&self, name: &str) -> Option<&Partition> {
        self.parts.iter().find(|p| p.name == name)
    }

    /// The partition whose byte range covers `off`.
    pub fn part_containing(&self, off: u64) -> Option<&Partition> {
        self.parts
            .iter()
            .find(|p| off >= p.off && off < p.off + p.size)
    }
}

/// Parse a GPT from a dump.  Returns None when there is no protective MBR
/// "EFI PART" header at LBA 1.
pub fn parse_gpt(d: &[u8]) -> Option<Gpt> {
    if d.len() < 0x200 + 92 || &d[0x200..0x208] != b"EFI PART" {
        return None;
    }
    let h = &d[0x200..0x200 + 92];
    let rd64 = |o: usize| -> u64 {
        u64::from_le_bytes(h[o..o + 8].try_into().unwrap())
    };
    let _cur = rd64(24);
    let _bak = rd64(32);
    let _first = rd64(40);
    let _last = rd64(48);
    let ent_lba = rd64(72);
    let n = u32::from_le_bytes(h[80..84].try_into().unwrap());
    let esz = u32::from_le_bytes(h[84..88].try_into().unwrap());
    let disk_guid = hex::encode(&h[56..72]);
    let mut g = Gpt {
        ent_lba,
        n,
        esz,
        disk_guid,
        parts: Vec::new(),
    };
    if esz == 0 || n == 0 {
        return Some(g);
    }
    for i in 0..n as usize {
        let off = ent_lba as usize * SECTOR + i * esz as usize;
        if off + esz as usize > d.len() {
            break;
        }
        let e = &d[off..off + esz as usize];
        if e[..16].iter().all(|&b| b == 0) {
            continue;
        }
        let st = u64::from_le_bytes(e[32..40].try_into().unwrap());
        let en = u64::from_le_bytes(e[40..48].try_into().unwrap());
        // partition name: UTF-16LE, 72 bytes
        let name_u16: Vec<u16> = e[56..128]
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        let mut name = String::from_utf16_lossy(&name_u16);
        while name.ends_with('\u{0}') {
            name.pop();
        }
        if en < st {
            continue;
        }
        g.parts.push(Partition {
            idx: g.parts.len(),
            name,
            start: st,
            end: en,
            sectors: en - st + 1,
            guid: hex::encode(&e[0..16]),
            off: st * SECTOR as u64,
            size: (en - st + 1) * SECTOR as u64,
        });
    }
    Some(g)
}

#[derive(Debug, Clone)]
pub struct AbSlot {
    pub priority: u8,
    pub tries: u8,
    pub successful: u8,
    pub bootable: bool,
}

#[derive(Debug, Clone)]
pub struct AbInfo {
    /// byte offset of the AvbABData block in the dump
    pub off: u64,
    pub magic: [u8; 4],
    pub valid: bool,
    pub crc: u32,
    pub crc_ok: bool,
    pub slots: [AbSlot; 2],
    pub last_boot: u8,
    /// active slot index chosen the way common/spl/spl_ab.c does
    pub index: usize,
    pub why: String,
}

/// Decode AvbABData exactly like common/spl/spl_ab.c does.
///
/// Layout at misc + AB_METADATA_OFFSET*512 (0x800):
///     0  magic "\0AB0"      8  slot0 {priority,tries,successful,unused}
///     4  version            12  slot1 {...}
///    16  last_boot          28  crc32 (big endian) over bytes 0..27
///
/// Returns None when there is no misc partition or the block is truncated.
/// `valid`/`crc_ok` tell the caller whether SPL would trust it.
pub fn read_ab(d: &[u8], g: &Gpt) -> Option<AbInfo> {
    let misc = g.part("misc")?;
    let off = misc.off + AB_METADATA_OFFSET * SECTOR as u64;
    if off as usize + 32 > d.len() {
        return None;
    }
    let blk = &d[off as usize..(off as usize + 64).min(d.len())];
    let mut magic = [0u8; 4];
    magic.copy_from_slice(&blk[0..4]);
    let valid = magic == AVB_AB_MAGIC;
    let mut slots = Vec::new();
    let mut bootable = Vec::new();
    for i in 0..2 {
        let pr = blk[8 + i * 4];
        let tr = blk[9 + i * 4];
        let su = blk[10 + i * 4];
        let b = pr > 0 && (su != 0 || tr > 0);
        slots.push(AbSlot {
            priority: pr,
            tries: tr,
            successful: su,
            bootable: b,
        });
        bootable.push(b);
    }
    let last_boot = blk[16];
    let crc = u32::from_be_bytes(blk[28..32].try_into().unwrap());
    let crc_ok = crc == crc32fast::hash(&blk[0..28]);
    let (index, why);
    if valid && crc_ok {
        let boot: Vec<usize> = (0..2).filter(|&i| bootable[i]).collect();
        let mut w = String::new();
        let idx = if boot.len() == 2 {
            w.push_str("both bootable -> higher priority wins");
            if slots[0].priority >= slots[1].priority { 0 } else { 1 }
        } else if boot.len() == 1 {
            w.push_str("only bootable slot");
            boot[0]
        } else {
            w.push_str("none bootable -> last_boot");
            last_boot as usize
        };
        w.push_str(&format!(" (metadata valid, crc 0x{crc:08x})"));
        index = idx;
        why = w;
    } else {
        // spl_ab_data_init() defaults
        slots = vec![
            AbSlot {
                priority: AVB_AB_MAX_PRIORITY,
                tries: AVB_AB_MAX_TRIES,
                successful: 0,
                bootable: true,
            },
            AbSlot {
                priority: AVB_AB_MAX_PRIORITY - 1,
                tries: AVB_AB_MAX_TRIES,
                successful: 0,
                bootable: true,
            },
        ];
        index = last_boot as usize;
        let state = if !valid {
            "absent".to_string()
        } else {
            format!("crc BAD 0x{crc:08x}")
        };
        why = format!(
            "metadata {state} -> SPL re-inits with defaults, last_boot={last_boot}"
        );
    }
    Some(AbInfo {
        off,
        magic,
        valid,
        crc,
        crc_ok,
        slots: [slots[0].clone(), slots[1].clone()],
        last_boot,
        index,
        why,
    })
}
