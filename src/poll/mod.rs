pub mod fetch;
pub mod info;
pub mod prom;
pub mod quantile;
pub mod rate;

use std::collections::BTreeSet;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use tokio::sync::mpsc;

use crate::config::Config;
use crate::error::PollError;
use crate::model::snapshot::{Connection, RankGranularity, RankRow, Snapshot};
use crate::poll::fetch::Http;
use crate::poll::quantile::HistTracker;
use crate::poll::rate::RateTracker;

/// Poller-owned conversion state: counter→rate baselines and histogram
/// windows. Both live here, never in the UI.
#[derive(Default)]
pub struct PollState {
    pub rates: RateTracker,
    pub hist: HistTracker,
}

/// Consecutive failures tolerated before `Degraded`, and before `Lost`.
const DEGRADED_AFTER: u32 = 1;
const LOST_AFTER: u32 = 4;

/// UI→poller runtime controls, written by `App` (pause / interval) and read
/// once per loop by `run`. A plain `RwLock` (not atomic) because two fields
/// must be read together and std atomics don't compose. The draw tick is
/// independent, so pausing stops scraping, not drawing — btop's `u` idiom.
#[derive(Debug, Default)]
pub struct PollControl {
    pub paused: bool,
    /// Healthy-scrape pacing in ms; `+`/`-` adjust within [100, 10000].
    pub interval_ms: u64,
}

impl PollControl {
    pub fn step_interval(&mut self, delta_ms: i64) {
        let next = self.interval_ms as i64 + delta_ms;
        self.interval_ms = next.clamp(100, 10_000) as u64;
    }
}

/// A nudge to the UI: the poller has already written into the shared
/// `Snapshot`; redraw now.
pub struct Tick;

/// Background poller: owns the HTTP client + rate state, is the *only* writer
/// of the shared `Snapshot` (via `build`). The UI holds a read lock per frame.
///
/// Backoff lives here, not in the UI: the interval only paces healthy scrapes;
/// failures escalate `1→2→4→8→15s` until a success returns to healthy.
pub async fn run(
    config: Config,
    control: Arc<RwLock<PollControl>>,
    snapshot: Arc<RwLock<Snapshot>>,
    tx: mpsc::Sender<Tick>,
) {
    let http = Http::new(&config);
    let mut st = PollState::default();
    let mut consecutive: u32 = 0;
    let mut backoff_idx: usize = 0;
    const BACKOFF_MS: [u64; 5] = [1000, 2000, 4000, 8000, 15000];
    // Server identity gate: (re)fetch /model_info+/server_info once per
    // identity so a stable server isn't auth-hammered every tick.
    let mut last_served_name: Option<String> = None;

    loop {
        // Read the runtime controls fresh each iteration — `u` and `+`/`-`
        // from the UI take effect within one pacing sleep.
        let (paused, interval_ms) = {
            let c = control.read().unwrap();
            (c.paused, c.interval_ms)
        };
        if paused {
            // Held: keep the last snapshot frozen (panels already dim via
            // nothing here — pausing is a deliberate freeze, not a fault).
            // No Tick, no scrape; wake on the next control change via the
            // draw tick's own cadence by polling the flag on a short sleep.
            tokio::time::sleep(Duration::from_millis(150)).await;
            continue;
        }
        let now = Instant::now();
        match scrape(&http, &config).await {
            Ok((m, rtt)) => {
                consecutive = 0;
                backoff_idx = 0;

                let served = m
                    .samples
                    .iter()
                    .find_map(|s| s.label("model_name"))
                    .unwrap_or_default()
                    .to_string();

                // Probe outside any lock: a std guard must never span .await.
                let need_meta = {
                    let s = snapshot.read().unwrap();
                    !s.server.loaded || last_served_name.as_deref() != Some(served.as_str())
                };
                let meta = if need_meta {
                    info::fetch(&http).await.ok()
                } else {
                    None
                };

                {
                    let mut s = snapshot.write().unwrap();
                    match meta {
                        Some(meta) => {
                            last_served_name = Some(meta.served_model_name.clone());
                            s.server = meta;
                        }
                        // A meta failure doesn't degrade the numbers; retry
                        // next tick (server.loaded stays false).
                        None if need_meta => last_served_name = None,
                        None => {}
                    }
                    build(&m, &mut st, now, rtt, &mut s);
                }

                // Newest-sample-wins: skip (never queue) when the UI is behind.
                let _ = tx.try_send(Tick);
                tokio::time::sleep(Duration::from_millis(interval_ms)).await;
            }
            Err(PollError::Status {
                status: 404,
                ref path,
            }) if path.ends_with("/metrics") => {
                // Fatal per product decision: server is up but metrics off.
                let mut s = snapshot.write().unwrap();
                s.conn = Connection::MetricsDisabled;
                let _ = tx.try_send(Tick);
                return;
            }
            Err(e) => {
                consecutive += 1;
                let msg = e.to_string();
                {
                    let mut s = snapshot.write().unwrap();
                    if consecutive >= LOST_AFTER {
                        s.conn = Connection::Lost {
                            since: s.conn.lost_since().unwrap_or(now),
                            last_err: msg,
                        };
                    } else if consecutive >= DEGRADED_AFTER {
                        // Only the *first* slip sets `since`; retried errors
                        // must not restart the staleness clock.
                        if !matches!(s.conn, Connection::Degraded { .. }) {
                            s.conn = Connection::Degraded {
                                since: now,
                                consecutive,
                                last_err: msg,
                            };
                        }
                    }
                }
                let _ = tx.try_send(Tick);

                let wait = BACKOFF_MS[backoff_idx.min(BACKOFF_MS.len() - 1)];
                backoff_idx = (backoff_idx + 1).min(BACKOFF_MS.len() - 1);
                tokio::time::sleep(Duration::from_millis(wait)).await;
            }
        }
    }
}

/// Scrape `/metrics` and parse it, honouring `--max-metrics-bytes` (cut on a
/// char boundary; the parser flags the severed tail as `truncated`).
pub async fn scrape(http: &Http, config: &Config) -> Result<(prom::Metrics, Duration), PollError> {
    let f = http.get("/metrics").await?;
    let body = clip_utf8(&f.body, config.max_metrics_bytes as usize);
    Ok((prom::parse(body), f.rtt))
}

fn clip_utf8(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// The single data-writing path: fold a fresh scrape into the live snapshot,
/// pushing exactly one history sample per ring (history lives *in* the
/// snapshot so it survives across ticks; a draw path could never replay it).
pub fn build(m: &prom::Metrics, st: &mut PollState, now: Instant, rtt: Duration, s: &mut Snapshot) {
    s.metrics_truncated = m.truncated;
    st.hist.update(m, now);
    st.hist.prune_to(m);
    // Percentiles read the window this very update just fed.
    let mut latency = crate::model::snapshot::LatencySet::default();
    st.hist.fill(&mut latency, now);
    let rates = &mut st.rates;
    // R4: with `--enable-dp-attention` but not `--enable-metrics-for-all-
    // schedulers`, every scheduler reports under tp_rank=0, so per-rank rows
    // would be duplicates of a single attribution. Detect it from /server_info
    // (already in `s.server`) and collapse RANKS to one aggregate row + a
    // footer reason, rather than render rows that look distinct but aren't.
    if s.server.loaded
        && s.server.enable_dp_attention
        && !s.server.enable_metrics_for_all_schedulers
    {
        s.rank_granularity = RankGranularity::Aggregated(
            "--enable-dp-attention without --enable-metrics-for-all-schedulers".into(),
        );
    }

    // --- aggregate reads ---------------------------------------------------
    // Queue-count gauges get a `priority=""` total series plus per-priority
    // breakdowns; only the default-priority series are totals.
    let first_val = |name: &str| -> Option<f64> {
        m.get(name)
            .find(|x| x.label("priority").map(|p| p.is_empty()).unwrap_or(true))
            .map(|x| x.value)
            .filter(|v| !v.is_nan())
    };

    // --- ENGINE ---
    let gen_total = sum_across_ranks(m, "sglang:gen_throughput", false);
    s.engine.gen_throughput = gen_total;
    s.engine.gen_series.push(gen_total);
    s.engine.prefill_tps = rate_of_mode(
        m,
        rates,
        "sglang:realtime_tokens_total",
        "prefill_compute",
        now,
    )
    .or_else(|| rate_of(m, rates, "sglang:prompt_tokens_total", now));
    s.engine
        .prefill_series
        .push(s.engine.prefill_tps.unwrap_or(0.0));
    s.engine.decode_tps = rate_of_mode(m, rates, "sglang:realtime_tokens_total", "decode", now)
        .or_else(|| rate_of(m, rates, "sglang:generation_tokens_total", now));
    s.engine
        .token_rate_series
        .push(s.engine.decode_tps.unwrap_or(0.0));
    s.engine.utilization = first_val("sglang:utilization");
    s.engine.fwd_occupancy = first_val("sglang:fwd_occupancy");
    // Queue totals: sum the per-scheduler rank-scoped series; fall back to the
    // aggregate-only series when ranks aren't attributed (sum_across_ranks).
    s.engine.running_reqs = sum_across_ranks(m, "sglang:num_running_reqs", true).max(0.0) as u64;
    s.engine.waiting_reqs = sum_across_ranks(m, "sglang:num_queue_reqs", true).max(0.0) as u64;

    // Helper-process CPU: rate of process_cpu_seconds_total = cores in use
    // right now. A pegged tokenizer (cores >> 1 is fine — it's multi-worker;
    // the readout is for trend/anomaly, not a hard threshold).
    {
        let mut cores = |component: &str| -> Option<f64> {
            let pts: Vec<(String, f64)> = m
                .get("sglang:process_cpu_seconds_total")
                .filter(|x| x.label("component") == Some(component))
                .map(|x| (x.key(), x.value))
                .collect();
            if pts.is_empty() {
                None
            } else {
                rates.rate_sum(now, pts.iter().map(|(k, v)| (k.as_str(), *v)))
            }
        };
        s.engine.tokenizer_cores = cores("tokenizer");
        s.engine.detokenizer_cores = cores("detokenizer");
    }

    // --- KV ---
    s.kv.used_tokens = sum_across_ranks(m, "sglang:num_used_tokens", true) as u64;
    s.kv.total_tokens = sum_across_ranks(m, "sglang:max_total_num_tokens", true) as u64;
    // The info overlay reads it from ServerMeta; the gauge is the owner.
    s.server.max_total_num_tokens = Some(s.kv.total_tokens);
    // token_usage is documented (sg_metrics.py:87) as max(full, swa, mamba)
    // per rank — the bottleneck. Cross-rank aggregate: max.
    s.kv.token_usage = max_val(m, "sglang:token_usage");
    s.kv.full_token_usage = first_val("sglang:full_token_usage");
    s.kv.swa_token_usage = first_val("sglang:swa_token_usage");
    s.kv.mamba_usage = first_val("sglang:mamba_usage");
    s.kv.swa_used = sum_if_present(m, "sglang:swa_used_tokens").map(|v| v as u64);
    s.kv.mamba_used = sum_if_present(m, "sglang:mamba_used_tokens").map(|v| v as u64);
    s.kv.available_tokens = sum_if_present(m, "sglang:kv_available_tokens").map(|v| v as u64);
    s.kv.evictable_tokens = sum_if_present(m, "sglang:kv_evictable_tokens").map(|v| v as u64);
    // Lifetime eviction pressure: how many device slots have *ever* been
    // freed. The instant `evictable` gauge only shows the current free-list;
    // this counter shows how hard the cache has actually been working.
    {
        let ev: Vec<(String, f64)> = m
            .get("sglang:evicted_tokens_total")
            .map(|x| (x.key(), x.value))
            .collect();
        if ev.is_empty() {
            s.kv.evicted_total = None;
            s.kv.evicted_tps = None;
        } else {
            s.kv.evicted_total = Some(ev.iter().map(|(_, v)| *v as u64).sum());
            s.kv.evicted_tps = rates.rate_sum(now, ev.iter().map(|(k, v)| (k.as_str(), *v)));
        }
    }
    s.kv.cache_hit_rate = first_val("sglang:cache_hit_rate").unwrap_or(0.0);
    s.kv.cache_hit_series.push(s.kv.cache_hit_rate);
    // CUDA-graph decode coverage, cumulative: decode passes executed under a
    // graph over all decode passes. Modes pair {prefill,decode}×{cuda_graph,
    // none} — match by prefix so unseen spellings ("decode_none"…) fold in.
    // Cumulative (not windowed) is the honest long-run coverage: a graph
    // fallback starting now shows up within a log-interval of samples.
    {
        let mut dec_all = 0.0f64;
        let mut dec_cg = 0.0f64;
        let mut any = false;
        for x in m.get("sglang:cuda_graph_passes_total") {
            let Some(mode) = x.label("mode") else {
                continue;
            };
            if !mode.starts_with("decode") || x.value.is_nan() {
                continue;
            }
            any = true;
            dec_all += x.value;
            if mode.contains("cuda_graph") {
                dec_cg += x.value;
            }
        }
        s.engine.cg_decode_share = if any && dec_all > 0.0 {
            Some((dec_cg / dec_all).clamp(0.0, 1.0))
        } else {
            None
        };
    }
    s.engine.new_token_ratio = first_val("sglang:new_token_ratio");
    // Startup timeline (constant after boot; rides the meta section of the
    // info overlay). Longest phase first — the headline is the bottleneck.
    {
        let mut phases: Vec<(String, f64)> = m
            .get("sglang:startup_time_seconds")
            .filter(|x| !x.value.is_nan())
            .filter_map(|x| x.label("phase").map(|p| (p.to_string(), x.value)))
            .collect();
        phases.sort_by(|a, b| b.1.total_cmp(&a.1));
        if !phases.is_empty() {
            s.server.startup_phases = phases;
            s.server.startup_graph_secs = m
                .get("sglang:startup_cuda_graph_time_seconds")
                .filter(|x| !x.value.is_nan())
                .map(|x| x.value)
                .sum::<f64>()
                .into();
            s.server.startup_free_gpu_gb =
                sum_if_present(m, "sglang:startup_available_gpu_memory_gb");
        }
    }
    s.kv.weight_gb = sum_if_present(m, "sglang:weight_memory_usage_gb");
    s.kv.kv_cache_gb = sum_if_present(m, "sglang:kv_cache_memory_usage_gb");
    s.kv.graph_gb = sum_if_present(m, "sglang:graph_memory_usage_gb");
    // HiCache host pool appears only with --enable-hierarchical-cache.
    if m.get("sglang:hicache_host_total_tokens").next().is_some() {
        let hicache = s.kv.hicache.get_or_insert(Default::default());
        hicache.host_used = sum_across_ranks(m, "sglang:hicache_host_used_tokens", true) as u64;
        hicache.host_total = sum_across_ranks(m, "sglang:hicache_host_total_tokens", true) as u64;
        // Eviction destination split: tokens evicted from device are either
        // backed up to host (recoverable) or destroyed (host full → lost work).
        hicache.backuped_total =
            sum_if_present(m, "sglang:hicache_backup_tokens_total").map(|v| v as u64);
        hicache.dropped_total =
            sum_if_present(m, "sglang:hicache_dropped_tokens_total").map(|v| v as u64);
        // Backups that were actually re-used (host→GPU reload) — the loop's
        // payoff: backup without load_back is speculating into dead memory.
        hicache.load_back_total =
            sum_if_present(m, "sglang:load_back_tokens_total").map(|v| v as u64);
        // Storage prefetches that arrived but couldn't be allocated in the
        // aux pools — paid-for bytes, thrown away. Non-zero = pool sizing bug.
        hicache.prefetch_failed_total =
            sum_if_present(m, "sglang:hicache_prefetch_aux_alloc_failed_tokens_total")
                .map(|v| v as u64);
        // L3 round trip (host ↔ storage). These counters are gated on a
        // storage backend, not on hicache alone: absent → None → row hidden.
        hicache.storage_backuped_total =
            sum_if_present(m, "sglang:backuped_tokens_total").map(|v| v as u64);
        hicache.storage_prefetched_total =
            sum_if_present(m, "sglang:prefetched_tokens_total").map(|v| v as u64);
        // Per-op copy latency, exact from sum/count (no bucket interpolation).
        // Zero count → None: "no copies yet" is not "copies are instant".
        let hist_mean = |fam: &str| -> Option<f64> {
            let count = sum_across_ranks(m, &format!("{fam}_count"), false);
            let sum = sum_across_ranks(m, &format!("{fam}_sum"), false);
            if count > 0.0 && sum.is_finite() && count.is_finite() {
                Some(sum / count)
            } else {
                None
            }
        };
        hicache.backup_mean_secs = hist_mean("sglang:hicache_backup_duration_seconds");
        hicache.load_back_mean_secs = hist_mean("sglang:load_back_duration_seconds");
    }

    // --- TRAFFIC (NET triple × two directions) ---
    s.traffic.gen_tps = s.engine.decode_tps.unwrap_or(0.0);
    s.traffic.gen_series.push(s.traffic.gen_tps);
    s.traffic.gen_peak_tps = s.traffic.gen_series.max().unwrap_or(s.traffic.gen_tps);
    s.traffic.gen_total_tokens = sum_across_ranks(m, "sglang:generation_tokens_total", true) as u64;

    s.traffic.prompt_tps = s.engine.prefill_tps.unwrap_or(0.0);
    s.traffic.prompt_series.push(s.traffic.prompt_tps);
    s.traffic.prompt_peak_tps = s
        .traffic
        .prompt_series
        .max()
        .unwrap_or(s.traffic.prompt_tps);
    s.traffic.prompt_total_tokens = sum_across_ranks(m, "sglang:prompt_tokens_total", true) as u64;

    s.traffic.req_rate_in = rate_of(m, rates, "sglang:num_requests_total", now).unwrap_or(0.0);
    s.traffic.total_requests = sum_across_ranks(m, "sglang:num_requests_total", true) as u64;
    // "out" direction on the wire is generated tokens; mirror the gen rate.
    s.traffic.req_rate_out = s.traffic.gen_tps;

    s.traffic.aborted = sum_across_ranks(m, "sglang:num_aborted_requests_total", true) as u64;
    // The real gauge name is num_retracted_reqs (with a legacy candidate);
    // sum_across_ranks only reads one name, so pick the first present.
    s.traffic.retracted = [
        "sglang:num_retracted_requests_total",
        "sglang:num_retracted_reqs",
    ]
    .iter()
    .find_map(|n| m.get(n).next().map(|_| sum_across_ranks(m, n, true)))
    .unwrap_or(0.0) as u64;
    s.traffic.paused = first_val("sglang:num_paused_reqs").unwrap_or(0.0).max(0.0) as u64;
    s.traffic.grammar_queue = first_val("sglang:num_grammar_queue_reqs")
        .unwrap_or(0.0)
        .max(0.0) as u64;
    // PD-disaggregation queues. The gauges exist on every server (unified
    // reports zeros), so read them *only* when the server told us it runs a
    // disaggregation role — otherwise a unified box would show four misleading
    // zeros and imply a PD topology that isn't there.
    if matches!(
        s.server.disaggregation_mode.as_str(),
        "prefill" | "decode" | "both"
    ) {
        s.traffic.pd_prefill_bootstrap =
            sum_if_present(m, "sglang:num_prefill_bootstrap_queue_reqs").map(|v| v as u64);
        s.traffic.pd_prefill_inflight =
            sum_if_present(m, "sglang:num_prefill_inflight_queue_reqs").map(|v| v as u64);
        s.traffic.pd_decode_prealloc =
            sum_if_present(m, "sglang:num_decode_prealloc_queue_reqs").map(|v| v as u64);
        s.traffic.pd_decode_transfer =
            sum_if_present(m, "sglang:num_decode_transfer_queue_reqs").map(|v| v as u64);
    }
    s.traffic.latency = latency;

    // --- sglang's own HTTP surface (our endpoint, /metrics, excluded) ------
    {
        let mine = |x: &prom::Sample| x.label("endpoint") == Some("/metrics");
        let req_pts: Vec<(String, f64)> = m
            .get("sglang:http_requests_total")
            .filter(|x| !mine(x))
            .map(|x| (x.key(), x.value))
            .collect();
        s.traffic.http_rps = if req_pts.is_empty() {
            None
        } else {
            rates.rate_sum(now, req_pts.iter().map(|(k, v)| (k.as_str(), *v)))
        };
        // Errors: responses with status >= 400 over the same window.
        let err_pts: Vec<(String, f64)> = m
            .get("sglang:http_responses_total")
            .filter(|x| {
                !mine(x)
                    && x.label("status_code")
                        .and_then(|s| s.parse::<u16>().ok())
                        .unwrap_or(0)
                        >= 400
            })
            .map(|x| (x.key(), x.value))
            .collect();
        s.traffic.http_err_rate = if err_pts.is_empty() {
            None
        } else {
            rates.rate_sum(now, err_pts.iter().map(|(k, v)| (k.as_str(), *v)))
        };
        s.traffic.http_active = {
            let mut sum = 0.0f64;
            let mut any = false;
            for x in m.get("sglang:http_requests_active") {
                if !mine(x) && !x.value.is_nan() {
                    sum += x.value;
                    any = true;
                }
            }
            if any { Some(sum.max(0.0) as u64) } else { None }
        };
    }

    // --- device prefix-cache inflow (cached_tokens_total, per source) ------
    {
        let dev: Vec<(String, f64)> = m
            .get("sglang:cached_tokens_total")
            .filter(|x| x.label("cache_source") == Some("device"))
            .map(|x| (x.key(), x.value))
            .collect();
        s.traffic.cached_device_tps = if dev.is_empty() {
            None
        } else {
            rates.rate_sum(now, dev.iter().map(|(k, v)| (k.as_str(), *v)))
        };
        s.traffic.cached_device_total = if dev.is_empty() {
            0
        } else {
            dev.iter().map(|(_, v)| *v as u64).sum()
        };
        // HiCache tiers ride the same family: cache_source is "host", or
        // "storage_<backend>" (the backend is baked into the value). Read by
        // prefix so a renamed backend (nixl→3fs→…) still lands.
        let tier_sum = |prefix: &str| -> u64 {
            m.get("sglang:cached_tokens_total")
                .filter(|x| {
                    x.label("cache_source")
                        .is_some_and(|s| s.starts_with(prefix))
                })
                .filter(|x| !x.value.is_nan())
                .map(|x| x.value.max(0.0) as u64)
                .sum()
        };
        s.traffic.cached_host_total = tier_sum("host");
        s.traffic.cached_storage_total = tier_sum("storage");

        // Windowed cache share (prefill_effective_tokens_total) — sglang's own
        // formula: rate(sum of *_hit) / rate(sum of all modes). The all-time
        // totals above go stale; this is what the cache is doing now.
        //
        // Unlike the additive reads elsewhere, this is a ratio, so a partial
        // baseline poisons it: rate_sum tolerates members still without a
        // baseline, and a numerator counting 3 of 4 modes against a denominator
        // counting all 4 silently understates the share. So require *every*
        // member to have one — a window that can't fully answer says N/A.
        let effective: Vec<(&str, String, f64)> = m
            .get("sglang:prefill_effective_tokens_total")
            .filter(|x| !x.value.is_nan())
            .map(|x| (x.label("mode").unwrap_or_default(), x.key(), x.value))
            .collect();
        s.traffic.cache_window_known = !effective.is_empty();
        s.traffic.cache_window_share = if effective.is_empty() {
            None
        } else {
            let rated: Vec<(&str, Option<f64>)> = effective
                .iter()
                .map(|(mode, key, v)| (*mode, rates.rate(key, now, *v)))
                .collect();
            let sum_where = |f: fn(&str) -> bool| -> Option<f64> {
                let mut sum = 0.0;
                for (mode, r) in &rated {
                    if f(mode) {
                        sum += (*r)?;
                    }
                }
                Some(sum)
            };
            let is_hit = |mode: &str| mode.ends_with("_hit");
            let everything = |_: &str| true;
            match (sum_where(is_hit), sum_where(everything)) {
                (Some(hit), Some(all)) if all > 0.0 => Some((hit / all).clamp(0.0, 1.0)),
                _ => None,
            }
        };
    }

    // --- request-length means (histogram sum/count) -------------------------
    // Cumulative means, exact from the two counters — no bucket interpolation
    // needed. A family absent or with zero count → N/A, not 0.
    {
        let mean = |fam: &str| -> Option<f64> {
            let count = sum_across_ranks(m, &format!("{fam}_count"), false);
            if count <= 0.0 || !count.is_finite() {
                return None;
            }
            let sum = sum_across_ranks(m, &format!("{fam}_sum"), false);
            if !sum.is_finite() || sum <= 0.0 {
                None
            } else {
                Some(sum / count)
            }
        };
        s.traffic.avg_prompt_len = mean("sglang:prompt_tokens_histogram");
        s.traffic.avg_gen_len = mean("sglang:generation_tokens_histogram");
        s.traffic.avg_uncached_len = mean("sglang:uncached_prompt_tokens_histogram");
    }

    // --- RANKS ---
    build_ranks(m, now, s);

    // --- server-level pause (pause_reason rides on the pause gauge) ---
    s.paused = m
        .samples
        .iter()
        .find(|x| {
            x.name == "sglang:num_paused_reqs" && x.value > 0.0 && x.label("pause_reason").is_some()
        })
        .and_then(|x| x.label("pause_reason"))
        .map(|r| r.to_string());

    // Drop rate baselines for label sets this scrape no longer contains
    // (rank shrank, model hot-swapped) so stale baselines can't pin memory.
    {
        let live: std::collections::HashSet<&str> =
            m.samples.iter().map(|x| x.name.as_str()).collect();
        rates.retain(|key| live.contains(key.split('|').next().unwrap_or("")));
    }

    s.conn = Connection::Live {
        rtt,
        metrics_ok: true,
    };
    s.now = now;
}

/// One aggregate row + one row per visible dp_rank. Attribution-collapse
/// detection (`--enable-dp-attention` without all-schedulers) happens in the
/// run loop against /server_info; here we report what the scrape shows.
fn build_ranks(m: &prom::Metrics, now: Instant, s: &mut Snapshot) {
    let first_val = |name: &str| -> Option<f64> {
        m.get(name)
            .find(|x| x.label("priority").map(|p| p.is_empty()).unwrap_or(true))
            .map(|x| x.value)
            .filter(|v| !v.is_nan())
    };

    let mut seen: BTreeSet<u32> = Default::default();
    if matches!(s.rank_granularity, RankGranularity::PerRank) {
        for x in m.get("sglang:num_running_reqs") {
            if let Some(r) = x.label("dp_rank").and_then(|v| v.parse::<u32>().ok()) {
                seen.insert(r);
            }
        }
    }

    let old_rows = std::mem::take(&mut s.ranks);

    let mut agg = RankRow {
        running: sum_across_ranks(m, "sglang:num_running_reqs", true).max(0.0) as u64,
        waiting: sum_across_ranks(m, "sglang:num_queue_reqs", true).max(0.0) as u64,
        used_tokens: sum_across_ranks(m, "sglang:num_used_tokens", true) as u64,
        total_tokens: sum_across_ranks(m, "sglang:max_total_num_tokens", true) as u64,
        token_usage: s.kv.token_usage,
        gen_throughput: s.engine.gen_throughput,
        cache_hit_rate: s.kv.cache_hit_rate,
        utilization: s.engine.utilization,
        spec_accept_length: first_val("sglang:spec_accept_length"),
        spec_accept_rate: first_val("sglang:spec_accept_rate"),
        ..Default::default()
    };
    // Carry the aggregate ring forward across ticks (index 0 is always agg).
    let mut agg_series = old_rows
        .first()
        .filter(|r| r.dp_rank == 0)
        .map(|r| r.gen_series.clone())
        .unwrap_or_default();
    agg_series.push(agg.gen_throughput);
    agg.gen_series = agg_series;

    let mut rows = vec![agg];
    for &rank in &seen {
        let want: Vec<(&str, String)> = vec![("dp_rank", rank.to_string())];
        let want_refs: Vec<(&str, &str)> = want.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let pick = |name: &str| -> Option<f64> {
            m.find(name, &want_refs)
                .map(|x| x.value)
                .filter(|v| !v.is_nan())
        };
        let mut row = RankRow {
            dp_rank: rank,
            running: pick("sglang:num_running_reqs").unwrap_or(0.0).max(0.0) as u64,
            waiting: pick("sglang:num_queue_reqs").unwrap_or(0.0).max(0.0) as u64,
            used_tokens: pick("sglang:num_used_tokens").unwrap_or(0.0).max(0.0) as u64,
            total_tokens: pick("sglang:max_total_num_tokens").unwrap_or(0.0) as u64,
            token_usage: pick("sglang:token_usage").unwrap_or(0.0),
            gen_throughput: pick("sglang:gen_throughput").unwrap_or(0.0),
            cache_hit_rate: pick("sglang:cache_hit_rate").unwrap_or(0.0),
            utilization: pick("sglang:utilization"),
            spec_accept_length: pick("sglang:spec_accept_length"),
            spec_accept_rate: pick("sglang:spec_accept_rate"),
            gen_series: Default::default(),
        };
        // Reuse this rank's ring from the previous tick (found by dp_rank),
        // so history follows the rank across rebuilds.
        if let Some(prev) = old_rows.iter().find(|r| r.dp_rank == rank) {
            row.gen_series = prev.gen_series.clone();
        }
        row.gen_series.push(row.gen_throughput);
        rows.push(row);
    }
    s.ranks = rows;

    let _ = now; // rate baselines for per-rank counters handled via scrape keys
}

// --- scrape aggregation helpers ---------------------------------------------

/// Sum a metric across dp_rank series; if no rank-scoped series exist, fall
/// back to the default-priority aggregate (never both — it would double-count).
/// `priority_scoped` marks the queue-count family that carries `priority=""`.
fn sum_across_ranks(m: &prom::Metrics, name: &str, priority_scoped: bool) -> f64 {
    let mut sum = 0.0;
    let mut any_rank = false;
    for x in m.get(name) {
        if x.label("dp_rank").is_some() {
            any_rank = true;
            if !x.value.is_nan() {
                sum += x.value;
            }
        }
    }
    if any_rank {
        return sum;
    }
    m.get(name)
        .filter(|x| !priority_scoped || x.label("priority").map(|p| p.is_empty()).unwrap_or(true))
        .map(|x| if x.value.is_nan() { 0.0 } else { x.value })
        .sum()
}

/// Sum only when the family appears at all — an absent family is N/A, a
/// present-but-zero one is a real zero.
fn sum_if_present(m: &prom::Metrics, name: &str) -> Option<f64> {
    m.get(name).next()?;
    Some(sum_across_ranks(m, name, false))
}

fn max_val(m: &prom::Metrics, name: &str) -> f64 {
    m.get(name)
        .map(|x| x.value)
        .filter(|v| !v.is_nan())
        .fold(0.0f64, f64::max)
}

/// Counter→rate for a whole family: sum rank-scoped series' rates, or the
/// default-priority aggregate's when no rank series exist. `None` until every
/// contributing series has a baseline (first scrape) — render N/A, not 0.
fn rate_of(m: &prom::Metrics, rates: &mut RateTracker, name: &str, now: Instant) -> Option<f64> {
    let mut rank_scoped: Vec<(String, f64)> = Vec::new();
    let mut agg: Vec<(String, f64)> = Vec::new();
    for x in m.get(name) {
        if x.label("dp_rank").is_some() {
            rank_scoped.push((x.key(), x.value));
        } else if x.label("priority").map(|p| p.is_empty()).unwrap_or(true) {
            agg.push((x.key(), x.value));
        }
    }
    if !rank_scoped.is_empty() {
        return rates.rate_sum(now, rank_scoped.iter().map(|(k, v)| (k.as_str(), *v)));
    }
    rates.rate_sum(now, agg.iter().map(|(k, v)| (k.as_str(), *v)))
}

/// Rate of one `mode="…"` series of `sglang:realtime_tokens_total` — the
/// realtime decode/prefill token counters. Returns `None` when the family or
/// mode is absent (older servers), so the caller can fall back to the
/// end-of-request `generation`/`prompt_tokens_total` counters.
fn rate_of_mode(
    m: &prom::Metrics,
    rates: &mut RateTracker,
    name: &str,
    mode: &str,
    now: Instant,
) -> Option<f64> {
    let keyed: Vec<(String, f64)> = m
        .get(name)
        .filter(|x| x.label("mode") == Some(mode))
        .map(|x| (x.key(), x.value))
        .collect();
    if keyed.is_empty() {
        return None;
    }
    rates.rate_sum(now, keyed.iter().map(|(k, v)| (k.as_str(), *v)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::snapshot::RankGranularity;

    // A representative scrape from a dp_size=2 server, transcribed against
    // main@2026-09 metrics_collector.py label sets.
    const FIXTURE: &str = r#"# HELP sglang:num_running_reqs generated
# TYPE sglang:num_running_reqs gauge
sglang:num_running_reqs{model_name="qwen",engine_type="unified",tp_rank="0",pp_rank="0",moe_ep_rank="0",priority=""} 8
sglang:num_running_reqs{model_name="qwen",engine_type="unified",tp_rank="0",pp_rank="0",moe_ep_rank="0",dp_rank="0",priority=""} 3
sglang:num_running_reqs{model_name="qwen",engine_type="unified",tp_rank="0",pp_rank="0",moe_ep_rank="0",dp_rank="1",priority=""} 5
sglang:num_queue_reqs{model_name="qwen",engine_type="unified",tp_rank="0",pp_rank="0",moe_ep_rank="0",dp_rank="0",priority=""} 1
sglang:num_queue_reqs{model_name="qwen",engine_type="unified",tp_rank="0",pp_rank="0",moe_ep_rank="0",dp_rank="1",priority=""} 2
sglang:num_used_tokens{model_name="qwen",engine_type="unified",tp_rank="0",pp_rank="0",moe_ep_rank="0",dp_rank="0"} 1200
sglang:num_used_tokens{model_name="qwen",engine_type="unified",tp_rank="0",pp_rank="0",moe_ep_rank="0",dp_rank="1"} 800
sglang:max_total_num_tokens{model_name="qwen",engine_type="unified",tp_rank="0",pp_rank="0",moe_ep_rank="0",dp_rank="0"} 60000
sglang:max_total_num_tokens{model_name="qwen",engine_type="unified",tp_rank="0",pp_rank="0",moe_ep_rank="0",dp_rank="1"} 60000
sglang:token_usage{model_name="qwen",engine_type="unified",tp_rank="0",pp_rank="0",moe_ep_rank="0",dp_rank="0"} 0.02
sglang:token_usage{model_name="qwen",engine_type="unified",tp_rank="0",pp_rank="0",moe_ep_rank="0",dp_rank="1"} 0.05
sglang:gen_throughput{model_name="qwen",engine_type="unified",tp_rank="0",pp_rank="0",moe_ep_rank="0",dp_rank="0"} 210
sglang:gen_throughput{model_name="qwen",engine_type="unified",tp_rank="0",pp_rank="0",moe_ep_rank="0",dp_rank="1"} 190
sglang:cache_hit_rate{model_name="qwen",engine_type="unified",tp_rank="0",pp_rank="0",moe_ep_rank="0",dp_rank="0"} 0.73
sglang:utilization{model_name="qwen",engine_type="unified",tp_rank="0",pp_rank="0",moe_ep_rank="0"} 0.61
sglang:cuda_graph_passes_total{model_name="qwen",engine_type="unified",tp_rank="0",pp_rank="0",moe_ep_rank="0",mode="decode_cuda_graph"} 800
sglang:cuda_graph_passes_total{model_name="qwen",engine_type="unified",tp_rank="0",pp_rank="0",moe_ep_rank="0",mode="decode_none"} 200
sglang:cuda_graph_passes_total{model_name="qwen",engine_type="unified",tp_rank="0",pp_rank="0",moe_ep_rank="0",mode="prefill_none"} 50
sglang:new_token_ratio{model_name="qwen",engine_type="unified",tp_rank="0",pp_rank="0",moe_ep_rank="0"} 0.4
sglang:startup_time_seconds{model_name="qwen",engine_type="unified",phase="load_weight"} 193.1
sglang:startup_time_seconds{model_name="qwen",engine_type="unified",phase="scheduler_e2e"} 245.1
sglang:startup_cuda_graph_time_seconds{model_name="qwen",engine_type="unified",phase="target_verify"} 2.8
sglang:startup_available_gpu_memory_gb{model_name="qwen",engine_type="unified"} 7.5
sglang:fwd_occupancy{model_name="qwen",engine_type="unified",tp_rank="0",pp_rank="0",moe_ep_rank="0"} 92.5
sglang:generation_tokens_total{model_name="qwen",engine_type="unified",tp_rank="0",pp_rank="0",moe_ep_rank="0",dp_rank="0"} 500000
sglang:generation_tokens_total{model_name="qwen",engine_type="unified",tp_rank="0",pp_rank="0",moe_ep_rank="0",dp_rank="1"} 480000
sglang:prompt_tokens_total{model_name="qwen",engine_type="unified",tp_rank="0",pp_rank="0",moe_ep_rank="0",dp_rank="0"} 300000
sglang:num_requests_total{model_name="qwen",engine_type="unified",tp_rank="0",pp_rank="0",moe_ep_rank="0",dp_rank="0"} 1234
sglang:num_aborted_requests_total{model_name="qwen",engine_type="unified",tp_rank="0",pp_rank="0",moe_ep_rank="0",dp_rank="0"} 3
sglang:num_paused_reqs{model_name="qwen",engine_type="unified",tp_rank="0",pp_rank="0",moe_ep_rank="0",dp_rank="0"} 0
sglang:num_grammar_queue_reqs{model_name="qwen",engine_type="unified",tp_rank="0",pp_rank="0",moe_ep_rank="0",dp_rank="0"} 0
"#;

    fn two_builds(bump_gen: u64) -> Snapshot {
        let m1 = prom::parse(FIXTURE);
        let mut st = PollState::default();
        let mut s = Snapshot::default();
        let t0 = Instant::now();
        build(&m1, &mut st, t0, Duration::from_millis(5), &mut s);
        let bumped = FIXTURE.replace(
            &format!(
                "sglang:generation_tokens_total{{model_name=\"qwen\",engine_type=\"unified\",tp_rank=\"0\",pp_rank=\"0\",moe_ep_rank=\"0\",dp_rank=\"0\"}} {}",
                500000
            ),
            &format!(
                "sglang:generation_tokens_total{{model_name=\"qwen\",engine_type=\"unified\",tp_rank=\"0\",pp_rank=\"0\",moe_ep_rank=\"0\",dp_rank=\"0\"}} {}",
                500000 + bump_gen
            ),
        );
        let m2 = prom::parse(&bumped);
        build(
            &m2,
            &mut st,
            t0 + Duration::from_secs(2),
            Duration::from_millis(5),
            &mut s,
        );
        s
    }

    #[test]
    fn running_reqs_from_default_priority_series() {
        let s = two_builds(0);
        // The priority="" no-dp-rank series is the cluster total (8); per-rank
        // series (3,5) must not be double-added on top of it.
        assert_eq!(s.engine.running_reqs, 8);
    }

    #[test]
    fn token_usage_is_bottleneck_max_not_sum() {
        let s = two_builds(0);
        assert!(
            (s.kv.token_usage - 0.05).abs() < 1e-9,
            "{}",
            s.kv.token_usage
        );
    }

    #[test]
    fn used_and_total_tokens_sum_across_ranks() {
        let s = two_builds(0);
        assert_eq!(s.kv.used_tokens, 2000);
        assert_eq!(s.kv.total_tokens, 120000);
    }

    #[test]
    fn startup_timeline_sorts_bottleneck_first() {
        let s = two_builds(0);
        let p = &s.server.startup_phases;
        assert_eq!(p.len(), 2);
        assert_eq!(p[0].0, "scheduler_e2e", "longest phase first");
        assert!((p[0].1 - 245.1).abs() < 1e-9);
        assert!((s.server.startup_graph_secs.unwrap() - 2.8).abs() < 1e-9);
        assert!((s.server.startup_free_gpu_gb.unwrap() - 7.5).abs() < 1e-9);
    }

    #[test]
    fn cg_share_counts_only_decode_passes() {
        let s = two_builds(0);
        // decode: 800 cuda_graph + 200 none = 1000 → 0.8; prefill excluded.
        assert!(
            (s.engine.cg_decode_share.unwrap() - 0.8).abs() < 1e-9,
            "{:?}",
            s.engine.cg_decode_share
        );
        assert!(
            (s.engine.new_token_ratio.unwrap() - 0.4).abs() < 1e-9,
            "{:?}",
            s.engine.new_token_ratio
        );
    }

    #[test]
    fn first_scrape_has_no_rates_second_does() {
        let mut st = PollState::default();
        let mut s = Snapshot::default();
        let m = prom::parse(FIXTURE);
        let t0 = Instant::now();
        build(&m, &mut st, t0, Duration::ZERO, &mut s);
        assert!(s.engine.decode_tps.is_none(), "no baseline → N/A, not 0");

        let s2 = two_builds(400);
        // dp_rank=0 grew 400 over 2s → 200/s summed across ranks.
        let dtps = s2.engine.decode_tps.unwrap();
        assert!((dtps - 200.0).abs() < 1.0, "decode rate {dtps}");
    }

    #[test]
    fn history_accumulates_across_polls() {
        let s = two_builds(0);
        // Two builds = two samples per ring — rings live in the snapshot,
        // they must not reset when a new scrape replaces panel values.
        assert_eq!(s.engine.gen_series.len(), 2);
        assert_eq!(s.traffic.gen_series.len(), 2);
        assert_eq!(s.kv.cache_hit_series.len(), 2);
    }

    #[test]
    fn rank_rows_aggregate_plus_per_rank() {
        let s = two_builds(0);
        assert!(matches!(s.rank_granularity, RankGranularity::PerRank));
        assert_eq!(s.ranks.len(), 3); // agg + rank 0 + rank 1
        assert_eq!(s.ranks[0].running, 8); // aggregate
        assert_eq!(s.ranks[1].dp_rank, 0);
        assert_eq!(s.ranks[1].running, 3);
        assert_eq!(s.ranks[2].dp_rank, 1);
        assert_eq!(s.ranks[2].running, 5);
    }

    #[test]
    fn rank_rings_survive_rebuild() {
        let s = two_builds(0);
        assert_eq!(s.ranks[2].gen_series.len(), 2, "per-rank ring carried over");
    }

    #[test]
    fn nan_gauge_degrades_to_na_not_zero() {
        let m = prom::parse(
            "sglang:utilization{dp_rank=\"0\"} NaN\nsglang:gen_throughput{dp_rank=\"0\"} 50\n",
        );
        let mut st = PollState::default();
        let mut s = Snapshot::default();
        build(&m, &mut st, Instant::now(), Duration::ZERO, &mut s);
        assert!(s.engine.utilization.is_none(), "NaN utilization -> N/A");
        assert_eq!(s.engine.gen_throughput, 50.0);
    }

    #[test]
    fn missing_metrics_yield_defaults_and_live_conn() {
        let m = prom::parse("");
        let mut st = PollState::default();
        let mut s = Snapshot::default();
        build(
            &m,
            &mut st,
            Instant::now(),
            Duration::from_millis(3),
            &mut s,
        );
        assert_eq!(s.engine.running_reqs, 0);
        assert!(s.kv.available_tokens.is_none());
        assert_eq!(s.ranks.len(), 1, "aggregate row always exists");
        assert!(matches!(
            s.conn,
            Connection::Live {
                metrics_ok: true,
                ..
            }
        ));
    }

    #[test]
    fn hicache_appears_only_when_present() {
        let mut st = PollState::default();
        let mut s = Snapshot::default();
        build(
            &prom::parse(FIXTURE),
            &mut st,
            Instant::now(),
            Duration::ZERO,
            &mut s,
        );
        assert!(s.kv.hicache.is_none(), "no hicache metrics → None");

        let with_hc = format!(
            "{FIXTURE}\nsglang:hicache_host_used_tokens{{dp_rank=\"0\"}} 100\nsglang:hicache_host_total_tokens{{dp_rank=\"0\"}} 400\nsglang:hicache_backup_tokens_total{{pool=\"kv\"}} 900\nsglang:hicache_dropped_tokens_total{{pool=\"kv\",reason=\"host_pressure\"}} 5\nsglang:load_back_tokens_total{{pool=\"kv\"}} 120\nsglang:hicache_prefetch_aux_alloc_failed_tokens_total{{storage_backend=\"nixl\"}} 30\nsglang:backuped_tokens_total{{storage_backend=\"nixl\",dp_rank=\"0\"}} 800\nsglang:prefetched_tokens_total{{storage_backend=\"nixl\",dp_rank=\"0\"}} 10\nsglang:hicache_backup_duration_seconds_sum{{cache_type=\"UnifiedRadixCache\"}} 20\nsglang:hicache_backup_duration_seconds_count{{cache_type=\"UnifiedRadixCache\"}} 100\nsglang:load_back_duration_seconds_sum{{cache_type=\"UnifiedRadixCache\"}} 5\nsglang:load_back_duration_seconds_count{{cache_type=\"UnifiedRadixCache\"}} 10\n"
        );
        let mut s2 = Snapshot::default();
        build(
            &prom::parse(&with_hc),
            &mut st,
            Instant::now(),
            Duration::ZERO,
            &mut s2,
        );
        let hc = s2.kv.hicache.expect("hicache present");
        assert_eq!(hc.host_used, 100);
        assert_eq!(hc.backuped_total, Some(900));
        assert_eq!(hc.dropped_total, Some(5));
        assert_eq!(hc.load_back_total, Some(120));
        assert_eq!(hc.host_total, 400);
        // L3 round trip (distinct from the device→host hicache_backup family).
        assert_eq!(hc.storage_backuped_total, Some(800));
        assert_eq!(hc.storage_prefetched_total, Some(10));
        // Per-op copy latency from histogram sum/count (exact, no buckets).
        assert_eq!(hc.backup_mean_secs, Some(0.2));
        assert_eq!(hc.load_back_mean_secs, Some(0.5));

        // hicache present but the duration histograms absent (older build) →
        // None, so the UI drops the latency half instead of claiming 0s.
        let no_hist = with_hc
            .lines()
            .filter(|l| !l.contains("duration_seconds"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut s3 = Snapshot::default();
        build(
            &prom::parse(&no_hist),
            &mut st,
            Instant::now(),
            Duration::ZERO,
            &mut s3,
        );
        let hc3 = s3.kv.hicache.expect("hicache still present");
        assert_eq!(hc3.backup_mean_secs, None);
        assert_eq!(hc3.load_back_mean_secs, None);
    }

    #[test]
    fn clip_respects_char_boundary() {
        assert_eq!(clip_utf8("héllo", 2), "h");
        assert_eq!(clip_utf8("héllo", 100), "héllo");
    }

    #[test]
    fn build_records_truncation_from_the_scrape() {
        let mut st = PollState::default();
        // A body severed mid-line (no trailing newline on the last sample)
        // is what clip_utf8 produces at the byte cap.
        let cut = "sglang:num_running_reqs{model_name=\"qwen\"} 2\nsglang:token_us";
        let mut s = Snapshot::default();
        build(
            &prom::parse(cut),
            &mut st,
            Instant::now(),
            Duration::ZERO,
            &mut s,
        );
        assert!(s.metrics_truncated, "severed tail must flag");

        let mut s2 = Snapshot {
            metrics_truncated: true,
            ..Snapshot::default()
        };
        let clean = "sglang:num_running_reqs{model_name=\"qwen\"} 2\n";
        build(
            &prom::parse(clean),
            &mut st,
            Instant::now(),
            Duration::ZERO,
            &mut s2,
        );
        assert!(
            !s2.metrics_truncated,
            "a clean scrape must clear a stale flag"
        );
    }

    // A scrape matching the real server's label shape: realtime counters
    // carry mode=…, request-scoped counters carry is_streaming=… (no rank).
    const RT_FIXTURE: &str = r#"
sglang:realtime_tokens_total{engine_type="unified",mode="decode",model_name="qwen",tp_rank="0"} 1000
sglang:realtime_tokens_total{engine_type="unified",mode="prefill_compute",model_name="qwen",tp_rank="0"} 2000
sglang:generation_tokens_total{engine_type="unified",is_streaming="true",model_name="qwen"} 500
sglang:prompt_tokens_total{engine_type="unified",is_streaming="true",model_name="qwen"} 700
"#;

    #[test]
    fn realtime_mode_counters_prefer_over_request_scoped() {
        let mut st = PollState::default();
        let mut s = Snapshot::default();
        let t0 = Instant::now();
        build(
            &prom::parse(RT_FIXTURE),
            &mut st,
            t0,
            Duration::ZERO,
            &mut s,
        );
        let bumped = RT_FIXTURE
            .replace(
                "mode=\"decode\",model_name=\"qwen\",tp_rank=\"0\"} 1000",
                "mode=\"decode\",model_name=\"qwen\",tp_rank=\"0\"} 1400",
            )
            .replace(
                "mode=\"prefill_compute\",model_name=\"qwen\",tp_rank=\"0\"} 2000",
                "mode=\"prefill_compute\",model_name=\"qwen\",tp_rank=\"0\"} 2600",
            )
            // Request-scoped counters move too — decode/prefill must ignore
            // them when realtime exists (real server: realtime 174/s vs 47/s).
            .replace(
                "is_streaming=\"true\",model_name=\"qwen\"} 500",
                "is_streaming=\"true\",model_name=\"qwen\"} 900",
            );
        build(
            &prom::parse(&bumped),
            &mut st,
            t0 + Duration::from_secs(2),
            Duration::ZERO,
            &mut s,
        );
        // decode: realtime +400/2s = 200, NOT the generation_tokens_total +400
        // (both move identically here, so assert on prefill for the split).
        assert!(
            (s.engine.decode_tps.unwrap() - 200.0).abs() < 1.0,
            "decode {}/s should come from realtime decode",
            s.engine.decode_tps.unwrap()
        );
        // prefill: realtime prefill_compute +600/2s = 300; prompt_tokens_total
        // didn't move, so a 0 there would prove the fallback answered instead.
        assert!(
            (s.engine.prefill_tps.unwrap() - 300.0).abs() < 1.0,
            "prefill {}/s should come from realtime prefill_compute",
            s.engine.prefill_tps.unwrap()
        );
    }

    #[test]
    fn request_scoped_counters_are_the_fallback() {
        // Build path with NO realtime family at all → the fallback source
        // starts its own baseline on first use (a source never queried has
        // no baseline, so its first tick after a switch is N/A, not a
        // guessed rate).
        let without_rt = RT_FIXTURE
            .lines()
            .filter(|l| !l.contains("realtime_tokens_total"))
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        let mut st = PollState::default();
        let mut s = Snapshot::default();
        let t0 = Instant::now();
        build(
            &prom::parse(&without_rt),
            &mut st,
            t0,
            Duration::ZERO,
            &mut s,
        );
        assert!(s.engine.decode_tps.is_none(), "first scrape: no baseline");
        let bumped = without_rt.replace(
            "is_streaming=\"true\",model_name=\"qwen\"} 500",
            "is_streaming=\"true\",model_name=\"qwen\"} 1300",
        );
        build(
            &prom::parse(&bumped),
            &mut st,
            t0 + Duration::from_secs(2),
            Duration::ZERO,
            &mut s,
        );
        assert!(
            (s.engine.decode_tps.unwrap() - 400.0).abs() < 1.0,
            "fallback decode rate {}/s from generation_tokens_total",
            s.engine.decode_tps.unwrap()
        );
    }

    #[test]
    fn dp_attention_without_all_schedulers_collapses_ranks() {
        let mut st = PollState::default();
        let mut s = Snapshot::default();
        s.server.loaded = true;
        s.server.enable_dp_attention = true;
        s.server.enable_metrics_for_all_schedulers = false;
        build(
            &prom::parse(FIXTURE),
            &mut st,
            Instant::now(),
            Duration::ZERO,
            &mut s,
        );
        assert!(matches!(s.rank_granularity, RankGranularity::Aggregated(_)));
        assert_eq!(
            s.ranks.len(),
            1,
            "aggregate row only — no fake per-rank rows"
        );
    }

    /// Ground truth: a real dump captured from the live server. Expected
    /// values are the ones the proven Python implementation asserts against
    /// the same file — the two parsers must agree on reality.
    #[test]
    fn real_dump_parses_to_known_values() {
        let text = include_str!("../../tests/fixtures/metrics_sample.txt");
        let m = prom::parse(text);
        let mut st = PollState::default();
        let mut s = Snapshot::default();
        let t0 = Instant::now();
        build(&m, &mut st, t0, Duration::ZERO, &mut s);
        build(
            &m,
            &mut st,
            t0 + Duration::from_secs(1),
            Duration::ZERO,
            &mut s,
        );

        assert_eq!(s.engine.running_reqs, 2);
        assert_eq!(s.engine.waiting_reqs, 3);
        assert_eq!(s.kv.total_tokens, 693824);
        assert!((s.engine.gen_throughput - 297.78808501358833).abs() < 1e-6);
        let row = &s.ranks[0];
        assert!((row.spec_accept_rate.unwrap() - 0.5541666666666667).abs() < 1e-9);
        assert!((s.kv.weight_gb.unwrap() - 81.490234375).abs() < 1e-9);
        assert!(s.engine.fwd_occupancy.is_none(), "NaN series → N/A, not 0");
        // The real server has no dp_rank labels on request metrics → a single
        // aggregate rank row, not per-rank rows.
        assert_eq!(s.ranks.len(), 1);
        // realtime_tokens_total is present on this build → rates come from it
        // (values identical across the two scrapes → 0/s, but present).
        assert!(s.engine.decode_tps.is_some() && s.engine.prefill_tps.is_some());
    }

    // The server build's real names: num_retracted_reqs (not the legacy
    // *_requests_total), plus the http_* / cached_tokens_total families the
    // TRAFFIC panel now surfaces.
    const HTTP_FIXTURE: &str = r#"
sglang:num_retracted_reqs{engine_type="unified",tp_rank="0"} 4.0
sglang:http_requests_total{endpoint="/generate",method="POST"} 100.0
sglang:http_requests_total{endpoint="/metrics",method="GET"} 9000.0
sglang:http_responses_total{endpoint="/generate",method="POST",status_code="200"} 95.0
sglang:http_responses_total{endpoint="/generate",method="POST",status_code="500"} 5.0
sglang:http_requests_active{endpoint="/generate",method="POST"} 2.0
sglang:http_requests_active{endpoint="/metrics",method="GET"} 1.0
sglang:cached_tokens_total{cache_source="device",engine_type="unified"} 1000.0
sglang:cached_tokens_total{cache_source="host",engine_type="unified"} 500.0
sglang:cached_tokens_total{cache_source="storage_HiCacheNixl",engine_type="unified"} 30.0
sglang:prompt_tokens_histogram_sum{engine_type="unified"} 2000.0
sglang:prompt_tokens_histogram_count{engine_type="unified"} 10.0
sglang:generation_tokens_histogram_sum{engine_type="unified"} 500.0
sglang:generation_tokens_histogram_count{engine_type="unified"} 10.0
sglang:uncached_prompt_tokens_histogram_sum{engine_type="unified"} 100.0
sglang:uncached_prompt_tokens_histogram_count{engine_type="unified"} 10.0
sglang:evicted_tokens_total{cache_type="UnifiedRadixCache"} 7000.0
sglang:process_cpu_seconds_total{component="tokenizer"} 100.0
sglang:process_cpu_seconds_total{component="detokenizer"} 10.0
"#;

    #[test]
    fn http_and_cache_counters_use_real_names_and_exclude_our_polling() {
        let m1 = prom::parse(HTTP_FIXTURE);
        let mut st = PollState::default();
        let mut s = Snapshot::default();
        let t0 = Instant::now();
        build(&m1, &mut st, t0, Duration::from_millis(5), &mut s);

        // Gauge-ish reads land on the first scrape already.
        assert_eq!(s.traffic.retracted, 4, "num_retracted_reqs (real name)");
        assert_eq!(
            s.traffic.http_active,
            Some(2),
            "/metrics excluded from active"
        );
        assert_eq!(s.traffic.cached_device_total, 1000, "device source only");
        assert_eq!(s.traffic.cached_host_total, 500);
        assert_eq!(
            s.traffic.cached_storage_total, 30,
            "storage_<backend> matched by prefix"
        );
        assert_eq!(s.traffic.avg_prompt_len, Some(200.0), "sum/count");
        assert_eq!(s.traffic.avg_gen_len, Some(50.0));
        assert_eq!(s.traffic.avg_uncached_len, Some(10.0));
        assert_eq!(s.kv.evicted_total, Some(7000));
        assert!(s.traffic.http_rps.is_none(), "no baseline yet → N/A, not 0");
        assert!(
            s.engine.tokenizer_cores.is_none(),
            "cpu cores need a baseline"
        );

        // Second scrape +2s: /generate grew 10 (rps 5), errors grew 2 (err/s 1),
        // device cache grew 100 (50/s). /metrics grew a lot and must not count.
        let m2 = prom::parse(
            &HTTP_FIXTURE
                .replace(
                    "/generate\",method=\"POST\"} 100.0",
                    "/generate\",method=\"POST\"} 110.0",
                )
                .replace(
                    "/metrics\",method=\"GET\"} 9000.0",
                    "/metrics\",method=\"GET\"} 9200.0",
                )
                .replace("status_code=\"500\"} 5.0", "status_code=\"500\"} 7.0")
                .replace(
                    "cache_source=\"device\",engine_type=\"unified\"} 1000.0",
                    "cache_source=\"device\",engine_type=\"unified\"} 1100.0",
                )
                .replace(
                    "component=\"tokenizer\"} 100.0",
                    "component=\"tokenizer\"} 106.0",
                )
                .replace(
                    "component=\"detokenizer\"} 10.0",
                    "component=\"detokenizer\"} 12.0",
                ),
        );
        build(
            &m2,
            &mut st,
            t0 + Duration::from_secs(2),
            Duration::from_millis(5),
            &mut s,
        );
        let rps = s.traffic.http_rps.unwrap();
        assert!(
            (rps - 5.0).abs() < 0.01,
            "/generate-only rate, our polling excluded: {rps}"
        );
        let err = s.traffic.http_err_rate.unwrap();
        assert!((err - 1.0).abs() < 0.01, "status>=400 rate: {err}");
        let cps = s.traffic.cached_device_tps.unwrap();
        assert!((cps - 50.0).abs() < 0.1, "device cache inflow: {cps}");
        let tk = s.engine.tokenizer_cores.unwrap();
        assert!((tk - 3.0).abs() < 0.05, "6 cpu-sec / 2s = 3 cores: {tk}");
        let dk = s.engine.detokenizer_cores.unwrap();
        assert!((dk - 1.0).abs() < 0.05, "detok cores: {dk}");
    }

    // The windowed cache share is a *ratio*, not another counter: it needs
    // every mode's rate before the divide, and the ratio itself must be
    // hit/all — not the all-time totals the tier row already shows.
    const EFFECTIVE_FIXTURE: &str = r#"
sglang:prefill_effective_tokens_total{engine_type="unified",mode="input"} 0.0
sglang:prefill_effective_tokens_total{engine_type="unified",mode="device_hit"} 0.0
sglang:prefill_effective_tokens_total{engine_type="unified",mode="host_hit"} 0.0
sglang:prefill_effective_tokens_total{engine_type="unified",mode="storage_hit"} 0.0
"#;

    #[test]
    fn cache_window_share_is_a_ratio_and_needs_a_full_baseline() {
        let m1 = prom::parse(EFFECTIVE_FIXTURE);
        let mut st = PollState::default();
        let mut s = Snapshot::default();
        let t0 = Instant::now();
        build(&m1, &mut st, t0, Duration::from_millis(5), &mut s);

        // Family present, but no second sample → we know the server speaks it,
        // yet cannot answer the question. `known` fixes the UI's spelling.
        assert!(s.traffic.cache_window_known);
        assert!(
            s.traffic.cache_window_share.is_none(),
            "first scrape has no rate"
        );

        let m2 = prom::parse(
            &EFFECTIVE_FIXTURE
                .replace("mode=\"input\"} 0.0", "mode=\"input\"} 100.0")
                .replace("mode=\"device_hit\"} 0.0", "mode=\"device_hit\"} 300.0")
                .replace("mode=\"host_hit\"} 0.0", "mode=\"host_hit\"} 60.0")
                .replace("mode=\"storage_hit\"} 0.0", "mode=\"storage_hit\"} 40.0"),
        );
        build(
            &m2,
            &mut st,
            t0 + Duration::from_secs(2),
            Duration::from_millis(5),
            &mut s,
        );
        // (300+60+40) / (100+300+60+40) = 0.8 — the `input` mode is the
        // uncached remainder and belongs in the denominator only.
        let win = s.traffic.cache_window_share.unwrap();
        assert!((win - 0.8).abs() < 1e-6, "windowed hit share: {win}");
    }

    // A server that never exposes the family says nothing at all: not N/A, not
    // 0% — the UI's `now` column simply doesn't exist there.
    #[test]
    fn cache_window_absent_family_stays_unknown() {
        let m = prom::parse(HTTP_FIXTURE);
        let mut st = PollState::default();
        let mut s = Snapshot::default();
        build(
            &m,
            &mut st,
            Instant::now(),
            Duration::from_millis(5),
            &mut s,
        );
        assert!(!s.traffic.cache_window_known);
        assert!(s.traffic.cache_window_share.is_none());
    }
}
