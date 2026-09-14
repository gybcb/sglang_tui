use ratatui::{buffer::Buffer, layout::Rect, style::Style};

use crate::ui::theme::Theme;

/// Braille dot bit layout (U+2800 base), left column top→bottom = bits
/// 0,1,2,6 and right column = 3,4,5,7.
const LEFT: [u16; 4] = [0b0000_0001, 0b0000_0010, 0b0000_0100, 0b0100_0000];
const RIGHT: [u16; 4] = [0b0000_1000, 0b0001_0000, 0b0010_0000, 0b1000_0000];
const BASE: u16 = 0x2800;

/// btop's dotted placeholder inside an empty graph region.
const BG: char = '⣤';

/// Render `series` (oldest→newest) into `area` as a btop-style braille area
/// graph: 2 samples per cell, filled bottom-up to each sample's value, each
/// cell coloured from `family`'s gradient at its own value (low→green,
/// high→red, per btop's `Default_theme` CPU ramp).
///
/// Honest-by-construction:
/// - Right-aligned: newest sample always in the last cell; a short history
///   draws a short graph, never zero-padded.
/// - `no_zero` (btop's same-named option): a flat-zero series still paints the
///   bottom dot row instead of disappearing into the background.
/// - Scale is `autoscale_max` when `Some`, else the window max (peak-relative).
pub fn draw(
    buf: &mut Buffer,
    area: Rect,
    series: &[f64],
    th: &Theme,
    family: &str,
    autoscale_max: Option<f64>,
    no_zero: bool,
) {
    let (w, h) = (area.width as usize, area.height as usize);
    if w == 0 || h == 0 {
        return;
    }

    for y in 0..h {
        for x in 0..w {
            let c = &mut buf[(area.x + x as u16, area.y + y as u16)];
            c.set_symbol(&BG.to_string());
            c.set_style(Style::default().fg(th.c("div_line")));
        }
    }
    if series.is_empty() {
        return;
    }

    let cols = w * 2; // one sample per braille sub-column
    let rows = h * 4; // dot rows, measured bottom-up
    let vis = &series[series.len().saturating_sub(cols)..];
    let pad = cols - vis.len(); // left padding (history shorter than width)

    let scale = autoscale_max
        .unwrap_or_else(|| vis.iter().cloned().fold(0.0f64, f64::max))
        .max(f64::EPSILON);

    for cx in 0..w {
        // Height (in dot rows, 0..=rows) of each of this cell's two columns.
        let mut hgt = [0usize; 2];
        let mut peak = 0.0f64;
        let mut painted = false;
        for (j, hh) in hgt.iter_mut().enumerate() {
            let si = cx * 2 + j;
            if si < pad {
                continue; // left padding — this column has no sample
            }
            let v = vis[si - pad].max(0.0);
            let mut fill = ((v / scale) * rows as f64).round() as usize;
            if no_zero && fill == 0 {
                fill = 1;
            }
            *hh = fill.min(rows);
            painted = true;
            peak = peak.max(v);
        }
        if !painted {
            continue;
        }

        let pct = (((peak / scale) * 100.0).round() as usize).min(100);
        let style = Style::default().fg(th.grad(family, pct));

        for dy in 0..h {
            // `dy` is a char row from the top; within it, dots k=0..3 run
            // top→bottom (braille bits LEFT/RIGHT are stored in that order).
            let mut bits = 0u16;
            for k in 0..4 {
                // 1-based level of this dot measured from the graph bottom.
                let from_bottom = rows - dy * 4 - k;
                if hgt[0] >= from_bottom {
                    bits |= LEFT[k];
                }
                if hgt[1] >= from_bottom {
                    bits |= RIGHT[k];
                }
            }
            if bits == 0 {
                continue;
            }
            let glyph = char::from_u32((BASE + bits) as u32).unwrap_or(' ');
            let c = &mut buf[(area.x + cx as u16, area.y + dy as u16)];
            c.set_symbol(&glyph.to_string());
            c.set_style(style);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::theme::{ColorMode, Theme};
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;

    fn buf(w: u16, h: u16) -> Buffer {
        Buffer::empty(Rect::new(0, 0, w, h))
    }

    fn glyph(b: &Buffer, x: u16, y: u16) -> char {
        b[(x, y)].symbol().chars().next().unwrap_or(' ')
    }

    #[test]
    fn empty_series_leaves_only_background() {
        let th = Theme::builtin("Default", ColorMode::TrueColor);
        let mut b = buf(4, 3);
        draw(&mut b, Rect::new(0, 0, 4, 3), &[], &th, "cpu", None, false);
        assert_eq!(glyph(&b, 0, 0), BG);
        assert_eq!(glyph(&b, 3, 2), BG);
    }

    #[test]
    fn right_aligns_newest_sample() {
        // 10-wide graph = 20 columns; a 3-sample series occupies the 3
        // rightmost columns → cells 8 and 9 painted, cell 0 stays background.
        let th = Theme::builtin("Default", ColorMode::TrueColor);
        let mut b = buf(10, 2);
        draw(
            &mut b,
            Rect::new(0, 0, 10, 2),
            &[1.0, 1.0, 1.0],
            &th,
            "cpu",
            None,
            false,
        );
        assert_eq!(glyph(&b, 0, 0), BG, "left padding stays background");
        assert_ne!(glyph(&b, 9, 1), BG, "newest cell painted");
    }

    #[test]
    fn flat_zero_with_no_zero_paints_bottom_row() {
        let th = Theme::builtin("Default", ColorMode::TrueColor);
        let mut b = buf(3, 2);
        draw(
            &mut b,
            Rect::new(0, 0, 3, 2),
            &[0.0, 0.0],
            &th,
            "cpu",
            None,
            true,
        );
        // Bottom char row (y=1) must show a braille glyph, not background.
        let g = glyph(&b, 2, 1);
        assert_ne!(g, BG, "no_zero paints the baseline dot");
        assert!(
            ('\u{2800}'..='\u{28ff}').contains(&g),
            "a braille char: {g}"
        );
    }

    #[test]
    fn full_scale_reaches_top_row() {
        let th = Theme::builtin("Default", ColorMode::TrueColor);
        let mut b = buf(2, 2);
        // Value == autoscale max → fills every dot row, including the top.
        // Right-aligned: the 2 samples land in the rightmost cell (x=1).
        draw(
            &mut b,
            Rect::new(0, 0, 2, 2),
            &[50.0, 50.0],
            &th,
            "cpu",
            Some(50.0),
            false,
        );
        let g = glyph(&b, 1, 0);
        assert!(('\u{2800}'..='\u{28ff}').contains(&g), "top painted: {g}");
        assert_ne!(g, BG);
    }

    #[test]
    fn colour_scales_with_value() {
        let th = Theme::builtin("Default", ColorMode::TrueColor);
        // Fixed scale 100: a low sample → green end, a full sample → red end.
        let mut b = buf(2, 2);
        draw(
            &mut b,
            Rect::new(0, 0, 2, 2),
            &[0.0, 0.0],
            &th,
            "cpu",
            Some(100.0),
            true,
        );
        let low = b[(1, 1)].style().fg;
        let mut b2 = buf(2, 2);
        draw(
            &mut b2,
            Rect::new(0, 0, 2, 2),
            &[100.0, 100.0],
            &th,
            "cpu",
            Some(100.0),
            false,
        );
        let high = b2[(1, 1)].style().fg;
        assert_eq!(low, Some(th.grad("cpu", 0)), "low → gradient start");
        assert_eq!(high, Some(th.grad("cpu", 100)), "high → gradient end");
    }
}
