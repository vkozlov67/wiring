#!/usr/bin/env bash
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"
if [[ "${CI:-}" == true || ! -d node_modules ]]; then npm ci; fi
npm run typecheck
npm run build
for name in router api i18n profile deck chat-gesture analytics inbox push; do
  npm run "test:$name"
done
"${PYTHON:-python3}" -m unittest discover -s tests -p test_i18n.py -q
./scripts/test-api.sh
