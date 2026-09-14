use ratatui::{style::Style, text::Span};

use crate::ui::theme::Theme;

pub const BLOCK: char = '■';

/// A btop-style meter run: `width` cells of `■`, each cell taking the next
/// colour from the family's 101-entry gradient (btop fills cell-by-cell and
/// reads its colour from the precomputed table — no interpolation at draw
/// time). Unfilled cells sit in `meter_bg`. `frac` is 0..=1.
///
/// btop also draws a partial glyph (◧/◩/◪) for the in-between cell; ratatui
/// cells can't hold two colours, so the partial cell is rendered filled —
/// the visible error is sub-cell.
pub fn meter_spans(frac: f64, width: usize, family: &str, th: &Theme) -> Vec<Span<'static>> {
    let frac = frac.clamp(0.0, 1.0);
    let filled = (frac * width as f64).round() as usize;
    let mut spans = Vec::with_capacity(width);
    for i in 0..width {
        let ch = BLOCK.to_string();
        let style = if i < filled {
            // Walk the gradient across the *filled* extent, so a half-full
            // 40-wide meter spans start→mid, not start→end-of-table.
            let pct = if filled <= 1 {
                0
            } else {
                (i * 100) / (filled - 1)
            };
            Style::default().fg(th.grad(family, pct))
        } else {
            Style::default().fg(th.c("meter_bg"))
        };
        spans.push(Span::styled(ch, style));
    }
    spans
}

/// A meter run boxed in thin vertical caps `▏ … ▕` — the modern KV-bar
/// treatment. `width` is the run length *between* the caps; the returned
/// vec is `width + 2` long. Caps take `meter_bg` so the box reads as one
/// unit whether or not the bar is full.
pub fn meter_bar(frac: f64, width: usize, family: &str, th: &Theme) -> Vec<Span<'static>> {
    let cap = Style::default().fg(th.c("meter_bg"));
    let mut spans = Vec::with_capacity(width + 2);
    spans.push(Span::styled("▏", cap));
    spans.extend(meter_spans(frac, width, family, th));
    spans.push(Span::styled("▕", cap));
    spans
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::theme::{ColorMode, Theme};

    #[test]
    fn width_is_exact_and_cells_graded() {
        let th = Theme::builtin("Default", ColorMode::TrueColor);
        let full = meter_spans(1.0, 10, "cpu", &th);
        assert_eq!(full.len(), 10);
        let half = meter_spans(0.0, 10, "cpu", &th);
        assert_eq!(half.len(), 10);
        // All-empty: every cell meter_bg.
        assert!(half.iter().all(|s| s.style.fg == Some(th.c("meter_bg"))));
        // Full: first cell = gradient start, last = gradient end.
        let full = meter_spans(1.0, 10, "cpu", &th);
        assert_eq!(full[0].style.fg, Some(th.grad("cpu", 0)));
        assert_eq!(full[9].style.fg, Some(th.grad("cpu", 100)));
    }

    #[test]
    fn clamps_out_of_range() {
        let th = Theme::builtin("Default", ColorMode::TrueColor);
        assert_eq!(meter_spans(-1.0, 5, "cpu", &th).len(), 5);
        assert_eq!(meter_spans(3.0, 5, "cpu", &th).len(), 5);
    }

    #[test]
    fn meter_bar_caps_both_ends() {
        let th = Theme::builtin("Default", ColorMode::TrueColor);
        let bar = meter_bar(0.5, 10, "cpu", &th);
        assert_eq!(bar.len(), 12, "run + two caps");
        assert_eq!(bar[0].content, "▏");
        assert_eq!(bar[11].content, "▕");
        assert_eq!(bar[0].style.fg, Some(th.c("meter_bg")));
        assert_eq!(bar[11].style.fg, Some(th.c("meter_bg")));
        // Interior is the plain run.
        assert_eq!(bar[1].content, BLOCK.to_string());
    }
}
