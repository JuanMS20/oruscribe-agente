#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

if ! command -v node >/dev/null 2>&1; then
  echo "node no disponible" >&2
  exit 1
fi

node --test frontend/tests/*.test.mjs

while IFS= read -r -d '' modulo; do
  node --input-type=module --check < "$modulo"
done < <(find frontend/dist/js -type f \( -name '*.js' -o -name '*.mjs' \) -print0)
