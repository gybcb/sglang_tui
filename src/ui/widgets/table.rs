use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::{Scrollbar, ScrollbarOrientation, StatefulWidget},
};

use crate::model::snapshot::{RankGranularity, RankRow, Snapshot};
use crate::ui::frame;
use crate::ui::panels::human::{human_count, human_pct, human_rate, value_cell};
use crate::ui::theme::Theme;

use super::Gui;

/// Sortable column of the RANKS table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortCol {
    Rank,
    Run,
    Wait,
    Used,
    Usage,
    Gen,
    Hit,
    Util,
}

impl SortCol {
    pub const ALL: [SortCol; 8] = [
        SortCol::Rank,
        SortCol::Run,
        SortCol::Wait,
        SortCol::Used,
        SortCol::Usage,
        SortCol::Gen,
        SortCol::Hit,
        SortCol::Util,
    ];

    pub fn next(self) -> SortCol {
        Self::ALL[(Self::ALL.iter().position(|c| *c == self).unwrap() + 1) % Self::ALL.len()]
    }
    pub fn prev(self) -> SortCol {
        let n = Self::ALL.len();
        Self::ALL[(Self::ALL.iter().position(|c| *c == self).unwrap() + n - 1) % n]
    }

    pub fn label(self) -> &'static str {
        match self {
            SortCol::Rank => "rank",
            SortCol::Run => "run",
            SortCol::Wait => "wait",
            SortCol::Used => "used",
            SortCol::Usage => "usage",
            SortCol::Gen => "gen/s",
            SortCol::Hit => "hit",
            SortCol::Util => "util",
        }
    }

    /// Cell width this column occupies (shared by header and rows).
    pub fn width(self) -> usize {
        match self {
            SortCol::Rank => 5,
            SortCol::Run | SortCol::Wait => 8,
            SortCol::Used => 9,
            SortCol::Usage | SortCol::Gen | SortCol::Hit | SortCol::Util => 7,
        }
    }

    /// Sortable value. `None`-valued rows (only `util` can be) get `None` and
    /// are pinned to the end of the ordering in either direction.
    fn value(self, r: &RankRow, agg: bool) -> Option<f64> {
        match self {
            SortCol::Rank => Some(if agg { -1.0 } else { r.dp_rank as f64 }),
            SortCol::Run => Some(r.running as f64),
            SortCol::Wait => Some(r.waiting as f64),
            SortCol::Used => Some(r.used_tokens as f64),
            SortCol::Usage => Some(r.token_usage),
            SortCol::Gen => Some(r.gen_throughput),
            SortCol::Hit => Some(r.cache_hit_rate),
            SortCol::Util => r.utilization,
        }
    }
}

/// Display order: the aggregate row (index 0) pinned first, the rest sorted by
/// `col`; missing values trail in both directions. Returns indices into
/// `s.ranks`.
pub fn order(s: &Snapshot, col: SortCol, reverse: bool) -> Vec<usize> {
    let mut idx: Vec<usize> = (1..s.ranks.len()).collect();
    idx.sort_by(|&a, &b| {
        let va = col.value(&s.ranks[a], false);
        let vb = col.value(&s.ranks[b], false);
        match (va, vb) {
            (Some(x), Some(y)) => {
                let o = x.partial_cmp(&y).unwrap_or(std::cmp::Ordering::Equal);
                let o = if reverse { o.reverse() } else { o };
                o.then(a.cmp(&b))
            }
            (None, None) => a.cmp(&b),
            // Missing trails whichever way we sort.
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (Some(_), None) => std::cmp::Ordering::Less,
        }
    });
    let mut all = Vec::with_capacity(s.ranks.len());
    if !s.ranks.is_empty() {
        all.push(0);
    }
    all.extend(idx);
    all
}

/// Clamp a selection index to the visible row set.
pub fn clamp_sel(sel: usize, n_ranks: usize) -> usize {
    if n_ranks == 0 {
        0
    } else {
        sel.min(n_ranks - 1)
    }
}

/// Scroll offset (index into `order`) that keeps `sel` visible inside
/// `rows_h` rows.
fn scroll_for(sel: usize, rows_h: usize) -> usize {
    if rows_h == 0 {
        return 0;
    }
    sel.saturating_sub(rows_h - 1)
}

/// Width of the numeric column block (rank + 7 value columns + separators).
const NUM_COLS: usize = 5 + 8 + 8 + 9 + 7 + 7 + 7 + 7;

/// RANKS — btop's PROC-box analog with DP ranks as the subject (SGLang's HTTP
/// surface has no per-request introspection). Row 0 is the aggregate, pinned.
#[allow(clippy::too_many_arguments)] // render fns take their whole subject
pub fn ranks(
    s: &Snapshot,
    th: &Theme,
    gui: Gui,
    rect: Rect,
    buf: &mut Buffer,
    sel: usize,
    col: SortCol,
    reverse: bool,
) {
    let dim = s.conn.dimmed();
    let sel = clamp_sel(sel, s.ranks.len());
    let order = order(s, col, reverse);
    let inner_h = rect.height.saturating_sub(2) as usize;
    // Interior rows: header (1) + rule (1) + data (rows_h) + page footer (1).
    if inner_h < 4 {
        return;
    }
    let rows_h = inner_h - 3;
    let scroll = scroll_for(sel, rows_h);

    let mut lines: Vec<Line<'static>> = Vec::with_capacity(inner_h + 1);
    lines.push(Line::from(header_spans(col, reverse, th)));
    // A divider under the header bars (modern-table idiom), in `div_line`,
    // spanning the numeric block so it reads as a column-set separator rather
    // than a stray line.
    lines.push(Line::from(vec![Span::styled(
        "─".repeat(NUM_COLS),
        Style::default().fg(th.c("div_line")),
    )]));

    for &i in order.iter().skip(scroll).take(rows_h) {
        let r = &s.ranks[i];
        let selected = i == sel;
        let name = if i == 0 {
            "agg".to_string()
        } else {
            format!("{}", r.dp_rank)
        };
        let name_style = if selected {
            Style::default()
                .fg(th.c("selected_fg"))
                .bg(th.c("selected_bg"))
                .bold()
        } else if i == 0 {
            Style::default().fg(th.c("proc_misc"))
        } else {
            Style::default().fg(th.c("main_fg"))
        };
        let cell = |c: SortCol, v: Option<String>| -> Span<'static> {
            let mut sp = value_cell(v, c.width(), th, dim);
            if selected {
                sp.style = sp.style.fg(th.c("selected_fg")).bg(th.c("selected_bg"));
            }
            sp
        };
        // btop keeps the value column colours; the sort column's header is
        // already highlighted, and colouring its whole column too would fight
        // the selection bar, so cells stay main_fg.
        let _ = col;
        lines.push(Line::from(vec![
            Span::styled(format!("{name:<5}"), name_style),
            cell(SortCol::Run, Some(human_count(r.running))),
            cell(SortCol::Wait, Some(human_count(r.waiting))),
            cell(SortCol::Used, Some(human_count(r.used_tokens))),
            cell(SortCol::Usage, Some(human_pct(r.token_usage))),
            cell(SortCol::Gen, Some(human_rate(r.gen_throughput))),
            cell(SortCol::Hit, Some(human_pct(r.cache_hit_rate))),
            cell(SortCol::Util, r.utilization.map(human_pct)),
        ]));
    }

    while lines.len() < rows_h + 2 {
        lines.push(Line::from(""));
    }

    // btop's proc footer: `┘shown/total└`, plus the §8-R4 reason when the
    // server can't attribute per-rank values.
    let shown = order.len().saturating_sub(scroll).min(rows_h);
    let mut footer = vec![Span::styled(
        format!("┘{}/{}└ ", shown, order.len()),
        Style::default().fg(th.c("graph_text")),
    )];
    if let RankGranularity::Aggregated(reason) = &s.rank_granularity {
        footer.push(Span::styled(
            format!("aggregated: {reason}"),
            Style::default().fg(th.c("graph_text")).italic(),
        ));
    }
    lines.push(Line::from(footer));

    frame::write_lines(buf, rect, &lines);

    // Per-row mini graph, in the columns the numeric block leaves over.
    // Block style gets a one-row sparkline; braille style keeps btop's
    // dotted row.
    let gw = rect.width.saturating_sub(NUM_COLS as u16 + 3) as usize;
    if gw >= 6 {
        let gx = rect.x + 1 + NUM_COLS as u16;
        for (k, &i) in order.iter().skip(scroll).take(rows_h).enumerate() {
            let y = rect.y + 1 + 2 + k as u16; // below the header + rule lines
            let area = Rect {
                x: gx,
                y,
                width: gw as u16,
                height: 1,
            };
            let series = s.ranks[i].gen_series.tail_owned(usize::MAX);
            if gui.braille_graphs {
                crate::ui::widgets::braille::draw(buf, area, &series, th, "cpu", None, true);
            } else {
                crate::ui::widgets::block::sparkline(buf, area, &series, th, "cpu");
            }
            if i == sel {
                // Keep the selection bar continuous across the graph column.
                for x in gx..gx + gw as u16 {
                    let c = &mut buf[(x, y)];
                    let fg = c.style().fg.unwrap_or(th.c("main_fg"));
                    c.set_style(Style::default().bg(th.c("selected_bg")).fg(fg));
                }
            }
        }
    }

    // btop's proc-box scrollbar column, only when the list overflows.
    if order.len() > rows_h {
        let mut sb_state = ratatui::widgets::ScrollbarState::new(order.len())
            .viewport_content_length(rows_h)
            .position(scroll);
        Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None)
            .thumb_symbol("█")
            .thumb_style(Style::default().fg(th.c("selected_fg")))
            .track_style(Style::default().fg(th.c("div_line")))
            .render(
                Rect {
                    x: rect.x + rect.width.saturating_sub(2),
                    y: rect.y + 1,
                    width: 1,
                    height: rect.height.saturating_sub(2),
                },
                buf,
                &mut sb_state,
            );
    }
}

/// Enter-detail: like btop's proc-box detail, replaces the table interior
/// with the selected row's full readout + its gen/s braille graph.
pub fn rank_detail(s: &Snapshot, th: &Theme, rect: Rect, buf: &mut Buffer, sel: usize) {
    let dim = s.conn.dimmed();
    let sel = clamp_sel(sel, s.ranks.len());
    if s.ranks.is_empty() {
        return;
    }
    let r = &s.ranks[sel];
    let name = if sel == 0 {
        "agg".to_string()
    } else {
        format!("rank {}", r.dp_rank)
    };

    let label = |t: &str| Span::styled(format!("{t} "), Style::default().fg(th.c("title")).bold());
    let mut lines: Vec<Line<'static>> = Vec::new();
    lines.push(Line::from(vec![
        Span::styled(name, Style::default().fg(th.c("proc_misc")).bold()),
        Span::styled(
            "  (esc back)",
            Style::default().fg(th.c("graph_text")).italic(),
        ),
    ]));
    lines.push(Line::from(vec![
        label("running"),
        value_cell(Some(human_count(r.running)), 8, th, dim),
        label("  waiting"),
        value_cell(Some(human_count(r.waiting)), 8, th, dim),
    ]));
    lines.push(Line::from(vec![
        label("used   "),
        value_cell(Some(human_count(r.used_tokens)), 8, th, dim),
        label("  total "),
        value_cell(Some(human_count(r.total_tokens)), 8, th, dim),
        label("  usage "),
        value_cell(Some(human_pct(r.token_usage)), 6, th, dim),
    ]));
    lines.push(Line::from(vec![
        label("gen/s  "),
        value_cell(Some(human_rate(r.gen_throughput)), 8, th, dim),
        label("  hit   "),
        value_cell(Some(human_pct(r.cache_hit_rate)), 8, th, dim),
        label("  util  "),
        value_cell(r.utilization.map(human_pct), 6, th, dim),
    ]));
    lines.push(Line::from(vec![
        label("spec   "),
        value_cell(
            r.spec_accept_length.map(|v| format!("{:.2} tok/fwd", v)),
            10,
            th,
            dim,
        ),
        label("  accept "),
        value_cell(r.spec_accept_rate.map(human_pct), 8, th, dim),
    ]));

    frame::write_lines(buf, rect, &lines);

    // The selected rank's own gen/s history — btop's proc-detail graphs.
    let used = lines.len().min(rect.height.saturating_sub(2) as usize);
    let graph = Rect {
        x: rect.x + 1,
        y: rect.y + 1 + used as u16,
        width: rect.width.saturating_sub(2),
        height: rect.height.saturating_sub(2 + used as u16),
    };
    crate::ui::widgets::braille::draw(
        buf,
        graph,
        &r.gen_series.tail_owned(usize::MAX),
        th,
        "cpu",
        None,
        true,
    );
}

/// Header row: column names right-aligned like their data, sort column
/// `hi_fg` + a ▲/▼ arrow (btop marks the active proc sort the same way).
fn header_spans(col: SortCol, reverse: bool, th: &Theme) -> Vec<Span<'static>> {
    SortCol::ALL
        .iter()
        .map(|&c| {
            let arrow = if c == col {
                if reverse { "▼" } else { "▲" }
            } else {
                ""
            };
            let style = if c == col {
                Style::default().fg(th.c("hi_fg")).bold()
            } else {
                Style::default().fg(th.c("title")).bold()
            };
            let txt = format!("{}{}", c.label(), arrow);
            if c == SortCol::Rank {
                Span::styled(format!("{txt:<5}"), style)
            } else {
                Span::styled(format!("{:>w$}", txt, w = c.width()), style)
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::snapshot::RankRow;
    use crate::ui::theme::{ColorMode, Theme};
    use ratatui::layout::Rect;

    fn snap_with(n: usize) -> Snapshot {
        // Index 0 is the aggregate; ranks 1..n carry descending `running` so
        // an ascending sort must invert them.
        Snapshot {
            ranks: (0..n)
                .map(|i| RankRow {
                    dp_rank: i as u32,
                    running: (n - i) as u64,
                    ..RankRow::default()
                })
                .collect(),
            ..Snapshot::default()
        }
    }

    fn rendered(s: &Snapshot, sel: usize, col: SortCol, rev: bool, w: u16, h: u16) -> String {
        let th = Theme::builtin("Default", ColorMode::TrueColor);
        let mut buf = Buffer::empty(Rect::new(0, 0, w, h));
        ranks(
            s,
            &th,
            crate::ui::widgets::Gui::MODERN,
            Rect::new(0, 0, w, h),
            &mut buf,
            sel,
            col,
            rev,
        );
        let b = &buf;
        (0..h)
            .flat_map(|y| (0..w).map(move |x| b[(x, y)].symbol().to_owned()))
            .collect()
    }

    #[test]
    fn aggregate_row_stays_first_whatever_the_sort() {
        let s = snap_with(4);
        for col in SortCol::ALL {
            for rev in [false, true] {
                assert_eq!(order(&s, col, rev)[0], 0, "agg pinned: {col:?} rev={rev}");
            }
        }
    }

    #[test]
    fn sorts_by_column_and_reverse_flips() {
        let s = snap_with(3); // ranks 1,2 with running 2,1
        assert_eq!(order(&s, SortCol::Run, false), vec![0, 2, 1]);
        assert_eq!(order(&s, SortCol::Run, true), vec![0, 1, 2]);
    }

    #[test]
    fn missing_values_trail_in_both_directions() {
        let mut s = snap_with(3);
        s.ranks[1].utilization = Some(90.0);
        s.ranks[2].utilization = None;
        assert_eq!(order(&s, SortCol::Util, false), vec![0, 1, 2]);
        assert_eq!(
            order(&s, SortCol::Util, true),
            vec![0, 1, 2],
            "None still trails reversed"
        );
    }

    #[test]
    fn column_cycles_forward_backward() {
        assert_eq!(SortCol::Rank.next(), SortCol::Run);
        assert_eq!(SortCol::Util.next(), SortCol::Rank);
        assert_eq!(SortCol::Rank.prev(), SortCol::Util);
    }

    #[test]
    fn header_marks_the_sort_column_with_an_arrow() {
        let s = snap_with(2);
        let text = rendered(&s, 0, SortCol::Gen, false, 66, 8);
        assert!(text.contains("gen/s▲"), "sort column marked: {text}");
        let rev = rendered(&s, 0, SortCol::Gen, true, 66, 8);
        assert!(rev.contains("gen/s▼"), "reverse arrow: {rev}");
    }

    #[test]
    fn header_rule_divides_titles_from_rows() {
        let s = snap_with(2);
        let th = Theme::builtin("Default", ColorMode::TrueColor);
        let mut buf = Buffer::empty(Rect::new(0, 0, 66, 12));
        ranks(
            &s,
            &th,
            crate::ui::widgets::Gui::MODERN,
            Rect::new(0, 0, 66, 12),
            &mut buf,
            0,
            SortCol::Rank,
            false,
        );
        // Interior row 1 (just under the header titles) is the divider.
        let rule: String = (1..65).map(|x| buf[(x, 2)].symbol().to_owned()).collect();
        assert!(
            rule.starts_with(&"─".repeat(NUM_COLS)),
            "rule spans the numeric block: {rule:?}"
        );
        assert_eq!(buf[(5, 2)].style().fg, Some(th.c("div_line")));
    }

    #[test]
    fn dp_size_one_renders_agg_and_a_single_row_footer() {
        // The real server's shape: one aggregate row, no per-rank series.
        let s = snap_with(1);
        let text = rendered(&s, 0, SortCol::Rank, false, 66, 12);
        assert!(text.contains("agg"), "{text}");
        assert!(text.contains("1/1"), "page footer: {text}");
    }

    #[test]
    fn aggregated_granularity_note_appears_in_footer() {
        let mut s = snap_with(1);
        s.rank_granularity =
            RankGranularity::Aggregated("dp-attention without all-schedulers metrics".into());
        let text = rendered(&s, 0, SortCol::Rank, false, 66, 12);
        assert!(text.contains("aggregated"), "{text}");
    }

    #[test]
    fn selection_out_of_range_clamps_instead_of_panicking() {
        let s = snap_with(2);
        let _ = rendered(&s, 99, SortCol::Rank, false, 66, 12);
    }

    #[test]
    fn long_list_scrolls_to_keep_selection_visible() {
        // 10 ranks + agg in a 6-row interior: selecting the last must bring it
        // on-screen (its row label present) and the first rank off it.
        let s = snap_with(11);
        let text = rendered(&s, 10, SortCol::Rank, false, 66, 9);
        assert!(text.contains("10"), "selected tail rank visible: {text}");
        assert!(
            !text.contains("agg"),
            "scrolled past the aggregate row: {text}"
        );
    }

    #[test]
    fn scroll_offset_tracks_selection() {
        assert_eq!(scroll_for(0, 5), 0);
        assert_eq!(scroll_for(4, 5), 0);
        assert_eq!(scroll_for(5, 5), 1);
        assert_eq!(scroll_for(9, 5), 5);
        assert_eq!(
            scroll_for(3, 0),
            0,
            "zero-height interior must not underflow"
        );
    }

    #[test]
    fn clamp_sel_bounds_and_empty() {
        assert_eq!(clamp_sel(5, 3), 2);
        assert_eq!(clamp_sel(5, 0), 0);
    }
}
