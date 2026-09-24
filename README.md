# sgtop

A [btop](https://github.com/aristocratos/btop)-style terminal monitor for an
[SGLang](https://github.com/sgl-project/sglang) inference server — braille
area graphs, gradient meters, live per-rank tables, all fed by the server's
Prometheus `/metrics`.

```
╭─┐¹engine┌─┐?help┌┐options┌┐14:23:58┌──┐- 1000ms +┌────────────────────────╮
│gen   232/451 tok/s  prefill  8.91k/12.3k tok/s   decode   232/451 tok/s  │
│engine busy ■■■■■■■■■■■■□□□□□□□□□□    52%  fwd  98.1%                      │
│running  6   waiting  0   ctx  19.2k   tok  3.1c  detok  0.4c             │
│⣀⣀⣀⡠⠤⠤⠤⣀⣀⣀⣀⣠⠴⠛⠛⠛⠉⠉⠉⠉⠉⠉⠉⠉⠉⠉⠉⠉⠉⠉⠉⠉⠉⠉⠉⠉⠉⠉⠉⠉⠉⠉⠉⠉⠉⠉⠉⠉⠉⠉⠉⠉│
╰──────────────────────────────────────────────────────────────────────────╯
╭─┐²token pool┌─────────────╮╭─┐⁴ranks┌─────────────────────────────────────╮
│Total:  488k  used 226k    ││rank▲    run   wait   used    usage  gen/s  │
│■■■■■■■□□□□□□□□□□□□□□□ 46% ││agg        6      0  226k      46%     232   │
│available 5.57k  evict 30  ││   0       6      0  226k      46%     232   │
│hit 0%/100%  gone 123M 2ms ││┘1/1└                                       │
╰───────────────────────────╯╰─────────────────────────────────────────────╯
```

## What it shows

| Panel | Content |
|---|---|
| **engine** | gen / prefill / decode token rates, engine busy meter (software forward-time ratio — not GPU util), CUDA-graph coverage, admission pressure, helper-process CPU |
| **token pool** | KV pool used/available/evictable with sub-pool bars (full / SWA / mamba), prefix-cache hit rate, eviction flow, HiCache tiers when hierarchical cache is on |
| **traffic** | token in/out current·peak·accumulated, request rate, HTTP rps/active/errors per route, cache share (all-time + 30s window), request-length means, latency percentiles |
| **ranks** | one row per DP rank: running/waiting/used tokens/usage/gen throughput/cache hit — plus an aggregate row |

Percentiles (queue / TTFT / ITL / E2E), the per-route HTTP breakdown (with
per-route error counts), request-stage timings and startup phases live in the
`i` server-info overlay.

**Honesty rules the renderer follows:** a metric the server doesn't expose
renders as `N/A` or hides its section — never a fabricated `0`. If the server
goes unreachable the last values freeze and grey out with a staleness clock;
the graphs never extend a gap with zeros.

## Requirements

- An SGLang server started with **`--enable-metrics`** — sgtop reads only
  `/metrics` for live numbers (`/server_info` and `/model_info` for static
  metadata). A server without the flag is detected (404) and reported.
- A terminal with UTF-8 and ideally 256-colour/truecolor (falls back to 16).
- The full four-panel layout needs ≥55×24; a `toosmall` screen tells you.

## Install

### One-line install script (Linux/macOS)

```bash
curl --proto '=https' --tlsv1.2 -fsSL https://raw.githubusercontent.com/gybcb/sglang_tui/main/install.sh | bash
```

Downloads the latest prebuilt binary for your OS/arch into `~/.local/bin`
(set `SGTOP_INSTALL_DIR` to change it; `sudo` is used only if the target dir
isn't writable). Skips the download if that version is already installed;
`SGTOP_FORCE=1` overrides.

### From a release

Grab the tarball for your platform from the
[latest release](https://github.com/gybcb/sglang_tui/releases/latest):

| Platform | Asset |
|---|---|
| Linux x86_64 | `sgtop-x86_64-unknown-linux-gnu.tar.gz` |
| Linux aarch64 | `sgtop-aarch64-unknown-linux-gnu.tar.gz` |
| macOS Apple Silicon | `sgtop-aarch64-apple-darwin.tar.gz` |
| macOS Intel | `sgtop-x86_64-apple-darwin.tar.gz` |

```bash
tar xzf sgtop-<target>.tar.gz && sudo install -m755 sgtop /usr/local/bin/
```

### From source

```bash
cargo install --git https://github.com/gybcb/sglang_tui --locked
# or: git clone && cargo install --path . --locked
```

Requires Rust ≥ 1.88 (edition 2024).

## Usage

```bash
sgtop                                  # http://127.0.0.1:30000
sgtop -u http://192.168.1.10:8002 -i 500
SGLANG_TUI_API_KEY=sk-... sgtop -u https://host   # auth-gated servers
sgtop --theme tokyo-night --graph block
```

| Flag | Default | Meaning |
|---|---|---|
| `-u, --url` | `http://127.0.0.1:30000` | server base URL |
| `-i, --interval` | `1000` | poll interval ms (100–10000) |
| `--timeout` | `2000` | per-request HTTP timeout ms |
| `--api-key` | env `SGLANG_TUI_API_KEY` | bearer token (`/server_info` is auth-gated) |
| `-t, --theme` | `default` | `default` · `tty` · `gruvbox-dark` · `tokyo-night` · `catppuccin-mocha`, or a `.theme` file basename |
| `--color` | `auto` | `auto` · `256` · `16` |
| `--graph` | `btop` | `btop` (braille) · `block` (solid area) |
| `--max-metrics-bytes` | `8388608` | `/metrics` body cap before truncation |
| `-c, --config` | `$XDG_CONFIG_HOME/sgtop/config.toml` | config file |
| `--print-config` | | dump merged config as TOML and exit |

Precedence: flag > env > config file > builtin default.

### Keys

```
q / ^C      quit            1 2 3 4   toggle panels
? / F1      help            ↑↓ j k    select rank      ← →  sort column
o / F2      options         r         reverse sort     Enter rank detail
i / F9      server info     u         pause scraping   + -  interval ±100ms
esc         close overlay
```

## Building from a fork

```bash
cargo build --release            # thin LTO, single codegen unit
cargo test --lib                 # 143 unit tests
cargo clippy --all-targets -- -D warnings
```

Releases build automatically via `.github/workflows/release.yml` on `v*`
tags (and can be dispatched manually from the Actions tab).

## License

MIT
