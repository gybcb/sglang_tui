#!/usr/bin/env bash
# sgtop installer — fetches the latest prebuilt release binary.
#
#   curl -fsSL https://raw.githubusercontent.com/gybcb/sglang_tui/main/install.sh | bash
#
# Env overrides:
#   SGTOP_INSTALL_DIR   where to install (default: ~/.local/bin)
#   SGTOP_VERSION       release tag to install (default: latest)
#   SGTOP_FORCE=1       reinstall even if the same version is present
set -euo pipefail

REPO="gybcb/sglang_tui"
BIN="sgtop"
INSTALL_DIR="${SGTOP_INSTALL_DIR:-$HOME/.local/bin}"

say() { printf 'sgtop: %s\n' "$*"; }
die() { printf 'sgtop: error: %s\n' "$*" >&2; exit 1; }

# --- detect platform, map to a release asset target triple ------------------
os=$(uname -s); arch=$(uname -m)
case "$os" in
  Linux)  os_part="unknown-linux-gnu" ;;
  Darwin) os_part="apple-darwin" ;;
  *) die "unsupported OS: $os (macOS and Linux only)" ;;
esac
case "$arch" in
  x86_64|amd64)  arch_part="x86_64" ;;
  aarch64|arm64) arch_part="aarch64" ;;
  *) die "unsupported architecture: $arch" ;;
esac
target="${arch_part}-${os_part}"
asset="${BIN}-${target}.tar.gz"

# --- resolve version --------------------------------------------------------
api="https://api.github.com/repos/${REPO}/releases"
if [ -n "${SGTOP_VERSION:-}" ]; then
  tag="$SGTOP_VERSION"
else
  # /latest 302-redirects; grabbing Location is cheaper than parsing JSON
  # (and immune to unauthenticated-API rate limits returning JSON errors).
  tag=$(curl -fsSI "$api/latest" \
        | tr -d '\r' \
        | awk -F/ '/^location: /{print $NF; exit}' \
        | awk -F'releases/tag/' '{print $NF}')
  [ -n "$tag" ] || die "could not determine the latest release (check network)"
fi
version="${tag#v}"

# --- skip when already current ----------------------------------------------
mkdir -p "$INSTALL_DIR"
if [ "${SGTOP_FORCE:-0}" != "1" ] && [ -x "$INSTALL_DIR/$BIN" ]; then
  have=$("$INSTALL_DIR/$BIN" --version 2>/dev/null | awk '{print $2}' || true)
  if [ "$have" = "$version" ]; then
    say "$BIN $version already installed at $INSTALL_DIR/$BIN (SGTOP_FORCE=1 to reinstall)"
    exit 0
  fi
fi

# --- download + verify + unpack ---------------------------------------------
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

url="https://github.com/${REPO}/releases/download/${tag}/${asset}"
say "downloading $asset ($tag)"
curl -fSL --proto '=https' --tlsv1.2 -o "$tmp/$asset" "$url" \
  || die "download failed: $url"

# Checksums ship with every release; verify unless the asset is missing them.
if curl -fsSL -o "$tmp/sha256sums.txt" \
     "https://github.com/${REPO}/releases/download/${tag}/sha256sums.txt" 2>/dev/null; then
  want=$(awk -v a="$asset" '$2==a || $2=="./"a {print $1}' "$tmp/sha256sums.txt")
  if [ -n "$want" ]; then
    if command -v sha256sum >/dev/null 2>&1; then
      got=$(sha256sum "$tmp/$asset" | awk '{print $1}')
    elif command -v shasum >/dev/null 2>&1; then
      got=$(shasum -a 256 "$tmp/$asset" | awk '{print $1}')
    else
      die "no sha256sum/shasum available to verify the download"
    fi
    [ "$got" = "$want" ] || die "checksum mismatch (got $got, expected $want)"
    say "checksum ok"
  else
    say "warning: $asset not listed in sha256sums.txt — skipping verification"
  fi
fi

tar xzf "$tmp/$asset" -C "$tmp"
[ -f "$tmp/$BIN" ] || die "archive did not contain $BIN"

# --- install -----------------------------------------------------------------
dest="$INSTALL_DIR/$BIN"
if [ -w "$INSTALL_DIR" ]; then
  install -m755 "$tmp/$BIN" "$dest"
else
  say "$INSTALL_DIR not writable — using sudo"
  sudo install -m755 "$tmp/$BIN" "$dest"
fi
say "installed $BIN $version -> $dest"

case ":$PATH:" in
  *":$INSTALL_DIR:"*) ;;
  *) say "note: $INSTALL_DIR is not on PATH — add it:"
     say "  export PATH=\"$INSTALL_DIR:\$PATH\"" ;;
esac
