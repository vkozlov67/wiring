#!/usr/bin/env bash
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
output=${1:?Usage: deploy/build-rust-release.sh OUTPUT_DIRECTORY}
mkdir -p "$output"
output="$(cd "$output" && pwd)"
if [[ -n "$(ls -A "$output")" ]]; then
  printf 'Release output directory must be empty: %s\n' "$output" >&2
  exit 1
fi

image=$(docker build --quiet --platform linux/amd64 --target places -f "$root/deploy/beta/Dockerfile" "$root")
container=$(docker create --platform linux/amd64 "$image")
trap 'docker rm "$container" >/dev/null' EXIT
docker cp "$container:/usr/local/bin/wiring-beta-places" "$output/wiring-beta-places"
docker cp "$container:/catalog" "$output/catalog"
docker cp "$container:/documents" "$output/documents"
chmod 755 "$output/wiring-beta-places"
