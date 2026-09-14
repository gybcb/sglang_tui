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
    // No wall-clock "uptime seconds" gauge exists in sglang's metrics;
    // `startup_time_seconds` is a per-phase startup *duration*, not an epoch.
    // Faking uptime from it would be a false claim about the server → N/A.
    s.engine.uptime = None;

    // --- KV ---
    s.kv.used_tokens = sum_across_ranks(m, "sglang:num_used_tokens", true) as u64;
    s.kv.total_tokens = sum_across_ranks(m, "sglang:max_total_num_tokens", true) as u64;
    // token_usage is documented (sg_metrics.py:87) as max(full, swa, mamba)
    // per rank — the bottleneck. Cross-rank aggregate: max.
    s.kv.token_usage = max_val(m, "sglang:token_usage");
    s.kv.full_token_usage = first_val("sglang:full_token_usage");
    s.kv.swa_token_usage = first_val("sglang:swa_token_usage");
    s.kv.mamba_usage = first_val("sglang:mamba_usage");
    s.kv.available_tokens = sum_if_present(m, "sglang:kv_available_tokens").map(|v| v as u64);
    s.kv.evictable_tokens = sum_if_present(m, "sglang:kv_evictable_tokens").map(|v| v as u64);
    s.kv.cache_hit_rate = first_val("sglang:cache_hit_rate").unwrap_or(0.0);
    s.kv.cache_hit_series.push(s.kv.cache_hit_rate);
    s.kv.weight_gb = sum_if_present(m, "sglang:weight_memory_usage_gb");
    s.kv.kv_cache_gb = sum_if_present(m, "sglang:kv_cache_memory_usage_gb");
    s.kv.graph_gb = sum_if_present(m, "sglang:graph_memory_usage_gb");
    // HiCache host pool appears only with --enable-hierarchical-cache.
    if m.get("sglang:hicache_host_total_tokens").next().is_some() {
        let hicache = s.kv.hicache.get_or_insert(Default::default());
        hicache.host_used = sum_across_ranks(m, "sglang:hicache_host_used_tokens", true) as u64;
        hicache.host_total = sum_across_ranks(m, "sglang:hicache_host_total_tokens", true) as u64;
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
    s.traffic.retracted = sum_across_ranks(m, "sglang:num_retracted_requests_total", true) as u64;
    s.traffic.paused = first_val("sglang:num_paused_reqs").unwrap_or(0.0).max(0.0) as u64;
    s.traffic.grammar_queue = first_val("sglang:num_grammar_queue_reqs")
        .unwrap_or(0.0)
        .max(0.0) as u64;
    s.traffic.latency = latency;

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
            ..Default::default()
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
            "{FIXTURE}\nsglang:hicache_host_used_tokens{{dp_rank=\"0\"}} 100\nsglang:hicache_host_total_tokens{{dp_rank=\"0\"}} 400\n"
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
        assert_eq!(hc.host_total, 400);
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
}
