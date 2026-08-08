#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
HOST="$ROOT/host"
SERVER="$ROOT/server"
ENV="$HOST/.env.host"
[ -d "$SERVER" ] || { echo 'FAIL: generated server missing. Run ./host/scripts/prepare-server.sh'; exit 2; }
[ -f "$ENV" ] || { echo 'FAIL: .env.host missing. cp host/.env.host.example host/.env.host'; exit 2; }
grep -Eq '^DISCORD_OWNER_BOT_TOKEN=.+$' "$ENV" || { echo 'FAIL: owner bot token empty'; exit 2; }
grep -Eq '^DISCORD_SPEAKER_BOT_TOKEN_1=.+$' "$ENV" || { echo 'FAIL: speaker bot token 1 empty'; exit 2; }
grep -Eq '^LLB_RADIO_SECRET=[^[:space:]]{32,}$' "$ENV" || { echo 'FAIL: radio secret missing or too short'; exit 2; }
command -v docker >/dev/null || { echo 'FAIL: docker missing'; exit 2; }
docker info >/dev/null 2>&1 || { echo 'FAIL: docker daemon unavailable / permission denied'; exit 2; }
docker compose version >/dev/null 2>&1 || { echo 'FAIL: docker compose plugin missing'; exit 2; }

cd "$HOST"
export COMPOSE_PARALLEL_LIMIT=1
export DOCKER_BUILDKIT=1
printf '[BUILD] first smoke can take a while on old CPU; runtime image is much lighter than build stage.\n'
docker compose -f compose.yaml up -d --build bot

printf '[WAIT] radio API healthz\n'
for i in $(seq 1 60); do
  if command -v curl >/dev/null 2>&1 && curl -fsS -o /dev/null http://127.0.0.1:17777/healthz; then
    echo 'PASS: container + radio API healthy'
    docker compose -f compose.yaml ps
    docker stats --no-stream llb-command-radio || true
    exit 0
  fi
  sleep 1
done

echo 'FAIL: healthz not ready; recent logs:'
docker compose -f compose.yaml logs --tail 160 bot
exit 1
