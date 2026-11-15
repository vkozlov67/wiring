#!/usr/bin/env bash
# Read-only check to run on the production host before and after stopping wiring.service.
set -euo pipefail

expect_stopped=false
case "${1:-}" in
  --expect-stopped) expect_stopped=true ;;
  '') ;;
  *) printf 'Usage: %s [--expect-stopped]\n' "$0" >&2; exit 2 ;;
esac

failed=0
if $expect_stopped; then
  active=$(systemctl show -p ActiveState --value wiring.service)
  load=$(systemctl show -p LoadState --value wiring.service)
  enabled=$(systemctl show -p UnitFileState --value wiring.service)
  if [[ "$active" != inactive ||
        ( "$load" != not-found && "$enabled" != disabled && "$enabled" != masked ) ]] ||
     ! listeners=$(ss -ltnH 'sport = :5070') || [[ -n "$listeners" ]]; then
    printf 'Flask must be stopped and disabled or absent, with no listener on port 5070\n' >&2
    failed=1
  fi
  if ! systemctl is-active --quiet wiring-places.service; then
    printf 'Rust is not running\n' >&2
    failed=1
  fi
fi

for feature in photo-upload transcribe admin-replies; do
  if [[ ! -f "/etc/systemd/system/wiring-places.service.d/$feature.conf" ]]; then
    printf 'Still requires Flask: %s\n' "$feature" >&2
    failed=1
  fi
done

if ! systemctl is-active --quiet wiring-sendpulse.timer ||
   ! systemctl show wiring-sendpulse.service --property=ExecStart --value | grep -Fq 'sync-sendpulse'; then
  printf 'SendPulse timer is not running the Rust worker\n' >&2
  failed=1
fi

if ! nginx_config=$(nginx -T 2>&1); then
  printf 'Could not validate nginx configuration\n' >&2
  exit 1
fi
if awk '
  /^# configuration file / { file=$0 }
  /^[^#]*127[.]0[.]0[.]1:5070([^0-9]|$)/ {
    if (!seen[file]++) print "Still refers to Flask: " file > "/dev/stderr"
    found=1
  }
  END { exit !found }' <<< "$nginx_config"; then
  failed=1
fi

for host in wiring.club wiring.date; do
  catalog=$(curl -fsS --max-time 8 --resolve "$host:443:127.0.0.1" "https://$host/api/catalog") || exit 1
  if ! python3 -c 'import json,sys; sys.exit(json.load(sys.stdin).get("ok") is not True)' <<< "$catalog"; then
    printf 'Catalog failed on %s\n' "$host" >&2
    failed=1
  fi
done

(( failed == 0 )) || exit 1
printf 'Both production catalogs respond; nginx has no Flask upstreams and the Rust integrations are configured.\n'
if $expect_stopped; then
  printf 'Flask is inactive and disabled or absent; port 5070 has no listener.\n'
fi
