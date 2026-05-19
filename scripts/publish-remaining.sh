#!/usr/bin/env bash
# Resume the crates.io publish chain after a 429 rate-limit hit.
# Retries each remaining crate with backoff on 429, in dependency order.
#
# Already published in this session:
#   merlion-core, merlion-config, merlion-memory, merlion-skills, merlion-session
# Remaining (this script):
#   merlion-llm, merlion-mcp, merlion-tools, merlion-gateway, merlion-cron,
#   merlion-agent

set -u

REMAINING=(merlion-llm merlion-mcp merlion-tools merlion-gateway merlion-cron merlion-agent)

# Wait until the deadline from the 429 response, plus a small safety margin.
echo "[$(date -u +%H:%M:%S)] sleeping 540s until rate-limit resets"
sleep 540

for crate in "${REMAINING[@]}"; do
  attempt=1
  while true; do
    echo "[$(date -u +%H:%M:%S)] publish $crate (attempt $attempt)"
    out=$(cargo publish -p "$crate" --no-verify 2>&1)
    status=$?
    echo "$out" | tail -3
    if [ $status -eq 0 ]; then
      break
    fi
    if echo "$out" | grep -q '429 Too Many Requests'; then
      # crates.io's burst window is ~10 min; sleep 65s between burst items,
      # ~610s if we hit the burst ceiling.
      if [ $attempt -ge 3 ]; then
        echo "[$(date -u +%H:%M:%S)] $crate hit 429 thrice — sleeping 610s"
        sleep 610
      else
        sleep 65
      fi
      attempt=$((attempt + 1))
      continue
    fi
    # Non-429 error — bail.
    echo "[$(date -u +%H:%M:%S)] $crate failed with non-429 error; stopping"
    exit 1
  done
  # Inter-crate cooldown to stay under the steady-state rate.
  sleep 5
done

echo "[$(date -u +%H:%M:%S)] all 11 crates published"
