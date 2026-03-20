use std::time::{Duration, Instant};

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

pub(crate) fn timeout_to<E: Clone>(deadline: Instant, err: E) -> impl Fn() -> Result<Duration, E> {
    move || {
        deadline
            .checked_duration_since(Instant::now())
            .filter(|x| !x.is_zero())
            .ok_or(err.clone())
    }
}
