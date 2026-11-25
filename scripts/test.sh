#!/usr/bin/env bash
# Tiers de prueba de Oruscribe Agent.
#
#   scripts/test.sh unit          unitarias (lib + bins)      ~ segundos/minutos
#   scripts/test.sh integration   binarios de tests/           ~ lo que tarde
#   scripts/test.sh all           todo
#   scripts/test.sh smoke         humo de la ventana en Linux  ~ un minuto
#   scripts/test.sh release       todo + build release + smoke de Linux
#
# El humo corre contra el binario release; ORUS_BIN apunta a otro:
#   ORUS_BIN=/ruta/a/orus-shell scripts/test.sh smoke
#
# La regla: unitarias en cada cambio, integración antes del push, todo antes de
# sacar versión. Correr `all` en cada guardado es lo que hace que la suite dure
# una hora y nadie la corra.
set -euo pipefail
cd "$(dirname "$0")/.."

TIER="${1:-unit}"
BIN="${ORUS_BIN:-target/release/orus-shell}"

run() {
  local label="$1"; shift
  local t0; t0=$(date +%s)
  printf '\n== %s\n' "$label"
  "$@"
  printf '== %s: %ss\n' "$label" "$(( $(date +%s) - t0 ))"
}

PYTHON="$(command -v python3 || command -v python)"
run "lint de tests" "$PYTHON" scripts/lint-tests.py

smoke() {
  # `-f`, no `-x`: en git el script es 100644 y un clon limpio no lo marca
  # ejecutable; con `-x` el humo se saltaba en silencio. Se invoca con `sh`.
  if [ ! -f scripts/smoke-linux.sh ] || [ "$(uname -s)" != "Linux" ]; then
    echo "== smoke linux: omitido (no es Linux o falta el script)"
    return
  fi
  # Xvfb cuando exista: el humo captura la ventana con xwd, y sin servidor X
  # propio (CI, WSL, servidor sin sesión) eso falla con «no se pudo capturar».
  if command -v xvfb-run >/dev/null 2>&1; then
    run "smoke linux" xvfb-run -a -s "-screen 0 1280x1024x24" sh scripts/smoke-linux.sh "$BIN"
  else
    run "smoke linux" sh scripts/smoke-linux.sh "$BIN"
  fi
}

case "$TIER" in
  unit)
    run "frontend (tests y sintaxis)" bash scripts/test-frontend.sh
    run "unitarias (lib + bins)" cargo test --workspace --lib --bins --locked
    ;;
  integration)
    run "integración (tests/)" cargo test --workspace --tests --locked
    ;;
  all)
    run "todo (lib + bins + tests)" cargo test --workspace --locked
    ;;
  smoke)
    smoke
    ;;
  release)
    run "todo" cargo test --workspace --locked
    run "build release" cargo build --release --locked -p orus-shell
    smoke
    ;;
  *)
    echo "uso: $0 [unit|integration|all|smoke|release]" >&2
    exit 2
    ;;
esac
