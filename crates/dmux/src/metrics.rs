//! Frame/latency instrumentation. Everything the HUD shows lives here so the
//! perf budget in the plan is measured, not asserted.

use std::time::{Duration, Instant};

use hdrhistogram::Histogram;

pub struct Metrics {
    pub frame_total_us: Histogram<u64>,
    pub frame_diff_us: Histogram<u64>,
    pub frame_write_us: Histogram<u64>,
    pub frames: u64,
    pub full_repaints: u64,
    pub coalesced: u64,
    pub bytes_in_total: u64,
    pub bytes_out_total: u64,
    pub pauses: u64,
    started: Instant,
    /// Rolling input-rate window.
    window_start: Instant,
    window_bytes_in: u64,
    pub bytes_in_per_sec: f64,
}

impl Metrics {
    pub fn new() -> Self {
        Self {
            frame_total_us: Histogram::new(3).unwrap(),
            frame_diff_us: Histogram::new(3).unwrap(),
            frame_write_us: Histogram::new(3).unwrap(),
            frames: 0,
            full_repaints: 0,
            coalesced: 0,
            bytes_in_total: 0,
            bytes_out_total: 0,
            pauses: 0,
            started: Instant::now(),
            window_start: Instant::now(),
            window_bytes_in: 0,
            bytes_in_per_sec: 0.0,
        }
    }

    pub fn record_input(&mut self, bytes: usize) {
        self.bytes_in_total += bytes as u64;
        self.window_bytes_in += bytes as u64;
        let elapsed = self.window_start.elapsed();
        if elapsed >= Duration::from_secs(1) {
            self.bytes_in_per_sec = self.window_bytes_in as f64 / elapsed.as_secs_f64();
            self.window_bytes_in = 0;
            self.window_start = Instant::now();
        }
    }

    pub fn record_frame(
        &mut self,
        total: Duration,
        diff: Duration,
        write: Duration,
        bytes_out: usize,
        full: bool,
    ) {
        self.frames += 1;
        if full {
            self.full_repaints += 1;
        }
        self.bytes_out_total += bytes_out as u64;
        let _ = self.frame_total_us.record(total.as_micros().max(1) as u64);
        let _ = self.frame_diff_us.record(diff.as_micros().max(1) as u64);
        let _ = self.frame_write_us.record(write.as_micros().max(1) as u64);
    }

    pub fn uptime(&self) -> Duration {
        self.started.elapsed()
    }

    /// HUD lines, ready to draw.
    pub fn hud_lines(&self) -> Vec<String> {
        let p = |h: &Histogram<u64>, q: f64| h.value_at_quantile(q) as f64 / 1000.0;
        vec![
            format!(
                "frames {}  full {}  coalesced {}",
                self.frames, self.full_repaints, self.coalesced
            ),
            format!(
                "frame ms p50 {:.2} p95 {:.2} p99 {:.2} max {:.2}",
                p(&self.frame_total_us, 0.50),
                p(&self.frame_total_us, 0.95),
                p(&self.frame_total_us, 0.99),
                self.frame_total_us.max() as f64 / 1000.0
            ),
            format!(
                "diff ms p95 {:.2}  write ms p95 {:.2}",
                p(&self.frame_diff_us, 0.95),
                p(&self.frame_write_us, 0.95)
            ),
            format!(
                "in {:.1} KB/s ({} MB total)  out {} MB  pauses {}",
                self.bytes_in_per_sec / 1024.0,
                self.bytes_in_total / (1024 * 1024),
                self.bytes_out_total / (1024 * 1024),
                self.pauses
            ),
            format!("uptime {}s", self.uptime().as_secs()),
        ]
    }
}
