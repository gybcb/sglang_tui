use std::path::PathBuf;

use clap::Parser;
use serde::Deserialize;

use crate::ui::theme::{ColorMode, Theme};

/// sgtop — a btop-style monitor for an SGLang inference server.
#[derive(Parser, Debug)]
#[command(name = "sgtop", version, about)]
pub struct Cli {
    /// Base URL of the SGLang server.
    #[arg(short, long, default_value = "http://127.0.0.1:30000")]
    pub url: String,

    /// Poll interval in milliseconds.
    #[arg(short, long, default_value_t = 1000)]
    pub interval: u64,

    /// Per-request HTTP timeout in milliseconds.
    #[arg(long, default_value_t = 2000)]
    pub timeout: u64,

    /// Bearer token for auth-gated endpoints; prefer the env var.
    #[arg(long, env = "SGLANG_TUI_API_KEY", default_value = "")]
    pub api_key: String,

    /// Theme name (builtin, or a `.theme` basename on the search path).
    #[arg(short, long, default_value = "default")]
    pub theme: String,

    /// Colour mode; `auto` detects from COLORTERM/TERM.
    #[arg(long, default_value = "auto")]
    pub color: String,

    /// Maximum `/metrics` response body bytes before truncation.
    #[arg(long, default_value_t = 8_388_608)]
    pub max_metrics_bytes: u64,

    /// Graph style: `block` (solid half-block area) or `btop` (braille dots).
    #[arg(long, default_value = "block")]
    pub graph: String,

    /// Path to a config file; defaults to `$XDG_CONFIG_HOME/sgtop/config.toml`.
    #[arg(short, long)]
    pub config: Option<PathBuf>,

    /// Dump the merged config as TOML and exit.
    #[arg(long)]
    pub print_config: bool,
}

/// Merged, resolved configuration consumed by the rest of the program.
#[derive(Debug, Clone, serde::Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub url: String,
    pub interval_ms: u64,
    pub timeout_ms: u64,
    pub api_key: String,
    pub theme: String,
    pub color_mode: String,
    pub max_metrics_bytes: u64,
    /// Graph style: "block" (modern solid) or "btop" (braille).
    pub graph: String,
    pub round_corners: bool,
    /// Layout percentage/geometry prefs mirroring btop's box toggles.
    pub cpu_bottom: bool,
    pub mem_below_net: bool,
    pub proc_left: bool,
    pub vim_keys: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            url: "http://127.0.0.1:30000".into(),
            interval_ms: 1000,
            timeout_ms: 2000,
            api_key: String::new(),
            theme: "default".into(),
            color_mode: "auto".into(),
            max_metrics_bytes: 8_388_608,
            graph: "block".into(),
            round_corners: true,
            cpu_bottom: false,
            mem_below_net: false,
            proc_left: false,
            vim_keys: false,
        }
    }
}

impl Config {
    /// Merge precedence: flag > env (via clap defaults) > file > builtin default.
    /// A clap flag that equals its default value does not override the file.
    pub fn build(cli: &Cli) -> Config {
        let mut cfg = Self::load_file(cli.config.as_deref());

        // A flag "sticks" over the file only if the user actually passed it,
        // detected by comparing to the clap default rather than trusting presence.
        if cli.url != default_url() {
            cfg.url = cli.url.clone();
        }
        if cli.interval != 1000 {
            cfg.interval_ms = cli.interval;
        }
        if cli.timeout != 2000 {
            cfg.timeout_ms = cli.timeout;
        }
        if !cli.api_key.is_empty() {
            cfg.api_key = cli.api_key.clone();
        }
        if cli.theme != "default" {
            cfg.theme = cli.theme.clone();
        }
        if cli.color != "auto" {
            cfg.color_mode = cli.color.clone();
        }
        if cli.max_metrics_bytes != 8_388_608 {
            cfg.max_metrics_bytes = cli.max_metrics_bytes;
        }
        if cli.graph != "block" {
            cfg.graph = cli.graph.clone();
        }
        cfg.interval_ms = cfg.interval_ms.clamp(100, 10_000);
        cfg
    }

    /// Whether graphs render in btop's braille style (else modern block).
    pub fn graph_braille(&self) -> bool {
        self.graph.eq_ignore_ascii_case("btop")
    }

    fn load_file(explicit: Option<&std::path::Path>) -> Config {
        let path = match explicit {
            Some(p) => p.to_path_buf(),
            None => default_config_path(),
        };
        match std::fs::read_to_string(&path) {
            Ok(s) => toml::from_str(&s).unwrap_or_default(),
            Err(_) => Config::default(),
        }
    }

    pub fn color_mode(&self) -> ColorMode {
        match self.color_mode.as_str() {
            "truecolor" | "24bit" => ColorMode::TrueColor,
            "256" => ColorMode::Color256,
            "16" => ColorMode::Color16,
            _ => ColorMode::detect(),
        }
    }

    /// Resolve the theme: builtin names are in-memory; otherwise search the
    /// btop-style theme path for `<name>.theme`. Falls back to Default.
    pub fn theme(&self) -> Theme {
        let mode = self.color_mode();
        let lower = self.theme.to_lowercase();
        if matches!(
            lower.as_str(),
            "default" | "tty" | "gruvbox-dark" | "tokyo-night" | "catppuccin-mocha"
        ) {
            return Theme::builtin(&lower, mode);
        }
        for dir in theme_search_dirs() {
            let cand = dir.join(format!("{name}.theme", name = self.theme));
            if let Ok(contents) = std::fs::read_to_string(&cand) {
                return Theme::from_file(&self.theme, &contents, mode);
            }
        }
        Theme::builtin("Default", mode)
    }
}

fn default_url() -> String {
    "http://127.0.0.1:30000".into()
}

fn default_config_path() -> PathBuf {
    dirs::config_dir()
        .map(|d| d.join("sgtop").join("config.toml"))
        .unwrap_or_else(|| PathBuf::from("sgtop.toml"))
}

/// Search order mirrors btop: `--themes-dir` (unused in v1) → XDG → `~/.config`
/// → `/usr/local/share`. First `<name>.theme` wins.
fn theme_search_dirs() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(c) = dirs::config_dir() {
        v.push(c.join("sgtop").join("themes"));
    }
    if let Some(h) = dirs::home_dir() {
        v.push(h.join(".config").join("sgtop").join("themes"));
    }
    v.push(PathBuf::from("/usr/local/share/sgtop/themes"));
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_clamp_interval() {
        let cli = Cli::try_parse_from(["sgtop", "-i", "5"]).unwrap();
        let cfg = Config::build(&cli);
        assert_eq!(cfg.interval_ms, 100);
    }

    #[test]
    fn explicit_url_overrides() {
        let cli = Cli::try_parse_from(["sgtop", "-u", "http://h:1"]).unwrap();
        assert_eq!(Config::build(&cli).url, "http://h:1");
    }

    #[test]
    fn builtin_themes_resolve_without_file_search() {
        let cli =
            Cli::try_parse_from(["sgtop", "-t", "tokyo-night", "--color", "truecolor"]).unwrap();
        let cfg = Config::build(&cli);
        // Resolves to the builtin even with no .theme file anywhere.
        assert_eq!(
            cfg.theme().c("hi_fg"),
            ratatui::style::Color::Rgb(0xf7, 0x76, 0x8e)
        );
    }
}
