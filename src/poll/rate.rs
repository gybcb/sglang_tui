use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

/// Window over which a counter's rate is computed. SGLang flushes some
/// counters in bursts on a log interval — a per-scrape delta reads those
/// bursts as a spike-then-zero sawtooth. Keeping a sliding baseline (rate =
/// first→last point inside the window) smears a burst back into a smooth
/// per-second value. Ported from the behavior the proven Python implementation
/// locks with its burst test.
pub const RATE_WINDOW: Duration = Duration::from_secs(30);

/// Per-label-set counter→rate conversion over a sliding window. Rates are
/// computed here, in the poller, never in the UI. A counter smaller than its
/// window history ⇒ the process restarted ⇒ clear the window and re-baseline
/// (None until two points again) rather than emit a false spike.
pub struct RateTracker {
    window: Duration,
    series: HashMap<String, VecDeque<(Instant, f64)>>,
}

impl Default for RateTracker {
    fn default() -> Self {
        RateTracker {
            window: RATE_WINDOW,
            series: HashMap::new(),
        }
    }
}

impl RateTracker {
    pub fn with_window(window: Duration) -> RateTracker {
        RateTracker {
            window,
            series: HashMap::new(),
        }
    }

    /// Observe `value` for `key` at `now`, then report the rate over the
    /// retained window. `None` while fewer than two points exist (no baseline
    /// yet — render N/A, not 0) or when no two points are time-separated.
    pub fn rate(&mut self, key: &str, now: Instant, value: f64) -> Option<f64> {
        let dq = self.series.entry(key.to_string()).or_default();
        // Any retained point larger than the current value means a reset.
        if dq.iter().any(|&(_, v)| value < v) {
            dq.clear();
        }
        dq.push_back((now, value));
        while dq.len() > 1 && now.duration_since(dq[0].0) > self.window {
            dq.pop_front();
        }
        if dq.len() < 2 {
            return None;
        }
        let (t0, v0) = dq[0];
        let (t1, v1) = *dq.back().unwrap();
        let dt = t1.duration_since(t0).as_secs_f64();
        if dt <= 0.0 {
            return None;
        }
        Some(((v1 - v0) / dt).max(0.0))
    }

    /// Summed rate over a group of label sets (rates are additive across
    /// label sets that measure the same quantity). `None` only when *no*
    /// member has a usable baseline yet.
    pub fn rate_sum<'a>(
        &mut self,
        now: Instant,
        samples: impl Iterator<Item = (&'a str, f64)>,
    ) -> Option<f64> {
        let mut sum = None;
        for (key, v) in samples {
            if let Some(r) = self.rate(key, now, v) {
                sum = Some(sum.unwrap_or(0.0) + r);
            }
        }
        sum
    }

    /// Drop tracking state for keys the scrape no longer contains (rank set
    /// shrank, model reloaded) so a disappeared label set can't pin memory.
    pub fn retain(&mut self, mut keep: impl FnMut(&str) -> bool) {
        self.series.retain(|k, _| keep(k));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_observation_has_no_rate() {
        let mut rt = RateTracker::default();
        let t0 = Instant::now();
        assert_eq!(rt.rate("c", t0, 100.0), None);
    }

    #[test]
    fn second_observation_needs_time_separation() {
        let mut rt = RateTracker::default();
        let t0 = Instant::now();
        rt.rate("c", t0, 100.0);
        // Same instant twice → no divide-by-zero, still N/A.
        assert_eq!(rt.rate("c", t0, 110.0), None);
    }

    #[test]
    fn delta_over_elapsed() {
        let mut rt = RateTracker::default();
        let t0 = Instant::now();
        rt.rate("c", t0, 100.0);
        let t1 = t0 + Duration::from_secs(2);
        let r = rt.rate("c", t1, 110.0).unwrap();
        assert!((r - 5.0).abs() < 0.01, "{r}");
    }

    #[test]
    fn sliding_baseline_smears_a_burst() {
        // A counter that jumps then goes idle must read as the burst spread
        // over the elapsed window, not as a huge spike then zero.
        let mut rt = RateTracker::with_window(Duration::from_secs(45));
        let t0 = Instant::now();
        rt.rate("c", t0, 1000.0);
        let t1 = t0 + Duration::from_secs(30);
        // Burst reads spread over the elapsed window, not a point spike.
        assert!((rt.rate("c", t1, 1200.0).unwrap() - 200.0 / 30.0).abs() < 1e-9);
        let t2 = t0 + Duration::from_secs(35);
        rt.rate("c", t2, 1200.0); // no growth; baseline at t0 still in window
        let r = rt.rate("c", t2, 1200.0).unwrap();
        assert!((r - 200.0 / 35.0).abs() < 1e-9, "{r}");
    }

    #[test]
    fn window_expiry_drops_old_baseline() {
        let mut rt = RateTracker::with_window(Duration::from_secs(30));
        let t0 = Instant::now();
        rt.rate("c", t0, 1000.0);
        let t1 = t0 + Duration::from_secs(40); // t0 now outside 30s window
        rt.rate("c", t1, 1200.0);
        // Only the t1 point survives → no baseline → N/A, not (200/40).
        assert_eq!(rt.rate("c", t1, 1200.0), None);
    }

    #[test]
    fn reset_clears_history_and_rebaselines() {
        let mut rt = RateTracker::default();
        let t0 = Instant::now();
        rt.rate("c", t0, 100.0);
        let t1 = t0 + Duration::from_secs(1);
        // Value drops below history → restart; window cleared, N/A this tick.
        assert_eq!(rt.rate("c", t1, 3.0), None);
        let t2 = t1 + Duration::from_secs(1);
        assert!((rt.rate("c", t2, 5.0).unwrap() - 2.0).abs() < 1e-9);
    }
}
