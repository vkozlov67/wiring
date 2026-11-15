#!/usr/bin/env bash
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
tmp="$(mktemp -d "${TMPDIR:-/tmp}/wiring-api.XXXXXXXX")"
suffix=${tmp##*.}
project="wiring-api-$(printf '%s' "$suffix" | tr '[:upper:]' '[:lower:]')"
export WIRING_LOCAL_SESSION_KEY=synthetic-test-only-session-key-not-for-production
export WIRING_LOCAL_ADMIN_TOKEN=synthetic-test-only-admin-token-not-for-production
export WIRING_LOCAL_PORT=5070 WIRING_LOCAL_PUBLISHED_PORT=0 WIRING_LOCAL_GOOGLE_OAUTH_ENABLED=1
compose=(docker compose --env-file /dev/null -f "$root/docker-compose.yml" -p "$project" --profile test)
cleanup() {
  local code=$?
  if (( code != 0 )); then "${compose[@]}" logs --tail=45 postgres schema fixtures rust proxy >&2 || true; fi
  "${compose[@]}" down --volumes --remove-orphans >/dev/null 2>&1 || true
  rm -rf -- "$tmp"
}
trap cleanup EXIT
"${compose[@]}" up --build --wait --wait-timeout 100 proxy
"${compose[@]}" run --build --rm --no-deps api-tests
