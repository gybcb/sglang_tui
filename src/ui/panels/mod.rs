pub mod human;

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::Style,
    text::{Line, Span},
};

use crate::model::snapshot::Snapshot;
use crate::ui::frame;
use crate::ui::panels::human::{human_count, human_duration, human_pct, human_rate, value_cell};
use crate::ui::theme::Theme;
use crate::ui::widgets::meter::meter_spans;

pub use crate::ui::widgets::Gui;

/// ENGINE — btop's CPU-box analog: headline rates, a busy meter, totals.
/// "engine busy" is the *software* forward-time ratio; deliberately not
/// labelled "GPU" (SGLang exposes no NVML readings).
pub fn engine(s: &Snapshot, th: &Theme, gui: Gui, rect: Rect, buf: &mut Buffer) {
    let dim = s.conn.dimmed();
    let mut lines: Vec<Line<'static>> = Vec::new();

    // `cur/peak tok/s` — peak is the max over the samples the graph below
    // actually shows (braille paints 2 samples per cell, block 1), so the
    // readout and the visible curve always agree.
    let vis = (rect.width.saturating_sub(2) as usize) * if gui.braille_graphs { 2 } else { 1 };
    let pair = |cur: Option<f64>, r: &crate::model::hist::Ring| -> Option<String> {
        cur.map(|c| {
            let mx = r.tail_owned(vis).into_iter().fold(0.0f64, f64::max);
            format!("{}/{} tok/s", human_rate(c), human_rate(mx))
        })
    };

    lines.push(Line::from(vec![
        Span::styled("gen ", Style::default().fg(th.c("title")).bold()),
        value_cell(
            pair(Some(s.engine.gen_throughput), &s.engine.gen_series),
            18,
            th,
            dim,
        ),
        Span::styled("  prefill ", Style::default().fg(th.c("title")).bold()),
        value_cell(
            pair(s.engine.prefill_tps, &s.engine.prefill_series),
            18,
            th,
            dim,
        ),
        Span::styled("  decode ", Style::default().fg(th.c("title")).bold()),
        value_cell(
            pair(s.engine.decode_tps, &s.engine.token_rate_series),
            18,
            th,
            dim,
        ),
    ]));

    let busy = s.engine.utilization.unwrap_or(0.0);
    let mut busy_line = vec![Span::styled(
        "engine busy ",
        Style::default().fg(th.c("title")).bold(),
    )];
    busy_line.extend(meter_spans(busy, 24, "cpu", th));
    busy_line.push(Span::styled(
        format!(" {:>4.0}%", busy * 100.0),
        Style::default().fg(th.c("main_fg")),
    ));
    busy_line.push(Span::styled(
        "  fwd ",
        Style::default().fg(th.c("title")).bold(),
    ));
    busy_line.push(value_cell(
        s.engine.fwd_occupancy.map(|v| format!("{v:.1}%")),
        8,
        th,
        dim,
    ));
    lines.push(Line::from(busy_line));

    let mut row3 = vec![
        Span::styled("running ", Style::default().fg(th.c("title")).bold()),
        value_cell(Some(human_count(s.engine.running_reqs)), 8, th, dim),
        Span::styled("  waiting ", Style::default().fg(th.c("title")).bold()),
        value_cell(Some(human_count(s.engine.waiting_reqs)), 8, th, dim),
    ];
    // Helper-process CPU (cores in use right now): the tokenizer is the
    // classic hidden bottleneck — when it saturates, GPU goes idle upstream.
    if s.engine.tokenizer_cores.is_some() || s.engine.detokenizer_cores.is_some() {
        row3.push(Span::styled(
            "  tok ",
            Style::default().fg(th.c("title")).bold(),
        ));
        row3.push(value_cell(
            s.engine.tokenizer_cores.map(|v| format!("{v:.1}c")),
            7,
            th,
            dim,
        ));
        row3.push(Span::styled(
            "  detok ",
            Style::default().fg(th.c("graph_text")),
        ));
        row3.push(value_cell(
            s.engine.detokenizer_cores.map(|v| format!("{v:.1}c")),
            7,
            th,
            dim,
        ));
    }
    lines.push(Line::from(row3));

    // Engine internals that fail silently: CUDA-graph coverage (decode falls
    // out of graphs without an error — throughput drops, logs stay clean)
    // and the admission-pressure dial (low ratio = scheduler throttling for
    // memory). Hidden until the families exist — both are build/version
    // gated, and an absent one must not squat the row.
    if s.engine.cg_decode_share.is_some() || s.engine.new_token_ratio.is_some() {
        let mut row4: Vec<Span> = Vec::new();
        if let Some(sh) = s.engine.cg_decode_share {
            row4.push(Span::styled(
                "cg ",
                Style::default().fg(th.c("title")).bold(),
            ));
            // A partial share is the whole point of the readout — colour it.
            let style = if dim {
                th.c("inactive_fg")
            } else if sh < 0.95 {
                th.c("hi_fg")
            } else {
                th.c("main_fg")
            };
            row4.push(Span::styled(
                format!("{:>4.0}%", sh * 100.0),
                Style::default().fg(style),
            ));
        }
        if let Some(ntr) = s.engine.new_token_ratio {
            if !row4.is_empty() {
                row4.push(Span::styled("  ", Style::default()));
            }
            row4.push(Span::styled(
                "admit ",
                Style::default().fg(th.c("title")).bold(),
            ));
            row4.push(Span::styled(
                format!("{:>4.0}%", ntr * 100.0),
                Style::default().fg(th.c("main_fg")),
            ));
        }
        lines.push(Line::from(row4));
    }

    if let Some(reason) = &s.paused {
        lines.push(Line::from(Span::styled(
            format!("PAUSED — {reason}"),
            Style::default().fg(th.c("hi_fg")).bold(),
        )));
    }

    frame::write_lines(buf, rect, &lines);

    // The rest of the interior is the gen-throughput area graph — btop's
    // CPU-box graph region under the readout lines. Block style adds a `↑ peak`
    // annotation; braille style keeps btop's dotted frame.
    let used = lines.len().min(rect.height.saturating_sub(2) as usize);
    let graph = Rect {
        x: rect.x + 1,
        y: rect.y + 1 + used as u16,
        width: rect.width.saturating_sub(2),
        height: rect.height.saturating_sub(2 + used as u16),
    };
    let series = s.engine.gen_series.tail_owned(usize::MAX);
    let peak = if gui.braille_graphs || series.is_empty() {
        None
    } else {
        let mx = series.iter().cloned().fold(0.0f64, f64::max);
        Some(format!("↑ {} tok/s", human_rate(mx)))
    };
    crate::ui::widgets::area_graph(buf, graph, &series, th, "cpu", gui.braille_graphs, peak);
}

/// KV — btop's MEM-box analog: a bounded pool, used/available/evictable, hit
/// rate, sub-pool ratios, optional HiCache. `token_usage` is the bottleneck
/// ratio — the source itself calls the name misleading, so sub-pools ride
/// alongside and the bar is titled "token pool" (on the border).
pub fn kv(s: &Snapshot, th: &Theme, gui: Gui, rect: Rect, buf: &mut Buffer) {
    let dim = s.conn.dimmed();
    let mut lines: Vec<Line<'static>> = Vec::new();

    let usage = s.kv.token_usage;

    // btop's MEM-box reading order: total, then the pool bar itself.
    lines.push(Line::from(vec![
        Span::styled("Total:", Style::default().fg(th.c("title")).bold()),
        value_cell(Some(human_count(s.kv.total_tokens)), 12, th, dim),
        Span::styled(
            format!("  used {}", human_count(s.kv.used_tokens)),
            Style::default().fg(th.c("main_fg")),
        ),
    ]));

    // Token-pool meter: btop's MEM bar analog — the bar *is* the headline,
    // coloured across the used fraction with the cpu heat gradient.
    {
        let bar_w = rect.width.saturating_sub(10) as usize;
        let mut line = meter_spans(usage, bar_w, "cpu", th);
        line.push(Span::styled(
            format!(" {}", human_pct(usage)),
            Style::default().fg(th.c("main_fg")),
        ));
        lines.push(Line::from(line));
    }

    lines.push(Line::from(vec![
        Span::styled("available ", Style::default().fg(th.c("title")).bold()),
        value_cell(s.kv.available_tokens.map(human_count), 10, th, dim),
        Span::styled("  evict ", Style::default().fg(th.c("title")).bold()),
        value_cell(s.kv.evictable_tokens.map(human_count), 10, th, dim),
    ]));

    // GPU memory ledger (btop's MEM sub-rows analog): weights vs the KV pool
    // vs captured graphs. There is no free-total here on purpose — these are
    // the only three ledgers sglang accounts for; their sum is NOT the card's
    // usage (activations, NCCL, fragmentation all live outside it).
    if s.kv.weight_gb.is_some() || s.kv.kv_cache_gb.is_some() || s.kv.graph_gb.is_some() {
        let gb = |v: Option<f64>| value_cell(v.map(|g| format!("{g:.1}G")), 7, th, dim);
        lines.push(Line::from(vec![
            Span::styled("mem ", Style::default().fg(th.c("title")).bold()),
            Span::styled("w ", Style::default().fg(th.c("graph_text"))),
            gb(s.kv.weight_gb),
            Span::styled(" kv ", Style::default().fg(th.c("graph_text"))),
            gb(s.kv.kv_cache_gb),
            Span::styled(" graph ", Style::default().fg(th.c("graph_text"))),
            gb(s.kv.graph_gb),
        ]));
    }

    // `hit cur/peak` over the visible window — the rate is an instantaneous
    // scheduler gauge that reads 0 while idle, so the window peak plus the
    // history graph below is what actually shows cache behaviour.
    {
        let vis = (rect.width.saturating_sub(2) as usize) * if gui.braille_graphs { 2 } else { 1 };
        let hit = s.kv.cache_hit_rate;
        let mx =
            s.kv.cache_hit_series
                .tail_owned(vis)
                .into_iter()
                .fold(0.0f64, f64::max);
        let mut row = vec![
            Span::styled("hit ", Style::default().fg(th.c("title")).bold()),
            value_cell(
                Some(format!("{}/{}", human_pct(hit), human_pct(mx))),
                12,
                th,
                dim,
            ),
        ];
        // Cumulative eviction counter rides along when the family exists —
        // "how many tokens were ever evicted (rate)" vs the instantaneous
        // free-list size on the row above.
        if let Some(total) = s.kv.evicted_total {
            row.push(Span::styled(
                "  gone ",
                Style::default().fg(th.c("graph_text")),
            ));
            row.push(value_cell(
                Some(match s.kv.evicted_tps {
                    Some(r) => format!("{} ({}/s)", human_count(total), human_rate(r)),
                    None => human_count(total),
                }),
                18,
                th,
                dim,
            ));
        }
        lines.push(Line::from(row));
    }

    // Sub-pool ratios appear only when this model has those pools.
    for (label, v, used) in [
        ("full", s.kv.full_token_usage, None),
        ("swa", s.kv.swa_token_usage, s.kv.swa_used),
        ("mamba", s.kv.mamba_usage, s.kv.mamba_used),
    ] {
        if let Some(v) = v {
            let mut row = vec![Span::styled(
                format!("{label} "),
                Style::default().fg(th.c("title")).bold(),
            )];
            row.extend(meter_spans(v, 16, "cpu", th));
            row.push(Span::styled(
                format!(" {}", human_pct(v)),
                Style::default().fg(th.c("main_fg")),
            ));
            // Absolute active slots ride alongside the ratio: 100% of a
            // 8-slot mamba pool and 100% of a huge pool read identically as a
            // ratio but differ completely in urgency.
            if let Some(u) = used {
                row.push(Span::styled(
                    format!("  {u}"),
                    Style::default().fg(th.c("graph_text")),
                ));
            }
            lines.push(Line::from(row));
        }
    }

    if let Some(hc) = &s.kv.hicache {
        let frac = if hc.host_total > 0 {
            hc.host_used as f64 / hc.host_total as f64
        } else {
            0.0
        };
        let mut row = vec![Span::styled(
            "hicache ",
            Style::default().fg(th.c("title")).bold(),
        )];
        row.extend(meter_spans(frac, 16, "cached", th));
        row.push(Span::styled(
            format!(
                " {}/{}",
                human_count(hc.host_used),
                human_count(hc.host_total)
            ),
            Style::default().fg(th.c("main_fg")),
        ));
        lines.push(Line::from(row));
        // Where the evicted tokens went. A rising `drop` under host pressure
        // means HiCache can no longer keep up — evictions become lost work.
        if hc.backuped_total.is_some() || hc.dropped_total.is_some() {
            let failed = hc.prefetch_failed_total.unwrap_or(0);
            // Columns by keep-priority (alarm first). write_lines right-clips
            // silently, so drop the least-important column until the row fits
            // the interior — `fail` must survive even in narrow layouts.
            let avail = rect.width.saturating_sub(2) as usize; // interior width
            let mut cols: Vec<(String, Style)> = vec![(
                format!("backup {}", human_count(hc.backuped_total.unwrap_or(0))),
                Style::default().fg(th.c("graph_text")),
            )];
            cols.push((
                format!("drop {}", human_count(hc.dropped_total.unwrap_or(0))),
                Style::default().fg(if hc.dropped_total.unwrap_or(0) > 0 {
                    th.c("hi_fg")
                } else {
                    th.c("graph_text")
                }),
            ));
            if let Some(rb) = hc.load_back_total {
                cols.push((
                    format!("back {rb}", rb = human_count(rb)),
                    Style::default().fg(th.c("graph_text")),
                ));
            }
            // Storage-prefetch bytes thrown away at arrival (aux pools full):
            // hidden while zero — an alarm, not a routine readout.
            if failed > 0 {
                cols.push((
                    format!("fail {}", human_count(failed)),
                    Style::default().fg(th.c("hi_fg")),
                ));
            }
            // "          " indent + two spaces between every column.
            let width = |cols: &[(String, Style)]| {
                10 + cols.iter().map(|c| c.0.len()).sum::<usize>()
                    + 2 * cols.len().saturating_sub(1)
            };
            // Priority order for dropping: `back` first, then `backup`,
            // never `drop`/`fail` (the two alarms/context for host pressure).
            let droppable = ["back ", "backup "];
            for drop_idx in 0..=droppable.len() {
                if width(&cols) <= avail || cols.len() <= 2 {
                    break;
                }
                // Drop the droppable column (in priority order) still present.
                if let Some(i) = droppable
                    .get(drop_idx)
                    .and_then(|p| cols.iter().position(|c| c.0.starts_with(p)))
                {
                    cols.remove(i);
                }
            }
            let mut row: Vec<Span> = vec![Span::styled(
                "          ",
                Style::default().fg(th.c("title")),
            )];
            for (i, (text, style)) in cols.into_iter().enumerate() {
                if i > 0 {
                    row.push(Span::styled("  ", Style::default().fg(th.c("graph_text"))));
                }
                row.push(Span::styled(text, style));
            }
            lines.push(Line::from(row));
        }
    }

    frame::write_lines(buf, rect, &lines);

    // The rest of the interior: the cache-hit history graph. The gauge reads
    // 0 between batches, so the *shape* over time — when caching actually
    // kicked in, and how high — is the useful reading, not the instant value.
    let used = lines.len().min(rect.height.saturating_sub(2) as usize);
    let graph = Rect {
        x: rect.x + 1,
        y: rect.y + 1 + used as u16,
        width: rect.width.saturating_sub(2),
        height: rect.height.saturating_sub(2 + used as u16),
    };
    if graph.height >= 1 {
        let series = s.kv.cache_hit_series.tail_owned(usize::MAX);
        crate::ui::widgets::area_graph(buf, graph, &series, th, "cached", gui.braille_graphs, None);
    }
}

/// TRAFFIC — btop's NET-box analog, structure borrowed 1:1: current / peak /
/// accumulated × two directions (out=generated tokens, in=prompt tokens),
/// then the request-rate block and queue decompositions.
pub fn traffic(s: &Snapshot, th: &Theme, gui: Gui, rect: Rect, buf: &mut Buffer) {
    let dim = s.conn.dimmed();
    let mut lines: Vec<Line<'static>> = Vec::new();

    let triple = |dir: &str, cur: f64, peak: f64, accum: u64| {
        Line::from(vec![
            Span::styled(format!("{dir} "), Style::default().fg(th.c("title")).bold()),
            value_cell(Some(human_rate(cur)), 8, th, dim),
            Span::styled("  peak ", Style::default().fg(th.c("graph_text"))),
            value_cell(Some(human_rate(peak)), 8, th, dim),
            Span::styled("  accum ", Style::default().fg(th.c("graph_text"))),
            value_cell(Some(human_count(accum)), 10, th, dim),
            Span::styled(" tok", Style::default().fg(th.c("graph_text"))),
        ])
    };

    lines.push(triple(
        "out",
        s.traffic.gen_tps,
        s.traffic.gen_peak_tps,
        s.traffic.gen_total_tokens,
    ));
    lines.push(triple(
        "in ",
        s.traffic.prompt_tps,
        s.traffic.prompt_peak_tps,
        s.traffic.prompt_total_tokens,
    ));

    lines.push(Line::from(vec![
        Span::styled("req ", Style::default().fg(th.c("title")).bold()),
        value_cell(Some(human_rate(s.traffic.req_rate_in)), 8, th, dim),
        Span::styled("  total ", Style::default().fg(th.c("graph_text"))),
        value_cell(Some(human_count(s.traffic.total_requests)), 10, th, dim),
        Span::styled("  aborted ", Style::default().fg(th.c("graph_text"))),
        value_cell(Some(human_count(s.traffic.aborted)), 8, th, dim),
    ]));

    // sglang's own HTTP counters (/metrics endpoint excluded — our polling
    // would dominate it). N/A rows stay visible: absence is information.
    {
        let mut row = vec![Span::styled(
            "http ",
            Style::default().fg(th.c("title")).bold(),
        )];
        row.push(Span::styled(
            "rps ",
            Style::default().fg(th.c("graph_text")),
        ));
        row.push(value_cell(s.traffic.http_rps.map(human_rate), 7, th, dim));
        row.push(Span::styled(
            " active ",
            Style::default().fg(th.c("graph_text")),
        ));
        row.push(value_cell(
            s.traffic.http_active.map(human_count),
            6,
            th,
            dim,
        ));
        row.push(Span::styled(
            " err/s ",
            Style::default().fg(th.c("graph_text")),
        ));
        row.push(value_cell(
            s.traffic.http_err_rate.map(human_rate),
            7,
            th,
            dim,
        ));
        lines.push(Line::from(row));
    }

    // All-time prefix-cache share: cached_tokens_total vs all prompt tokens.
    // Unlike the instantaneous `hit` gauge (0 between batches), this ratio
    // only accumulates — the durable answer about cache behaviour. The
    // numerator spans every tier (device+host+storage): a HiCache server
    // whose device share reads "cache idle" while host does 90% of the hits
    // would otherwise get blamed for a cache that is working. With tiers
    // present the row shows the breakdown instead of the device-only rate.
    {
        let cached_all = s.traffic.cached_device_total
            + s.traffic.cached_host_total
            + s.traffic.cached_storage_total;
        let share = if s.traffic.prompt_total_tokens > 0 {
            cached_all as f64 / s.traffic.prompt_total_tokens as f64
        } else {
            0.0
        };
        let tiers_on = s.traffic.cached_host_total + s.traffic.cached_storage_total > 0;
        let mut row = vec![Span::styled(
            "cache ",
            Style::default().fg(th.c("title")).bold(),
        )];
        if tiers_on {
            let val = |v: u64| {
                Span::styled(
                    human_count(v),
                    Style::default().fg(if dim {
                        th.c("inactive_fg")
                    } else {
                        th.c("main_fg")
                    }),
                )
            };
            let lab =
                |t: &str| Span::styled(t.to_string(), Style::default().fg(th.c("graph_text")));
            row.push(lab("all "));
            row.push(val(cached_all));
            row.push(lab("  d "));
            row.push(val(s.traffic.cached_device_total));
            row.push(lab("  h "));
            row.push(val(s.traffic.cached_host_total));
            row.push(lab("  s "));
            row.push(val(s.traffic.cached_storage_total));
            row.push(Span::styled(
                format!("  {}", human_pct(share)),
                Style::default().fg(th.c("graph_text")),
            ));
        } else {
            row.push(Span::styled(
                "rate ",
                Style::default().fg(th.c("graph_text")),
            ));
            row.push(value_cell(
                s.traffic
                    .cached_device_tps
                    .map(|v| format!("{} tok/s", human_rate(v))),
                12,
                th,
                dim,
            ));
            row.push(Span::styled(
                " all ",
                Style::default().fg(th.c("graph_text")),
            ));
            row.push(value_cell(
                Some(format!(
                    "{} ({})",
                    human_pct(share),
                    human_count(cached_all)
                )),
                14,
                th,
                dim,
            ));
        }
        lines.push(Line::from(row));
    }

    // Mean request lengths (histogram sum/count): "what kind of traffic is
    // this" at a glance — long-context vs chatty — invisible in rate rows.
    // `comp` = computed (uncached) prompt length: next to `in`, the gap is
    // the per-request compute the cache saved.
    if s.traffic.avg_prompt_len.is_some()
        || s.traffic.avg_gen_len.is_some()
        || s.traffic.avg_uncached_len.is_some()
    {
        let mut row = vec![Span::styled(
            "len ",
            Style::default().fg(th.c("title")).bold(),
        )];
        row.push(Span::styled("in ", Style::default().fg(th.c("graph_text"))));
        row.push(value_cell(
            s.traffic
                .avg_prompt_len
                .map(|v| format!("{} tok", human_count(v as u64))),
            10,
            th,
            dim,
        ));
        row.push(Span::styled(
            " out ",
            Style::default().fg(th.c("graph_text")),
        ));
        row.push(value_cell(
            s.traffic
                .avg_gen_len
                .map(|v| format!("{} tok", human_count(v as u64))),
            8,
            th,
            dim,
        ));
        if s.traffic.avg_uncached_len.is_some() {
            row.push(Span::styled(
                " comp ",
                Style::default().fg(th.c("graph_text")),
            ));
            row.push(value_cell(
                s.traffic
                    .avg_uncached_len
                    .map(|v| format!("{} tok", human_count(v as u64))),
                9,
                th,
                dim,
            ));
        }
        lines.push(Line::from(row));
    }

    lines.push(Line::from(vec![
        Span::styled("retracted ", Style::default().fg(th.c("title")).bold()),
        value_cell(Some(human_count(s.traffic.retracted)), 8, th, dim),
        Span::styled("  paused ", Style::default().fg(th.c("graph_text"))),
        value_cell(Some(human_count(s.traffic.paused)), 6, th, dim),
        Span::styled("  grammar ", Style::default().fg(th.c("graph_text"))),
        value_cell(Some(human_count(s.traffic.grammar_queue)), 6, th, dim),
    ]));

    // PD-disaggregation queues — rendered only when /server_info says the
    // server runs a disaggregation role. The four gauges exist on unified
    // servers too (permanently zero), and a zero that means "topology absent"
    // must never sit next to zeros that mean "queue empty".
    if matches!(
        s.server.disaggregation_mode.as_str(),
        "prefill" | "decode" | "both"
    ) {
        let pd = |label: &str, v: Option<u64>| {
            Span::styled(
                format!(
                    " {} ",
                    match v {
                        Some(n) => format!("{label}{}", human_count(n)),
                        None => format!("{label}N/A"),
                    }
                ),
                Style::default().fg(th.c("graph_text")),
            )
        };
        lines.push(Line::from(vec![
            Span::styled("pd ", Style::default().fg(th.c("title")).bold()),
            pd("boot ", s.traffic.pd_prefill_bootstrap),
            pd("pre ", s.traffic.pd_decode_prealloc),
            pd("inflight ", s.traffic.pd_prefill_inflight),
            pd("xfer ", s.traffic.pd_decode_transfer),
        ]));
    }

    // Latency percentiles — one row per family the server actually exposes
    // (`known`), so a renamed metric drops its row rather than the panel.
    // Values are N/A until the 30s window holds a first histogram delta.
    for (i, (label, p)) in [
        ("queue", s.traffic.latency.queue),
        ("ttft ", s.traffic.latency.ttft),
        ("itl  ", s.traffic.latency.itl),
        ("e2e  ", s.traffic.latency.e2e),
    ]
    .into_iter()
    .enumerate()
    {
        if !s.traffic.latency.known[i] {
            continue;
        }
        lines.push(Line::from(vec![
            Span::styled(
                format!("{label} "),
                Style::default().fg(th.c("title")).bold(),
            ),
            Span::styled("p50 ", Style::default().fg(th.c("graph_text"))),
            value_cell(p.map(|x| human_duration(x.p50)), 7, th, dim),
            Span::styled(" p90 ", Style::default().fg(th.c("graph_text"))),
            value_cell(p.map(|x| human_duration(x.p90)), 7, th, dim),
            Span::styled(" p99 ", Style::default().fg(th.c("graph_text"))),
            value_cell(p.map(|x| human_duration(x.p99)), 7, th, dim),
            Span::styled(" μ ", Style::default().fg(th.c("graph_text"))),
            value_cell(p.and_then(|x| x.mean).map(human_duration), 7, th, dim),
        ]));
    }

    frame::write_lines(buf, rect, &lines);

    // btop's NET box graph region: two stacked area graphs sharing the
    // interior under the readout rows — out (generated tokens) above, in
    // (prompt tokens) below, each on its own ramp (upload/download).
    let used = lines.len().min(rect.height.saturating_sub(2) as usize);
    let inner_h = rect.height.saturating_sub(2 + used as u16);
    if inner_h >= 2 {
        let top = inner_h / 2;
        let gx = rect.x + 1;
        let gw = rect.width.saturating_sub(2);
        let peak = |series: &[f64]| -> Option<String> {
            if gui.braille_graphs || series.is_empty() {
                None
            } else {
                let mx = series.iter().cloned().fold(0.0f64, f64::max);
                Some(format!("↑ {}", human_rate(mx)))
            }
        };
        let gen_s = s.traffic.gen_series.tail_owned(usize::MAX);
        crate::ui::widgets::area_graph(
            buf,
            Rect {
                x: gx,
                y: rect.y + 1 + used as u16,
                width: gw,
                height: top,
            },
            &gen_s,
            th,
            "upload",
            gui.braille_graphs,
            peak(&gen_s),
        );
        let prompt = s.traffic.prompt_series.tail_owned(usize::MAX);
        crate::ui::widgets::area_graph(
            buf,
            Rect {
                x: gx,
                y: rect.y + 1 + used as u16 + top,
                width: gw,
                height: inner_h - top,
            },
            &prompt,
            th,
            "download",
            gui.braille_graphs,
            peak(&prompt),
        );
    }
}

/// RANKS — delegates to `widgets::table` (btop's PROC-box analog with DP ranks
/// as the subject). Selection/sort state is owned by `App` and threaded here.
#[allow(clippy::too_many_arguments)] // render fns take their whole subject
pub fn ranks(
    s: &Snapshot,
    th: &Theme,
    gui: Gui,
    rect: Rect,
    buf: &mut Buffer,
    sel: usize,
    col: crate::ui::widgets::table::SortCol,
    reverse: bool,
) {
    crate::ui::widgets::table::ranks(s, th, gui, rect, buf, sel, col, reverse);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::snapshot::{LatencySet, Percentile};
    use crate::ui::theme::{ColorMode, Theme};

    fn render_traffic(s: &Snapshot) -> String {
        let th = Theme::builtin("Default", ColorMode::TrueColor);
        let mut buf = Buffer::empty(Rect::new(0, 0, 60, 20));
        traffic(s, &th, Gui::MODERN, Rect::new(0, 0, 60, 20), &mut buf);
        let b = &buf;
        (0..20)
            .flat_map(|y| (0..60).map(move |x| b[(x, y)].symbol().to_owned()))
            .collect()
    }

    fn render_kv(s: &Snapshot, width: u16) -> String {
        let th = Theme::builtin("Default", ColorMode::TrueColor);
        let mut buf = Buffer::empty(Rect::new(0, 0, width, 12));
        kv(s, &th, Gui::MODERN, Rect::new(0, 0, width, 12), &mut buf);
        let b = &buf;
        (0..12)
            .flat_map(|y| (0..width).map(move |x| b[(x, y)].symbol().to_owned()))
            .collect()
    }

    // The eviction-destination row drops columns to fit, but the `fail`/`drop`
    // alarms must survive where the routine `backup`/`back` readouts do not.
    fn snap_hicache() -> Snapshot {
        let hc = crate::model::snapshot::HiCache {
            host_used: 2_000_000,
            host_total: 2_100_000,
            backuped_total: Some(72_500_000),
            dropped_total: Some(0),
            load_back_total: Some(11_100_000),
            prefetch_failed_total: Some(48_800),
        };
        Snapshot {
            kv: crate::model::snapshot::KvPanel {
                hicache: Some(hc),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn hicache_row_keeps_alarm_columns_when_narrow() {
        let wide = render_kv(&snap_hicache(), 60);
        assert!(
            wide.contains("backup") && wide.contains("back"),
            "wide: {wide}"
        );

        // A width too small for all four columns: the least-important
        // (`backup`, then `back`) yield, but the `fail` alarm stays visible.
        let narrow = render_kv(&snap_hicache(), 34);
        assert!(
            narrow.contains("fail"),
            "fail alarm survives narrow: {narrow}"
        );
        assert!(
            !narrow.contains("backup"),
            "routine backup dropped to keep fail: {narrow}"
        );
    }

    fn snap_with_latency(set: LatencySet) -> Snapshot {
        Snapshot {
            traffic: crate::model::snapshot::TrafficPanel {
                latency: set,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn snap_pd(mode: &str) -> Snapshot {
        Snapshot {
            server: crate::model::snapshot::ServerMeta {
                disaggregation_mode: mode.to_string(),
                ..Default::default()
            },
            traffic: crate::model::snapshot::TrafficPanel {
                pd_prefill_bootstrap: Some(3),
                pd_decode_prealloc: Some(1),
                pd_prefill_inflight: Some(0),
                pd_decode_transfer: Some(0),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn pd_row_only_for_disaggregation_servers() {
        // Unified servers carry the four gauges as permanent zeros — showing
        // them would imply a PD topology that doesn't exist.
        let unified = render_traffic(&snap_pd("null"));
        assert!(
            !unified.contains("boot"),
            "unified must not render pd row: {unified}"
        );

        let pd = render_traffic(&snap_pd("prefill"));
        assert!(
            pd.contains("boot 3") && pd.contains("pre 1"),
            "prefill server shows queue counts: {pd}"
        );
    }

    fn render_engine(s: &Snapshot) -> String {
        let th = Theme::builtin("Default", ColorMode::TrueColor);
        let mut buf = Buffer::empty(Rect::new(0, 0, 80, 12));
        engine(s, &th, Gui::MODERN, Rect::new(0, 0, 80, 12), &mut buf);
        let b = &buf;
        (0..12)
            .flat_map(|y| (0..80).map(move |x| b[(x, y)].symbol().to_owned()))
            .collect()
    }

    #[test]
    fn rate_readouts_show_current_slash_window_peak() {
        use crate::model::hist::Ring;
        let mut ring = Ring::default();
        for v in [100.0, 1500.0, 300.0] {
            ring.push(v);
        }
        let s = Snapshot {
            engine: crate::model::snapshot::EnginePanel {
                gen_throughput: 300.0,
                gen_series: ring.clone(),
                prefill_tps: None,
                prefill_series: ring.clone(),
                decode_tps: Some(300.0),
                token_rate_series: ring,
                ..Default::default()
            },
            ..Default::default()
        };
        let text = render_engine(&s);
        // cur 300 over window peak 1500 → "300/1.50k tok/s", one unit at the
        // end only; a None rate stays an honest N/A, not "0/…".
        assert!(text.contains("300/1.50k"), "cur/max pair: {text}");
        let seg = &text[text.find("prefill").unwrap()..text.find("decode").unwrap()];
        assert!(seg.contains("N/A"), "None prefill → N/A: {seg}");
        assert!(
            !seg.contains("1.50k"),
            "None rate shows N/A, not a cur/peak pair: {seg}"
        );
    }

    #[test]
    fn latency_rows_hidden_when_family_unknown() {
        let text = render_traffic(&snap_with_latency(LatencySet::default()));
        assert!(
            !text.contains("p50"),
            "no known family → no percentile rows"
        );
    }

    #[test]
    fn known_family_with_no_percentile_yet_shows_na() {
        let set = LatencySet {
            known: [false, true, false, false], // ttft only
            ..Default::default()
        };
        let text = render_traffic(&snap_with_latency(set));
        assert!(text.contains("ttft"), "row present for the known family");
        assert!(text.contains("N/A"), "window not filled → honest N/A");
    }

    #[test]
    fn known_family_with_percentiles_renders_humanized() {
        let set = LatencySet {
            ttft: Some(Percentile {
                p50: 0.42,
                p90: 1.1,
                p99: 3.0,
                mean: Some(0.6),
            }),
            known: [false, true, false, false],
            ..Default::default()
        };
        let mut s = snap_with_latency(set);
        // Fill the http/cache rows too — this test checks "no N/A anywhere
        // once filled", and those rows are legitimately N/A on an empty snap.
        s.traffic.http_rps = Some(2.0);
        s.traffic.http_err_rate = Some(0.0);
        s.traffic.http_active = Some(1);
        s.traffic.cached_device_tps = Some(10.0);
        s.traffic.cached_device_total = 100;
        s.traffic.prompt_total_tokens = 200;
        let text = render_traffic(&s);
        assert!(text.contains("420ms"), "p50 humanized: {text}");
        assert!(text.contains("50%"), "all-time cache share: {text}");
        assert!(!text.contains("N/A"), "filled → no N/A in the ttft row");
    }

    // A HiCache server's device-only share hides the tier doing the work —
    // with host hits present the row must break the tiers out.
    #[test]
    fn cache_row_breaks_out_hicache_tiers() {
        let mut s = Snapshot::default();
        s.traffic.prompt_total_tokens = 2_000;
        s.traffic.cached_device_tps = Some(10.0);
        s.traffic.cached_device_total = 1_000;
        s.traffic.cached_host_total = 500;
        s.traffic.cached_storage_total = 0;
        let text = render_traffic(&s);
        assert!(text.contains("d 1.00k") && text.contains("h 500"), "{text}");
        // Share spans every tier: (1000+500)/2000 = 75%, not 50%.
        assert!(text.contains("75%"), "{text}");
    }

    // The GPU memory ledger appears only once sglang reports the ledgers, and
    // renders each independently (a server without graph capture shows w/kv).
    #[test]
    fn mem_ledger_row_renders_present_ledgers() {
        let mut s = Snapshot::default();
        s.kv.weight_gb = Some(81.45);
        s.kv.kv_cache_gb = Some(5.93);
        s.kv.graph_gb = None; // no graph memory reported
        let text = render_kv(&s, 40);
        assert!(text.contains("mem "), "{text}");
        assert!(text.contains("81.5G") && text.contains("5.9G"), "{text}");
        // An absent ledger's slot renders N/A, not a false 0. (Check the
        // window right after the graph label — N/A also rides the available
        // row on an empty snapshot, so a bare contains would pass anyway.)
        let g = text.find("graph").expect("graph label present");
        assert!(
            text[g..g + 14].contains("N/A"),
            "graph ledger absent → N/A: {text}"
        );

        // No ledgers at all → the whole row is hidden, not three N/A cells.
        let bare = render_kv(&Snapshot::default(), 40);
        assert!(
            !bare.contains("mem "),
            "row hidden when families absent: {bare}"
        );
    }
}
