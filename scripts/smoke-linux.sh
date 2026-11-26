#!/bin/sh
# Humo de la ventana en Linux: comprueba que el WebView se monta y pinta.
#
# Uso: xvfb-run -a -s "-screen 0 1280x1024x24" scripts/smoke-linux.sh target/release/orus-shell
#
# Falla si el log trae "can only contain one widget" (el WebView no montó y
# GTK rechazó el segundo widget) o si la captura de la ventana tiene 8 colores
# o menos (quedó el fondo del tema, como en la 0.6.0).
set -eu

# El humo corre sobre Xvfb, que solo entiende X11. Si el entorno trae
# WAYLAND_DISPLAY (por ejemplo WSL), tao puede abrir una superficie Wayland
# que xdotool no ve; se fuerza el backend X11 solo para esta prueba.
unset WAYLAND_DISPLAY

BIN="${1:-}"
if [ -z "$BIN" ]; then
  echo "uso: $0 <ruta-al-binario-orus-shell>" >&2
  exit 2
fi
if [ ! -x "$BIN" ]; then
  echo "no existe o no es ejecutable: $BIN" >&2
  exit 2
fi
if [ -z "${DISPLAY:-}" ]; then
  echo "DISPLAY vacío: corre el script bajo xvfb-run o con una sesión X" >&2
  exit 2
fi

FALTAN=$(ldd "$BIN" 2>&1 | grep "not found" || true)
if [ -n "$FALTAN" ]; then
  echo "faltan librerías del sistema para arrancar el binario:" >&2
  echo "$FALTAN" | sed 's/^/  /' >&2
  exit 1
fi

TITULO="Oruscribe Agente"
TEMPORALES=$(mktemp -d "${TMPDIR:-/tmp}/orus-smoke.XXXXXX")
LOG="$TEMPORALES/app.log"
CAPTURA="$TEMPORALES/ventana.xwd"
APP=""

limpiar() {
  if [ -n "$APP" ] && kill -0 "$APP" 2>/dev/null; then
    kill "$APP" 2>/dev/null || true
    wait "$APP" 2>/dev/null || true
  fi
  rm -rf "$TEMPORALES"
}
trap limpiar EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

# Perfil aislado: el bloqueo de instancia única y los datos no tocan el
# perfil de la persona que corre la prueba. Sin compositor (Xvfb), WebKit
# necesita el modo de composición por software para llegar a pintar.
ORUSCRIBE_DATA_DIR="$TEMPORALES/datos" \
WEBKIT_DISABLE_COMPOSITING_MODE=1 \
WINIT_UNIX_BACKEND=x11 \
GDK_BACKEND=x11 \
  "$BIN" >"$LOG" 2>&1 &
APP=$!

# Espera la ventana y vigila el error de GTK; si la app muere sin ventana,
# el bucle se agota y el log queda impreso en el error.
ID=""
INTENTO=0
while [ "$INTENTO" -lt 30 ]; do
  INTENTO=$((INTENTO + 1))
  if grep -q "can only contain one widget" "$LOG" 2>/dev/null; then
    echo "el WebView no montó: el log dice 'can only contain one widget'" >&2
    sed 's/^/  /' "$LOG" >&2
    exit 1
  fi
  ID=$(xdotool search --name "$TITULO" 2>/dev/null | head -n 1 || true)
  if [ -n "$ID" ]; then
    break
  fi
  sleep 1
done

if [ -z "$ID" ]; then
  echo "no apareció una ventana llamada '$TITULO' en 30 s" >&2
  sed 's/^/  /' "$LOG" >&2
  exit 1
fi

# La ventana existe antes de que WebKit pinte; se espera a que la captura
# deje de ser plana en vez de dormir un tiempo fijo.
COLORES=0
CAPTURADA=0
INTENTO=0
while [ "$INTENTO" -lt 20 ]; do
  INTENTO=$((INTENTO + 1))
  if grep -q "can only contain one widget" "$LOG" 2>/dev/null; then
    echo "el WebView no montó: el log dice 'can only contain one widget'" >&2
    sed 's/^/  /' "$LOG" >&2
    exit 1
  fi
  if ! xwd -silent -id "$ID" >"$CAPTURA" 2>/dev/null; then
    sleep 1
    continue
  fi
  CAPTURADA=1
  COLORES=$(convert "$CAPTURA" -format %k info: 2>/dev/null || echo 0)
  if [ "$COLORES" -gt 8 ]; then
    break
  fi
  sleep 1
done

if [ "$CAPTURADA" -eq 0 ]; then
  echo "no se pudo capturar la ventana $ID con xwd" >&2
  sed 's/^/  /' "$LOG" >&2
  exit 1
fi

if [ "$COLORES" -le 8 ]; then
  echo "la ventana está vacía: $COLORES colores (esperaba más de 8)" >&2
  sed 's/^/  /' "$LOG" >&2
  exit 1
fi

if grep -q "can only contain one widget" "$LOG"; then
  echo "el WebView no montó: el log dice 'can only contain one widget'" >&2
  exit 1
fi

if ! kill -0 "$APP" 2>/dev/null; then
  echo "la app murió después de pintar la ventana" >&2
  sed 's/^/  /' "$LOG" >&2
  exit 1
fi

if grep -Eq "panicked at|stack backtrace" "$LOG"; then
  echo "el log trae un pánico" >&2
  sed 's/^/  /' "$LOG" >&2
  exit 1
fi

descendientes() {
  ps -e -o pid=,ppid=,comm= | awk -v raiz="$1" '
    { hijo[$1] = $2; nombre[$1] = $3 }
    END {
      for (p in hijo) {
        q = p
        while (q != "" && q != 0 && q != 1) {
          if (q == raiz) { print nombre[p]; break }
          q = hijo[q]
        }
      }
    }'
}

WEBKIT=0
INTENTO=0
while [ "$INTENTO" -lt 10 ]; do
  INTENTO=$((INTENTO + 1))
  if descendientes "$APP" | grep -q "^WebKitWebProces"; then
    WEBKIT=1
    break
  fi
  sleep 1
done
if [ "$WEBKIT" -eq 0 ]; then
  echo "WebKit no arrancó: la app no tiene un WebKitWebProcess hijo" >&2
  sed 's/^/  /' "$LOG" >&2
  exit 1
fi

echo "ok: ventana '$TITULO' con WebKit y $COLORES colores"
