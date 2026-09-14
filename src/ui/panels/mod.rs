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
use crate::ui::widgets::meter::{meter_bar, meter_spans};

pub use crate::ui::widgets::Gui;

/// ENGINE — btop's CPU-box analog: headline rates, a busy meter, totals.
/// "engine busy" is the *software* forward-time ratio; deliberately not
/// labelled "GPU" (SGLang exposes no NVML readings).
pub fn engine(s: &Snapshot, th: &Theme, gui: Gui, rect: Rect, buf: &mut Buffer) {
    let dim = s.conn.dimmed();
    let mut lines: Vec<Line<'static>> = Vec::new();

    lines.push(Line::from(vec![
        Span::styled("gen ", Style::default().fg(th.c("title")).bold()),
        value_cell(
            Some(format!("{} tok/s", human_rate(s.engine.gen_throughput))),
            14,
            th,
            dim,
        ),
        Span::styled("  prefill ", Style::default().fg(th.c("title")).bold()),
        value_cell(
            s.engine
                .prefill_tps
                .map(|v| format!("{} tok/s", human_rate(v))),
            14,
            th,
            dim,
        ),
        Span::styled("  decode ", Style::default().fg(th.c("title")).bold()),
        value_cell(
            s.engine
                .decode_tps
                .map(|v| format!("{} tok/s", human_rate(v))),
            14,
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

    lines.push(Line::from(vec![
        Span::styled("running ", Style::default().fg(th.c("title")).bold()),
        value_cell(Some(human_count(s.engine.running_reqs)), 8, th, dim),
        Span::styled("  waiting ", Style::default().fg(th.c("title")).bold()),
        value_cell(Some(human_count(s.engine.waiting_reqs)), 8, th, dim),
        Span::styled("  uptime ", Style::default().fg(th.c("title")).bold()),
        value_cell(
            s.engine.uptime.map(|u| human_duration(u.as_secs_f64())),
            10,
            th,
            dim,
        ),
    ]));

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
pub fn kv(s: &Snapshot, th: &Theme, rect: Rect, buf: &mut Buffer) {
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
    // coloured across the used fraction with the cpu heat gradient, closed at
    // both ends by thin caps (`meter_bar`) instead of btop's bare run.
    {
        let bar_w = rect.width.saturating_sub(12) as usize;
        let mut line = meter_bar(usage, bar_w, "cpu", th);
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

    lines.push(Line::from(vec![
        Span::styled("hit ", Style::default().fg(th.c("title")).bold()),
        value_cell(Some(human_pct(s.kv.cache_hit_rate)), 6, th, dim),
    ]));

    // Sub-pool ratios appear only when this model has those pools.
    for (label, v) in [
        ("full", s.kv.full_token_usage),
        ("swa", s.kv.swa_token_usage),
        ("mamba", s.kv.mamba_usage),
    ] {
        if let Some(v) = v {
            let mut row = vec![Span::styled(
                format!("{label} "),
                Style::default().fg(th.c("title")).bold(),
            )];
            row.extend(meter_bar(v, 14, "cpu", th));
            row.push(Span::styled(
                format!(" {}", human_pct(v)),
                Style::default().fg(th.c("main_fg")),
            ));
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
        row.extend(meter_bar(frac, 14, "cached", th));
        row.push(Span::styled(
            format!(
                " {}/{}",
                human_count(hc.host_used),
                human_count(hc.host_total)
            ),
            Style::default().fg(th.c("main_fg")),
        ));
        lines.push(Line::from(row));
    }

    frame::write_lines(buf, rect, &lines);
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

    lines.push(Line::from(vec![
        Span::styled("retracted ", Style::default().fg(th.c("title")).bold()),
        value_cell(Some(human_count(s.traffic.retracted)), 8, th, dim),
        Span::styled("  paused ", Style::default().fg(th.c("graph_text"))),
        value_cell(Some(human_count(s.traffic.paused)), 6, th, dim),
        Span::styled("  grammar ", Style::default().fg(th.c("graph_text"))),
        value_cell(Some(human_count(s.traffic.grammar_queue)), 6, th, dim),
    ]));

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
        // A p50·p90·p99 mini-sparkline rides at the row's tail: three eighth-
        // blocks whose heights are each percentile as a fraction of p99 (the
        // row's own max), so the tail is a scale-free shape of the tail
        // without needing a global time axis.
        let tail = match p {
            Some(x) if x.p99 > 0.0 => {
                const H: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
                let h = |v: f64| -> char {
                    let e = ((v / x.p99) * 8.0).round() as usize;
                    H[e.clamp(1, 8) - 1]
                };
                let st = Style::default().fg(th.c("graph_text"));
                Some(Span::styled(
                    format!("  {}{}{} ", h(x.p50), h(x.p90), h(x.p99)),
                    st,
                ))
            }
            _ => None,
        };
        let mut row = vec![
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
        ];
        if let Some(t) = tail {
            row.push(t);
        }
        lines.push(Line::from(row));
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

    fn snap_with_latency(set: LatencySet) -> Snapshot {
        Snapshot {
            traffic: crate::model::snapshot::TrafficPanel {
                latency: set,
                ..Default::default()
            },
            ..Default::default()
        }
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
        let text = render_traffic(&snap_with_latency(set));
        assert!(text.contains("420ms"), "p50 humanized: {text}");
        assert!(!text.contains("N/A"), "filled → no N/A in the ttft row");
    }

    #[test]
    fn latency_row_tails_with_a_mini_sparkline() {
        // p50/p90 vs the row's own p99: 0.42/3.0 → 1/8 → ▁, 1.1/3.0 → 3/8 → ▃,
        // p99 itself → █.
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
        let text = render_traffic(&snap_with_latency(set));
        assert!(text.contains("▁▃█"), "mini-bars scaled to p99: {text}");
    }

    #[test]
    fn latency_row_has_no_mini_sparkline_without_data() {
        let text = render_traffic(&snap_with_latency(LatencySet {
            known: [false, true, false, false],
            ..Default::default()
        }));
        assert!(
            !text.contains('▁') && !text.contains('█'),
            "no percentiles → no mini-bars: {text}"
        );
    }
}
