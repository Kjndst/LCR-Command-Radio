#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
HOST="$ROOT/host"
source "$HOST/scripts/runtime-env.sh"
cd "$HOST"
command -v docker >/dev/null || { echo 'FAIL: docker not found'; exit 2; }
docker info >/dev/null 2>&1 || { echo 'FAIL: docker daemon unavailable / permission denied'; exit 2; }
ensure_lcr_runtime_env "$HOST"
docker compose --env-file .env.runtime -f compose.yaml ps
printf '\n[RESOURCE SNAPSHOT]\n'; docker stats --no-stream llb-command-radio || true
printf '\n[HEALTH]\n'; curl -sS -i --max-time 3 http://127.0.0.1:17777/healthz | head -n 8 || true
printf '\n[LOG TAIL]\n'; docker compose --env-file .env.runtime -f compose.yaml logs --tail 80 bot
