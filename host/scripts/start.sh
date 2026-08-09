#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
HOST="$ROOT/host"
ENV="$HOST/.env.host"
source "$HOST/scripts/runtime-env.sh"
[ -f "$ENV" ] || { echo 'FAIL: .env.host missing. cp host/.env.host.example host/.env.host'; exit 2; }
grep -Eq '^DISCORD_OWNER_BOT_TOKEN=.+$' "$ENV" || { echo 'FAIL: owner bot token empty'; exit 2; }
grep -Eq '^DISCORD_SPEAKER_BOT_TOKEN_1=.+$' "$ENV" || { echo 'FAIL: speaker bot token 1 empty'; exit 2; }
grep -Eq '^DISCORD_SPEAKER_BOT_TOKEN_2=.+$' "$ENV" || { echo 'FAIL: speaker bot token 2 empty'; exit 2; }
grep -Eq '^DISCORD_SPEAKER_BOT_TOKEN_3=.+$' "$ENV" || { echo 'FAIL: speaker bot token 3 empty'; exit 2; }
grep -Eq '^LLB_RADIO_SECRET=[^[:space:]]{32,}$' "$ENV" || { echo 'FAIL: radio secret missing or too short'; exit 2; }
command -v docker >/dev/null || { echo 'FAIL: docker missing'; exit 2; }
docker info >/dev/null 2>&1 || { echo 'FAIL: docker daemon unavailable / permission denied'; exit 2; }
docker compose version >/dev/null 2>&1 || { echo 'FAIL: docker compose plugin missing'; exit 2; }
docker image inspect llb-command-radio:0.3.2 >/dev/null 2>&1 || {
  echo 'FAIL: required image llb-command-radio:0.3.2 is not loaded.'
  echo 'Load the approved image.tar or run an explicit developer source-build workflow; start.sh never builds images.'
  exit 2
}

ensure_lcr_runtime_env "$HOST"

cd "$HOST"
printf '[START] using preloaded llb-command-radio:0.3.2 (no build)\n'
docker compose --env-file .env.runtime -f compose.yaml up -d --no-build bot

printf '[WAIT] radio API healthz\n'
for i in $(seq 1 60); do
  if command -v curl >/dev/null 2>&1 && curl -fsS -o /dev/null http://127.0.0.1:17777/healthz; then
    echo 'PASS: container + radio API healthy'
    docker compose --env-file .env.runtime -f compose.yaml ps
    docker stats --no-stream llb-command-radio || true
    exit 0
  fi
  sleep 1
done

echo 'FAIL: healthz not ready; recent logs:'
docker compose --env-file .env.runtime -f compose.yaml logs --tail 160 bot
exit 1
