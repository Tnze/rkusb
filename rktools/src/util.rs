use humansize::{BINARY, SizeFormatter};
use rkusb::RkFlashInfo;

pub(crate) fn parse_u32(input: &str) -> Result<u32, String> {
    if let Some(hex) = input
        .strip_prefix("0x")
        .or_else(|| input.strip_prefix("0X"))
    {
        u32::from_str_radix(hex, 16).map_err(|e| e.to_string())
    } else {
        input.parse::<u32>().map_err(|e| e.to_string())
    }
}

pub(crate) fn parse_u8(input: &str) -> Result<u8, String> {
    if let Some(hex) = input
        .strip_prefix("0x")
        .or_else(|| input.strip_prefix("0X"))
    {
        u8::from_str_radix(hex, 16).map_err(|e| e.to_string())
    } else {
        input.parse::<u8>().map_err(|e| e.to_string())
    }
}

pub(crate) fn format_bytes(bytes: u64) -> String {
    SizeFormatter::new(bytes, BINARY).to_string()
}

pub(crate) fn storage_label(storage: Option<u8>, info: &RkFlashInfo) -> String {
    let slug = match storage {
        Some(code) => rkusb::storage_slug(code),
        None => "Unknown".to_string(),
    };
    let size = format_bytes(info.flash_size_bytes()).replace(' ', "");
    format!("{slug}_{size}")
}

pub(crate) fn partition_table_filename(storage: Option<u8>, info: &RkFlashInfo) -> String {
    format!("{}_partition_table.txt", storage_label(storage, info))
}

pub(crate) fn sanitize_filename(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for c in name.chars() {
        if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' {
            out.push(c);
        } else if !out.ends_with('_') {
            out.push('_');
        }
    }
    let trimmed = out.trim_matches('_');
    if trimmed.is_empty() {
        "partition".to_string()
    } else {
        trimmed.to_string()
    }
}

pub(crate) fn hexdump(data: &[u8]) {
    for (i, chunk) in data.chunks(16).enumerate() {
        print!("{:08x}  ", i * 16);
        for (j, b) in chunk.iter().enumerate() {
            print!("{b:02x} ");
            if j == 7 {
                print!(" ");
            }
        }
        for j in chunk.len()..16 {
            print!("   ");
            if j == 7 {
                print!(" ");
            }
        }
        print!(" |");
        for b in chunk {
            let c = if b.is_ascii_graphic() || *b == b' ' {
                *b as char
            } else {
                '.'
            };
            print!("{c}");
        }
        println!("|");
    }
}
