//! Bucket-histogram → windowed percentiles.
//!
//! Semantics (SGLang-specific, verified against a live capture):
//! - `_bucket` counters are cumulative in `le` and flush in bursts. The
//!   scrape-to-scrape delta of each bucket is one frame's worth of new
//!   observations; differencing across `le` turns cumulative deltas into
//!   per-interval counts. A first scrape has no delta → percentiles are
//!   `None` (N/A), never fabricated.
//! - Frames accumulate in a 30s sliding window; `quantile()` merges the
//!   window and interpolates linearly inside the bucket holding the rank.
//! - `mean` rides the same window via `_sum`/`_count` deltas.
//! - Only the four rendered families are baselined — the server carries a
//!   dozen more histograms we never draw, and memory follows the consumers.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::time::{Duration, Instant};

use crate::model::snapshot::{LatencySet, Percentile};
use crate::poll::prom::{self, Sample};

pub const HIST_WINDOW: Duration = Duration::from_secs(30);

/// The latency families TRAFFIC renders, as (metric base, LatencySet slot).
/// Names are *chosen*, not required — a rename degrades its row to N/A (R1).
pub const FAMILIES: [&str; 4] = [
    "sglang:queue_time_seconds",
    "sglang:time_to_first_token_seconds",
    "sglang:inter_token_latency_seconds",
    "sglang:e2e_request_latency_seconds",
];

/// Request-length histograms tracked for their *windowed* mean only (no
/// percentiles drawn): an all-time sum/count mean goes stale — 65.7k avg
/// prompt at request 21k looks identical to one at request 2M — while the
/// rate-window mean says what the traffic is *now*, the same argument that
/// put `now …` on the cache row. Buckets aren't scraped for these families.
pub const MEAN_ONLY: [&str; 3] = [
    "sglang:prompt_tokens_histogram",
    "sglang:generation_tokens_histogram",
    "sglang:uncached_prompt_tokens_histogram",
];

/// Bucket edge as an orderable key (f64 is not Ord; total order via
/// `total_cmp`, which orders -0.0/+0.0 and puts +Inf above every finite).
/// NaN never reaches here — `update` rejects non-finite values upstream.
#[derive(Clone, Copy, Debug)]
struct Le(f64);

impl PartialEq for Le {
    fn eq(&self, other: &Self) -> bool {
        self.0.to_bits() == other.0.to_bits()
    }
}
impl Eq for Le {}
impl Ord for Le {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.total_cmp(&other.0)
    }
}
impl PartialOrd for Le {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// One histogram series (label set without `le`) plus its sum/count twin.
struct Subject {
    prev_bucket: BTreeMap<Le, f64>,
    /// One entry per scrape with ≥1 new observation: (ts, interval counts).
    /// Each interval carries its own true (lower, upper) bucket bounds, not
    /// just its upper edge — an interval's lower bound is the bucket edge
    /// just below it on the full staircase, even when that lower edge's own
    /// interval count is zero and never reaches the merged window (see
    /// `quantile_of`).
    window: VecDeque<(Instant, Vec<Interval>)>,
    prev_sum: Option<f64>,
    prev_count: Option<f64>,
    sums: VecDeque<(Instant, f64, f64)>, // (ts, Δsum, Δcount)
}

/// One bucket interval's new observations: `count` of them in `(lower, upper]`.
#[derive(Clone, Copy)]
struct Interval {
    lower: f64,
    upper: f64,
    count: f64,
}

impl Subject {
    fn new() -> Self {
        Subject {
            prev_bucket: BTreeMap::new(),
            window: VecDeque::new(),
            prev_sum: None,
            prev_count: None,
            sums: VecDeque::new(),
        }
    }

    /// `buckets` sorted by le. Scrape-to-scrape deltas of cumulative
    /// buckets are themselves cumulative — differencing across le turns
    /// them into per-interval counts, one window frame per scrape. Each
    /// interval's `lower` is the previous edge in this scrape's own sorted
    /// ladder (0.0 before the first finite edge), so it's correct even when
    /// no new observation landed in the bucket below.
    fn record_buckets(&mut self, now: Instant, buckets: &[(f64, f64)]) {
        let mut deltas = Vec::with_capacity(buckets.len());
        let mut prev_d = 0.0; // running cumulative delta, for intervaling
        let mut prev_le = 0.0f64; // this scrape's edge ladder, for true lower bounds
        for &(le, v) in buckets {
            let d = match self.prev_bucket.get(&Le(le)) {
                None => 0.0,                      // first sight: no baseline, no fake sample
                Some(&old) => (v - old).max(0.0), // reset clamps to 0
            };
            self.prev_bucket.insert(Le(le), v);
            let interval = (d - prev_d).max(0.0);
            prev_d = d;
            if interval > 0.0 {
                deltas.push(Interval {
                    lower: prev_le,
                    upper: le,
                    count: interval,
                });
            }
            // Only finite edges advance the ladder: +Inf sorts last and a
            // duplicated +Inf line must not shift `lower` off the real edge.
            if le.is_finite() {
                prev_le = le;
            }
        }
        if !deltas.is_empty() {
            self.window.push_back((now, deltas));
        }
    }

    fn record_sum_count(&mut self, now: Instant, sum: Option<f64>, count: Option<f64>) {
        if let (Some(s), Some(c), Some(ps), Some(pc)) = (sum, count, self.prev_sum, self.prev_count)
        {
            let (ds, dc) = ((s - ps).max(0.0), (c - pc).max(0.0));
            if dc > 0.0 {
                self.sums.push_back((now, ds, dc));
            }
        }
        if let Some(s) = sum {
            self.prev_sum = Some(s);
        }
        if let Some(c) = count {
            self.prev_count = Some(c);
        }
    }

    fn expire(&mut self, now: Instant) {
        while self
            .window
            .front()
            .is_some_and(|(ts, _)| now.duration_since(*ts) > HIST_WINDOW)
        {
            self.window.pop_front();
        }
        while self
            .sums
            .front()
            .is_some_and(|(ts, ..)| now.duration_since(*ts) > HIST_WINDOW)
        {
            self.sums.pop_front();
        }
    }
}

pub struct HistTracker {
    subjects: HashMap<String, BTreeMap<String, Subject>>, // base → subject key
}

impl Default for HistTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl HistTracker {
    pub fn new() -> Self {
        HistTracker {
            subjects: HashMap::new(),
        }
    }

    pub fn update(&mut self, m: &prom::Metrics, now: Instant) {
        // base → subject → (buckets | sum | count), grouped before recording.
        let mut scraped: BTreeMap<&str, BTreeMap<String, Scraped>> = BTreeMap::new();
        for x in &m.samples {
            let base = if let Some(b) = x.name.strip_suffix("_bucket") {
                b
            } else if let Some(b) = x.name.strip_suffix("_sum") {
                b
            } else if let Some(b) = x.name.strip_suffix("_count") {
                b
            } else {
                continue;
            };
            let mean_only = MEAN_ONLY.contains(&base);
            if (!FAMILIES.contains(&base) && !mean_only) || !x.value.is_finite() || x.value < 0.0 {
                continue;
            }
            let entry = scraped
                .entry(base)
                .or_default()
                .entry(subject_key(x))
                .or_default();
            if x.name.ends_with("_bucket") {
                if mean_only {
                    continue; // no percentiles drawn for these families
                }
                let le = match x.label("le") {
                    Some("+Inf") => f64::INFINITY,
                    Some(v) => match v.parse::<f64>() {
                        Ok(f) if f.is_finite() => f,
                        _ => continue, // malformed edges are skipped, not trusted
                    },
                    None => continue,
                };
                entry.buckets.push((le, x.value));
            } else if x.name.ends_with("_sum") {
                entry.sum = Some(x.value);
            } else {
                entry.count = Some(x.value);
            }
        }

        for (base, groups) in scraped {
            let subjects = self.subjects.entry(base.to_string()).or_default();
            for (key, mut sc) in groups {
                sc.buckets.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
                let sub = subjects.entry(key).or_insert_with(Subject::new);
                sub.record_buckets(now, &sc.buckets);
                sub.record_sum_count(now, sc.sum, sc.count);
                sub.expire(now);
            }
        }
    }

    /// p50/p90/p99 (bucket-interpolated) + mean (sum/count) for one family,
    /// merging every subject (streaming variants share the row). All-None
    /// until the window holds observations.
    pub fn latency(&self, base: &str, now: Instant) -> Option<Percentile> {
        let subjects = self.subjects.get(base)?;
        // Key by (lower, upper) rather than just the upper edge: two subjects
        // sharing a family normally share a bucket ladder, but the true lower
        // bound travels with each interval regardless.
        let mut merged: BTreeMap<(Le, Le), f64> = BTreeMap::new();
        let (mut dsum, mut dcnt) = (0.0f64, 0.0f64);
        for sub in subjects.values() {
            for (ts, entries) in &sub.window {
                if now.duration_since(*ts) > HIST_WINDOW {
                    continue;
                }
                for &iv in entries {
                    *merged.entry((Le(iv.lower), Le(iv.upper))).or_insert(0.0) += iv.count;
                }
            }
            for (ts, s, c) in &sub.sums {
                if now.duration_since(*ts) <= HIST_WINDOW {
                    dsum += s;
                    dcnt += c;
                }
            }
        }
        if merged.values().sum::<f64>() <= 0.0 {
            return None;
        }
        let mean = if dcnt > 0.0 { Some(dsum / dcnt) } else { None };
        Some(Percentile {
            p50: quantile_of(&merged, 0.5)?,
            p90: quantile_of(&merged, 0.9)?,
            p99: quantile_of(&merged, 0.99)?,
            mean,
        })
    }

    /// Fill a `LatencySet` for the snapshot; absent families stay None, and
    /// `known[i]` records whether the family's series exist at all (independent
    /// of whether enough deltas have landed to interpolate yet).
    pub fn fill(&self, s: &mut LatencySet, now: Instant) {
        s.known = [
            self.known(FAMILIES[0]),
            self.known(FAMILIES[1]),
            self.known(FAMILIES[2]),
            self.known(FAMILIES[3]),
        ];
        s.queue = self.latency(FAMILIES[0], now);
        s.ttft = self.latency(FAMILIES[1], now);
        s.itl = self.latency(FAMILIES[2], now);
        s.e2e = self.latency(FAMILIES[3], now);
    }

    /// Windowed mean (Δsum/Δcount over HIST_WINDOW, all subjects merged) for
    /// a MEAN_ONLY family. None while the window holds no observations —
    /// a quiet window says nothing, and N/A beats a stale all-time mean.
    pub fn windowed_mean(&self, base: &str, now: Instant) -> Option<f64> {
        let subjects = self.subjects.get(base)?;
        let (mut dsum, mut dcnt) = (0.0f64, 0.0f64);
        for sub in subjects.values() {
            for (ts, s, c) in &sub.sums {
                if now.duration_since(*ts) <= HIST_WINDOW {
                    dsum += s;
                    dcnt += c;
                }
            }
        }
        if dcnt > 0.0 && dsum.is_finite() {
            Some(dsum / dcnt)
        } else {
            None
        }
    }

    /// True once the latest scrapes have baselined at least one series of this
    /// family — the row is worth drawing (N/A until the first delta fills it).
    fn known(&self, base: &str) -> bool {
        self.subjects.get(base).is_some_and(|s| !s.is_empty())
    }

    /// Drop subjects the latest scrape no longer contains (model reload,
    /// streaming split appeared/vanished) so a disappeared series can't pin
    /// its window in memory. Call after `update` with the same scrape.
    pub fn prune_to(&mut self, m: &prom::Metrics) {
        let mut alive: std::collections::HashSet<(&str, String)> = std::collections::HashSet::new();
        for x in &m.samples {
            if let Some(base) = x.name.strip_suffix("_bucket")
                && FAMILIES.contains(&base)
            {
                alive.insert((base, subject_key(x)));
            }
            // MEAN_ONLY families carry no bucket lines; _count marks them alive.
            if let Some(base) = x.name.strip_suffix("_count")
                && MEAN_ONLY.contains(&base)
            {
                alive.insert((base, subject_key(x)));
            }
        }
        for (base, subjects) in self.subjects.iter_mut() {
            subjects.retain(|key, _| alive.contains(&(base.as_str(), key.clone())));
        }
        self.subjects.retain(|_, subjects| !subjects.is_empty());
    }
}

#[derive(Default)]
struct Scraped {
    buckets: Vec<(f64, f64)>,
    sum: Option<f64>,
    count: Option<f64>,
}

/// Linear-interpolated quantile over merged interval counts. `merged` maps
/// `(lower, upper)` → count; walked ordered by `upper` so cumulative mass
/// accumulates up the ladder, interpolating inside the interval holding the
/// rank using that interval's own true lower bound.
fn quantile_of(merged: &BTreeMap<(Le, Le), f64>, q: f64) -> Option<f64> {
    let total: f64 = merged.values().sum();
    if total <= 0.0 {
        return None;
    }
    let mut ivs: Vec<(f64, f64, f64)> = merged
        .iter()
        .map(|((lower, upper), &count)| (lower.0, upper.0, count))
        .collect();
    ivs.sort_by(|a, b| a.1.total_cmp(&b.1));
    let target = q * total;
    let mut cum = 0.0;
    for (lower, upper, count) in ivs {
        if cum + count >= target {
            if !upper.is_finite() {
                return Some(lower); // +Inf: this interval's lower edge is the bound
            }
            let frac = if count <= 0.0 {
                0.0
            } else {
                (target - cum) / count
            };
            return Some(lower + (upper - lower) * frac);
        }
        cum += count;
    }
    None
}

/// Labels without `le`, sorted — identifies one histogram series.
fn subject_key(x: &Sample) -> String {
    let mut s = String::new();
    for (k, v) in x.labels.iter().filter(|(k, _)| k != "le") {
        s.push_str(k);
        s.push('=');
        s.push_str(v);
        s.push(',');
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scrape(v1: f64, v2: f64) -> String {
        format!(
            "sglang:time_to_first_token_seconds_bucket{{is_streaming=\"false\",le=\"1.0\"}} {v1}\n\
             sglang:time_to_first_token_seconds_bucket{{is_streaming=\"false\",le=\"2.0\"}} {v2}\n\
             sglang:time_to_first_token_seconds_bucket{{is_streaming=\"false\",le=\"+Inf\"}} {v2}\n"
        )
    }

    const TTFT: &str = "sglang:time_to_first_token_seconds";

    #[test]
    fn first_scrape_has_no_percentiles() {
        let mut t = HistTracker::new();
        let now = Instant::now();
        t.update(&prom::parse(&scrape(3.0, 5.0)), now);
        assert!(t.latency(TTFT, now).is_none(), "no delta → honest N/A");
    }

    #[test]
    fn interpolates_inside_the_containing_bucket() {
        let mut t = HistTracker::new();
        let t0 = Instant::now();
        t.update(&prom::parse(&scrape(3.0, 5.0)), t0);
        // Deltas: le=1.0 +2, le=2.0 +2, +Inf +0 → intervals [0,1)=2, rest 0.
        t.update(&prom::parse(&scrape(5.0, 7.0)), t0 + Duration::from_secs(1));
        let p = t.latency(TTFT, t0 + Duration::from_secs(2)).unwrap();
        assert!(
            (p.p50 - 0.5).abs() < 1e-9,
            "mid of the [0,1) mass: {}",
            p.p50
        );
    }

    #[test]
    fn window_merges_across_scrapes() {
        let mut t = HistTracker::new();
        let t0 = Instant::now();
        t.update(&prom::parse(&scrape(0.0, 0.0)), t0);
        t.update(&prom::parse(&scrape(4.0, 4.0)), t0 + Duration::from_secs(1)); // [0,1)=4
        t.update(&prom::parse(&scrape(4.0, 8.0)), t0 + Duration::from_secs(2)); // [1,2)=4
        let p = t.latency(TTFT, t0 + Duration::from_secs(3)).unwrap();
        assert!((p.p50 - 1.0).abs() < 1e-9, "boundary edge: {}", p.p50);
        assert!((p.p90 - 1.8).abs() < 1e-9, "p90 inside [1,2): {}", p.p90);
    }

    #[test]
    fn window_expiry_empties_percentiles_again() {
        let mut t = HistTracker::new();
        let t0 = Instant::now();
        t.update(&prom::parse(&scrape(0.0, 0.0)), t0);
        t.update(
            &prom::parse(&scrape(10.0, 10.0)),
            t0 + Duration::from_secs(1),
        );
        assert!(t.latency(TTFT, t0 + Duration::from_secs(31)).is_some());
        assert!(t.latency(TTFT, t0 + Duration::from_secs(61)).is_none());
    }

    #[test]
    fn mean_from_sum_count_window() {
        let mut t = HistTracker::new();
        let t0 = Instant::now();
        let s = |a: f64, b: f64, sm: f64, ct: f64| {
            format!(
                "{}sglang:time_to_first_token_seconds_sum{{is_streaming=\"false\"}} {sm}\n\
                 sglang:time_to_first_token_seconds_count{{is_streaming=\"false\"}} {ct}\n",
                scrape(a, b)
            )
        };
        t.update(&prom::parse(&s(0.0, 0.0, 0.0, 0.0)), t0);
        // +10 observations, +8s of latency → mean 0.8s.
        t.update(
            &prom::parse(&s(10.0, 10.0, 8.0, 10.0)),
            t0 + Duration::from_secs(1),
        );
        let p = t.latency(TTFT, t0 + Duration::from_secs(2)).unwrap();
        assert!((p.mean.unwrap() - 0.8).abs() < 1e-9, "mean: {:?}", p.mean);
    }

    #[test]
    fn plus_inf_mass_clamps_to_last_finite_edge() {
        let mut t = HistTracker::new();
        let t0 = Instant::now();
        t.update(&prom::parse(&scrape(0.0, 0.0)), t0);
        // Only the +Inf bucket moves: le=+Inf emitted twice (same subject,
        // parser keeps both lines; grouping merges by le within the frame).
        let b = "sglang:time_to_first_token_seconds_bucket{is_streaming=\"false\",le=\"+Inf\"} 7\n";
        let second = format!("{}{}", scrape(0.0, 0.0), b);
        t.update(&prom::parse(&second), t0 + Duration::from_secs(1));
        let p = t.latency(TTFT, t0 + Duration::from_secs(2)).unwrap();
        assert_eq!(p.p50, 2.0, "clamped to the last finite edge");
    }

    #[test]
    fn counter_reset_clamps_to_zero_not_negative() {
        let mut t = HistTracker::new();
        let t0 = Instant::now();
        t.update(&prom::parse(&scrape(5.0, 9.0)), t0);
        t.update(&prom::parse(&scrape(1.0, 1.0)), t0 + Duration::from_secs(1)); // restart
        // First frame had no baseline; reset frame clamps → zero observations.
        assert!(t.latency(TTFT, t0 + Duration::from_secs(2)).is_none());
    }

    #[test]
    fn untracked_families_are_not_baselined() {
        let mut t = HistTracker::new();
        t.update(
            &prom::parse("sglang:eviction_duration_seconds_bucket{le=\"1.0\"} 5\n"),
            Instant::now(),
        );
        assert!(!t.subjects.contains_key("sglang:eviction_duration_seconds"));
    }

    #[test]
    fn real_fixture_interpolates_against_real_bucket_edges() {
        let raw = include_str!("../../tests/fixtures/live_metrics.txt");
        let mut t = HistTracker::new();
        let t0 = Instant::now();
        t.update(&prom::parse(raw), t0);

        // Bump the is_streaming="false" ttft subject's le=0.4 cumulative by
        // exactly 100; every other line is untouched.
        let from = "sglang:time_to_first_token_seconds_bucket{engine_type=\"unified\",is_streaming=\"false\",le=\"0.4\",model_name=\"Qwen3.8-Flash-Next\"} 404.0";
        let to = "sglang:time_to_first_token_seconds_bucket{engine_type=\"unified\",is_streaming=\"false\",le=\"0.4\",model_name=\"Qwen3.8-Flash-Next\"} 504.0";
        assert!(raw.contains(from), "fixture line drifted");
        let m2 = prom::parse(&raw.replace(from, to));
        t.update(&m2, t0 + Duration::from_secs(1));

        let now = t0 + Duration::from_secs(2);
        let p = t.latency(TTFT, now).unwrap();
        // All 100 new obs land in (0.2, 0.4]: p50 = 0.2 + 0.2·(50/100) = 0.3.
        assert!((p.p50 - 0.3).abs() < 1e-6, "against real edges: {}", p.p50);
        assert!(p.p99 <= 0.4, "p99 inside the bumped bucket: {}", p.p99);
    }
}
