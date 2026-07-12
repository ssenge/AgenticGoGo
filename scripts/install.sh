#!/bin/sh
# AgenticGoGo (`agg`) installer — collapses the "download the right binary + chmod +
# put it on PATH" dance into one line:
#
#   curl -fsSL https://raw.githubusercontent.com/ssenge/AgenticGoGo/main/scripts/install.sh | sh
#
# What it does: detect your OS/arch, download the matching release binary from the
# latest GitHub Release, make it executable, and install it onto your PATH (preferring
# /usr/local/bin, falling back to ~/.local/bin). Then it verifies `agg --version`.
#
# Overrides (env vars):
#   AGG_VERSION=v0.0.2        install a specific tag instead of the latest release
#   AGG_INSTALL_DIR=~/bin     install to a specific dir (skips the /usr/local/bin try)
#
# This installs ONLY the `agg` CLI. The `/agg:*` skills are separate, and work on ALL THREE agents
# (Claude Code, OpenAI Codex, GitHub Copilot) — once `agg` is on your PATH:
#
#   agg skills install --agent claude|codex|copilot
#
# …or via the plugin marketplace, which all three consume from the same manifest. See the README.

set -eu

REPO="ssenge/AgenticGoGo"
BIN="agg"

# ---- pretty output (no color if not a tty) ----
if [ -t 1 ]; then
  BOLD="$(printf '\033[1m')"; DIM="$(printf '\033[2m')"; RED="$(printf '\033[31m')"
  GRN="$(printf '\033[32m')"; YEL="$(printf '\033[33m')"; RST="$(printf '\033[0m')"
else
  BOLD=""; DIM=""; RED=""; GRN=""; YEL=""; RST=""
fi
say()  { printf '%s\n' "$*"; }
info() { printf '%s▸%s %s\n' "$GRN" "$RST" "$*"; }
warn() { printf '%s⚠%s %s\n' "$YEL" "$RST" "$*" >&2; }
die()  { printf '%s✗ %s%s\n' "$RED" "$*" "$RST" >&2; exit 1; }

# ---- detect a downloader ----
if command -v curl >/dev/null 2>&1; then
  DL="curl -fsSL -o"
elif command -v wget >/dev/null 2>&1; then
  DL="wget -qO"
else
  die "need either curl or wget to download the binary."
fi

# ---- detect OS / arch → release target triple ----
os="$(uname -s)"
arch="$(uname -m)"
case "$os" in
  Linux)
    case "$arch" in
      x86_64|amd64) target="x86_64-unknown-linux-gnu" ;;
      aarch64|arm64) die "no prebuilt Linux arm64 binary yet. Build from source: https://github.com/$REPO#install" ;;
      *) die "unsupported Linux arch '$arch'. Build from source: https://github.com/$REPO#install" ;;
    esac ;;
  Darwin)
    case "$arch" in
      arm64) target="aarch64-apple-darwin" ;;
      x86_64) target="x86_64-apple-darwin" ;;
      *) die "unsupported macOS arch '$arch'." ;;
    esac ;;
  MINGW*|MSYS*|CYGWIN*)
    die "Windows: download agg-x86_64-pc-windows-msvc.exe from https://github.com/$REPO/releases/latest" ;;
  *)
    die "unsupported OS '$os'. See https://github.com/$REPO#install" ;;
esac
asset="${BIN}-${target}"
info "platform: ${BOLD}${os} ${arch}${RST} → asset ${BOLD}${asset}${RST}"

# ---- resolve the download URL (latest, or a pinned AGG_VERSION) ----
if [ "${AGG_VERSION:-}" != "" ]; then
  url="https://github.com/${REPO}/releases/download/${AGG_VERSION}/${asset}"
  info "version: ${BOLD}${AGG_VERSION}${RST} (pinned)"
else
  url="https://github.com/${REPO}/releases/latest/download/${asset}"
  info "version: ${BOLD}latest${RST}"
fi

# ---- download to a temp file ----
tmp="$(mktemp 2>/dev/null || echo "/tmp/${asset}.$$")"
cleanup() { rm -f "$tmp"; }
trap cleanup EXIT INT TERM
info "downloading…"
# shellcheck disable=SC2086
if ! $DL "$tmp" "$url"; then
  die "download failed: $url
   • check your connection, or
   • a release for this platform may not exist yet — see https://github.com/$REPO/releases"
fi
# guard against a 404 HTML page masquerading as the binary
if [ ! -s "$tmp" ]; then
  die "downloaded an empty file from $url"
fi
# 0755 so a system-wide install (/usr/local/bin) is runnable by all users, not just
# whoever ran the installer (mktemp defaults to a private 0600 mask).
chmod 755 "$tmp"

# ---- choose an install dir ----
# Priority: AGG_INSTALL_DIR override → /usr/local/bin (with sudo if needed) → ~/.local/bin.
install_to() {
  dir="$1"
  mkdir -p "$dir" 2>/dev/null || return 1
  dest="$dir/$BIN"
  if [ -w "$dir" ]; then
    mv -f "$tmp" "$dest" || return 1
  elif command -v sudo >/dev/null 2>&1; then
    warn "$dir needs root — using sudo"
    sudo mv -f "$tmp" "$dest" || return 1
  else
    return 1
  fi
  trap - EXIT INT TERM   # moved successfully; don't let cleanup delete it
  echo "$dest"
  return 0
}

dest=""
if [ "${AGG_INSTALL_DIR:-}" != "" ]; then
  dest="$(install_to "$AGG_INSTALL_DIR")" || die "couldn't install to AGG_INSTALL_DIR=$AGG_INSTALL_DIR"
else
  if dest="$(install_to /usr/local/bin)"; then
    :
  else
    warn "/usr/local/bin not writable — falling back to ~/.local/bin"
    dest="$(install_to "$HOME/.local/bin")" || die "couldn't install to ~/.local/bin either."
  fi
fi
info "installed: ${BOLD}${dest}${RST}"

# ---- PATH check ----
dest_dir="$(dirname "$dest")"
case ":$PATH:" in
  *":$dest_dir:"*) ;;  # already on PATH
  *) warn "$dest_dir is not on your PATH. Add it, e.g.:
     echo 'export PATH=\"$dest_dir:\$PATH\"' >> ~/.profile && . ~/.profile" ;;
esac

# ---- verify ----
if "$dest" --version >/dev/null 2>&1; then
  ver="$("$dest" --version 2>/dev/null || true)"
  info "verified: ${BOLD}${ver}${RST}"
else
  warn "installed, but '$dest --version' didn't run cleanly. Try a new shell."
fi

# ---- nudge about the two other pieces of the setup ----
say ""
say "${BOLD}Next steps${RST}"
if command -v claude >/dev/null 2>&1; then
  say "  ${GRN}✓${RST} Claude Code CLI found (agg drives it to run workers)."
else
  warn "Claude Code CLI (\`claude\`) not found — agg needs it. Install: https://claude.com/claude-code"
fi
say "  ${DIM}•${RST} scaffold a project:  ${BOLD}agg init${RST}"
say "  ${DIM}•${RST} check your setup:    ${BOLD}agg doctor${RST}"
say "  ${DIM}•${RST} plugin (/agg:* skills), inside Claude Code:"
say "       /plugin marketplace add ${REPO}"
say "       /plugin install agg@agenticgogo"
