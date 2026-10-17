use std::{
    io::{self, IsTerminal, Write},
    time::{Duration, Instant},
};

use humansize::{BINARY, SizeFormatter};

const BAR_WIDTH: usize = 28;

pub struct ProgressBar {
    label: String,
    total: u64,
    done: u64,
    start: Instant,
    last_draw: Instant,
    last_pct_bucket: u64,
}

impl ProgressBar {
    pub fn new(label: impl Into<String>, total: u64) -> Self {
        let now = Instant::now();
        let mut bar = Self {
            label: label.into(),
            total,
            done: 0,
            start: now,
            last_draw: now.checked_sub(Duration::from_secs(1)).unwrap_or(now),
            last_pct_bucket: u64::MAX,
        };
        bar.draw(true);
        bar
    }

    pub fn set(&mut self, done: u64, total: u64) {
        self.done = done;
        if total != 0 {
            self.total = total;
        }
        self.draw(done >= self.total && self.total != 0);
    }

    pub fn finish(mut self) {
        if self.total != 0 {
            self.done = self.total;
        }
        self.draw(true);
        let _ = writeln!(io::stderr());
    }

    fn draw(&mut self, force: bool) {
        let now = Instant::now();
        if !force && now.duration_since(self.last_draw) < Duration::from_millis(80) {
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
        let bar = format!(
            "{}{}",
            "=".repeat(filled),
            " ".repeat(BAR_WIDTH - filled)
        );

        let elapsed = now.duration_since(self.start).as_secs_f64().max(1e-3);
        let speed = self.done as f64 / elapsed;
        let eta = if speed > 0.0 && self.done < self.total {
            format_eta((self.total - self.done) as f64 / speed)
        } else {
            "00:00".to_string()
        };

        let done_s = SizeFormatter::new(self.done, BINARY);
        let total_s = SizeFormatter::new(self.total, BINARY);
        let speed_s = SizeFormatter::new(speed as u64, BINARY);
        let line = format!(
            "{label} {pct:3.0}% [{bar}] {done_s}/{total_s}  {speed_s}/s  ETA {eta}",
            label = self.label
        );

        let mut stderr = io::stderr();
        if stderr.is_terminal() {
            let _ = write!(stderr, "\r{line:<120}");
            let _ = stderr.flush();
        } else {
            let bucket = pct as u64 / 10;
            if force || bucket != self.last_pct_bucket {
                self.last_pct_bucket = bucket;
                let _ = writeln!(stderr, "{line}");
            }
        }
    }
}

fn format_eta(seconds: f64) -> String {
    if !seconds.is_finite() || seconds < 0.0 {
        return "--:--".to_string();
    }
    let total = seconds.round() as u64;
    let h = total / 3600;
    let m = (total % 3600) / 60;
    let s = total % 60;
    if h > 0 {
        format!("{h:02}:{m:02}:{s:02}")
    } else {
        format!("{m:02}:{s:02}")
    }
}
