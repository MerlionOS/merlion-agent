#!/usr/bin/env bash
# Merlion Agent installer.
#
# - Verifies a working Rust toolchain (installs via rustup if missing).
# - Builds merlion in release mode.
# - Symlinks the binary to ~/.local/bin/merlion (in PATH on most setups).
# - Creates ~/.merlion/ with subdirs for memory, skills, sessions.
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/MerlionOS/merlion-agent/main/scripts/install.sh | bash
# or:
#   ./scripts/install.sh

set -euo pipefail

REPO_URL="${MERLION_REPO_URL:-https://github.com/MerlionOS/merlion-agent.git}"
INSTALL_DIR="${MERLION_INSTALL_DIR:-$HOME/.merlion-src}"
BIN_DIR="${MERLION_BIN_DIR:-$HOME/.local/bin}"

step() { printf "\033[1;34m==>\033[0m %s\n" "$*"; }
warn() { printf "\033[1;33m!\033[0m  %s\n" "$*" >&2; }
fail() { printf "\033[1;31mxx\033[0m %s\n" "$*" >&2; exit 1; }

if ! command -v cargo >/dev/null 2>&1; then
  step "rustup not detected — installing Rust toolchain"
  if ! command -v curl >/dev/null 2>&1; then
    fail "curl is required to bootstrap rustup; install curl and re-run."
  fi
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable
  # shellcheck disable=SC1091
  source "$HOME/.cargo/env"
fi

if [ -d "$INSTALL_DIR/.git" ]; then
  step "Updating source checkout at $INSTALL_DIR"
  git -C "$INSTALL_DIR" pull --ff-only
else
  step "Cloning $REPO_URL → $INSTALL_DIR"
  git clone --depth 1 "$REPO_URL" "$INSTALL_DIR"
fi

step "Building merlion (release)"
cargo build --release --manifest-path "$INSTALL_DIR/Cargo.toml"

mkdir -p "$BIN_DIR"
ln -sf "$INSTALL_DIR/target/release/merlion" "$BIN_DIR/merlion"
step "Symlinked $BIN_DIR/merlion → $INSTALL_DIR/target/release/merlion"

mkdir -p "$HOME/.merlion/memory" "$HOME/.merlion/skills"
[ -f "$HOME/.merlion/config.yaml" ] || cat >"$HOME/.merlion/config.yaml" <<'YAML'
model:
  id: openai:gpt-4o-mini
max_iterations: 32
YAML

step "Done. Run `merlion doctor` to verify, then `merlion`."
if ! echo "$PATH" | tr ':' '\n' | grep -qx "$BIN_DIR"; then
  warn "$BIN_DIR is not on your PATH. Add this to your shell rc:"
  warn "  export PATH=\"$BIN_DIR:\$PATH\""
fi
