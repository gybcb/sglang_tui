/// btop-style humanizing of rates/counts, plus the `N/A` discipline: an
/// unknown is rendered dimmed `N/A` (a claim about our knowledge), never `0`
/// (which would be a false claim about the server).
use ratatui::{
    style::Style,
    text::{Line, Span},
};
use unicode_width::UnicodeWidthStr;

use crate::ui::theme::Theme;

/// Tokens/requests/s → "1.2k" / "12.3M" style with 3 significant digits.
pub fn human_rate(v: f64) -> String {
    human_count(v as u64)
}

/// Counts → thousands/mega/giga with one decimal above 999.
pub fn human_count(v: u64) -> String {
    const UNITS: [&str; 4] = ["", "k", "M", "G"];
    let mut val = v as f64;
    let mut u = 0;
    while val >= 1000.0 && u < UNITS.len() - 1 {
        val /= 1000.0;
        u += 1;
    }
    if u == 0 {
        format!("{v}")
    } else if val < 10.0 {
        format!("{val:.2}{}", UNITS[u])
    } else if val < 100.0 {
        format!("{val:.1}{}", UNITS[u])
    } else {
        format!("{val:.0}{}", UNITS[u])
    }
}

/// Seconds → "1.2s" / "340ms" / "2m13s" / "4h11m".
pub fn human_duration(secs: f64) -> String {
    if secs < 1.0 {
        return format!("{}ms", (secs * 1000.0).round());
    }
    let s = secs.round() as u64;
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m{:02}s", s / 60, s % 60)
    } else {
        format!("{}h{:02}m", s / 3600, (s % 3600) / 60)
    }
}

/// Fraction 0..1 → "73%".
pub fn human_pct(frac: f64) -> String {
    format!("{:.0}%", frac * 100.0)
}

/// A right-aligned value cell; `None` becomes dimmed `N/A`.
pub fn value_cell(v: Option<String>, width: usize, th: &Theme, dimmed: bool) -> Span<'static> {
    let (text, style) = match v {
        Some(s) => (s, Style::default().fg(th.c("main_fg"))),
        None => (
            "N/A".to_string(),
            Style::default().fg(th.c("inactive_fg")).italic(),
        ),
    };
    let style = if dimmed { style.dim() } else { style };
    let pad = width.saturating_sub(text.width());
    Span::styled(format!("{}{}", " ".repeat(pad), text), style)
}

/// `label: value` row with the value right-aligned at `width`.
pub fn kv_row(
    label: &str,
    value: Option<String>,
    width: usize,
    th: &Theme,
    dimmed: bool,
) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("{label}:"),
            Style::default().fg(th.c("title")).bold(),
        ),
        value_cell(value, width, th, dimmed),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts() {
        assert_eq!(human_count(999), "999");
        assert_eq!(human_count(1000), "1.00k");
        assert_eq!(human_count(12345), "12.3k");
        assert_eq!(human_count(1_234_567), "1.23M");
        assert_eq!(human_count(2_500_000_000), "2.50G");
    }

    #[test]
    fn durations() {
        assert_eq!(human_duration(0.23), "230ms");
        assert_eq!(human_duration(9.4), "9s");
        assert_eq!(human_duration(75.0), "1m15s");
        assert_eq!(human_duration(3726.0), "1h02m");
    }

    #[test]
    fn pct() {
        assert_eq!(human_pct(0.731), "73%");
    }
}
