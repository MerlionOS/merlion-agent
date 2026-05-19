#!/usr/bin/env bash
# Publish all 11 merlion crates to crates.io in dependency order.
# Idempotent retries on 429 rate limits.
#
# publish-update (existing crate, new version) is rate-limited more
# generously than publish-new (we hit ~5/min on v0.1.0 first publish);
# this should be much faster.

set -u

ORDER=(
  merlion-core
  merlion-config
  merlion-memory
  merlion-skills
  merlion-session
  merlion-llm
  merlion-mcp
  merlion-tools
  merlion-gateway
  merlion-cron
  merlion-agent
)

for crate in "${ORDER[@]}"; do
  attempt=1
  while true; do
    echo "[$(date -u +%H:%M:%S)] publish $crate (attempt $attempt)"
    out=$(cargo publish -p "$crate" --no-verify 2>&1)
    status=$?
    echo "$out" | tail -2
    if [ $status -eq 0 ]; then
      break
    fi
    if echo "$out" | grep -q '429 Too Many Requests'; then
      sleep_for=65
      [ $attempt -ge 3 ] && sleep_for=610
      echo "[$(date -u +%H:%M:%S)] $crate hit 429; sleeping ${sleep_for}s"
      sleep $sleep_for
      attempt=$((attempt + 1))
      continue
    fi
    # already-published-this-version error is fine, skip
    if echo "$out" | grep -q 'already exists'; then
      echo "[$(date -u +%H:%M:%S)] $crate already at this version; skipping"
      break
    fi
    echo "[$(date -u +%H:%M:%S)] $crate failed with non-retryable error; stopping"
    exit 1
  done
  sleep 3
done

echo "[$(date -u +%H:%M:%S)] all 11 crates published"
