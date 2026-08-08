#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT/host"
command -v docker >/dev/null || { echo 'FAIL: docker not found'; exit 2; }
docker compose -f compose.yaml stop bot 2>/dev/null || true
echo 'PASS: bot stopped (or was already stopped)'
