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
    pub max_total_num_tokens: Option<u64>,
    /// True once at least one info fetch has populated the above.
    pub loaded: bool,
}

/// Top strip — btop's CPU-box analog (headline time-series + total meter +
/// per-unit rows). No GPU hardware readings here: `utilization` is a *software*
/// forward-pass wall-time ratio, so the panel is titled "engine", not "GPU".
#[derive(Debug)]
pub struct EnginePanel {
    pub gen_throughput: f64,
    pub gen_series: Ring,
    pub prefill_tps: Option<f64>,
    pub decode_tps: Option<f64>,
    pub token_rate_series: Ring,
    /// Engine-software busy ratio (labelled "engine busy").
    pub utilization: Option<f64>,
    /// Forward-pass occupancy %; may be NaN → N/A.
    pub fwd_occupancy: Option<f64>,
    pub running_reqs: u64,
    pub waiting_reqs: u64,
    pub uptime: Option<Duration>,
}

impl Default for EnginePanel {
    fn default() -> Self {
        EnginePanel {
            gen_throughput: 0.0,
            gen_series: Ring::default(),
            prefill_tps: None,
            decode_tps: None,
            token_rate_series: Ring::default(),
            utilization: None,
            fwd_occupancy: None,
            running_reqs: 0,
            waiting_reqs: 0,
            uptime: None,
        }
    }
}

/// Left top — btop's MEM-box analog: a bounded pool with used/available/evict.
#[derive(Debug)]
pub struct KvPanel {
    pub used_tokens: u64,
    pub total_tokens: u64,
    pub active_tokens: u64,
    pub max_running_requests: u64,
    /// The bottleneck ratio (0..1). "misleadingly named" at source — surface
    /// the sub-pool ratios alongside and title the bar "token pool".
    pub token_usage: f64,
    pub full_token_usage: Option<f64>,
    pub swa_token_usage: Option<f64>,
    pub mamba_usage: Option<f64>,
    pub available_tokens: Option<u64>,
    pub evictable_tokens: Option<u64>,
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
    pub hit_rate: Option<f64>,
}

impl Default for KvPanel {
    fn default() -> Self {
        KvPanel {
            used_tokens: 0,
            total_tokens: 0,
            active_tokens: 0,
            max_running_requests: 0,
            token_usage: 0.0,
            full_token_usage: None,
            swa_token_usage: None,
            mamba_usage: None,
            available_tokens: None,
            evictable_tokens: None,
            cache_hit_rate: 0.0,
            cache_hit_series: Ring::default(),
            weight_gb: None,
            kv_cache_gb: None,
            graph_gb: None,
            hicache: None,
        }
    }
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
    // HTTP concurrency / rate
    pub http_rps: Option<f64>,
    pub http_err_rate: Option<f64>,
    pub http_active: Option<u64>,
    // queue breakdown — btop's per-direction rows
    pub req_rate_in: f64,
    pub req_rate_out: f64,
    pub total_requests: u64,
    pub aborted: u64,
    pub retracted: u64,
    pub paused: u64,
    pub grammar_queue: u64,
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
            req_rate_in: 0.0,
            req_rate_out: 0.0,
            total_requests: 0,
            aborted: 0,
            retracted: 0,
            paused: 0,
            grammar_queue: 0,
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
    pub waiting_uncached_tokens: u64,
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
            waiting_uncached_tokens: 0,
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
