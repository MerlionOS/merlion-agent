#!/usr/bin/env bash
# Point this checkout's git at the in-tree .githooks/ directory.
# Run once per clone. Idempotent.

set -e

cd "$(git rev-parse --show-toplevel)"
git config core.hooksPath .githooks
chmod +x .githooks/*
echo "merlion git hooks installed: $(git config core.hooksPath)"
echo "  pre-commit: cargo fmt --check + cargo clippy -D warnings"
echo "  pre-push:   cargo test --workspace"
echo "bypass with --no-verify if you ever need to."
