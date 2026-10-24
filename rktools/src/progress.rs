use std::{
    io::{self, IsTerminal, Write},
    time::{Duration, Instant},
};

use humansize::{BINARY, SizeFormatter};

const BAR_WIDTH: usize = 28;
/// Shortest interval between two redraws on a terminal.
const REDRAW_INTERVAL: Duration = Duration::from_millis(100);
/// Shortest interval between two throughput samples. Sampling on every chunk
/// would make the number jump, because a single 64 KiB chunk takes a few ms.
const SAMPLE_INTERVAL: Duration = Duration::from_millis(250);
/// Weight of the newest sample in the smoothed throughput.
const SPEED_ALPHA: f64 = 0.4;
/// Width the line is padded to, so a shorter line fully overwrites the
/// previous one on a terminal.
const LINE_WIDTH: usize = 120;

/// Instantaneous transfer rate, exponentially smoothed.
///
/// This used to be the *average* rate since the transfer started, which hides a
/// device that slows down or stalls halfway through a large partition. Tracking
/// the live rate makes that visible while the transfer is still running.
struct Rate {
    last_sample: Instant,
    last_done: u64,
    bytes_per_sec: f64,
}

impl Rate {
    fn new(now: Instant) -> Self {
        Self {
            last_sample: now,
            last_done: 0,
            bytes_per_sec: 0.0,
        }
    }

    /// Feed a new cumulative byte count and return the smoothed rate.
    fn sample(&mut self, done: u64, now: Instant) -> f64 {
        let elapsed = now.duration_since(self.last_sample);
        if elapsed >= SAMPLE_INTERVAL {
            let delta = done.saturating_sub(self.last_done) as f64;
            let instant = delta / elapsed.as_secs_f64();
            self.bytes_per_sec = if self.bytes_per_sec <= 0.0 {
                instant
            } else {
                SPEED_ALPHA * instant + (1.0 - SPEED_ALPHA) * self.bytes_per_sec
            };
            self.last_sample = now;
            self.last_done = done;
        }
        self.bytes_per_sec
    }
}

pub struct ProgressBar {
    label: String,
    total: u64,
    done: u64,
    start: Instant,
    last_draw: Instant,
    last_pct_bucket: u64,
    rate: Rate,
    drew: bool,
}

impl ProgressBar {
    pub fn new(label: impl Into<String>, total: u64) -> Self {
        let now = Instant::now();
        let mut bar = Self {
            label: label.into(),
            total,
            done: 0,
            start: now,
            last_draw: now.checked_sub(REDRAW_INTERVAL).unwrap_or(now),
            last_pct_bucket: u64::MAX,
            rate: Rate::new(now),
            drew: false,
        };
        bar.draw(true);
        bar
    }

    /// Record progress. Called once per transferred chunk, so this is also
    /// where the live throughput is sampled.
    pub fn set(&mut self, done: u64, total: u64) {
        self.done = done;
        if total != 0 {
            self.total = total;
        }
        let now = Instant::now();
        self.rate.sample(done, now);
        self.draw(false);
    }

    pub fn finish(mut self) {
        if self.total != 0 {
            self.done = self.total;
        }
        let elapsed = self.start.elapsed();
        self.draw(true);
        let avg = if elapsed.as_secs_f64() > 0.0 {
            self.done as f64 / elapsed.as_secs_f64()
        } else {
            0.0
        };
        let summary = format!(
            "{} done in {} (avg {})",
            SizeFormatter::new(self.done, BINARY),
            format_duration(elapsed),
            format_rate(avg)
        );
        let mut stderr = io::stderr();
        if stderr.is_terminal() {
            let _ = writeln!(stderr);
        }
        let _ = writeln!(stderr, "{summary}");
        // The line is already terminated; keep Drop from adding a blank line.
        self.drew = false;
    }

    fn draw(&mut self, force: bool) {
        let now = Instant::now();
        if !force && now.duration_since(self.last_draw) < REDRAW_INTERVAL {
            return;
        }
        self.last_draw = now;

        let ratio = if self.total == 0 {
            1.0
        } else {
            (self.done as f64 / self.total as f64).clamp(0.0, 1.0)
        };
        let pct = ratio * 100.0;
        let filled = ((BAR_WIDTH as f64 * ratio).round() as usize).min(BAR_WIDTH);
        let bar = format!("{}{}", "=".repeat(filled), " ".repeat(BAR_WIDTH - filled));

        let speed = self.rate.bytes_per_sec;
        let remaining = self.total.saturating_sub(self.done);
        let eta = if self.done >= self.total {
            format_duration(self.start.elapsed())
        } else if speed > 0.5 {
            format_duration(Duration::from_secs_f64(remaining as f64 / speed))
        } else {
            "--:--".to_string()
        };

        let done_s = SizeFormatter::new(self.done, BINARY);
        let total_s = SizeFormatter::new(self.total, BINARY);
        let line = format!(
            "{label} {pct:5.1}% [{bar}] {done_s}/{total_s}  {rate}  ETA {eta}",
            label = self.label,
            rate = format_rate(speed),
        );

        let mut stderr = io::stderr();
        if stderr.is_terminal() {
            let padded = format!("{line:<width$}", width = LINE_WIDTH);
            let _ = write!(stderr, "\r{padded}");
            let _ = stderr.flush();
            self.drew = true;
        } else {
            // Not a terminal: one line per 10 % keeps logs compact.
            let bucket = pct as u64 / 10;
            if force || bucket != self.last_pct_bucket {
                self.last_pct_bucket = bucket;
                let _ = writeln!(stderr, "{line}");
            }
        }
    }
}

impl Drop for ProgressBar {
    fn drop(&mut self) {
        // A bar that never reached `finish` (the operation failed) must not
        // leave the cursor in the middle of a half-drawn line.
        if self.drew && io::stderr().is_terminal() {
            let _ = writeln!(io::stderr());
        }
    }
}

/// Render a byte-per-second figure. Keeps two decimals for scaled units so a
/// slow but moving device stays distinguishable from a stalled one.
pub(crate) fn format_rate(bytes_per_sec: f64) -> String {
    if !bytes_per_sec.is_finite() || bytes_per_sec < 0.0 {
        return "--/s".to_string();
    }
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes_per_sec;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{value:.0} B/s")
    } else {
        format!("{value:.2} {}/s", UNITS[unit])
    }
}

fn format_duration(duration: Duration) -> String {
    let total = duration.as_secs();
    let (h, m, s) = (total / 3600, (total % 3600) / 60, total % 60);
    if h > 0 {
        format!("{h:02}:{m:02}:{s:02}")
    } else {
        format!("{m:02}:{s:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_scales_units() {
        assert_eq!(format_rate(512.0), "512 B/s");
        assert_eq!(format_rate(1536.0), "1.50 KiB/s");
        assert_eq!(format_rate(18.0 * 1024.0 * 1024.0), "18.00 MiB/s");
        assert_eq!(format_rate(f64::NAN), "--/s");
    }

    #[test]
    fn duration_is_hh_mm_ss_when_long() {
        assert_eq!(format_duration(Duration::from_secs(9)), "00:09");
        assert_eq!(
            format_duration(Duration::from_secs(3 * 3600 + 61)),
            "03:01:01"
        );
    }

    #[test]
    fn live_rate_falls_when_the_device_stalls() {
        let start = Instant::now();
        let mut rate = Rate::new(start);
        let moving = rate.sample(1024 * 1024, start + Duration::from_secs(1));
        assert!((moving - 1024.0 * 1024.0).abs() < 1.0, "moving {moving}");
        // Nothing moved during the next second: the live rate must drop instead
        // of staying at the lifetime average.
        let stalled = rate.sample(1024 * 1024, start + Duration::from_secs(2));
        assert!(stalled < moving * 0.9, "stalled {stalled}");
    }

    /// Drives a whole bar through its draw paths; run with --nocapture to inspect
    /// the rendered lines (10 % steps when stderr is not a terminal).
    #[test]
    fn draws_a_full_bar() {
        let total = 10 * 1024 * 1024;
        let mut bar = ProgressBar::new("read test", total);
        for step in 1..=10u64 {
            bar.set(step * 1024 * 1024, total);
        }
        bar.finish();
    }

    #[test]
    fn rate_ignores_samples_inside_the_interval() {
        let start = Instant::now();
        let mut rate = Rate::new(start);
        let _ = rate.sample(4096, start + Duration::from_secs(1));
        let before = rate.bytes_per_sec;
        let after = rate.sample(8192, start + Duration::from_millis(1100));
        assert_eq!(before, after);
    }
}
