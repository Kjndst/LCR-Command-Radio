#!/usr/bin/env bash
set -u
printf '=== LLB COMMAND RADIO — LINUX DOCKER PREFLIGHT (READ ONLY) ===\n'
printf 'time: %s\n\n' "$(date -Is 2>/dev/null || date)"
printf '[OS]\n'; uname -a || true
if [ -r /etc/os-release ]; then . /etc/os-release; printf '%s %s\n' "${PRETTY_NAME:-$NAME}" "${VERSION_ID:-}"; fi
printf '\n[ARCH / CPU]\n'; uname -m || true
if command -v lscpu >/dev/null 2>&1; then lscpu | grep -E 'Model name|CPU\(s\)|Thread|Core|Architecture' || true; fi
printf '\n[MEMORY]\n'; free -h || true
printf '\n[DISK]\n'; df -h / || true
printf '\n[DOCKER]\n'
if command -v docker >/dev/null 2>&1; then
  docker --version || true
  docker compose version || true
  printf '\n[DOCKER DAEMON]\n'
  docker info --format 'Server={{.ServerVersion}} Driver={{.Driver}} Cgroup={{.CgroupVersion}} CPUs={{.NCPU}} Mem={{.MemTotal}}' 2>&1 || true
else
  echo 'docker: NOT FOUND'
fi
printf '\n[PORT 17777]\n'
if command -v ss >/dev/null 2>&1; then ss -ltnp 2>/dev/null | grep ':17777' || echo 'free / not listening'; else echo 'ss unavailable'; fi
printf '\n[OUTBOUND]\n'
if command -v curl >/dev/null 2>&1; then curl -I -L --max-time 10 -sS https://github.com/ | head -n 1 || true; else echo 'curl: NOT FOUND'; fi
printf '\n=== END PREFLIGHT ===\n'
