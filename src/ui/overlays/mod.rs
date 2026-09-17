//! Full-screen menu overlays: help, options, server-info. Drawn on top of a
//! still-live screen (the poller keeps running underneath — btop's "menu opens
//! instantly" idiom, because the base frame was already computed). Any key
//! closes an open overlay; see `App::on_key`.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Clear, Paragraph, Widget},
};

use crate::config::Config;
use crate::model::snapshot::Snapshot;
use crate::ui::theme::Theme;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Overlay {
    Help,
    Options,
    ServerInfo,
}

/// A `key   description` help row: key in the hot colour, description dimmed.
fn help_row(key: &str, desc: &str, th: &Theme) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("{key:<6}"),
            Style::default().fg(th.c("hi_fg")).bold(),
        ),
        Span::styled(desc.to_string(), Style::default().fg(th.c("main_fg"))),
    ])
}

fn on_off(v: bool) -> (&'static str, bool) {
    (if v { "ON" } else { "OFF" }, v)
}

/// `label  VALUE` options row; the value wears green(ON)/red(OFF) when it's a
/// boolean, else main_fg.
fn opt_row(label: &str, value: &str, good: bool, th: &Theme) -> Line<'static> {
    let vstyle = Style::default()
        .fg(if good {
            th.c("proc_misc")
        } else {
            th.c("hi_fg")
        })
        .bold();
    Line::from(vec![
        Span::styled(
            format!("{label:<16}"),
            Style::default().fg(th.c("title")).bold(),
        ),
        Span::styled(value.to_string(), vstyle),
    ])
}

fn meta_row(label: &str, value: &str, th: &Theme) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("{label:<16}"),
            Style::default().fg(th.c("title")).bold(),
        ),
        Span::styled(value.to_string(), Style::default().fg(th.c("main_fg"))),
    ])
}

/// meta_row with a wider label column, for the startup block where some
/// phase names (`kv_cache_allocation`) outgrow 16 and glue onto the value.
fn wide_row(label: &str, value: &str, th: &Theme) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("{label:<20}"),
            Style::default().fg(th.c("title")).bold(),
        ),
        Span::styled(value.to_string(), Style::default().fg(th.c("main_fg"))),
    ])
}

/// Live poll-control state the options menu reflects (snapshot of `PollControl`
/// taken by `App::draw` so the overlay never holds a lock while rendering).
#[derive(Debug, Clone, Copy)]
pub struct Live {
    pub paused: bool,
    pub interval_ms: u64,
}

/// Render the given overlay centered over `area` into `buf`.
pub fn render(
    which: Overlay,
    cfg: &Config,
    snap: &Snapshot,
    live: Live,
    th: &Theme,
    area: Rect,
    buf: &mut Buffer,
) {
    // Panel sized to the larger content, capped inside the area. Built body
    // first so the height fits what's actually shown: startup timelines and
    // stage breakdowns are server-dependent and used to clip at 26 rows.
    let mut body: Vec<Line<'static>> = match which {
        Overlay::Help => help_lines(th),
        Overlay::Options => options_lines(cfg, live.paused, live.interval_ms, th),
        Overlay::ServerInfo => server_lines(cfg, snap, th),
    };
    body.push(Line::from(""));
    body.push(Line::from(Span::styled(
        "any key: back",
        Style::default()
            .fg(th.c("inactive_fg"))
            .add_modifier(Modifier::ITALIC),
    )));

    let w = 66u16.min(area.width.saturating_sub(2));
    let h = 26u16
        .max(body.len() as u16 + 2)
        .min(area.height.saturating_sub(2));
    if w < 20 || h < 6 {
        return;
    }
    let rect = centered_rect(area, w, h);

    // Clear + framed block so the live screen doesn't bleed through the panel.
    Clear.render(rect, buf);
    let frame = border_style_key(which);
    let block = Block::bordered()
        .title(Line::from(Span::styled(
            format!(" {} ", title(which)),
            Style::default().fg(th.c(frame)).bold(),
        )))
        .border_style(Style::default().fg(th.c(frame)))
        .style(Style::default().fg(th.c("main_fg")));
    let inner = block.inner(rect);
    block.render(rect, buf);

    Paragraph::new(body).render(inner, buf);
}

fn title(which: Overlay) -> &'static str {
    match which {
        Overlay::Help => "help",
        Overlay::Options => "options",
        Overlay::ServerInfo => "server info",
    }
}

fn border_style_key(which: Overlay) -> &'static str {
    match which {
        Overlay::Help => "title",
        Overlay::Options => "proc_misc",
        Overlay::ServerInfo => "free_mid",
    }
}

fn help_lines(th: &Theme) -> Vec<Line<'static>> {
    let rows = [
        ("q / ^C", "quit"),
        ("? / F1", "this help"),
        ("o / F2", "options"),
        ("i / F9", "server info"),
        ("esc", "close an open overlay"),
        ("1 2 3 4", "toggle engine / pool / traffic / ranks"),
        ("↑↓ / j k", "select rank  (Home/End jump)"),
        ("← →", "change sort column"),
        ("r", "reverse sort order"),
        ("Enter", "rank detail  (esc back)"),
        ("u", "pause / resume scraping"),
        ("+ / -", "poll interval ±100ms (100ms–10s)"),
    ];
    let mut v: Vec<Line> = rows.iter().map(|(k, d)| help_row(k, d, th)).collect();
    v.push(Line::from(""));
    v.push(Line::from(Span::styled(
        "Percentile rows appear only for latency families the server exposes.",
        Style::default().fg(th.c("graph_text")).italic(),
    )));
    v
}

fn options_lines(cfg: &Config, paused: bool, interval_ms: u64, th: &Theme) -> Vec<Line<'static>> {
    let mut v = vec![
        opt_row("poll interval", &format!("{interval_ms} ms"), true, th),
        Line::from(Span::styled(
            "  (live: + / -  ±100ms, u pauses)",
            Style::default().fg(th.c("graph_text")).italic(),
        )),
    ];
    let (_pv, pon) = on_off(!paused);
    v.push(opt_row(
        "scraping",
        if pon { "running" } else { "PAUSED" },
        pon,
        th,
    ));
    v.push(Line::from(""));
    v.push(opt_row("theme", &cfg.theme, true, th));
    v.push(opt_row("color mode", &cfg.color_mode, true, th));
    let (rc, rcon) = on_off(cfg.round_corners);
    v.push(opt_row("round corners", rc, rcon, th));
    v
}

fn server_lines(cfg: &Config, snap: &Snapshot, th: &Theme) -> Vec<Line<'static>> {
    let s = &snap.server;
    let yes_no = |b: bool| (if b { "yes" } else { "no" }, b);
    let mut v = vec![
        meta_row("url", &cfg.url, th),
        meta_row(
            "served model",
            if s.loaded {
                &s.served_model_name
            } else {
                "—"
            },
            th,
        ),
        meta_row("model path", if s.loaded { &s.model_path } else { "—" }, th),
        meta_row("version", if s.loaded { &s.version } else { "—" }, th),
        meta_row(
            "tp / pp / dp",
            &format!("{} / {} / {}", s.tp_size, s.pp_size, s.dp_size),
            th,
        ),
    ];
    let (v_, _) = yes_no(s.enable_dp_attention);
    v.push(opt_row("dp attention", v_, s.enable_dp_attention, th));
    let (v_, _) = yes_no(s.enable_metrics_for_all_schedulers);
    v.push(opt_row(
        "all-sched metrics",
        v_,
        s.enable_metrics_for_all_schedulers,
        th,
    ));
    let (v_, _) = yes_no(s.enable_hierarchical_cache);
    v.push(opt_row(
        "hierarchical cache",
        v_,
        s.enable_hierarchical_cache,
        th,
    ));
    v.push(meta_row(
        "max_total_tokens",
        &s.max_total_num_tokens
            .map(|n| n.to_string())
            .unwrap_or_else(|| "—".into()),
        th,
    ));
    v.push(meta_row(
        "context_len",
        &s.context_len
            .map(|n| n.to_string())
            .unwrap_or_else(|| "—".into()),
        th,
    ));
    // Names the topology the traffic panel's `pd` row depends on: "null" is
    // sglang's own spelling for a unified server, which reads as a bug.
    v.push(meta_row(
        "disaggregation",
        match s.disaggregation_mode.as_str() {
            "" => "—",
            "null" => "unified",
            other => other,
        },
        th,
    ));
    // Startup timeline: why boot took as long as it did, without logs.
    if !s.startup_phases.is_empty() {
        v.push(Line::from(""));
        v.push(Line::from(Span::styled(
            "startup",
            Style::default().fg(th.c("title")).bold(),
        )));
        // Phase names are dynamic and some are long — pad to 20 so values
        // never glue onto the label (meta_row's 16 would clip
        // "kv_cache_allocation" into the number).
        for (phase, secs) in &s.startup_phases {
            v.push(wide_row(phase, &format!("{secs:.1}s"), th));
        }
        if let Some(g) = s.startup_graph_secs {
            v.push(wide_row("graph capture", &format!("{g:.1}s"), th));
        }
        if let Some(free) = s.startup_free_gpu_gb {
            v.push(wide_row("free gpu @boot", &format!("{free:.1} GB"), th));
        }
    }
    // Full latency percentile set — every family the server exposes, not
    // just the two that fit the traffic panel's nine interior rows (itl/e2e
    // silently clip there). Same rationale as the stage block below: the
    // panel has no spare row, the overlay has the room.
    let lat = &snap.traffic.latency;
    let lat_rows = [
        ("queue", lat.known[0], lat.queue),
        ("ttft", lat.known[1], lat.ttft),
        ("itl", lat.known[2], lat.itl),
        ("e2e", lat.known[3], lat.e2e),
    ];
    if lat_rows.iter().any(|(_, k, _)| *k) {
        v.push(Line::from(""));
        v.push(Line::from(Span::styled(
            "latency percentiles",
            Style::default().fg(th.c("title")).bold(),
        )));
        for (label, known, p) in lat_rows {
            if !known {
                continue;
            }
            let value = match p {
                Some(p) => {
                    let mean = p
                        .mean
                        .map(crate::ui::panels::human::human_duration)
                        .unwrap_or_else(|| "—".into());
                    format!(
                        "p50 {}  p90 {}  p99 {}  avg {}",
                        crate::ui::panels::human::human_duration(p.p50),
                        crate::ui::panels::human::human_duration(p.p90),
                        crate::ui::panels::human::human_duration(p.p99),
                        mean,
                    )
                }
                None => "—".into(),
            };
            v.push(wide_row(label, &value, th));
        }
    }
    // Request-lifecycle stages: where a served request spends its time,
    // slowest first. Same shape as the startup block (phase → seconds, read
    // from the server, never hardcoded) and the same rationale for living
    // here — it's diagnostic detail, and the traffic panel has no spare row.
    if !snap.traffic.stage_means.is_empty() {
        v.push(Line::from(""));
        v.push(Line::from(Span::styled(
            "request stages",
            Style::default().fg(th.c("title")).bold(),
        )));
        for (stage, secs) in &snap.traffic.stage_means {
            v.push(wide_row(
                stage,
                &crate::ui::panels::human::human_duration(*secs),
                th,
            ));
        }
    }
    if !s.loaded {
        v.push(Line::from(Span::styled(
            "metadata not yet fetched (auth-gated /server_info)",
            Style::default().fg(th.c("graph_text")).italic(),
        )));
    }
    v
}

fn centered_rect(area: Rect, w: u16, h: u16) -> Rect {
    let x = area.x + area.width.saturating_sub(w) / 2;
    let y = area.y + area.height.saturating_sub(h) / 2;
    Rect {
        x,
        y,
        width: w.min(area.width),
        height: h.min(area.height),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::theme::{ColorMode, Theme};

    fn render_overlay(
        which: Overlay,
        cfg: &Config,
        snap: &Snapshot,
        paused: bool,
        interval_ms: u64,
        w: u16,
        h: u16,
    ) -> String {
        let th = Theme::builtin("Default", ColorMode::TrueColor);
        let mut buf = Buffer::empty(Rect::new(0, 0, w, h));
        render(
            which,
            cfg,
            snap,
            Live {
                paused,
                interval_ms,
            },
            &th,
            Rect::new(0, 0, w, h),
            &mut buf,
        );
        let b = &buf;
        (0..h)
            .flat_map(|y| (0..w).map(move |x| b[(x, y)].symbol().to_owned()))
            .collect()
    }

    #[test]
    fn help_lists_the_hot_keys() {
        let text = render_overlay(
            Overlay::Help,
            &Config::default(),
            &Snapshot::default(),
            false,
            1000,
            80,
            30,
        );
        assert!(text.contains("quit"), "{text}");
        assert!(text.contains("pause"), "{text}");
    }

    #[test]
    fn options_reflect_live_interval_and_pause() {
        let run = render_overlay(
            Overlay::Options,
            &Config::default(),
            &Snapshot::default(),
            false,
            850,
            80,
            30,
        );
        assert!(run.contains("850 ms"), "{run}");
        let held = render_overlay(
            Overlay::Options,
            &Config::default(),
            &Snapshot::default(),
            true,
            850,
            80,
            30,
        );
        assert!(held.contains("PAUSED"), "{held}");
    }

    #[test]
    fn server_info_shows_meta_when_loaded() {
        let mut snap = Snapshot::default();
        snap.server.loaded = true;
        snap.server.served_model_name = "Qwen3.8-Flash-Next".into();
        snap.server.tp_size = 2;
        let text = render_overlay(
            Overlay::ServerInfo,
            &Config::default(),
            &snap,
            false,
            1000,
            80,
            30,
        );
        assert!(text.contains("Qwen3.8-Flash-Next"), "{text}");
    }

    // The overlay lists every exposed latency family — including itl/e2e,
    // which the traffic panel clips at common heights.
    #[test]
    fn server_info_lists_latency_percentiles() {
        let mut snap = Snapshot::default();
        snap.server.loaded = true;
        snap.traffic.latency.known = [true, true, true, true];
        snap.traffic.latency.queue = Some(crate::model::snapshot::Percentile {
            p50: 0.004,
            p90: 0.021,
            p99: 0.048,
            mean: Some(0.012),
        });
        snap.traffic.latency.itl = Some(crate::model::snapshot::Percentile {
            p50: 0.031,
            p90: 0.052,
            p99: 0.061,
            mean: None,
        });
        let text = render_overlay(
            Overlay::ServerInfo,
            &Config::default(),
            &snap,
            false,
            1000,
            80,
            44,
        );
        assert!(text.contains("latency percentiles"), "{text}");
        assert!(text.contains("queue") && text.contains("4ms"), "{text}");
        assert!(text.contains("itl") && text.contains("31ms"), "{text}");
        // Family known but no delta yet: e2e shows an em-dash, never a
        // fabricated percentile (48ms is queue's p99, not e2e's).
        assert!(text.contains("e2e") && text.contains('—'), "{text}");
        assert!(!text.contains("e2e             48ms"), "{text}");
    }

    // The stage breakdown needs more rows than the base 26 — the panel
    // grows to fit its body (this used to clip silently).
    #[test]
    fn server_info_lists_request_stages() {
        let mut snap = Snapshot::default();
        snap.server.loaded = true;
        snap.traffic.stage_means = vec![
            ("prefill_forward".into(), 0.374),
            ("chunked_prefill".into(), 0.319),
            ("request_process".into(), 0.0002),
        ];
        let text = render_overlay(
            Overlay::ServerInfo,
            &Config::default(),
            &snap,
            false,
            1000,
            80,
            40,
        );
        assert!(text.contains("request stages"), "{text}");
        assert!(
            text.contains("prefill_forward") && text.contains("374ms"),
            "{text}"
        );
        assert!(
            text.contains("request_process") && text.contains("0ms"),
            "{text}"
        );
    }
}
