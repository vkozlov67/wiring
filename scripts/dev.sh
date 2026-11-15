#!/usr/bin/env bash
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"
umask 077
secrets="$root/data/local-rust.env"
unset WIRING_LOCAL_SESSION_KEY WIRING_LOCAL_ADMIN_TOKEN
export WIRING_LOCAL_PORT=5070 WIRING_LOCAL_PUBLISHED_PORT=5070 WIRING_LOCAL_GOOGLE_OAUTH_ENABLED=0
mkdir -p "$(dirname "$secrets")"
if [[ ! -e "$secrets" ]]; then
  tmp=$(mktemp "${secrets}.XXXXXXXX")
  trap 'rm -f -- "$tmp"' EXIT
  printf 'WIRING_LOCAL_SESSION_KEY=%s\nWIRING_LOCAL_ADMIN_TOKEN=%s\n' "$(openssl rand -hex 32)" "$(openssl rand -hex 24)" > "$tmp"
  ln "$tmp" "$secrets" 2>/dev/null || test -f "$secrets"
fi
compose=(docker compose --env-file "$secrets" -f "$root/docker-compose.yml" -p wiring-local)
case "${1:-start}" in
  start)
    "${compose[@]}" up --build --wait --wait-timeout 100 proxy
    printf 'WIRING: http://127.0.0.1:5070/\nАдминка: http://127.0.0.1:5070/admin (токен в data/local-rust.env)\n'
    ;;
  stop) "${compose[@]}" down ;;
  logs) "${compose[@]}" logs --follow rust proxy ;;
  *) printf 'Usage: %s [start|stop|logs]\n' "$0" >&2; exit 2 ;;
esac
