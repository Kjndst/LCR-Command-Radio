#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BASE='9845af2e26d7e39ad15843a674102311b1ef43df'
URL="https://github.com/sealbro/go-discord-caller/archive/${BASE}.tar.gz"
SERVER="$ROOT/server"
PATCHER="$ROOT/host/bin/patch-server"
REFRESH=0
[ "${1:-}" = '--refresh' ] && REFRESH=1

command -v curl >/dev/null || { echo 'FAIL: curl not found'; exit 2; }
command -v tar >/dev/null || { echo 'FAIL: tar not found'; exit 2; }
[ -x "$PATCHER" ] || { echo "FAIL: patcher missing/not executable: $PATCHER"; exit 2; }

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
mkdir -p "$TMP/extract"
tar -xzf "$TMP/upstream.tar.gz" -C "$TMP/extract"
SRC="$(find "$TMP/extract" -mindepth 1 -maxdepth 1 -type d | head -n 1)"
[ -n "$SRC" ] || { echo 'FAIL: archive has no source dir'; exit 4; }
mv "$SRC" "$SERVER"
printf '[PATCH] guarded Command Radio overlay\n'
"$PATCHER" -repo "$SERVER" -project "$ROOT"
printf '%s' "$BASE" > "$SERVER/.llb-generated-base"
echo 'PASS: patched server source ready'
echo "server: $SERVER"
