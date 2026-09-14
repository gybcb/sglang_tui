use ratatui::{buffer::Buffer, layout::Rect, style::Style};

use crate::ui::theme::Theme;

/// Half-block heights, bottom-fraction 1/8 .. 8/8.
const BLOCKS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

/// Modern solid-area graph: one sample per column, painted bottom-up with
/// full `█` cells and one eighth-resolution partial block on the boundary
/// row. No braille dot noise — empty cells keep whatever background the
/// panel left (transparent). Colour grades per column along `family`'s
/// ramp at the sample's own value.
///
/// Honest-by-construction like the braille graph: right-aligned (newest
/// sample in the last column), short history draws short, never zero-padded.
/// `peak_label` (when the caller wants it) prints `↑ <human peak>` at the
/// graph's top-left in `graph_text`.
pub fn draw(buf: &mut Buffer, area: Rect, series: &[f64], th: &Theme, family: &str) {
    let (w, h) = (area.width as usize, area.height as usize);
    if w == 0 || h == 0 || series.is_empty() {
        return;
    }

    let vis = &series[series.len().saturating_sub(w)..];
    let pad = w - vis.len();
    let max = vis.iter().cloned().fold(0.0f64, f64::max).max(f64::EPSILON);

    for cx in pad..w {
        let v = vis[cx - pad].max(0.0);
        let eighths = (((v / max) * (h * 8) as f64).round() as usize).min(h * 8);
        if eighths == 0 {
            continue; // a true-zero sample on a graph with real peaks stays blank
        }
        let pct = (((v / max) * 100.0).round() as usize).min(100);
        let style = Style::default().fg(th.grad(family, pct));
        let mut left = eighths;
        for cy in (0..h).rev() {
            let x = area.x + cx as u16;
            let y = area.y + cy as u16;
            let cell = &mut buf[(x, y)];
            if left >= 8 {
                cell.set_symbol(&BLOCKS[7].to_string());
                cell.set_style(style);
                left -= 8;
            } else if left > 0 {
                cell.set_symbol(&BLOCKS[left - 1].to_string());
                cell.set_style(style);
                left = 0;
            } else {
                break;
            }
        }
    }
}

/// A one-line sparkline (single character per sample) — the RANKS mini-graph.
/// Same right-align/no-zero-pad honesty as [`draw`], sized to one text row.
pub fn sparkline(buf: &mut Buffer, area: Rect, series: &[f64], th: &Theme, family: &str) {
    if area.width == 0 || area.height == 0 || series.is_empty() {
        return;
    }
    let w = area.width as usize;
    let vis = &series[series.len().saturating_sub(w)..];
    let start = area.x + (w - vis.len()) as u16;
    let max = vis.iter().cloned().fold(0.0f64, f64::max).max(f64::EPSILON);
    for (i, &v) in vis.iter().enumerate() {
        let eighths = (((v.max(0.0) / max) * 8.0).round() as usize).clamp(1, 8);
        let pct = (((v.max(0.0) / max) * 100.0).round() as usize).min(100);
        let cell = &mut buf[(start + i as u16, area.y)];
        cell.set_symbol(&BLOCKS[eighths - 1].to_string());
        cell.set_style(Style::default().fg(th.grad(family, pct)));
    }
}

/// Peak annotation drawn inside a graph's top-left corner: `↑ 1.2k`.
/// Drawn *after* [`draw`] so the text overwrites the (rarely tall) corner.
pub fn peak_label(
    buf: &mut Buffer,
    area: Rect,
    series: &[f64],
    th: &Theme,
    fmt: impl Fn(f64) -> String,
) {
    if area.width < 8 || area.height == 0 || series.is_empty() {
        return;
    }
    let peak = series.iter().cloned().fold(0.0f64, f64::max);
    let text = format!("↑ {}", fmt(peak));
    let style = Style::default().fg(th.c("graph_text"));
    put_text(buf, area, &text, style);
}

/// Write single-width text onto a graph's first row, clipped at the area's
/// right edge (peak annotation).
pub fn put_text(buf: &mut Buffer, area: Rect, text: &str, style: Style) {
    if area.height == 0 {
        return;
    }
    for (i, ch) in text.chars().enumerate() {
        let x = area.x + i as u16;
        if x >= area.right() {
            return;
        }
        let cell = &mut buf[(x, area.y)];
        cell.set_symbol(&ch.to_string());
        cell.set_style(style);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::theme::{ColorMode, Theme};
    use ratatui::layout::Rect;

    fn th() -> Theme {
        Theme::builtin("Default", ColorMode::TrueColor)
    }

    #[test]
    fn tall_column_fills_bottom_up() {
        let mut b = Buffer::empty(Rect::new(0, 0, 2, 3));
        draw(&mut b, Rect::new(0, 0, 2, 3), &[10.0, 10.0], &th(), "cpu");
        // Full-scale column: all three rows are full blocks.
        assert_eq!(b[(1, 2)].symbol(), "█");
        assert_eq!(b[(1, 0)].symbol(), "█");
    }

    #[test]
    fn partial_column_gets_half_block() {
        let mut b = Buffer::empty(Rect::new(0, 0, 2, 1));
        draw(&mut b, Rect::new(0, 0, 2, 1), &[8.0, 4.0], &th(), "cpu");
        // Peak column is full; the half-of-peak column gets the 4/8 block.
        assert_eq!(b[(0, 0)].symbol(), "█");
        assert_eq!(b[(1, 0)].symbol(), "▄");
    }

    #[test]
    fn short_history_right_aligns_and_never_pads() {
        let mut b = Buffer::empty(Rect::new(0, 0, 5, 1));
        draw(&mut b, Rect::new(0, 0, 5, 1), &[1.0], &th(), "cpu");
        assert_eq!(b[(0, 0)].symbol(), " ");
        assert_eq!(b[(4, 0)].symbol(), "█");
    }

    #[test]
    fn sparkline_one_char_per_sample() {
        let mut b = Buffer::empty(Rect::new(0, 0, 3, 1));
        sparkline(
            &mut b,
            Rect::new(0, 0, 3, 1),
            &[1.0, 0.0, 0.5],
            &th(),
            "cpu",
        );
        assert_eq!(b[(0, 0)].symbol(), "█");
        // A zero sample still gets the smallest block — the line stays readable.
        assert_eq!(b[(1, 0)].symbol(), "▁");
        assert_eq!(b[(2, 0)].symbol(), "▄");
    }

    #[test]
    fn peak_label_prints_at_top_left() {
        let mut b = Buffer::empty(Rect::new(0, 0, 20, 2));
        draw(&mut b, Rect::new(0, 0, 20, 2), &[500.0], &th(), "cpu");
        peak_label(&mut b, Rect::new(0, 0, 20, 2), &[500.0], &th(), |v| {
            format!("{v:.0}")
        });
        let row: String = (0..6).map(|x| b[(x, 0)].symbol().to_string()).collect();
        assert_eq!(row, "↑ 500 ");
    }
}
