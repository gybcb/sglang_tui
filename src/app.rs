use std::sync::{Arc, RwLock};

use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

use crate::config::Config;
use crate::error::SgtopError;
use crate::model::snapshot::{Connection, Snapshot};
use crate::poll::PollControl;
use crate::ui::widgets::table::{self, SortCol};
use crate::ui::{
    frame::{self, BorderSet, Pane, border_set},
    layout::{self, Boxes, Layout},
    overlays::{self, Overlay},
    panels,
    theme::Theme,
};

/// Top-level application state. Owns layout toggles and selection; the data
/// lives in a snapshot shared with the poller (poll = only writer, UI = reader).
pub struct App {
    pub snapshot: Arc<RwLock<Snapshot>>,
    /// UI→poller controls (pause flag, live interval). Shared with the poller;
    /// the UI is the only writer, the poller reads it once per pacing loop.
    pub control: Arc<RwLock<PollControl>>,
    pub config: Config,
    pub theme: Theme,
    pub boxes: Boxes,
    pub layout: Layout,
    bs: BorderSet,
    gui: crate::ui::widgets::Gui,
    pub quit: bool,
    /// Selected RANKS row index (into `snapshot.ranks`, 0 = aggregate).
    pub rank_sel: usize,
    pub sort_col: SortCol,
    pub sort_reverse: bool,
    /// Enter-detail overlay: drawn over the ranks box interior, so the rest
    /// of the screen (and the poller) keeps running underneath — btop's menu
    /// idiom, not a mode switch.
    pub detail_open: bool,
    /// Full-screen help / options / server-info menu. Like btop's menus, the
    /// underlying screen (and the poller) keeps running while one is open.
    pub overlay: Option<Overlay>,
}

impl App {
    pub fn new(config: Config, snapshot: Arc<RwLock<Snapshot>>) -> App {
        let theme = config.theme();
        let bs = border_set(config.round_corners);
        let control = Arc::new(RwLock::new(PollControl {
            paused: false,
            interval_ms: config.interval_ms,
        }));
        App {
            snapshot,
            control,
            boxes: Boxes::all(),
            layout: Layout::default(),
            bs,
            gui: crate::ui::widgets::Gui {
                braille_graphs: config.graph_braille(),
            },
            quit: false,
            config,
            theme,
            rank_sel: 0,
            sort_col: SortCol::Rank,
            sort_reverse: false,
            detail_open: false,
            overlay: None,
        }
    }

    pub fn reflow(&mut self, area: Rect) {
        self.layout = layout::solve(area, &self.boxes);
    }

    pub fn on_key(&mut self, key: crossterm::event::KeyEvent) -> bool {
        use crossterm::event::{KeyCode, KeyEventKind, KeyModifiers};
        if key.kind != KeyEventKind::Press {
            return false;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('c')) {
            return true;
        }
        // An open overlay owns the keyboard: any key closes it and is consumed,
        // so the screen underneath never gets a stray toggle/quit.
        if self.overlay.is_some() {
            self.overlay = None;
            return false;
        }
        let n_ranks = self.snapshot.read().unwrap().ranks.len();
        match key.code {
            KeyCode::Char('q') => true,
            KeyCode::Char('?') | KeyCode::F(1) => {
                self.overlay = Some(Overlay::Help);
                false
            }
            KeyCode::Char('o') | KeyCode::F(2) => {
                self.overlay = Some(Overlay::Options);
                false
            }
            KeyCode::Char('i') | KeyCode::F(9) => {
                self.overlay = Some(Overlay::ServerInfo);
                false
            }
            // Poll control. `u` freezes scraping (drawing + the poller's own
            // backoff keep running); +/- retune the healthy-scrape interval.
            KeyCode::Char('u') => {
                self.control.write().unwrap().paused ^= true;
                false
            }
            KeyCode::Char('+') | KeyCode::Char('=') => {
                self.control.write().unwrap().step_interval(100);
                false
            }
            KeyCode::Char('-') => {
                self.control.write().unwrap().step_interval(-100);
                false
            }
            KeyCode::Char('1') => {
                self.boxes.engine ^= true;
                false
            }
            KeyCode::Char('2') => {
                self.boxes.kv ^= true;
                false
            }
            KeyCode::Char('3') => {
                self.boxes.traffic ^= true;
                false
            }
            KeyCode::Char('4') => {
                self.boxes.ranks ^= true;
                false
            }
            // RANKS navigation. Up/down walk the *display* order so the
            // highlight follows what the user sees, not raw dp_rank order.
            KeyCode::Up | KeyCode::Char('k') => {
                self.move_rank_sel(-1, n_ranks);
                false
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.move_rank_sel(1, n_ranks);
                false
            }
            KeyCode::Home => {
                self.rank_sel = 0;
                false
            }
            KeyCode::End => {
                self.rank_sel = table::clamp_sel(usize::MAX, n_ranks);
                false
            }
            KeyCode::Left => {
                self.sort_col = self.sort_col.prev();
                false
            }
            KeyCode::Right => {
                self.sort_col = self.sort_col.next();
                false
            }
            KeyCode::Char('r') => {
                self.sort_reverse ^= true;
                false
            }
            KeyCode::Enter => {
                if self.boxes.ranks {
                    self.detail_open = true;
                }
                false
            }
            KeyCode::Esc => {
                self.detail_open = false;
                false
            }
            _ => false,
        }
    }

    /// Step the selection ±1 through the current display order.
    fn move_rank_sel(&mut self, dy: i32, n_ranks: usize) {
        let order = {
            let snap = self.snapshot.read().unwrap();
            table::order(&snap, self.sort_col, self.sort_reverse)
        };
        self.rank_sel = table::clamp_sel(self.rank_sel, n_ranks);
        if order.is_empty() {
            return;
        }
        let pos = order.iter().position(|&i| i == self.rank_sel).unwrap_or(0);
        let next = if dy < 0 {
            pos.saturating_sub(1)
        } else {
            (pos + 1).min(order.len() - 1)
        };
        self.rank_sel = order[next];
    }

    /// Fatal-conditions gate: the poller parks the state machine in
    /// `MetricsDisabled`; the event loop turns that into an exit with a message.
    pub fn fatal_error(snap: &Arc<RwLock<Snapshot>>) -> Option<SgtopError> {
        match snap.read().unwrap().conn {
            Connection::MetricsDisabled => Some(SgtopError::MetricsDisabled),
            _ => None,
        }
    }

    pub fn draw(&mut self, f: &mut Frame) {
        let area = f.area();
        let (mw, mh) = layout::min_size(&self.boxes);
        if area.width < mw || area.height < mh {
            draw_toosmall(f, area, mw, mh, &self.theme);
            return;
        }
        self.reflow(area);

        let th = &self.theme;
        let bs = self.bs;
        let l = self.layout;

        // Header/footer chrome (btop's border-as-toolbar). Menu buttons, a
        // live interval stepper and the local clock ride the engine top
        // border; the hotkey hints ride the ranks bottom border. The clock
        // comes from the local system (chrono), redrawn every draw tick.
        let interval_ms = self.control.read().unwrap().interval_ms;
        let clock = chrono::Local::now().format("%H:%M:%S").to_string();
        let header_buttons = header_buttons(interval_ms, &clock, th);
        let footer_buttons = footer_buttons(th);

        let mut panes: Vec<(Rect, &'static str, &'static str)> = Vec::new();
        if self.boxes.engine {
            panes.push((l.engine, "cpu_box", "¹engine"));
        }
        if self.boxes.kv {
            panes.push((l.kv, "mem_box", "²token pool"));
        }
        if self.boxes.traffic {
            panes.push((l.traffic, "net_box", "³traffic"));
        }
        if self.boxes.ranks {
            panes.push((l.ranks, "proc_box", "⁴ranks"));
        }

        // btop draw order (calcSizes): cpu, mem, net, proc — later boxes'
        // borders overlay earlier ones on shared rows/cols, which is exactly
        // how the seams look in btop. No junction computation; drawing in
        // order *is* the idiom.
        let snap = self.snapshot.read().unwrap();
        for (r, key, title) in &panes {
            if r.width < 2 || r.height < 2 {
                continue;
            }
            // The engine box carries the header buttons; ranks the footer.
            // Offsets skip the `┐¹engine┌` title (~10 wide) / keep clear of the
            // `┘n/m└` page footer, and right-align the interval stepper.
            let top = if *title == "¹engine" {
                header_buttons.clone()
            } else {
                Vec::new()
            };
            let bottom = if *title == "⁴ranks" {
                footer_buttons.clone()
            } else {
                Vec::new()
            };
            let top = place_buttons(top, r.width, true);
            let bottom = place_buttons(bottom, r.width, false);
            Pane {
                rect: *r,
                line_color_key: key,
                title: Some(title),
                top_buttons: top,
                bottom_buttons: bottom,
            }
            .render(f.buffer_mut(), th, &bs);
        }

        // Panel bodies render into the interiors (frames already blanked them).
        let gui = self.gui;
        if self.boxes.engine && l.engine.height > 2 {
            panels::engine(&snap, th, gui, l.engine, f.buffer_mut());
        }
        if self.boxes.kv && l.kv.height > 2 {
            panels::kv(&snap, th, gui, l.kv, f.buffer_mut());
        }
        if self.boxes.traffic && l.traffic.height > 2 {
            panels::traffic(&snap, th, gui, l.traffic, f.buffer_mut());
        }
        if self.boxes.ranks && l.ranks.height > 2 {
            if self.detail_open {
                table::rank_detail(&snap, th, l.ranks, f.buffer_mut(), self.rank_sel);
            } else {
                panels::ranks(
                    &snap,
                    th,
                    gui,
                    l.ranks,
                    f.buffer_mut(),
                    self.rank_sel,
                    self.sort_col,
                    self.sort_reverse,
                );
            }
        }

        draw_connection(&snap, th, area, f);

        // Overlay on top of a still-live screen — the poller keeps writing
        // `snap` underneath; opening a menu never pauses it (btop's idiom).
        if let Some(which) = self.overlay {
            let live = {
                let c = self.control.read().unwrap();
                overlays::Live {
                    paused: c.paused,
                    interval_ms: c.interval_ms,
                }
            };
            overlays::render(which, &self.config, &snap, live, th, area, f.buffer_mut());
        }

        // btop paints the screen on `main_bg` rather than trusting the
        // terminal's ground — modern themes ship a coloured one. Post-pass so
        // it fills every cell that didn't set its own bg (selection bars,
        // overlays included), and nothing drawn earlier escapes it.
        let bg = th.c("main_bg");
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                let cell = &mut f.buffer_mut()[(x, y)];
                if matches!(cell.style().bg, None | Some(Color::Reset)) {
                    cell.set_style(Style::default().bg(bg));
                }
            }
        }
    }
}

/// Menu + interval-stepper buttons for the engine box's top border.
/// `button_spans` renders the first char in `hi_fg` (the hotkey), the rest in
/// title colour.
fn header_buttons(interval_ms: u64, clock: &str, th: &Theme) -> Vec<(Vec<Span<'static>>, u16)> {
    use frame::button_spans as b;
    vec![
        (b('?', "?help", th), 12),
        (b('o', "options", th), 19),
        (b('i', "info", th), 28),
        (
            vec![
                Span::styled(
                    "-",
                    Style::default()
                        .fg(th.c("hi_fg"))
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!(" {interval_ms}ms "),
                    Style::default().fg(th.c("title")),
                ),
                Span::styled(
                    "+",
                    Style::default()
                        .fg(th.c("hi_fg"))
                        .add_modifier(Modifier::BOLD),
                ),
            ],
            0,
        ),
        // Local clock, right-aligned just left of the interval stepper (the
        // right-align cursor in `place_buttons` chains them). Drawn on the
        // 250ms draw tick, independent of the poll interval.
        (
            vec![Span::styled(
                clock.to_string(),
                Style::default().fg(th.c("title")),
            )],
            0,
        ),
    ]
}

/// Hotkey hints on the ranks box's bottom border. The ↑/← hotkeys are
/// multi-byte, so they can't ride `button_spans` (it slices the label at
/// byte 1, which would land inside an arrow's UTF-8 sequence).
fn footer_buttons(th: &Theme) -> Vec<(Vec<Span<'static>>, u16)> {
    use frame::button_spans as b;
    let arrow = |key: &'static str, label: &'static str, off: u16| {
        (
            vec![
                Span::styled(
                    key,
                    Style::default()
                        .fg(th.c("hi_fg"))
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(label, Style::default().fg(th.c("title"))),
            ],
            off,
        )
    };
    vec![
        arrow("↑↓", "select", 8),
        arrow("←→", "sort", 18),
        (b('r', "r:rev", th), 26),
        (b('u', "u:pause", th), 33),
    ]
}

/// Assign concrete x-offsets to (spans, suggested-offset) pairs: entries with
/// offset 0 right-align against the box's right edge (a cursor walks left as
/// more of them stack up, so consecutive right-aligned buttons chain without
/// overlap); the rest stay put, clamped so they never overflow.
fn place_buttons(
    items: Vec<(Vec<Span<'static>>, u16)>,
    box_width: u16,
    top: bool,
) -> Vec<(Vec<Span<'static>>, u16)> {
    let gap = if top { 2 } else { 1 };
    let mut out: Vec<(Vec<Span<'static>>, u16)> = Vec::new();
    let mut right_edge = box_width.saturating_sub(2);
    for (spans, mut off) in items {
        let w: usize = spans
            .iter()
            .map(|s| unicode_width::UnicodeWidthStr::width(s.content.as_ref()))
            .sum::<usize>()
            + 2; // the two ┐┌ corner chars
        if off == 0 {
            right_edge = right_edge.saturating_sub(gap);
            off = right_edge.saturating_sub(w as u16);
            right_edge = off;
        } else {
            let last = box_width
                .saturating_sub(gap)
                .saturating_sub(w as u16)
                .saturating_sub(2);
            off = off.min(last);
        }
        out.push((spans, off));
    }
    out
}

/// Liveness messaging: a small centered notice on the engine box's top border
/// zone while Pending, and a footer-adjacent line on Lost/Degraded — values
/// stay frozen and dimmed (the panels read `conn.dimmed()`), never zero-filled.
fn draw_connection(snap: &Snapshot, th: &Theme, area: Rect, f: &mut Frame) {
    let line = match &snap.conn {
        Connection::Pending => Some(Line::from(Span::styled(
            "connecting…",
            Style::default()
                .fg(th.c("graph_text"))
                .add_modifier(Modifier::ITALIC),
        ))),
        Connection::Degraded {
            last_err,
            consecutive,
            ..
        } => Some(Line::from(Span::styled(
            format!("⚠ degraded ({consecutive}×): {last_err} — holding last values"),
            Style::default().fg(th.c("hi_fg")),
        ))),
        Connection::Lost { last_err, .. } => Some(Line::from(Span::styled(
            format!("⚠ connection lost: {last_err} — values frozen"),
            Style::default()
                .fg(th.c("hi_fg"))
                .add_modifier(Modifier::BOLD),
        ))),
        _ => None,
    };
    if let Some(line) = line {
        // Bottom margin row of the whole grid: last row of the area.
        let y = area.height.saturating_sub(1);
        frame::centered(f.buffer_mut(), area, y, &line);
    } else if snap.metrics_truncated {
        let line = Line::from(Span::styled(
            "⚠ metrics body truncated at --max-metrics-bytes — absent values may be cut, not missing",
            Style::default().fg(th.c("hi_fg")),
        ));
        let y = area.height.saturating_sub(1);
        frame::centered(f.buffer_mut(), area, y, &line);
    }
}

fn draw_toosmall(f: &mut Frame, area: Rect, mw: u16, mh: u16, th: &Theme) {
    use ratatui::style::{Color, Modifier, Style};
    use ratatui::text::{Line, Span};

    let ok = |b: bool| if b { "✓" } else { "✗" };
    let color = |b: bool| if b { th.c("free_mid") } else { Color::Red };
    let w = area.width;
    let h = area.height;

    let header = Line::from(Span::styled(
        "Terminal size too small",
        Style::default()
            .fg(th.c("hi_fg"))
            .add_modifier(Modifier::BOLD),
    ));
    let width_line = Line::from(vec![
        Span::styled("Width  ", Style::default().fg(th.c("title"))),
        Span::styled(format!("{w}"), Style::default().fg(color(w >= mw))),
        Span::styled(
            format!("  (min {mw}) {}", ok(w >= mw)),
            Style::default().fg(th.c("graph_text")),
        ),
    ]);
    let height_line = Line::from(vec![
        Span::styled("Height ", Style::default().fg(th.c("title"))),
        Span::styled(format!("{h}"), Style::default().fg(color(h >= mh))),
        Span::styled(
            format!("  (min {mh}) {}", ok(h >= mh)),
            Style::default().fg(th.c("graph_text")),
        ),
    ]);
    let hints = Line::from(Span::styled(
        "q: quit    1-4: toggle boxes",
        Style::default().fg(th.c("inactive_fg")),
    ));

    let block = [header, width_line, height_line, hints];
    let block_h = block.len() as u16;
    let y0 = area.height.saturating_sub(block_h) / 2;
    for (i, line) in block.iter().enumerate() {
        let y = y0 + i as u16;
        if y >= area.bottom() {
            break;
        }
        frame::centered(f.buffer_mut(), area, y, line);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};

    fn app_at(w: u16, h: u16) -> (App, Vec<Vec<String>>) {
        let mut app = App::new(
            Config::default(),
            Arc::new(RwLock::new(Snapshot::default())),
        );
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| app.draw(f)).unwrap();
        let buf = term.backend().buffer().clone();
        let grid = (0..h)
            .map(|y| {
                (0..w)
                    .map(|x| {
                        buf.cell((x, y))
                            .map(|c| c.symbol().to_string())
                            .unwrap_or_default()
                    })
                    .collect()
            })
            .collect();
        (app, grid)
    }

    fn render_at(w: u16, h: u16) -> Vec<Vec<String>> {
        app_at(w, h).1
    }

    #[test]
    fn clock_right_aligns_left_of_the_interval_stepper() {
        let th = Theme::builtin("Default", crate::ui::theme::ColorMode::TrueColor);
        let items = header_buttons(1000, "12:34:56", &th);
        let placed = place_buttons(items, 120, true);
        // The last two entries (both offset-0) must not overlap: the clock's
        // right edge must end left of the stepper's left edge.
        let stepper = &placed[placed.len() - 2];
        let clock = placed.last().unwrap();
        assert!(
            clock.1 < stepper.1,
            "clock {} < stepper {}",
            clock.1,
            stepper.1
        );
        let text: String = clock.0.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "12:34:56");
    }

    #[test]
    fn theme_main_bg_paints_the_ground() {
        // Modern themes ship an opaque main_bg; the draw post-pass must put it
        // under every cell that didn't choose its own background.
        let cfg = Config {
            theme: "tokyo-night".into(),
            color_mode: "truecolor".into(),
            ..Config::default()
        };
        let mut app = App::new(cfg, Arc::new(RwLock::new(Snapshot::default())));
        let mut term = Terminal::new(TestBackend::new(120, 40)).unwrap();
        term.draw(|f| app.draw(f)).unwrap();
        let buf = term.backend().buffer().clone();
        // A blank interior cell (kv box interior, far from any content).
        let bg = buf
            .cell((30, 25))
            .expect("cell")
            .style()
            .bg
            .expect("main_bg painted");
        assert_eq!(bg, app.theme.c("main_bg"));
    }

    fn cell(buf: &[Vec<String>], x: usize, y: usize) -> &str {
        let row = &buf[y];
        assert!(x < row.len(), "cell({x},{y}) out of 120-wide row");
        &row[x]
    }

    /// Flatten a grid to one string for substring content assertions.
    fn all_text(buf: &[Vec<String>]) -> String {
        buf.iter()
            .flat_map(|r| r.iter())
            .map(String::as_str)
            .collect()
    }

    #[test]
    fn four_boxes_geometry_at_120x40() {
        let b = render_at(120, 40);
        // engine: full width, ceil(40*.32)=13 rows → corners at (0,0)..(119,12)
        assert_eq!(cell(&b, 0, 0), "╭");
        assert_eq!(cell(&b, 119, 0), "╮");
        assert_eq!(cell(&b, 0, 12), "╰");
        assert_eq!(cell(&b, 119, 12), "╯");
        // kv: rows 13..28 (16 tall), cols 0..53
        assert_eq!(cell(&b, 0, 13), "╭");
        assert_eq!(cell(&b, 53, 28), "╯");
        // traffic: rows 29..39
        assert_eq!(cell(&b, 0, 29), "╭");
        assert_eq!(cell(&b, 53, 39), "╯");
        // ranks: x 54..119, rows 13..39
        assert_eq!(cell(&b, 54, 13), "╭");
        assert_eq!(cell(&b, 119, 39), "╯");
    }

    #[test]
    fn title_breaks_top_border() {
        let b = render_at(120, 40);
        // engine title: ┐ ¹ e n g i n e ┌ starting at x=2
        assert_eq!(cell(&b, 2, 0), "┐");
        assert_eq!(cell(&b, 3, 0), "¹");
        assert_eq!(cell(&b, 4, 0), "e");
        assert_eq!(cell(&b, 10, 0), "┌");
        // ranks box title break
        assert_eq!(cell(&b, 56, 13), "┐");
        assert_eq!(cell(&b, 57, 13), "⁴");
    }

    #[test]
    fn no_gaps_in_border_grid() {
        // Every seam row must be fully covered by borders (no blank cells
        // inside the tiling), i.e. rows 12 and 13 are continuous lines.
        let b = render_at(120, 40);
        for x in 1..119 {
            assert!(
                !cell(&b, x, 12).trim().is_empty(),
                "engine bottom row blank at x={x}"
            );
        }
    }

    #[test]
    fn toosmall_screen_replaces_layout() {
        let b = render_at(60, 10);
        let text: String = b.iter().flatten().map(|s| s.as_str()).collect();
        assert!(text.contains("Terminal size too small"), "{text}");
        assert!(text.contains("60"));
    }

    #[test]
    fn seams_are_btop_double_lines() {
        // btop's boxes are independent complete frames stacked with zero gap
        // (verified against calcSizes: mem.y = Cpu.height + 1 in 1-based coords).
        // So the cpu/mem seam is TWO adjacent rows: cpu's ╰╯ at y=12, mem's ╭╮
        // at y=13 — not merged, no T-junctions.
        let b = render_at(120, 40);
        assert_eq!(cell(&b, 0, 12), "╰"); // engine bottom-left
        assert_eq!(cell(&b, 0, 13), "╭"); // kv top-left, next row
        assert_eq!(cell(&b, 53, 28), "╯"); // kv bottom-right
        assert_eq!(cell(&b, 0, 29), "╭"); // traffic top-left
        // Horizontal seam: kv right border col 53, ranks left border col 54.
        assert_eq!(cell(&b, 53, 20), "│");
        assert_eq!(cell(&b, 54, 20), "│");
    }

    fn render_snapshot(snap: Arc<RwLock<Snapshot>>) -> Vec<Vec<String>> {
        let mut app = App::new(Config::default(), snap);
        let mut term = Terminal::new(TestBackend::new(120, 40)).unwrap();
        term.draw(|f| app.draw(f)).unwrap();
        let buf = term.backend().buffer().clone();
        (0..40u16)
            .map(|y| {
                (0..120u16)
                    .map(|x| {
                        buf.cell((x, y))
                            .map(|c| c.symbol().to_string())
                            .unwrap_or_default()
                    })
                    .collect()
            })
            .collect()
    }

    #[test]
    fn pending_conn_shows_connecting() {
        let text = all_text(&render_at(120, 40));
        assert!(text.contains("connecting"), "{text}");
    }

    #[test]
    fn lost_conn_freezes_and_warns() {
        let snap = Arc::new(RwLock::new(Snapshot::default()));
        {
            let mut s = snap.write().unwrap();
            s.conn = Connection::Lost {
                since: std::time::Instant::now(),
                last_err: "transport: refused".into(),
            };
            s.engine.running_reqs = 42; // frozen value must stay visible
        }
        let text = all_text(&render_snapshot(snap));
        assert!(text.contains("connection lost"), "{text}");
        assert!(text.contains("42"), "frozen value must still render");
    }

    #[test]
    fn truncation_warns_on_the_footer_row() {
        let snap = Arc::new(RwLock::new(Snapshot::default()));
        {
            let mut s = snap.write().unwrap();
            // A live-but-truncated scrape — the state the note is for.
            s.conn = Connection::Live {
                rtt: std::time::Duration::from_millis(5),
                metrics_ok: true,
            };
            s.metrics_truncated = true;
        }
        let text = all_text(&render_snapshot(snap));
        assert!(text.contains("truncated"), "{text}");
    }

    #[test]
    fn conn_warning_outranks_truncation_note() {
        let snap = Arc::new(RwLock::new(Snapshot::default()));
        {
            let mut s = snap.write().unwrap();
            s.metrics_truncated = true;
            s.conn = Connection::Lost {
                since: std::time::Instant::now(),
                last_err: "x".into(),
            };
        }
        let text = all_text(&render_snapshot(snap));
        assert!(text.contains("connection lost"), "{text}");
        assert!(!text.contains("truncated"), "one message per row");
    }

    #[test]
    fn rank_rows_render_when_present() {
        use crate::model::snapshot::RankRow;
        let snap = Arc::new(RwLock::new(Snapshot::default()));
        {
            let mut s = snap.write().unwrap();
            s.ranks = vec![RankRow {
                running: 9,
                ..RankRow::default()
            }];
        }
        let text = all_text(&render_snapshot(snap));
        assert!(text.contains("rank"), "header");
        assert!(text.contains("agg"), "aggregate row label");
    }

    fn key(code: crossterm::event::KeyCode) -> crossterm::event::KeyEvent {
        // KeyEvent::new defaults to KeyEventKind::Press, which is what on_key wants.
        crossterm::event::KeyEvent::new(code, crossterm::event::KeyModifiers::NONE)
    }

    fn app_with_ranks(n: usize) -> App {
        use crate::model::snapshot::RankRow;
        let snap = Arc::new(RwLock::new(Snapshot::default()));
        snap.write().unwrap().ranks = (0..n)
            .map(|i| RankRow {
                dp_rank: i as u32,
                running: (n - i) as u64,
                ..RankRow::default()
            })
            .collect();
        App::new(Config::default(), snap)
    }

    #[test]
    fn arrow_keys_walk_selection_and_sort() {
        let mut app = app_with_ranks(4);
        assert_eq!(app.rank_sel, 0);
        app.on_key(key(crossterm::event::KeyCode::Down));
        assert_eq!(app.rank_sel, 1);
        app.on_key(key(crossterm::event::KeyCode::Up));
        assert_eq!(app.rank_sel, 0, "up from agg stays clamped at agg");
        app.on_key(key(crossterm::event::KeyCode::End));
        assert_eq!(app.rank_sel, 3);
        app.on_key(key(crossterm::event::KeyCode::Right));
        assert_eq!(app.sort_col, SortCol::Run);
        app.on_key(key(crossterm::event::KeyCode::Left));
        assert_eq!(app.sort_col, SortCol::Rank);
        app.on_key(key(crossterm::event::KeyCode::Char('r')));
        assert!(app.sort_reverse);
    }

    #[test]
    fn selection_follows_display_order_after_sorting() {
        // Sort by run desc: display = agg, rank1(biggest run), …; Down from
        // agg must land on rank 1's row, not the raw next index, when sorted.
        let mut app = app_with_ranks(3); // running: agg=3, r1=2, r2=1
        app.on_key(key(crossterm::event::KeyCode::Right)); // sort by Run asc
        app.on_key(key(crossterm::event::KeyCode::Down));
        // Ascending run: agg pinned, then rank2 (run 1), then rank1.
        assert_eq!(app.rank_sel, 2, "down follows display order");
    }

    #[test]
    fn enter_opens_detail_esc_closes() {
        let mut app = app_with_ranks(2);
        assert!(!app.detail_open);
        app.on_key(key(crossterm::event::KeyCode::Enter));
        assert!(app.detail_open);
        app.on_key(key(crossterm::event::KeyCode::Esc));
        assert!(!app.detail_open);
    }

    #[test]
    fn u_toggles_pause_shared_with_poller() {
        let mut app = app_with_ranks(1);
        assert!(!app.control.read().unwrap().paused);
        app.on_key(key(crossterm::event::KeyCode::Char('u')));
        assert!(app.control.read().unwrap().paused);
        app.on_key(key(crossterm::event::KeyCode::Char('u')));
        assert!(!app.control.read().unwrap().paused);
    }

    #[test]
    fn plus_minus_step_the_live_interval() {
        let mut app = app_with_ranks(1);
        let start = app.control.read().unwrap().interval_ms;
        app.on_key(key(crossterm::event::KeyCode::Char('+')));
        assert_eq!(app.control.read().unwrap().interval_ms, start + 100);
        app.on_key(key(crossterm::event::KeyCode::Char('-')));
        app.on_key(key(crossterm::event::KeyCode::Char('-')));
        assert_eq!(
            app.control.read().unwrap().interval_ms,
            start.saturating_sub(100)
        );
    }

    #[test]
    fn interval_clamps_to_bounds() {
        let mut app = app_with_ranks(1);
        for _ in 0..200 {
            app.on_key(key(crossterm::event::KeyCode::Char('+')));
        }
        assert_eq!(app.control.read().unwrap().interval_ms, 10_000);
        for _ in 0..200 {
            app.on_key(key(crossterm::event::KeyCode::Char('-')));
        }
        assert_eq!(app.control.read().unwrap().interval_ms, 100);
    }

    #[test]
    fn overlay_opens_then_any_key_closes_and_is_consumed() {
        let mut app = app_with_ranks(1);
        app.on_key(key(crossterm::event::KeyCode::Char('?')));
        assert_eq!(app.overlay, Some(overlays::Overlay::Help));
        // While open, `q` closes the menu instead of quitting.
        assert!(!app.on_key(key(crossterm::event::KeyCode::Char('q'))));
        assert_eq!(app.overlay, None);
        // Closed again, `q` quits.
        assert!(app.on_key(key(crossterm::event::KeyCode::Char('q'))));
    }

    #[test]
    fn overlay_renders_over_the_screen() {
        let mut app = app_with_ranks(1);
        app.overlay = Some(overlays::Overlay::Help);
        let mut term = Terminal::new(TestBackend::new(120, 40)).unwrap();
        term.draw(|f| app.draw(f)).unwrap();
        let buf = term.backend().buffer().clone();
        let b = &buf;
        let text: String = (0..40u16)
            .flat_map(|y| {
                (0..120u16).map(move |x| {
                    b.cell((x, y))
                        .map(|c| c.symbol().to_string())
                        .unwrap_or_default()
                })
            })
            .collect();
        assert!(
            text.contains("quit"),
            "help text drawn over the layout: {text}"
        );
    }

    #[test]
    fn detail_replaces_ranks_interior_keeps_other_panels() {
        use crate::model::snapshot::RankRow;
        let snap = Arc::new(RwLock::new(Snapshot::default()));
        snap.write().unwrap().ranks = vec![
            RankRow {
                running: 5,
                spec_accept_length: Some(2.5),
                ..RankRow::default()
            },
            RankRow {
                dp_rank: 1,
                running: 5,
                spec_accept_length: Some(2.5),
                ..RankRow::default()
            },
        ];
        let mut app = App::new(Config::default(), snap);
        app.detail_open = true;
        app.rank_sel = 1;
        let mut term = Terminal::new(TestBackend::new(120, 40)).unwrap();
        term.draw(|f| app.draw(f)).unwrap();
        let buf = term.backend().buffer().clone();
        let b = &buf;
        let text: String = (0..40u16)
            .flat_map(|y| {
                (0..120u16).map(move |x| {
                    b.cell((x, y))
                        .map(|c| c.symbol().to_string())
                        .unwrap_or_default()
                })
            })
            .collect();
        assert!(text.contains("rank 1"), "detail names the row: {text}");
        assert!(text.contains("tok/fwd"), "spec detail row: {text}");
        assert!(text.contains("esc back"), "back hint: {text}");
    }
}
