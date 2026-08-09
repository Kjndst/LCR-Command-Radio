#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
HOST="$ROOT/host"
source "$HOST/scripts/runtime-env.sh"
cd "$HOST"
command -v docker >/dev/null || { echo 'FAIL: docker not found'; exit 2; }
ensure_lcr_runtime_env "$HOST"
docker compose --env-file .env.runtime -f compose.yaml stop bot 2>/dev/null || true
echo 'PASS: bot stopped (or was already stopped)'
