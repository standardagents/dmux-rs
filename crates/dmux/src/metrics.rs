//! Frame/latency instrumentation. Everything the profiler shows lives here so the
//! perf budget in the plan is measured, not asserted.

use std::time::{Duration, Instant};

use hdrhistogram::Histogram;

use crate::interaction::{Kind, Observation};

pub(crate) const PROFILER_REFRESH: Duration = Duration::from_millis(500);
const FPS_BUCKET: Duration = Duration::from_millis(250);
const FPS_HISTORY_BUCKETS: usize = 32;
const FPS_RATE_BUCKETS: usize = 8;
const FPS_RING_LEN: usize = FPS_HISTORY_BUCKETS + 1;
const FPS_TARGET_PER_BUCKET: u16 = 15;
const SPARKS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

struct FrameRate {
    buckets: [u16; FPS_RING_LEN],
    head: usize,
    completed: usize,
    bucket_start: Instant,
}

struct InteractionHistograms {
    queue_us: Histogram<u64>,
    pane_output_us: Histogram<u64>,
    frame_us: Histogram<u64>,
}

impl InteractionHistograms {
    fn new() -> Self {
        Self {
            queue_us: Histogram::new(3).unwrap(),
            pane_output_us: Histogram::new(3).unwrap(),
            frame_us: Histogram::new(3).unwrap(),
        }
    }
}

impl FrameRate {
    fn new(now: Instant) -> Self {
        Self {
            buckets: [0; FPS_RING_LEN],
            head: 0,
            completed: 0,
            bucket_start: now,
        }
    }

    fn record(&mut self, at: Instant) {
        let Some(elapsed) = at.checked_duration_since(self.bucket_start) else {
            return;
        };
        let steps = (elapsed.as_nanos() / FPS_BUCKET.as_nanos()) as usize;
        if steps >= FPS_HISTORY_BUCKETS {
            self.buckets.fill(0);
            self.head = 0;
            self.completed = 0;
            self.bucket_start = at;
        } else {
            for _ in 0..steps {
                self.head = (self.head + 1) % FPS_RING_LEN;
                self.buckets[self.head] = 0;
                self.completed = (self.completed + 1).min(FPS_HISTORY_BUCKETS);
            }
            self.bucket_start += FPS_BUCKET * steps as u32;
        }
        self.buckets[self.head] = self.buckets[self.head].saturating_add(1);
    }

    fn recent_counts(&self, count: usize) -> impl Iterator<Item = u16> + '_ {
        let count = count.min(self.completed);
        (1..=count).map(|back| self.buckets[(self.head + FPS_RING_LEN - back) % FPS_RING_LEN])
    }

    fn fps(&self) -> f64 {
        let count = FPS_RATE_BUCKETS.min(self.completed);
        if count == 0 {
            return 0.0;
        }
        self.recent_counts(count).map(u64::from).sum::<u64>() as f64
            / (count as f64 * FPS_BUCKET.as_secs_f64())
    }

    fn sparkline(&self) -> String {
        let mut out = String::with_capacity(FPS_HISTORY_BUCKETS);
        for _ in self.completed..FPS_HISTORY_BUCKETS {
            out.push('·');
        }
        for back in (1..=self.completed).rev() {
            let count = self.buckets[(self.head + FPS_RING_LEN - back) % FPS_RING_LEN];
            let level = (usize::from(count) * (SPARKS.len() - 1)
                / usize::from(FPS_TARGET_PER_BUCKET))
            .min(SPARKS.len() - 1);
            out.push(SPARKS[level]);
        }
        out
    }
}

pub struct Metrics {
    pub frame_total_us: Histogram<u64>,
    pub frame_diff_us: Histogram<u64>,
    pub frame_write_us: Histogram<u64>,
    pub frames: u64,
    pub full_repaints: u64,
    pub coalesced: u64,
    pub motion_coalesced: u64,
    pub bytes_in_total: u64,
    pub bytes_out_total: u64,
    pub pauses: u64,
    started: Instant,
    /// Rolling input-rate window.
    window_start: Instant,
    window_bytes_in: u64,
    pub bytes_in_per_sec: f64,
    frame_rate: FrameRate,
    interactions: [InteractionHistograms; 3],
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
            motion_coalesced: 0,
            bytes_in_total: 0,
            bytes_out_total: 0,
            pauses: 0,
            started: Instant::now(),
            window_start: Instant::now(),
            window_bytes_in: 0,
            bytes_in_per_sec: 0.0,
            frame_rate: FrameRate::new(Instant::now()),
            interactions: std::array::from_fn(|_| InteractionHistograms::new()),
        }
    }

    pub fn record_input_queue(&mut self, kind: Kind, elapsed: Duration) {
        let _ = self.interactions[kind.index()]
            .queue_us
            .record(elapsed.as_micros().max(1) as u64);
    }

    pub fn record_pane_output(&mut self, observation: Observation) {
        let _ = self.interactions[observation.kind.index()]
            .pane_output_us
            .record(observation.elapsed.as_micros().max(1) as u64);
    }

    pub fn record_interaction_frame(&mut self, observation: Observation) {
        let _ = self.interactions[observation.kind.index()]
            .frame_us
            .record(observation.elapsed.as_micros().max(1) as u64);
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
        self.frame_rate.record(Instant::now());
    }

    pub fn uptime(&self) -> Duration {
        self.started.elapsed()
    }

    /// profiler lines, ready to draw.
    pub fn profiler_lines(&self) -> Vec<String> {
        let p = |h: &Histogram<u64>, q: f64| h.value_at_quantile(q) as f64 / 1000.0;
        let observed = |h: &Histogram<u64>, q: f64| {
            if h.is_empty() {
                "--".to_string()
            } else {
                format!("{:.2}", p(h, q))
            }
        };
        let mut lines = vec![
            format!(
                "frames {}  full {}  frame coalesced {}  motion skipped {}",
                self.frames, self.full_repaints, self.coalesced, self.motion_coalesced
            ),
            format!(
                "fps {:5.1}/60  {}",
                self.frame_rate.fps(),
                self.frame_rate.sparkline()
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
        ];
        for kind in Kind::ALL {
            let histograms = &self.interactions[kind.index()];
            lines.push(format!(
                "{} queue95 {} first-out95 {} server-frame50/95 {}/{}",
                kind.label(),
                observed(&histograms.queue_us, 0.95),
                observed(&histograms.pane_output_us, 0.95),
                observed(&histograms.frame_us, 0.50),
                observed(&histograms.frame_us, 0.95)
            ));
        }
        lines.extend([
            format!(
                "pane in {:.1} KB/s ({} MB total)  out {} MB  pauses {}",
                self.bytes_in_per_sec / 1024.0,
                self.bytes_in_total / (1024 * 1024),
                self.bytes_out_total / (1024 * 1024),
                self.pauses
            ),
            format!("uptime {}s", self.uptime().as_secs()),
        ]);
        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rolling_fps_tracks_sixty_then_thirty_frames_per_second() {
        let start = Instant::now();
        let mut rate = FrameRate::new(start);
        for frame in 1..=120 {
            rate.record(start + Duration::from_secs_f64(frame as f64 / 60.0));
        }
        assert!((rate.fps() - 60.0).abs() < 1.0, "fps={}", rate.fps());
        assert!(rate.sparkline().ends_with("████"), "{}", rate.sparkline());

        for frame in 1..=60 {
            rate.record(
                start + Duration::from_secs(2) + Duration::from_secs_f64(frame as f64 / 30.0),
            );
        }
        assert!((rate.fps() - 30.0).abs() < 1.0, "fps={}", rate.fps());
    }

    #[test]
    fn long_idle_gap_clears_stale_fps_history() {
        let start = Instant::now();
        let mut rate = FrameRate::new(start);
        for _ in 0..30 {
            rate.record(start + Duration::from_millis(1));
        }
        rate.record(start + Duration::from_secs(8));

        assert_eq!(rate.fps(), 0.0);
        assert_eq!(rate.sparkline(), "·".repeat(FPS_HISTORY_BUCKETS));
    }

    #[test]
    fn sparkline_has_fixed_width_and_clamps_above_target() {
        let start = Instant::now();
        let mut rate = FrameRate::new(start);
        for _ in 0..100 {
            rate.record(start + Duration::from_millis(1));
        }
        rate.record(start + FPS_BUCKET);

        let sparkline = rate.sparkline();
        assert_eq!(sparkline.chars().count(), FPS_HISTORY_BUCKETS);
        assert!(sparkline.ends_with('█'), "{sparkline}");
    }

    #[test]
    fn profiler_includes_framerate_and_fixed_width_history() {
        let metrics = Metrics::new();
        let line = &metrics.profiler_lines()[1];

        assert!(line.starts_with("fps   0.0/60  "), "{line}");
        assert_eq!(line.chars().rev().take(FPS_HISTORY_BUCKETS).count(), 32);
        assert!(line.ends_with(&"·".repeat(FPS_HISTORY_BUCKETS)), "{line}");
    }

    #[test]
    fn interaction_histograms_report_each_server_side_stage() {
        let mut metrics = Metrics::new();
        metrics.record_input_queue(Kind::Key, Duration::from_millis(2));
        metrics.record_pane_output(Observation {
            kind: Kind::Key,
            elapsed: Duration::from_millis(4),
        });
        metrics.record_interaction_frame(Observation {
            kind: Kind::Key,
            elapsed: Duration::from_millis(7),
        });

        let lines = metrics.hud_lines();
        assert!(
            lines[4].contains("key queue95 2.00 first-out95 4.00 server-frame50/95 7.00/7.00"),
            "{}",
            lines[4]
        );
        assert!(lines[7].starts_with("pane in "), "{}", lines[7]);
    }

    #[test]
    fn untouched_interaction_categories_show_missing_samples() {
        let lines = Metrics::new().hud_lines();
        assert_eq!(
            lines[4],
            "key queue95 -- first-out95 -- server-frame50/95 --/--"
        );
        assert_eq!(
            lines[5],
            "pointer queue95 -- first-out95 -- server-frame50/95 --/--"
        );
    }
}
