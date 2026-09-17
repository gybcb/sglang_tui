use std::time::{Duration, Instant};

use crate::model::hist::Ring;

/// Everything the UI reads. Single source of truth: `ui/` never touches
/// `poll/`, and `poll/`'s `ingest` is the only writer.
///
/// Every `Option<T>` here renders as btop's dimmed `N/A` — a claim about our
/// knowledge — and never as `0`, which would be a false claim about the server.
#[derive(Debug)]
pub struct Snapshot {
    pub conn: Connection,
    pub server: ServerMeta,
    pub engine: EnginePanel,
    pub kv: KvPanel,
    pub traffic: TrafficPanel,
    pub ranks: Vec<RankRow>,
    /// Whether per-rank rows are trustworthy, or collapsed to one aggregate row
    /// because the server can't attribute them (§8-R4).
    pub rank_granularity: RankGranularity,
    /// Server-level pause from `/metrics` pause labels; reason from
    /// `sglang:paused_reason`.
    pub paused: Option<String>,
    /// True when the last scrape was cut at `--max-metrics-bytes`: absent
    /// fields may be missing because of the cut, not because the server
    /// lacks them — the UI says so instead of silently showing N/A.
    pub metrics_truncated: bool,
    pub now: Instant,
}

impl Default for Snapshot {
    fn default() -> Self {
        Snapshot {
            conn: Connection::Pending,
            server: ServerMeta::default(),
            engine: EnginePanel::default(),
            kv: KvPanel::default(),
            traffic: TrafficPanel::default(),
            ranks: Vec::new(),
            rank_granularity: RankGranularity::PerRank,
            paused: None,
            metrics_truncated: false,
            now: Instant::now(),
        }
    }
}

/// Liveness of the polled server. Drives banner + staleness, never zero-fill.
#[derive(Debug, Clone)]
pub enum Connection {
    /// First successful poll not yet completed since launch.
    Pending,
    Live {
        rtt: Duration,
        /// Metrics scrape succeeded; latency histograms / counters are available.
        metrics_ok: bool,
    },
    /// 1..N consecutive poll failures — keep last-known values, grey out
    /// sample-derived colour, but do not hide the shape of what was happening.
    Degraded {
        since: Instant,
        consecutive: u32,
        last_err: String,
    },
    /// Poll has failed past the staleness horizon: freeze graphs at last sample
    /// with a `╎` divider, banner across each affected box's bottom border.
    Lost { since: Instant, last_err: String },
    /// Server reachable but `/metrics` 404 — started without `--enable-metrics`.
    /// Fatal per product decision; UI shows the message then the app exits.
    MetricsDisabled,
}

impl Connection {
    /// True when values should be greyed to `inactive_fg` (sample colour off).
    pub fn dimmed(&self) -> bool {
        matches!(self, Connection::Degraded { .. } | Connection::Lost { .. })
    }
    /// True when we currently have no usable current sample (graphs freeze).
    pub fn frozen(&self) -> bool {
        matches!(self, Connection::Lost { .. } | Connection::Pending)
    }
    /// The moment liveness first slipped, if we're already Degraded/Lost — so
    /// escalating Degraded→Lost preserves the original failure time instead of
    /// restarting the staleness clock on every retried error.
    pub fn lost_since(&self) -> Option<Instant> {
        match self {
            Connection::Degraded { since, .. } | Connection::Lost { since, .. } => Some(*since),
            _ => None,
        }
    }
}

/// Static metadata from `/server_info` + `/model_info` (auth-gated, no poll,
/// no degradation role). Empty until the first successful fetch.
#[derive(Debug, Default)]
pub struct ServerMeta {
    pub model_path: String,
    pub served_model_name: String,
    pub version: String,
    pub tp_size: u32,
    pub pp_size: u32,
    pub dp_size: u32,
    pub enable_dp_attention: bool,
    pub enable_metrics_for_all_schedulers: bool,
    pub enable_hierarchical_cache: bool,
    /// PD-disaggregation role (`disaggregation_mode` from /server_info):
    /// "null" when unified. Gates the PD queue row — the gauges exist on every
    /// server but are meaningless zeros unless disaggregation is on.
    pub disaggregation_mode: String,
    pub max_total_num_tokens: Option<u64>,
    /// Context window in tokens (`context_length` from /server_info, or the
    /// `context_len` gauge) — with max_total_tokens it says how many
    /// max-length sequences the pool can actually host.
    pub context_len: Option<u64>,
    /// True once at least one info fetch has populated the above.
    pub loaded: bool,
    /// Engine startup duration by phase (`startup_time_seconds{phase=…}`),
    /// longest phase first, plus graph-capture seconds and the GPU memory
    /// that was free when startup began. Empty on builds without the family.
    /// Answers "why did this server take 4 minutes to come up" without
    /// digging through logs.
    pub startup_phases: Vec<(String, f64)>,
    pub startup_graph_secs: Option<f64>,
    pub startup_free_gpu_gb: Option<f64>,
}

/// Top strip — btop's CPU-box analog (headline time-series + total meter +
/// per-unit rows). No GPU hardware readings here: `utilization` is a *software*
/// forward-pass wall-time ratio, so the panel is titled "engine", not "GPU".
#[derive(Debug)]
pub struct EnginePanel {
    pub gen_throughput: f64,
    pub gen_series: Ring,
    pub prefill_tps: Option<f64>,
    /// Same-tick history of `prefill_tps` (N/A ticks push 0.0) — the
    /// current/max readout's max window.
    pub prefill_series: Ring,
    pub decode_tps: Option<f64>,
    /// Same-tick history of `decode_tps` (N/A ticks push 0.0).
    pub token_rate_series: Ring,
    /// Engine-software busy ratio (labelled "engine busy").
    pub utilization: Option<f64>,
    /// Forward-pass occupancy %; may be NaN → N/A.
    pub fwd_occupancy: Option<f64>,
    pub running_reqs: u64,
    pub waiting_reqs: u64,
    /// Live CPU cores (cpu-seconds per wall-second) of the tokenizer and
    /// detokenizer helper processes — the classic hidden SGLang bottleneck.
    /// None = `process_cpu_seconds_total` absent.
    pub tokenizer_cores: Option<f64>,
    pub detokenizer_cores: Option<f64>,
    /// Share of decode forward passes executed under CUDA graph
    /// (`cuda_graph_passes_total{mode="decode_*"}`). Silent degradation
    /// detector: graphs off (batch shape escaped capture, flag off) and
    /// decode throughput drops with no error anywhere. None = family absent.
    pub cg_decode_share: Option<f64>,
    /// Sum of all sequence lengths currently in decode
    /// (`decode_sum_seq_lens`). Against `running_reqs` it's the mean context
    /// each active sequence carries — the answer to "only 2 requests and the
    /// pool is 46% full". None = family absent (older server).
    pub decode_ctx_sum: Option<f64>,
    /// Scheduler admission-pressure dial (`new_token_ratio`): the estimated
    /// remaining-tokens fraction used to admit new requests. It decays as the
    /// pool tightens — low alongside high token-usage means the scheduler is
    /// throttling admission for memory. None = family absent.
    pub new_token_ratio: Option<f64>,
}

impl Default for EnginePanel {
    fn default() -> Self {
        EnginePanel {
            gen_throughput: 0.0,
            gen_series: Ring::default(),
            prefill_tps: None,
            prefill_series: Ring::default(),
            decode_tps: None,
            token_rate_series: Ring::default(),
            utilization: None,
            fwd_occupancy: None,
            running_reqs: 0,
            waiting_reqs: 0,
            tokenizer_cores: None,
            detokenizer_cores: None,
            cg_decode_share: None,
            decode_ctx_sum: None,
            new_token_ratio: None,
        }
    }
}

/// Left top — btop's MEM-box analog: a bounded pool with used/available/evict.
#[derive(Debug)]
pub struct KvPanel {
    pub used_tokens: u64,
    pub total_tokens: u64,
    /// The bottleneck ratio (0..1). "misleadingly named" at source — surface
    /// the sub-pool ratios alongside and title the bar "token pool".
    pub token_usage: f64,
    pub full_token_usage: Option<f64>,
    pub swa_token_usage: Option<f64>,
    pub mamba_usage: Option<f64>,
    /// Active slot counts for the hybrid sub-pools (mamba SSM / SWA window).
    /// Absolute alongside the ratio: a small pool at 100% and a huge pool at
    /// 100% read the same as a ratio but differ in urgency. None = family
    /// absent (non-hybrid model) → no absolute shown.
    pub swa_used: Option<u64>,
    pub mamba_used: Option<u64>,
    /// Truly-free slots per sub-pool — the ratio's missing half. A pool can
    /// read 20% used while nearly all its headroom is radix cache that must
    /// be evicted (recomputed) before reuse; `used`+ratio hides that, the
    /// free count says the pool is one allocation from churn. None = family
    /// absent → suffix hidden.
    pub swa_available: Option<u64>,
    pub mamba_available: Option<u64>,
    /// Radix-cached slots per sub-pool — the `free` count's counterpart, and
    /// the pair's punchline: `free 0  evict 30` says the visible headroom is
    /// all cache that must be recomputed before reuse. None = family absent.
    pub swa_evictable: Option<u64>,
    pub mamba_evictable: Option<u64>,
    pub available_tokens: Option<u64>,
    pub evictable_tokens: Option<u64>,
    /// Lifetime tokens evicted from the device pool (`evicted_tokens_total`);
    /// None = family absent → row hidden. Rate is over the rolling window.
    pub evicted_total: Option<u64>,
    pub evicted_tps: Option<f64>,
    /// Mean end-to-end duration of one device eviction pass
    /// (`eviction_duration_seconds` sum/count); under write_back it includes
    /// the blocking D->H copy. `gone 112M (333/s)` says how often eviction
    /// happens — the per-pass mean says what each one costs the serving path.
    /// None = family absent or no passes yet.
    pub evict_pass_mean_secs: Option<f64>,
    pub cache_hit_rate: f64,
    pub cache_hit_series: Ring,
    pub weight_gb: Option<f64>,
    pub kv_cache_gb: Option<f64>,
    pub graph_gb: Option<f64>,
    /// Hierarchical-cache host memory — present only with the flag on.
    pub hicache: Option<HiCache>,
}

#[derive(Debug, Default)]
pub struct HiCache {
    pub host_used: u64,
    pub host_total: u64,
    /// Where evicted device tokens went: backed up to host
    /// (`hicache_backup_tokens_total`) vs destroyed under host pressure
    /// (`hicache_dropped_tokens_total`). None = counters absent.
    /// backup+drop ≈ evicted — the split says whether HiCache keeps up.
    pub backuped_total: Option<u64>,
    pub dropped_total: Option<u64>,
    /// Host→GPU reloads (`load_back_tokens_total`): backed-up tokens that
    /// were actually reused. None = counter absent.
    pub load_back_total: Option<u64>,
    /// Mean per-op duration of the two L2 copies (histogram sum/count):
    /// device→host backup (`hicache_backup_duration_seconds`) and host→GPU
    /// reload (`load_back_duration_seconds`). Counts alone hide the silent
    /// degradation — PCIe/host-memory bandwidth saturation shows up here as
    /// creeping latency while every counter keeps ticking. None = family
    /// absent or no ops yet.
    pub backup_mean_secs: Option<f64>,
    pub load_back_mean_secs: Option<f64>,
    /// Achieved copy bandwidth (bytes/s) while transferring: sglang's own
    /// recipe — rate(`hicache_backup_bytes_total`) / rate of the duration
    /// histogram's sum. Strictly sharper than the per-op mean (which confuses
    /// big ops with a slow link). None = no transfer in the rate window (the
    /// UI then falls back to the per-op mean) or the family is absent.
    pub backup_gbps: Option<f64>,
    pub load_back_gbps: Option<f64>,
    /// Storage-tier prefetch tokens that failed aux-pool allocation on arrival
    /// (`hicache_prefetch_aux_alloc_failed_tokens_total`) — fetched data
    /// thrown away because mamba/aux pools were full. None = counter absent.
    pub prefetch_failed_total: Option<u64>,
    /// Storage-tier (L3) traffic pair. sglang's HELP text is unhelpful
    /// ("number of backuped tokens"); the label sets settle the direction —
    /// this family carries `storage_backend` (the device→host
    /// `hicache_backup_tokens_total` does not). A small prefetched/backuped
    /// share = the tier preserves everything and reuses little.
    /// None = family absent (no storage backend configured).
    pub storage_backuped_total: Option<u64>,
    pub storage_prefetched_total: Option<u64>,
}

impl Default for KvPanel {
    fn default() -> Self {
        KvPanel {
            used_tokens: 0,
            total_tokens: 0,
            token_usage: 0.0,
            full_token_usage: None,
            swa_token_usage: None,
            mamba_usage: None,
            swa_used: None,
            mamba_used: None,
            swa_available: None,
            mamba_available: None,
            swa_evictable: None,
            mamba_evictable: None,
            available_tokens: None,
            evictable_tokens: None,
            evicted_total: None,
            evicted_tps: None,
            evict_pass_mean_secs: None,
            cache_hit_rate: 0.0,
            cache_hit_series: Ring::default(),
            weight_gb: None,
            kv_cache_gb: None,
            graph_gb: None,
            hicache: None,
        }
    }
}

/// One HTTP route the server serves, from `http_requests_total{endpoint}`.
/// Window rate plus lifetime count — the count keeps quiet endpoints ordered
/// (and distinguishes "never served" from "not serving right now").
#[derive(Debug)]
pub struct HttpEndpoint {
    pub path: String,
    /// Window rate (req/s); None until this route has a rate baseline.
    pub rps: Option<f64>,
    /// Lifetime count of requests served on this route.
    pub total: u64,
    /// Lifetime count of 4xx/5xx responses on this route. Non-zero alongside a
    /// healthy rps is the signal: this route is failing (a client hammering a
    /// wrong-method endpoint looks identical to healthy traffic in the
    /// aggregate err_rate).
    pub err: u64,
}

/// Left bottom — btop's NET-box analog. The NET triple is current/peak/accum
/// per direction, mapped 1:1: current=rate, peak=max-of-series, accum=total.
#[derive(Debug)]
pub struct TrafficPanel {
    // decode-out direction
    pub gen_tps: f64,
    pub gen_peak_tps: f64,
    pub gen_total_tokens: u64,
    pub gen_series: Ring,
    // prefill-in direction
    pub prompt_tps: f64,
    pub prompt_peak_tps: f64,
    pub prompt_total_tokens: u64,
    pub prompt_series: Ring,
    // HTTP concurrency / rate — sglang's own server counters (`http_*`
    // families), with the `/metrics` endpoint excluded: sgtop's own polling
    // would otherwise dominate the rate.
    pub http_rps: Option<f64>,
    pub http_err_rate: Option<f64>,
    pub http_active: Option<u64>,
    /// Per-route rate breakdown behind `http_rps` — "what traffic is hitting
    /// this server right now". Sorted busiest-first. The panel has no spare
    /// row for it, so it lives in the `i` overlay (same division as request
    /// stages and the latency set). Empty when the family is absent.
    pub http_endpoints: Vec<HttpEndpoint>,
    /// Device prefix-cache inflow: rate (per second) and lifetime total of
    /// `cached_tokens_total{cache_source="device"}`. Total/prompt-total is the
    /// all-time cache share — the durable answer to "is caching working",
    /// unlike the instantaneous hit gauge.
    pub cached_device_tps: Option<f64>,
    pub cached_device_total: u64,
    /// All-time tier totals from the same `cached_tokens_total` family, whose
    /// `cache_source` label also carries `host` and `storage_*` (the storage
    /// spelling carries the backend: `storage_HiCacheNixl`). On a
    /// hierarchical-cache server the device-only share hides most of the wins
    /// — here host hits routinely dwarf device hits. 0 = tier absent (no
    /// such source on the server) → the tier isn't shown.
    pub cached_host_total: u64,
    pub cached_storage_total: u64,
    /// Window prefix-cache share: rate(hits) / rate(all modes) over the
    /// 30s rate window, per sglang's own formula on
    /// `prefill_effective_tokens_total`. The all-time totals above go stale —
    /// 15% at minute 30 looks identical to 15% at minute 100; this is what
    /// the cache is doing *now*. None while the window is too quiet to say.
    pub cache_window_share: Option<f64>,
    /// True when the last scrape carried the `prefill_effective_tokens_total`
    /// family — fixes the readout's spelling (`now …`) instead of letting it
    /// flicker between now/all-time as windows go quiet.
    pub cache_window_known: bool,
    /// Mean request length (tokens), from the request-length histograms'
    /// sum/count — cumulative means over all served requests. Histogram
    /// medians would need bucket arithmetic; the mean is exact and cheap.
    pub avg_prompt_len: Option<f64>,
    pub avg_gen_len: Option<f64>,
    /// Mean *computed* prompt length (`uncached_prompt_tokens_histogram`):
    /// what prefill actually processes after cache hits. Against avg_prompt_len
    /// it turns the cache ratio into per-request work saved.
    pub avg_uncached_len: Option<f64>,
    /// Same three means over the 30s histogram window, in the same order
    /// ([prompt, gen, uncached]) — what the traffic is *now*; the all-time
    /// means above go stale. None = quiet window → N/A on the panel row.
    pub len_window: [Option<f64>; 3],
    /// Request-lifecycle stage latencies (per_stage_req_latency_seconds):
    /// (stage, mean secs) sorted slowest-first. The percentile rows answer
    /// "how long end to end"; these answer "where did it go" (request
    /// handling vs prefill forward vs a chunk) — and the stage set is
    /// server-defined, so it is read, never hardcoded. Empty = family absent.
    pub stage_means: Vec<(String, f64)>,
    // queue breakdown — btop's per-direction rows
    pub req_rate_in: f64,
    pub req_rate_out: f64,
    pub total_requests: u64,
    pub aborted: u64,
    pub retracted: u64,
    pub paused: u64,
    pub grammar_queue: u64,
    /// PD-disaggregation queues: prefill-bootstrap waiting for KV, prefill
    /// in-flight, decode-prealloc waiting for slots, decode-transfer handing
    /// off KV. Only rendered when `disaggregation_mode` != null — the gauges
    /// exist on every server but are misleading zeros for a unified one.
    pub pd_prefill_bootstrap: Option<u64>,
    pub pd_prefill_inflight: Option<u64>,
    pub pd_decode_prealloc: Option<u64>,
    pub pd_decode_transfer: Option<u64>,
    pub latency: LatencySet,
}

/// Bucket-histogram-derived percentiles; None until `/metrics` provides the
/// families and enough samples to interpolate.
#[derive(Debug, Default, Clone)]
pub struct LatencySet {
    pub queue: Option<Percentile>,
    pub ttft: Option<Percentile>,
    pub itl: Option<Percentile>,
    pub e2e: Option<Percentile>,
    /// Which of the four families the latest scrape carried, in the same
    /// order. A row renders when its flag is set — N/A until the first
    /// delta — and stays hidden when the family is absent (renamed metrics
    /// lose their row, not the panel).
    pub known: [bool; 4],
}

#[derive(Debug, Clone, Copy)]
pub struct Percentile {
    pub p50: f64,
    pub p90: f64,
    pub p99: f64,
    pub mean: Option<f64>,
}

impl Default for TrafficPanel {
    fn default() -> Self {
        TrafficPanel {
            gen_tps: 0.0,
            gen_peak_tps: 0.0,
            gen_total_tokens: 0,
            gen_series: Ring::default(),
            prompt_tps: 0.0,
            prompt_peak_tps: 0.0,
            prompt_total_tokens: 0,
            prompt_series: Ring::default(),
            http_rps: None,
            http_err_rate: None,
            http_active: None,
            http_endpoints: Vec::new(),
            cached_device_tps: None,
            cached_device_total: 0,
            cached_host_total: 0,
            cached_storage_total: 0,
            cache_window_share: None,
            cache_window_known: false,
            avg_prompt_len: None,
            avg_gen_len: None,
            avg_uncached_len: None,
            len_window: [None; 3],
            stage_means: Vec::new(),
            req_rate_in: 0.0,
            req_rate_out: 0.0,
            total_requests: 0,
            aborted: 0,
            retracted: 0,
            paused: 0,
            grammar_queue: 0,
            pd_prefill_bootstrap: None,
            pd_prefill_inflight: None,
            pd_decode_prealloc: None,
            pd_decode_transfer: None,
            latency: LatencySet::default(),
        }
    }
}

/// One row in the RANKS box (btop's PROC analog). Row 0 is the aggregate; the
/// rest are per-DP-rank. SGLang's HTTP surface exposes only aggregate counts —
/// no per-request introspection route exists — so the PROC box's *subject*
/// shifts from processes to DP ranks while its structure holds 1:1.
#[derive(Debug)]
pub struct RankRow {
    pub dp_rank: u32,
    pub running: u64,
    pub waiting: u64,
    pub used_tokens: u64,
    pub total_tokens: u64,
    pub token_usage: f64,
    pub gen_throughput: f64,
    pub cache_hit_rate: f64,
    pub utilization: Option<f64>,
    pub spec_accept_length: Option<f64>,
    pub spec_accept_rate: Option<f64>,
    pub gen_series: Ring,
}

impl Default for RankRow {
    fn default() -> Self {
        RankRow {
            dp_rank: 0,
            running: 0,
            waiting: 0,
            used_tokens: 0,
            total_tokens: 0,
            token_usage: 0.0,
            gen_throughput: 0.0,
            cache_hit_rate: 0.0,
            utilization: None,
            spec_accept_length: None,
            spec_accept_rate: None,
            gen_series: Ring::default(),
        }
    }
}

#[derive(Debug, Clone)]
pub enum RankGranularity {
    PerRank,
    /// Every scheduler reported as `tp_rank=0` because the server runs
    /// `--enable-dp-attention` without `--enable-metrics-for-all-schedulers`;
    /// we show one aggregate row and say why in the footer.
    Aggregated(String),
}
