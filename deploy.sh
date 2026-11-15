#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
host=${REMOTE_HOST:-root@164.92.147.23}
port=${REMOTE_PORT:-22}
ssh_opts=(-p "$port" -o BatchMode=yes -o StrictHostKeyChecking=yes)
rsync_ssh="ssh -p $port -o BatchMode=yes -o StrictHostKeyChecking=yes"

if [[ -n "${WIRING_RUST_RELEASE_DIR:-}" ]]; then
  artifact=$WIRING_RUST_RELEASE_DIR
else
  artifact=$(mktemp -d "${TMPDIR:-/tmp}/wiring-build.XXXXXXXX")
  trap 'rm -rf "$artifact"' EXIT
  "$root/deploy/build-rust-release.sh" "$artifact"
fi
for path in "$artifact/wiring-beta-places" "$root/public/dist/app.js" "$root/public/landing.css" "$root/public/landing.js"; do
  test -s "$path" || { printf 'Missing release file: %s\n' "$path" >&2; exit 1; }
done
test -d "$artifact/catalog"
test -d "$artifact/documents"

candidate=$(ssh "${ssh_opts[@]}" "$host" bash -se <<'REMOTE'
test "$(uname -m)" = x86_64
test -L /opt/wiring-places/current
test -d /var/www/wiring-static
systemctl is-active --quiet wiring-places.service
mktemp -d /opt/wiring-places/releases/release.XXXXXXXX
REMOTE
)
if [[ ! "$candidate" =~ ^/opt/wiring-places/releases/release\.[[:alnum:]]{8}$ ]]; then
  printf 'Server did not return a Rust release directory\n' >&2
  exit 1
fi
rsync -rlt --no-links -e "$rsync_ssh" "$artifact/wiring-beta-places" "$artifact/catalog" "$artifact/documents" "$root/deploy/check-flask-retirement.sh" "$host:$candidate/"
rsync -rlt --no-links --exclude='.*' --chmod=D755,F644 -e "$rsync_ssh" "$root/public/" "$host:$candidate/site-static/"

ssh "${ssh_opts[@]}" "$host" bash -s -- "$candidate" <<'REMOTE'
set -eEuo pipefail
candidate=$1
current=/opt/wiring-places/current
static=/var/www/wiring-static
wait_for_rust() {
  for _ in {1..25}; do
    if curl -fsS --max-time 2 http://127.0.0.1:5082/health >/dev/null; then return; fi
    sleep 1
  done
  return 1
}
exec 9>/run/lock/wiring-cutover.lock
flock -n 9
# The release might have waited for another deploy to finish.
previous=$(readlink -f "$current")
bash "$candidate/check-flask-retirement.sh" --expect-stopped
test -d "$candidate/catalog"
test -d "$candidate/documents"
test -s "$candidate/site-static/dist/app.js"
chmod 755 "$candidate" "$candidate/wiring-beta-places"
chmod -R a+rX "$candidate/catalog" "$candidate/documents"
static_backup=$(mktemp -d /var/www/.wiring-static-before.XXXXXXXX)
cp -al "$static/." "$static_backup/"
activate() {
  ln -sfn "$1" "$candidate/current-link" &&
    mv -Tf "$candidate/current-link" "$current"
}
sync_static() {
  rsync -rlt --checksum --delay-updates --delete-delay --no-links --chmod=D755,F644 "$1/" "$static/"
}
rollback() {
  local code=$?
  trap - ERR
  set +e
  if activate "$previous" && sync_static "$static_backup" &&
     systemctl restart wiring-places.service && wait_for_rust; then
    printf 'Rust release failed; restored %s and the previous frontend\n' "$previous" >&2
  else
    printf 'Restore did not complete; previous release: %s; frontend snapshot: %s\n' "$previous" "$static_backup" >&2
  fi
  exit "$code"
}
trap rollback ERR
activate "$candidate"
sync_static "$candidate/site-static"
systemctl restart wiring-places.service
wait_for_rust
bash "$candidate/check-flask-retirement.sh" --expect-stopped
for path in /m /public/dist/app.js /public/landing.css /public/landing.js; do
  curl -fsS --max-time 8 --resolve wiring.club:443:127.0.0.1 "https://wiring.club$path" -o /dev/null
done
trap - ERR
rm -rf "$static_backup"
printf 'Published Rust and frontend: %s (previous: %s)\n' "$candidate" "$previous"
REMOTE

date_headers=$(curl -fsSI --max-time 10 https://wiring.date/)
grep -Eqi '^location: https://wiring.club/' <<< "$date_headers"
grep -Eq '^HTTP/.* (301|308)' <<< "$date_headers"
landing_html=$(curl -fsS --max-time 10 https://wiring.club/m)
grep -q 'id="hero-title"' <<< "$landing_html"
printf '%s' "$landing_html" | python3 "$root/scripts/check_apk_downloads.py" https://wiring.club/m
