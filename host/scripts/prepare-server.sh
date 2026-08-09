#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BASE='9845af2e26d7e39ad15843a674102311b1ef43df'
URL="https://github.com/sealbro/go-discord-caller/archive/${BASE}.tar.gz"
SHA256='7bDED2590BB752D90F3D6A9F35DBB0BCAE43A503CFFE0E12E113D2B53059B557'
SERVER="$ROOT/server"
REFRESH=0
[ "${1:-}" = '--refresh' ] && REFRESH=1

command -v curl >/dev/null || { echo 'FAIL: curl not found'; exit 2; }
command -v tar >/dev/null || { echo 'FAIL: tar not found'; exit 2; }
command -v sha256sum >/dev/null || { echo 'FAIL: sha256sum not found'; exit 2; }
command -v docker >/dev/null || { echo 'FAIL: docker not found; the patcher runs in an ephemeral Go container'; exit 2; }
docker info >/dev/null 2>&1 || { echo 'FAIL: docker daemon unavailable / permission denied'; exit 2; }

run_patcher() {
  local user_args=()
  if [ "$(id -u)" -ne 0 ]; then user_args=(--user "$(id -u):$(id -g)"); fi
  docker run --rm "${user_args[@]}" \
    -e HOME=/tmp -e GOCACHE=/tmp/go-build -e GOPATH=/tmp/go \
    -v "$ROOT:/src" -w /src/host/patcher \
    golang:1.23-bookworm go run . -repo /src/server -project /src
}

if [ -e "$SERVER" ]; then
  if [ "$REFRESH" -ne 1 ]; then
    echo "FAIL: $SERVER already exists. Refusing overwrite. Use --refresh only intentionally."
    exit 3
  fi
  [ -f "$SERVER/.llb-generated-base" ] || { echo 'FAIL: server not positively identified as generated base'; exit 3; }
  [ "$(cat "$SERVER/.llb-generated-base")" = "$BASE" ] || { echo 'FAIL: generated-base marker mismatch'; exit 3; }
  rm -rf -- "$SERVER"
fi

TMP="$(mktemp -d)"; trap 'rm -rf -- "$TMP"' EXIT
printf '[DOWNLOAD] pinned upstream %s\n' "$BASE"
curl -fL --retry 3 --connect-timeout 15 --max-time 180 "$URL" -o "$TMP/upstream.tar.gz"
printf '%s  %s\n' "$SHA256" "$TMP/upstream.tar.gz" | sha256sum -c -
mkdir -p "$TMP/extract"
tar -xzf "$TMP/upstream.tar.gz" -C "$TMP/extract"
SRC="$(find "$TMP/extract" -mindepth 1 -maxdepth 1 -type d | head -n 1)"
[ -n "$SRC" ] || { echo 'FAIL: archive has no source dir'; exit 4; }
mv "$SRC" "$SERVER"
printf '[PATCH] guarded Command Radio overlay\n'
run_patcher
printf '%s' "$BASE" > "$SERVER/.llb-generated-base"
echo 'PASS: patched server source ready'
echo "server: $SERVER"
