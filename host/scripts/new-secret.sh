#!/usr/bin/env bash
set -euo pipefail
if command -v openssl >/dev/null 2>&1; then
  openssl rand -hex 32
else
  od -An -N32 -tx1 /dev/urandom | tr -d ' \n'
  echo
fi
