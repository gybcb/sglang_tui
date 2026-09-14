use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
};
use unicode_width::UnicodeWidthStr;

use crate::ui::theme::Theme;

/// btop's glyph set (transcribed from `btop_draw.cpp::Symbols`). ratatui's
/// `symbols::border` stores `&str` sets, not char constants, so define ours.
pub const H_LINE: char = '─';
pub const V_LINE: char = '│';
/// Dotted vertical — btop's staleness/gap divider in frozen graphs.
pub const DOTTED_V: char = '╎';

#[derive(Clone, Copy)]
pub struct BorderSet {
    pub top_left: char,
    pub top_right: char,
    pub bottom_left: char,
    pub bottom_right: char,
}

pub const ROUNDED: BorderSet = BorderSet {
    top_left: '╭',
    top_right: '╮',
    bottom_left: '╰',
    bottom_right: '╯',
};

pub const SQUARE: BorderSet = BorderSet {
    top_left: '┌',
    top_right: '┐',
    bottom_left: '└',
    bottom_right: '┘',
};

pub fn border_set(rounded: bool) -> BorderSet {
    if rounded { ROUNDED } else { SQUARE }
}

/// btop's title-break glyphs — inverted corners around the label, which reads
/// as the border "opening" around the title text (`createBox`'s `title_left`/
/// `title_right`).
pub const TITLE_LEFT: char = '┐';
pub const TITLE_RIGHT: char = '┌';
pub const TITLE_LEFT_DOWN: char = '┘';
pub const TITLE_RIGHT_DOWN: char = '└';

pub const SUPERSCRIPT: [char; 10] = ['⁰', '¹', '²', '³', '⁴', '⁵', '⁶', '⁷', '⁸', '⁹'];

/// A box drawn with btop's border idiom: solid frame in the box's own colour,
/// plus `┐text┌` toolbar buttons punching holes in the top border and `┘text└`
/// buttons on the bottom, each at a caller-provided x-offset (header strips
/// place menu/clock/interval buttons at btop-like positions).
pub struct Pane<'a> {
    pub rect: Rect,
    pub line_color_key: &'a str,
    /// Title text, including any leading superscript the caller embedded.
    pub title: Option<&'a str>,
    /// `(spans, x-offset-from-left)` buttons on the top border.
    pub top_buttons: Vec<(Vec<Span<'a>>, u16)>,
    pub bottom_buttons: Vec<(Vec<Span<'a>>, u16)>,
}

/// Compose a border button's spans: bold `hi_fg` hotkey + title-fg label tail.
pub fn button_spans<'a>(hotkey: char, label: &'a str, th: &Theme) -> Vec<Span<'a>> {
    vec![
        Span::styled(
            hotkey.to_string(),
            Style::default()
                .fg(th.c("hi_fg"))
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(label[1..].to_string(), Style::default().fg(th.c("title"))),
    ]
}

impl Pane<'_> {
    /// Draw frame + decorations. btop draws each box as a complete independent
    /// frame; boxes are drawn in order (cpu, mem, net, proc) so a later box's
    /// border simply overlays an earlier one on a shared row/col. There are no
    /// merged borders or T-junctions — the seams are intentional double lines.
    pub fn render(&self, buf: &mut Buffer, th: &Theme, bs: &BorderSet) {
        self.frame(buf, th, bs);
        self.decorations(buf, th);
    }

    /// Draw the border: lines, corners, blank interior.
    pub fn frame(&self, buf: &mut Buffer, th: &Theme, bs: &BorderSet) {
        let r = self.rect;
        if r.width < 2 || r.height < 2 {
            return;
        }
        let style = Style::default().fg(th.c(self.line_color_key));

        let bottom = r.bottom() - 1;
        for x in r.left()..r.right() {
            buf[(x, r.top())]
                .set_symbol(&H_LINE.to_string())
                .set_style(style);
            buf[(x, bottom)]
                .set_symbol(&H_LINE.to_string())
                .set_style(style);
        }
        for y in r.top() + 1..bottom {
            buf[(r.left(), y)]
                .set_symbol(&V_LINE.to_string())
                .set_style(style);
            buf[(r.right() - 1, y)]
                .set_symbol(&V_LINE.to_string())
                .set_style(style);
            for x in r.left() + 1..r.right() - 1 {
                buf[(x, y)].set_symbol(" ").set_style(Style::default());
            }
        }
        buf[(r.left(), r.top())]
            .set_symbol(&bs.top_left.to_string())
            .set_style(style);
        buf[(r.right() - 1, r.top())]
            .set_symbol(&bs.top_right.to_string())
            .set_style(style);
        buf[(r.left(), bottom)]
            .set_symbol(&bs.bottom_left.to_string())
            .set_style(style);
        buf[(r.right() - 1, bottom)]
            .set_symbol(&bs.bottom_right.to_string())
            .set_style(style);
    }

    /// Title and border buttons — call after `frame`, or use [`Pane::render`].
    pub fn decorations(&self, buf: &mut Buffer, th: &Theme) {
        let r = self.rect;
        if r.width < 2 || r.height < 2 {
            return;
        }
        let style = Style::default().fg(th.c(self.line_color_key));
        if let Some(title) = self.title {
            let mut spans = vec![Span::styled(TITLE_LEFT.to_string(), style)];
            // A leading superscript box-index digit renders in hi_fg, per btop.
            let mut rest = title;
            if let Some(first) = title.chars().next()
                && SUPERSCRIPT.contains(&first)
            {
                spans.push(Span::styled(
                    first.to_string(),
                    Style::default().fg(th.c("hi_fg")),
                ));
                rest = &title[first.len_utf8()..];
            }
            spans.push(Span::styled(
                rest.to_string(),
                Style::default()
                    .fg(th.c("title"))
                    .add_modifier(Modifier::BOLD),
            ));
            spans.push(Span::styled(TITLE_RIGHT.to_string(), style));
            put_spans(buf, r.top(), r.left() + 2, &spans);
        }
        for (spans, off) in &self.top_buttons {
            let mut all = vec![Span::styled(TITLE_LEFT.to_string(), style)];
            all.extend(spans.iter().cloned());
            all.push(Span::styled(TITLE_RIGHT.to_string(), style));
            put_spans(buf, r.top(), r.left() + *off, &all);
        }
        for (spans, off) in &self.bottom_buttons {
            let mut all = vec![Span::styled(TITLE_LEFT_DOWN.to_string(), style)];
            all.extend(spans.iter().cloned());
            all.push(Span::styled(TITLE_RIGHT_DOWN.to_string(), style));
            put_spans(buf, r.bottom() - 1, r.left() + *off, &all);
        }
    }
}

/// Write spans onto a row starting at `x`, clipped at the buffer's right edge.
fn put_spans(buf: &mut Buffer, y: u16, x: u16, spans: &[Span<'_>]) {
    let mut cx = x;
    for span in spans {
        for ch in span.content.chars() {
            if cx >= buf.area.right() {
                return;
            }
            buf[(cx, y)]
                .set_symbol(&ch.to_string())
                .set_style(span.style);
            cx += 1;
        }
    }
}

/// Render `lines` into the interior of a framed rect (row 0 = first interior
/// row), overwriting the frame's blank interior. Cells past a line's end are
/// reset to spaces so content never bleeds from a previous frame.
pub fn write_lines(buf: &mut Buffer, r: Rect, lines: &[Line<'_>]) {
    let (left, right) = (r.left() + 1, r.right() - 1);
    let (top, bottom) = (r.top() + 1, r.bottom() - 1);
    if right <= left || bottom <= top {
        return;
    }
    for (i, line) in lines.iter().enumerate() {
        let y = top + i as u16;
        if y >= bottom {
            break;
        }
        let mut cx = left;
        for span in line.spans.iter() {
            for ch in span.content.chars() {
                if cx >= right {
                    break;
                }
                buf[(cx, y)]
                    .set_symbol(&ch.to_string())
                    .set_style(span.style);
                cx += 1;
            }
        }
        for x in cx..right {
            buf[(x, y)].set_symbol(" ").set_style(Style::default());
        }
    }
}

/// Center a line vertically+horizontally inside an area (toosmall/banner).
pub fn centered(buf: &mut Buffer, r: Rect, y: u16, line: &Line<'_>) {
    if r.width == 0 || y >= r.bottom() {
        return;
    }
    let w: usize = line.spans.iter().map(|s| s.content.width()).sum();
    let mut x = r.x;
    if w < r.width as usize {
        x += (r.width as usize - w) as u16 / 2;
    }
    for span in &line.spans {
        for ch in span.content.chars() {
            if x >= r.right() {
                return;
            }
            buf[(x, y)]
                .set_symbol(&ch.to_string())
                .set_style(span.style);
            x += 1;
        }
    }
}
