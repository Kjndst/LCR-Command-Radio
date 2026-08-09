#!/usr/bin/env bash
set -euo pipefail

ensure_lcr_runtime_env() {
  local root="${1:?deployment root is required}"
  local data_dir="$root/data"
  local runtime_env="$root/.env.runtime"

  mkdir -p "$data_dir"
  chmod u+rwx "$data_dir"
  [ -w "$data_dir" ] || {
    echo "FAIL: $data_dir is not writable by $(id -un). Fix this deployment-owned directory; do not use chmod 777."
    return 1
  }

  umask 077
  printf 'LCR_RUNTIME_UID=%s\nLCR_RUNTIME_GID=%s\n' "$(id -u)" "$(id -g)" > "$runtime_env"
  chmod 600 "$runtime_env"
}
