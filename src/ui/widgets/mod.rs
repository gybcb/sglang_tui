pub mod block;
pub mod braille;
pub mod meter;
pub mod table;

use ratatui::{buffer::Buffer, layout::Rect};

use crate::ui::theme::Theme;

/// Rendering-style options threaded from `Config` into panel/widget draws.
#[derive(Clone, Copy)]
pub struct Gui {
    /// Graphs use btop's braille dots; otherwise the modern block style.
    pub braille_graphs: bool,
}

impl Gui {
    /// The modern (block-graph) look — used by tests only; the runtime
    /// default is btop braille, built from `Config::graph_braille()`.
    pub const MODERN: Gui = Gui {
        braille_graphs: false,
    };
}

/// Style-dispatched area graph: btop's braille dots (default) or modern
/// solid `block` (`--graph block`). All panel call sites go through this so
/// the style is a single config switch.
pub fn area_graph(
    buf: &mut Buffer,
    area: Rect,
    series: &[f64],
    th: &Theme,
    family: &str,
    braille_style: bool,
    peak_label: Option<String>,
) {
    if braille_style {
        braille::draw(buf, area, series, th, family, None, true);
        return;
    }
    block::draw(buf, area, series, th, family);
    if let Some(text) = peak_label {
        use ratatui::style::Style;
        block::put_text(buf, area, &text, Style::default().fg(th.c("graph_text")));
    }
}
