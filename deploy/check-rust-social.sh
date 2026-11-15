#!/usr/bin/env bash
# Run both backends on loopback against an empty PostgreSQL cluster on this host.
set -euo pipefail
if (( $# != 6 )); then
  printf 'Usage: %s CHECKOUT PYTHON RUST_BINARY CATALOG DOCUMENTS TEST_FILES\n' "$0" >&2
  exit 2
fi
checkout="$1" python="$2" binary="$3" catalog="$4" documents="$5" test_files="$6"
pgbin="${PGBIN:-/usr/lib/postgresql/16/bin}"
pg_port=${SOCIAL_PG_PORT:-55431}
flask_port=${SOCIAL_FLASK_PORT:-55432}
internal_port=${SOCIAL_RUST_INTERNAL_PORT:-55433}
rust_port=${SOCIAL_RUST_PORT:-55434}
for port in "$pg_port" "$flask_port" "$internal_port" "$rust_port"; do
  [[ "$port" =~ ^[0-9]{4,5}$ ]] && (( port >= 1024 && port <= 65535 )) || exit 2
done
work="$(mktemp -d /tmp/wiring-rust-social.XXXXXXXX)"
test "$EUID" -eq 0
chmod 755 "$work"
install -d -o postgres -g postgres -m 700 "$work/db"
install -d -m 700 "$work/uploads"
install -d -m 700 "$work/media-cache"
flask_pid= rust_pid= nginx_pid=
cleanup() {
  local status=$?
  if [[ -n "$nginx_pid" ]]; then kill "$nginx_pid" 2>/dev/null || true; wait "$nginx_pid" 2>/dev/null || true; fi
  if [[ -n "$rust_pid" ]]; then kill "$rust_pid" 2>/dev/null || true; wait "$rust_pid" 2>/dev/null || true; fi
  if [[ -n "$flask_pid" ]]; then kill "$flask_pid" 2>/dev/null || true; wait "$flask_pid" 2>/dev/null || true; fi
  runuser -u postgres -- "$pgbin/pg_ctl" -D "$work/db" -m immediate -w stop >/dev/null 2>&1 || true
  if (( status != 0 )); then
    for file in flask.log rust.log db/db.log; do test ! -f "$work/$file" || tail -n 30 "$work/$file" >&2; done
  fi
  rm -rf -- "$work"
}
trap cleanup EXIT
runuser -u postgres -- "$pgbin/initdb" -U fixture_admin -A trust --no-locale -E UTF8 -D "$work/db" >/dev/null
runuser -u postgres -- "$pgbin/pg_ctl" -D "$work/db" -l "$work/db/db.log" -o "-h 127.0.0.1 -p $pg_port -k $work/db -c shared_buffers=16MB" -w start >/dev/null
url=postgresql://fixture_admin@127.0.0.1:$pg_port/wiring_rust_test
"$pgbin/createdb" -h 127.0.0.1 -p "$pg_port" -U fixture_admin wiring_rust_test
cd "$checkout"
settings=(PATH=/usr/bin:/bin "DATABASE_URL=$url" "UPLOAD_DIR=$work/uploads" APP_SECRET_KEY=isolated-fixture-only
  SITE_URL=https://wiring.club SESSION_COOKIE_SECURE=1 OPENAI_API_KEY= RESEND_API_KEY= SMTP_HOST= TESTING=1)
env -i "${settings[@]}" "$python" scripts/ensure_dev_user.py >/dev/null
"$pgbin/psql" -X -v ON_ERROR_STOP=1 "$url" -qc 'CREATE ROLE wiring_chat_writer LOGIN; CREATE ROLE wiring_purge_worker LOGIN'
for grants in "$test_files"/grants/*.sql; do "$pgbin/psql" -X -v ON_ERROR_STOP=1 "$url" -qf "$grants"; done
env -i "${settings[@]}" "$python" -c 'import sys; from app import app; app.run(host="127.0.0.1", port=int(sys.argv[1]), threaded=True, use_reloader=False)' "$flask_port" > "$work/flask.log" 2>&1 &
flask_pid=$!
env -i "${settings[@]}" "WIRING_PLACES_BIND=127.0.0.1:$internal_port" "WIRING_PUBLIC_BIND=127.0.0.1:$rust_port" \
  "WIRING_PLACES_CATALOG=$catalog" "WIRING_PUBLIC_DOCUMENTS=$documents" \
  "WIRING_CHAT_DATABASE_URL=postgresql://wiring_chat_writer@127.0.0.1:$pg_port/wiring_rust_test" \
  WIRING_CHAT_INTERNAL_TOKEN=synthetic-catalog-check-no-real-secrets \
  WIRING_PUBLIC_SERVICE_INFO=1 WIRING_PUBLIC_PROFILE_READ=1 WIRING_PUBLIC_PEOPLE_READ=1 \
  WIRING_PUBLIC_MESSAGE_EDITS=1 WIRING_PUBLIC_FEED_DELIVERY=1 WIRING_PUBLIC_SUPPORT_REPLIES=1 \
  WIRING_PUBLIC_ALBUMS=1 WIRING_PUBLIC_ONBOARD_SKIP=1 WIRING_PUBLIC_PLACES=1 WIRING_PUBLIC_PREMIUM=1 \
  WIRING_PUBLIC_NOTIFICATION_SETTINGS=1 WIRING_PUBLIC_CHAT_HISTORY=1 \
  WIRING_PUBLIC_ARCHIVE=1 \
  WIRING_PUBLIC_MESSAGE_SEND=1 WIRING_MESSAGE_TEST_NO_PUSH=1 WIRING_PUBLIC_SWIPE=1 \
  WIRING_PUBLIC_RECOMMENDATIONS=1 \
  "WIRING_PUBLIC_MEDIA=$([[ ${SOCIAL_CHECKS:-} == media ]] && printf 1 || printf 0)" \
  "THUMB_DIR=$work/media-cache" "WIRING_PUBLIC_STATIC_DIR=$checkout/public" \
  "WIRING_PUBLIC_DEMO=$([[ ${SOCIAL_CHECKS:-} == media ]] && printf 1 || printf 0)" \
  "WIRING_PUBLIC_REPORT=$([[ ${SOCIAL_CHECKS:-} == media ]] && printf 1 || printf 0)" \
  "WIRING_PUBLIC_MESSAGE_DELETE=$([[ ${SOCIAL_FILE_WRITE_CHECKS:-} == 1 ]] && printf 1 || printf 0)" \
  "WIRING_PUBLIC_TRANSCRIBE=$([[ ${SOCIAL_FILE_WRITE_CHECKS:-} == 1 ]] && printf 1 || printf 0)" \
  "WIRING_PUBLIC_SUPPORT_FORM=$([[ ${SOCIAL_SUPPORT_CHECKS:-} == 1 ]] && printf 1 || printf 0)" \
  "WIRING_PUBLIC_PHOTO_DELETE=$([[ ${SOCIAL_PHOTO_DELETE_CHECKS:-} == 1 ]] && printf 1 || printf 0)" \
  "WIRING_PUBLIC_VOICE_UPLOAD=$([[ ${SOCIAL_VOICE_UPLOAD_CHECKS:-} == 1 ]] && printf 1 || printf 0)" \
  "WIRING_PUBLIC_PHOTO_UPLOAD=$([[ ${SOCIAL_PHOTO_UPLOAD_CHECKS:-} == 1 ]] && printf 1 || printf 0)" \
  "WIRING_PUBLIC_SPA=$([[ ${SOCIAL_SUPPORT_CHECKS:-} == 1 ]] && printf 1 || printf 0)" \
  "WIRING_PUBLIC_ADMIN_ACTIONS=$([[ ${SOCIAL_ADMIN_CHECKS:-} == 1 ]] && printf 1 || printf 0)" \
  "WIRING_PUBLIC_ADMIN_PAGE=$([[ ${SOCIAL_ADMIN_PAGE_CHECKS:-} == 1 ]] && printf 1 || printf 0)" \
  "WIRING_PUBLIC_ADMIN_REPLIES=$([[ ${SOCIAL_ADMIN_REPLY_CHECKS:-} == 1 ]] && printf 1 || printf 0)" \
  ADMIN_TOKEN=synthetic-admin-key-for-scratch \
  WIRING_PUBLIC_BROWSER_PUSH=1 WIRING_PUBLIC_REWIND=1 WIRING_PUBLIC_PHOTO_METADATA=1 WIRING_PUBLIC_DISCOVERY=1 \
  "WIRING_PUBLIC_PLACE_VALIDATION=$([[ -f "$documents/place-countries.json" ]] && printf 1 || printf 0)" \
  "WIRING_PUBLIC_PROFILE_EDIT=$([[ -f "$documents/place-countries.json" ]] && printf 1 || printf 0)" \
  "$binary" > "$work/rust.log" 2>&1 &
rust_pid=$!
ready=0
for attempt in {1..20}; do
  kill -0 "$flask_pid" && kill -0 "$rust_pid" || exit 1
  if curl -fsS "http://127.0.0.1:$flask_port/health" -o /dev/null 2>/dev/null && curl -fsS "http://127.0.0.1:$rust_port/health" -o /dev/null 2>/dev/null; then ready=1; break; fi
  sleep 1
done
test "$ready" = 1
media_port=${SOCIAL_MEDIA_NGINX_PORT:-}
if [[ -n "$media_port" ]]; then
  [[ "$media_port" =~ ^[0-9]{4,5}$ ]] && (( media_port >= 1024 && media_port <= 65535 )) || exit 2
  install -d -m 755 "$work/nginx"
  sed -e "s|/etc/nginx/snippets/wiring-rust-public.conf|$work/nginx/proxy.conf|g" \
      -e "s|/var/lib/wiring/uploads|$work/uploads|g" -e "s|/var/lib/wiring/rust-media-cache|$work/media-cache|g" \
      -e "s|/var/www/wiring-static|$checkout/public|g" "$test_files/nginx-wiring-rust-media-routes.conf" > "$work/nginx/media.conf"
  printf 'proxy_pass http://127.0.0.1:%s;\nproxy_http_version 1.1;\nproxy_set_header Host $host;\n' "$rust_port" > "$work/nginx/proxy.conf"
  cat > "$work/nginx/nginx.conf" <<NGINX
daemon off;
master_process off;
error_log stderr;
pid $work/nginx/nginx.pid;
events {}
http {
  access_log off;
  include /etc/nginx/mime.types;
  server { listen 127.0.0.1:$media_port; include $work/nginx/media.conf; }
}
NGINX
  nginx -c "$work/nginx/nginx.conf" -p "$work/nginx/" > "$work/nginx.log" 2>&1 &
  nginx_pid=$!
  for attempt in {1..10}; do
    kill -0 "$nginx_pid" || { cat "$work/nginx.log" >&2; exit 1; }
    curl -sS --max-time 2 "http://127.0.0.1:$media_port/" -o /dev/null 2>/dev/null && break
    sleep 1
  done
fi
env -i "${settings[@]}" "SOCIAL_FILE_WRITE_CHECKS=${SOCIAL_FILE_WRITE_CHECKS:-0}" "SOCIAL_MEDIA_NGINX_PORT=$media_port" "SOCIAL_SUPPORT_CHECKS=${SOCIAL_SUPPORT_CHECKS:-0}" \
  "SOCIAL_PHOTO_DELETE_CHECKS=${SOCIAL_PHOTO_DELETE_CHECKS:-0}" \
  "SOCIAL_VOICE_UPLOAD_CHECKS=${SOCIAL_VOICE_UPLOAD_CHECKS:-0}" \
  "SOCIAL_PHOTO_UPLOAD_CHECKS=${SOCIAL_PHOTO_UPLOAD_CHECKS:-0}" \
  "SOCIAL_ADMIN_CHECKS=${SOCIAL_ADMIN_CHECKS:-0}" \
  "SOCIAL_ADMIN_PAGE_CHECKS=${SOCIAL_ADMIN_PAGE_CHECKS:-0}" \
  "SOCIAL_ADMIN_REPLY_CHECKS=${SOCIAL_ADMIN_REPLY_CHECKS:-0}" \
  "$python" "$test_files/compare_social_public.py" "$flask_port" "$rust_port" "${SOCIAL_CHECKS:-albums}"
if [[ "${SOCIAL_CHECKS:-}" == purge ]]; then
    dev_id="$("$pgbin/psql" -X -v ON_ERROR_STOP=1 "$url" -qAtc "SELECT id FROM users WHERE email='dev@wiring.test'")"
    peer_id="$("$pgbin/psql" -X -v ON_ERROR_STOP=1 "$url" -qAtc "SELECT id FROM users WHERE email='peer@wiring.test'")"
    mkdir -p "$work/uploads/chat/$peer_id" "$work/uploads/$dev_id" "$work/uploads/$peer_id"
    printf 'fixture\n' > "$work/uploads/chat/$peer_id/retained-only-until-account-erasure.jpg"
    printf 'fixture\n' > "$work/uploads/$dev_id/remove.jpg"
    printf 'fixture\n' > "$work/uploads/$peer_id/keep.jpg"
    "$pgbin/psql" -X -v ON_ERROR_STOP=1 "$url" -q <<'SQL'
UPDATE users SET deleted_at=100 WHERE email='dev@wiring.test';
UPDATE users SET deleted_at=EXTRACT(EPOCH FROM now())::bigint WHERE email='peer@wiring.test';
INSERT INTO messages(from_id,to_id,body,photo,created_at)
SELECT peer.id,dev.id,'fixture','chat/'||peer.id||'/retained-only-until-account-erasure.jpg',100
FROM users dev CROSS JOIN users peer WHERE dev.email='dev@wiring.test' AND peer.email='peer@wiring.test';
SQL
    purge_url=postgresql://wiring_purge_worker@127.0.0.1:$pg_port/wiring_rust_test
    env -i "WIRING_PURGE_DATABASE_URL=$purge_url" "$binary" purge-accounts --plan | grep -Fq '"deleted_accounts":1'
    env -i "WIRING_PURGE_DATABASE_URL=$purge_url" "UPLOAD_DIR=$work/uploads" "$binary" purge-accounts --apply | grep -Fq '"deleted_accounts":1'
    "$pgbin/psql" -X -v ON_ERROR_STOP=1 "$url" -qAtc "SELECT count(*) FROM users WHERE email='peer@wiring.test' OR email LIKE 'deleted-%@deleted.wiring'" | grep -Fxq 2
    test ! -e "$work/uploads/$dev_id/remove.jpg"
    test ! -e "$work/uploads/chat/$peer_id/retained-only-until-account-erasure.jpg"
    test -e "$work/uploads/$peer_id/keep.jpg"
    env -i "WIRING_PURGE_DATABASE_URL=$purge_url" "$binary" purge-accounts --plan | grep -Fq '"deleted_accounts":0'
fi
if [[ "${SOCIAL_GUEST_CLEANUP_CHECKS:-}" == 1 ]]; then
    "$pgbin/psql" -X -v ON_ERROR_STOP=1 "$url" -q <<'SQL'
INSERT INTO users(email,password_hash,name,age,city,gender,looking_for,created_at) VALUES
 ('guest-expired@wiring.guest','fixture','Guest',28,'','other','everyone',100),
 ('guest-active@wiring.guest','fixture','Guest',28,'','other','everyone',EXTRACT(EPOCH FROM now())::bigint),
 ('guest-external@wiring.test','fixture','Member',28,'','other','everyone',100);
SQL
    purge_url=postgresql://wiring_purge_worker@127.0.0.1:$pg_port/wiring_rust_test
    env -i "WIRING_PURGE_DATABASE_URL=$purge_url" WIRING_PURGE_EXPIRED_GUESTS=1 "$binary" purge-accounts --plan | grep -Fq '"expired_guests":1'
    env -i "WIRING_PURGE_DATABASE_URL=$purge_url" "UPLOAD_DIR=$work/uploads" WIRING_PURGE_EXPIRED_GUESTS=1 "$binary" purge-accounts --apply | grep -Fq '"expired_guests":1'
    "$pgbin/psql" -X -v ON_ERROR_STOP=1 "$url" -qAtc "SELECT email FROM users WHERE email IN ('guest-expired@wiring.guest','guest-active@wiring.guest','guest-external@wiring.test') ORDER BY email" \
        | diff -u <(printf '%s\n' guest-active@wiring.guest guest-external@wiring.test) -
fi
if [[ "${SOCIAL_SCHEMA_CHECKS:-}" == 1 ]]; then
    "$pgbin/createdb" -h 127.0.0.1 -p "$pg_port" -U fixture_admin wiring_rust_schema
    fresh="postgresql://fixture_admin@127.0.0.1:$pg_port/wiring_rust_schema"
    env -i "WIRING_SCHEMA_DATABASE_URL=$fresh" "$binary" init-schema
    env -i "WIRING_SCHEMA_DATABASE_URL=$fresh" "$binary" init-schema
    for file in "$test_files"/grants/*.sql; do "$pgbin/psql" -X -v ON_ERROR_STOP=1 "$fresh" -qf "$file"; done
    "$pgbin/psql" -X -v ON_ERROR_STOP=1 "$fresh" -qAtc 'SELECT count(*) FROM users' | grep -Fxq 0
    "$pgbin/psql" -X -v ON_ERROR_STOP=1 "$fresh" -qAtc "SELECT count(*) FROM promo_codes WHERE code IN ('WIRINGPLUS','WIRINGFORYOU')" | grep -Fxq 2
    if env -i "WIRING_SCHEMA_DATABASE_URL=$url" "$binary" init-schema; then exit 1; fi
fi
