use std::collections::HashMap;

use ratatui::style::Color;

/// Terminal colour capability, resolved from `COLORTERM`/`TERM` at startup.
/// ratatui does **not** auto-downgrade `Color::Rgb`, so the theme resolves every
/// colour to the mode's nearest representable value once, at load.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorMode {
    TrueColor,
    Color256,
    Color16,
}

impl ColorMode {
    /// Detect from environment, matching btop's own heuristic:
    /// COLORTERM=truecolor/24bit → TrueColor; a `256` in TERM → 256; else 16.
    pub fn detect() -> ColorMode {
        let ct = std::env::var("COLORTERM").unwrap_or_default();
        if ct == "truecolor" || ct == "24bit" {
            return ColorMode::TrueColor;
        }
        let term = std::env::var("TERM").unwrap_or_default();
        if term.contains("256") {
            return ColorMode::Color256;
        }
        ColorMode::Color16
    }
}

/// A resolved palette: base colours plus 101-entry gradient tables, all already
/// projected into `mode`. Lookups return `Color`, never `String`.
pub struct Theme {
    colors: HashMap<String, Color>,
    gradients: HashMap<String, Vec<Color>>,
    pub mode: ColorMode,
    pub name: String,
}

/// One `key = "#rrggbb"` (or btop shorthand `#cc`) entry, before resolution.
type RawTheme = HashMap<String, String>;

/// The `Default` builtin, transcribed from btop's `Default_theme`
/// (`src/btop_theme.cpp`). Shorthand like `#00` means grey `#000000`.
fn default_raw() -> RawTheme {
    let mut m = RawTheme::new();
    let mut add = |k: &str, v: &str| {
        m.insert(k.to_string(), v.to_string());
    };
    add("main_bg", "#00");
    add("main_fg", "#cc");
    add("title", "#ee");
    add("hi_fg", "#b54040");
    add("selected_bg", "#6a2f2f");
    add("selected_fg", "#ee");
    add("inactive_fg", "#40");
    add("graph_text", "#60");
    add("meter_bg", "#40");
    add("proc_misc", "#0de756");
    add("cpu_box", "#556d59");
    add("mem_box", "#6c6c4b");
    add("net_box", "#5c588d");
    add("proc_box", "#805252");
    add("div_line", "#30");
    add("cpu_start", "#77ca9b");
    add("cpu_mid", "#cbc06c");
    add("cpu_end", "#dc4c4c");
    add("free_start", "#384f21");
    add("free_mid", "#b5e685");
    add("free_end", "#dcff85");
    add("cached_start", "#163350");
    add("cached_mid", "#74e6fc");
    add("cached_end", "#26c5ff");
    add("available_start", "#4e3f0e");
    add("available_mid", "#ffd77a");
    add("available_end", "#ffb814");
    add("used_start", "#592b26");
    add("used_mid", "#d9626d");
    add("used_end", "#ff4769");
    add("download_start", "#291f75");
    add("download_mid", "#4f43a3");
    add("download_end", "#b0a9de");
    add("upload_start", "#620665");
    add("upload_mid", "#7d4180");
    add("upload_end", "#dcafde");
    add("process_start", "#80d0a3");
    add("process_mid", "#dcd179");
    add("process_end", "#d45454");
    add("proc_pause_bg", "#b54040");
    m
}

/// btop's 16-colour TTY palette, used verbatim under [`ColorMode::Color16`].
fn tty_raw() -> RawTheme {
    let mut m = RawTheme::new();
    let mut add = |k: &str, v: &str| {
        m.insert(k.to_string(), v.to_string());
    };
    add("main_bg", "#00");
    add("main_fg", "#c8c8c8");
    add("title", "#c8c8c8");
    add("hi_fg", "#b54040");
    add("selected_bg", "#3a3a3a");
    add("selected_fg", "#c8c8c8");
    add("inactive_fg", "#585858");
    add("graph_text", "#585858");
    add("meter_bg", "#585858");
    add("proc_misc", "#87d7af");
    add("cpu_box", "#87d7af");
    add("mem_box", "#d7d787");
    add("net_box", "#d7afd7");
    add("proc_box", "#d78787");
    add("div_line", "#585858");
    // Gradients collapse to nearest of the 16.
    add("cpu_start", "#5faf87");
    add("cpu_mid", "#d7d75f");
    add("cpu_end", "#d75f5f");
    add("free_start", "#5f8700");
    add("free_mid", "#87d75f");
    add("free_end", "#d7ff5f");
    add("cached_start", "#005f5f");
    add("cached_mid", "#00d7ff");
    add("cached_end", "#5fd7ff");
    add("available_start", "#5f5f00");
    add("available_mid", "#ffd75f");
    add("available_end", "#ffaf00");
    add("used_start", "#5f0000");
    add("used_mid", "#ff5f5f");
    add("used_end", "#ff5f87");
    add("download_start", "#5f5f87");
    add("download_mid", "#8787d7");
    add("download_end", "#d7d7d7");
    add("upload_start", "#5f005f");
    add("upload_mid", "#875f87");
    add("upload_end", "#d7afd7");
    add("process_start", "#5fd7af");
    add("process_mid", "#d7d75f");
    add("process_end", "#d75f5f");
    add("proc_pause_bg", "#b54040");
    m
}

/// Names whose `<name>_start`/`_mid`/`_end` triples expand into a gradient.
const GRADIENT_FAMILIES: &[&str] = &[
    "cpu",
    "free",
    "cached",
    "available",
    "used",
    "download",
    "upload",
    "process",
];

/// Modern builtin palettes overlaid on the btop Default. Each names only the
/// keys it changes; the rest inherits. Values are the palettes' published hex
/// (gruvbox dark, tokyo-night, catppuccin mocha).
fn named_overlays(name: &str) -> Option<Vec<(&'static str, &'static str)>> {
    let o: Vec<(&str, &str)> = match name {
        "gruvbox-dark" => vec![
            ("main_bg", "#282828"),
            ("main_fg", "#ebdbb2"),
            ("title", "#ebdbb2"),
            ("hi_fg", "#fb4934"),
            ("selected_bg", "#504945"),
            ("selected_fg", "#ebdbb2"),
            ("inactive_fg", "#665c54"),
            ("graph_text", "#928374"),
            ("meter_bg", "#3c3836"),
            ("proc_misc", "#b8bb26"),
            ("cpu_box", "#b8bb26"),
            ("mem_box", "#fabd2f"),
            ("net_box", "#83a598"),
            ("proc_box", "#d3869b"),
            ("div_line", "#3c3836"),
            ("cpu_start", "#b8bb26"),
            ("cpu_mid", "#fabd2f"),
            ("cpu_end", "#fb4934"),
            ("free_start", "#3c3836"),
            ("free_mid", "#b8bb26"),
            ("free_end", "#b8bb26"),
            ("cached_start", "#076678"),
            ("cached_mid", "#83a598"),
            ("cached_end", "#83a598"),
            ("available_start", "#3c3836"),
            ("available_mid", "#fabd2f"),
            ("available_end", "#fabd2f"),
            ("used_start", "#427b58"),
            ("used_mid", "#fe8019"),
            ("used_end", "#fb4934"),
            ("download_start", "#3c3836"),
            ("download_mid", "#83a598"),
            ("download_end", "#83a598"),
            ("upload_start", "#3c3836"),
            ("upload_mid", "#d3869b"),
            ("upload_end", "#d3869b"),
            ("process_start", "#b8bb26"),
            ("process_mid", "#fabd2f"),
            ("process_end", "#fb4934"),
            ("proc_pause_bg", "#fb4934"),
        ],
        "tokyo-night" => vec![
            ("main_bg", "#1a1b26"),
            ("main_fg", "#c0caf5"),
            ("title", "#c0caf5"),
            ("hi_fg", "#f7768e"),
            ("selected_bg", "#33467c"),
            ("selected_fg", "#c0caf5"),
            ("inactive_fg", "#3b4261"),
            ("graph_text", "#565f89"),
            ("meter_bg", "#292e42"),
            ("proc_misc", "#9ece6a"),
            ("cpu_box", "#9ece6a"),
            ("mem_box", "#e0af68"),
            ("net_box", "#7aa2f7"),
            ("proc_box", "#bb9af7"),
            ("div_line", "#292e42"),
            ("cpu_start", "#9ece6a"),
            ("cpu_mid", "#e0af68"),
            ("cpu_end", "#f7768e"),
            ("free_start", "#292e42"),
            ("free_mid", "#9ece6a"),
            ("free_end", "#9ece6a"),
            ("cached_start", "#164b5e"),
            ("cached_mid", "#2ac3de"),
            ("cached_end", "#2ac3de"),
            ("available_start", "#292e42"),
            ("available_mid", "#e0af68"),
            ("available_end", "#e0af68"),
            ("used_start", "#15706a"),
            ("used_mid", "#ff9e64"),
            ("used_end", "#f7768e"),
            ("download_start", "#292e42"),
            ("download_mid", "#7aa2f7"),
            ("download_end", "#7aa2f7"),
            ("upload_start", "#292e42"),
            ("upload_mid", "#bb9af7"),
            ("upload_end", "#bb9af7"),
            ("process_start", "#9ece6a"),
            ("process_mid", "#e0af68"),
            ("process_end", "#f7768e"),
            ("proc_pause_bg", "#f7768e"),
        ],
        "catppuccin-mocha" => vec![
            ("main_bg", "#1e1e2e"),
            ("main_fg", "#cdd6f4"),
            ("title", "#cdd6f4"),
            ("hi_fg", "#f38ba8"),
            ("selected_bg", "#45475a"),
            ("selected_fg", "#cdd6f4"),
            ("inactive_fg", "#45475a"),
            ("graph_text", "#6c7086"),
            ("meter_bg", "#313244"),
            ("proc_misc", "#a6e3a1"),
            ("cpu_box", "#a6e3a1"),
            ("mem_box", "#f9e2af"),
            ("net_box", "#89b4fa"),
            ("proc_box", "#cba6f7"),
            ("div_line", "#313244"),
            ("cpu_start", "#a6e3a1"),
            ("cpu_mid", "#f9e2af"),
            ("cpu_end", "#f38ba8"),
            ("free_start", "#313244"),
            ("free_mid", "#a6e3a1"),
            ("free_end", "#a6e3a1"),
            ("cached_start", "#0c4a6e"),
            ("cached_mid", "#89dceb"),
            ("cached_end", "#89dceb"),
            ("available_start", "#313244"),
            ("available_mid", "#f9e2af"),
            ("available_end", "#f9e2af"),
            ("used_start", "#2d6a5b"),
            ("used_mid", "#fab387"),
            ("used_end", "#f38ba8"),
            ("download_start", "#313244"),
            ("download_mid", "#89b4fa"),
            ("download_end", "#89b4fa"),
            ("upload_start", "#313244"),
            ("upload_mid", "#cba6f7"),
            ("upload_end", "#cba6f7"),
            ("process_start", "#a6e3a1"),
            ("process_mid", "#f9e2af"),
            ("process_end", "#f38ba8"),
            ("proc_pause_bg", "#f38ba8"),
        ],
        _ => return None,
    };
    Some(o)
}

impl Theme {
    /// Resolve a builtin theme: `Default`, `TTY` (16-colour), or a modern
    /// palette (`gruvbox-dark`, `tokyo-night`, `catppuccin-mocha`). Color16
    /// always collapses to TTY.
    pub fn builtin(name: &str, mode: ColorMode) -> Theme {
        if name.eq_ignore_ascii_case("tty") || mode == ColorMode::Color16 {
            return Theme::from_raw(name, tty_raw(), mode);
        }
        let mut raw = default_raw();
        if let Some(over) = named_overlays(&name.to_lowercase()) {
            for (k, v) in over {
                raw.insert(k.to_string(), v.to_string());
            }
        }
        Theme::from_raw(name, raw, mode)
    }

    /// Parse a btop flat theme file (`key = "#rrggbb"`, `#` line comments, or
    /// `key: value`), overlaying onto the Default for any missing keys so a
    /// partial theme still renders. Drop-in compatible with btop's `.theme` files.
    pub fn from_file(name: &str, contents: &str, mode: ColorMode) -> Theme {
        let mut raw = default_raw();
        for line in contents.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with("//") {
                continue;
            }
            // btop themes use `key = "value"`; separators vary.
            if let Some((k, v)) = split_kv(line) {
                let v = v.trim_matches(|c| c == '"' || c == '\'').trim();
                raw.insert(k.trim().to_string(), v.to_string());
            }
        }
        Theme::from_raw(name, raw, mode)
    }

    fn from_raw(name: &str, raw: RawTheme, mode: ColorMode) -> Theme {
        let resolve = |s: &str| expand_hex(s, mode);
        let mut colors = HashMap::new();
        for (k, v) in &raw {
            if k.ends_with("_start") || k.ends_with("_mid") || k.ends_with("_end") {
                continue; // gradient inputs handled below
            }
            if let Some(c) = resolve(v) {
                colors.insert(k.clone(), c);
            }
        }

        let mut gradients = HashMap::new();
        for fam in GRADIENT_FAMILIES {
            let start = raw.get(&format!("{fam}_start")).and_then(|s| expand_rgb(s));
            let mid = raw.get(&format!("{fam}_mid")).and_then(|s| expand_rgb(s));
            let end = raw.get(&format!("{fam}_end")).and_then(|s| expand_rgb(s));
            let table = build_gradient(start, mid, end, mode);
            gradients.insert((*fam).to_string(), table);
        }

        Theme {
            colors,
            gradients,
            mode,
            name: name.to_string(),
        }
    }

    /// Base colour lookup. Falls back to white so a missing key never crashes a
    /// draw call mid-frame.
    pub fn c(&self, key: &str) -> Color {
        self.colors.get(key).copied().unwrap_or(Color::Indexed(7))
    }

    /// Sample the gradient table `family` at percentage `pct` (0..=100).
    /// `start`-only families fill the whole table with the start colour.
    pub fn grad(&self, family: &str, pct: usize) -> Color {
        let pct = pct.min(100);
        self.gradients
            .get(family)
            .and_then(|t| t.get(pct))
            .copied()
            .unwrap_or(self.c("main_fg"))
    }
}

/// Split `key = "value"` or `key: value` on the first `=` or `:`.
fn split_kv(line: &str) -> Option<(String, String)> {
    let idx = line.find(['=', ':'])?;
    let (k, v) = line.split_at(idx);
    Some((k.trim().to_string(), v[1..].trim().to_string()))
}

/// Expand btop hex (`#rgb` per-nibble, `#rrggbb`, `#00` 2-digit byte) → RGB.
/// The 2-digit form (`#cc`) is already a full byte (0xcc = 204), NOT a nibble to
/// duplicate; only the 3-digit per-nibble form duplicates each digit (×17).
fn expand_rgb(s: &str) -> Option<(u8, u8, u8)> {
    let s = s.trim().trim_start_matches('#');
    match s.len() {
        2 => {
            let v = u8::from_str_radix(s, 16).ok()?;
            Some((v, v, v))
        }
        3 => {
            let dup = |c: &str| -> Option<u8> { u8::from_str_radix(c, 16).ok().map(|n| n * 17) };
            Some((dup(&s[0..1])?, dup(&s[1..2])?, dup(&s[2..3])?))
        }
        6 => {
            let r = u8::from_str_radix(&s[0..2], 16).ok()?;
            let g = u8::from_str_radix(&s[2..4], 16).ok()?;
            let b = u8::from_str_radix(&s[4..6], 16).ok()?;
            Some((r, g, b))
        }
        _ => None,
    }
}

fn expand_hex(s: &str, mode: ColorMode) -> Option<Color> {
    let (r, g, b) = expand_rgb(s)?;
    Some(to_color(r, g, b, mode))
}

/// Project an RGB triple into the active colour mode. TrueColor keeps it; 256
/// snaps to the xterm 6x6x6 cube + grayscale ramp; 16 snaps to the nearest of
/// the 16 ANSI. Done once at load, never on the redraw path.
pub fn to_color(r: u8, g: u8, b: u8, mode: ColorMode) -> Color {
    match mode {
        ColorMode::TrueColor => Color::Rgb(r, g, b),
        ColorMode::Color256 => Color::Indexed(rgb_to_256(r, g, b)),
        ColorMode::Color16 => Color::Indexed(rgb_to_16(r, g, b)),
    }
}

/// Nearest of the xterm-256 6x6x6 cube + 24-step grey ramp.
fn rgb_to_256(r: u8, g: u8, b: u8) -> u8 {
    // Near-grey → prefer the grey ramp (232..255).
    let mx = r.max(g).max(b);
    let mn = r.min(g).min(b);
    if mx.saturating_sub(mn) < 12 && (232..=255u8).count() > 0 {
        let v = (r as u16 + g as u16 + b as u16) / 3;
        let idx = ((v as f32 - 8.0) / 247.0 * 23.0).round().clamp(0.0, 23.0) as u8;
        return 232 + idx;
    }
    let q = |x: u8| -> u8 { ((x as f32 / 255.0) * 5.0).round() as u8 };
    let (qr, qg, qb) = (q(r), q(g), q(b));
    16 + 36 * qr + 6 * qg + qb
}

/// Snap RGB to the nearest of the 16 ANSI entries (indices 0..15).
fn rgb_to_16(r: u8, g: u8, b: u8) -> u8 {
    // The 16 reference colours (black,red,green,yellow,blue,magenta,cyan,ltgrey
    // then the bright variants).
    const ANSI: [(u8, u8, u8); 16] = [
        (0, 0, 0),
        (170, 0, 0),
        (0, 170, 0),
        (170, 85, 0),
        (0, 0, 170),
        (170, 0, 170),
        (0, 170, 170),
        (170, 170, 170),
        (85, 85, 85),
        (255, 85, 85),
        (85, 255, 85),
        (255, 255, 85),
        (85, 85, 255),
        (255, 85, 255),
        (85, 255, 255),
        (255, 255, 255),
    ];
    let (rr, gg, bb) = (r as i32, g as i32, b as i32);
    let mut best = 0usize;
    let mut bd = i32::MAX;
    for (i, (ar, ag, ab)) in ANSI.iter().enumerate() {
        let d = (rr - *ar as i32).pow(2) + (gg - *ag as i32).pow(2) + (bb - *ab as i32).pow(2);
        if d < bd {
            bd = d;
            best = i;
        }
    }
    best as u8
}

/// Linear RGB interpolation of `start`/`mid`/`end` into a 101-entry table,
/// matching btop's `generateGradients` two-pass 50+51 split.
fn build_gradient(
    start: Option<(u8, u8, u8)>,
    mid: Option<(u8, u8, u8)>,
    end: Option<(u8, u8, u8)>,
    mode: ColorMode,
) -> Vec<Color> {
    let mut out = Vec::with_capacity(101);
    let Some(st) = start else { return out };
    // start-only families: fill with the start colour (btop's fill fallback).
    let Some(en) = end else {
        let c = to_color(st.0, st.1, st.2, mode);
        out.resize(101, c);
        return out;
    };
    for i in 0..=100 {
        // Two-stop (no mid) gradient interpolates start→end over the whole 100;
        // three-stop splits 50+51 at the mid colour, matching btop.
        let (a, b, span, off) = match mid {
            Some(mi) if i <= 50 => (st, mi, 50, 0),
            Some(mi) => (mi, en, 50, 50),
            None => (st, en, 100, 0),
        };
        let t = (i - off) as f32 / span as f32;
        let lerp = |x: u8, y: u8| {
            (x as f32 + (y as f32 - x as f32) * t)
                .round()
                .clamp(0.0, 255.0) as u8
        };
        out.push(to_color(
            lerp(a.0, b.0),
            lerp(a.1, b.1),
            lerp(a.2, b.2),
            mode,
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shorthand_grayscale() {
        assert_eq!(expand_rgb("#cc"), Some((0xcc, 0xcc, 0xcc)));
        assert_eq!(expand_rgb("#00"), Some((0, 0, 0)));
        assert_eq!(expand_rgb("#77ca9b"), Some((0x77, 0xca, 0x9b)));
        assert_eq!(expand_rgb("#f0a"), Some((0xff, 0x00, 0xaa)));
    }

    #[test]
    fn gradient_terminals() {
        let t = Theme::builtin("Default", ColorMode::TrueColor);
        assert_eq!(t.grad("cpu", 0), Color::Rgb(0x77, 0xca, 0x9b));
        assert_eq!(t.grad("cpu", 100), Color::Rgb(0xdc, 0x4c, 0x4c));
        assert_eq!(t.grad("cpu", 50), Color::Rgb(0xcb, 0xc0, 0x6c));
    }

    #[test]
    fn color16_downgrade_never_panic() {
        let t = Theme::builtin("Default", ColorMode::Color16);
        for p in 0..=100 {
            let _ = t.grad("cpu", p);
        }
        assert!(matches!(t.c("title"), Color::Indexed(_)));
    }

    /// A modern palette ships an opaque `main_bg` by design, so the theme
    /// paints its own ground instead of borrowing the terminal's.
    #[test]
    fn modern_builtins_ship_a_main_bg() {
        for name in ["gruvbox-dark", "tokyo-night", "catppuccin-mocha"] {
            let t = Theme::builtin(name, ColorMode::TrueColor);
            assert!(matches!(t.c("main_bg"), Color::Rgb(..)), "{name} main_bg");
        }
    }

    #[test]
    fn modern_builtins_overlay_and_reramp() {
        let t = Theme::builtin("tokyo-night", ColorMode::TrueColor);
        // Overlaid key: tokyo-night red, not btop's default hi_fg.
        assert_eq!(t.c("hi_fg"), Color::Rgb(0xf7, 0x76, 0x8e));
        // Gradient family re-ramps from the theme's own stops.
        assert_eq!(t.grad("cpu", 0), Color::Rgb(0x9e, 0xce, 0x6a));
        assert_eq!(t.grad("cpu", 100), Color::Rgb(0xf7, 0x76, 0x8e));
    }

    #[test]
    fn modern_builtins_resolve_all_names() {
        for name in ["gruvbox-dark", "tokyo-night", "catppuccin-mocha"] {
            let t = Theme::builtin(name, ColorMode::TrueColor);
            for key in ["main_fg", "title", "hi_fg", "cpu_box", "proc_box"] {
                assert!(
                    matches!(t.c(key), Color::Rgb(..)),
                    "{name}.{key} resolves to a real colour"
                );
            }
            assert!(matches!(t.grad("cpu", 100), Color::Rgb(..)));
        }
    }

    #[test]
    fn modern_builtins_downgrade_to_tty_under_color16() {
        let t = Theme::builtin("gruvbox-dark", ColorMode::Color16);
        assert!(matches!(t.c("title"), Color::Indexed(_)));
    }
}
