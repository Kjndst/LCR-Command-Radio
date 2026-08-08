#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT/host"
docker compose -f compose.yaml ps
printf '\n[RESOURCE SNAPSHOT]\n'; docker stats --no-stream llb-command-radio || true
printf '\n[HEALTH]\n'; curl -sS -i --max-time 3 http://127.0.0.1:17777/healthz | head -n 8 || true
printf '\n[LOG TAIL]\n'; docker compose -f compose.yaml logs --tail 80 bot
