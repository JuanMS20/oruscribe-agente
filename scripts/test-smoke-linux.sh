#!/bin/sh
# Prueba del propio humo de Linux con aplicaciones de mentira: la que debe pasar
# pasa y cada fallo conocido se detecta con su mensaje.
#
# Uso: sh scripts/test-smoke-linux.sh
# Requiere: xvfb-run, xdotool, xwd (x11-apps), imagemagick y procps.
set -u

SMOKE="$(cd "$(dirname "$0")" && pwd)/smoke-linux.sh"
T=$(mktemp -d "${TMPDIR:-/tmp}/humo-prueba.XXXXXX")
trap 'rm -rf "$T"' EXIT
FALLOS=0

cp "$(command -v sleep)" "$T/WebKitWebProcess"
convert -size 300x300 gradient:red-blue "$T/img.png"
mkdir "$T/bin"

app() {
  nombre="$1"; prologo="$2"; ventana="$3"; webkit="$4"
  {
    echo '#!/bin/sh'
    [ -n "$prologo" ] && echo "$prologo"
    [ "$ventana" = 1 ] && echo "display -title 'Oruscribe Agente' $T/img.png &"
    [ "$webkit" = 1 ] && echo "$T/WebKitWebProcess 300 &"
    echo 'exec sleep 300'
  } > "$T/$nombre"
  chmod +x "$T/$nombre"
}

app ok '' 1 1
app sin_webkit '' 1 0
app con_panico "echo \"thread 'main' panicked at src/x.rs:1:1\" >&2" 1 1
app widget 'echo "GtkWindow can only contain one widget at a time" >&2' 0 0
app sin_ventana '' 0 0
printf '#!/bin/sh\necho "\tlibwebkit2gtk-4.1.so.0 => not found"\n' > "$T/bin/ldd"
chmod +x "$T/bin/ldd"

correr() {
  nombre="$1"; codigo_esperado="$2"; texto_esperado="$3"; shift 3
  salida=$("$@" 2>&1); codigo=$?
  if [ "$codigo" -ne "$codigo_esperado" ] || ! echo "$salida" | grep -q "$texto_esperado"; then
    echo "FALLA $nombre: exit=$codigo (esperaba $codigo_esperado) y el texto '$texto_esperado' no aparece" >&2
    echo "$salida" | sed 's/^/  /' >&2
    FALLOS=$((FALLOS + 1))
  else
    echo "ok $nombre"
  fi
}

humo() { xvfb-run -a -s "-screen 0 1280x1024x24" sh "$SMOKE" "$@"; }

correr ok 0 "con WebKit" humo "$T/ok"
correr sin_webkit 1 "WebKit no arrancó" humo "$T/sin_webkit"
correr con_panico 1 "trae un pánico" humo "$T/con_panico"
correr widget 1 "can only contain one widget" humo "$T/widget"
correr sin_ventana 1 "no apareció una ventana" humo "$T/sin_ventana"
correr ldd_falta 1 "faltan librerías" env PATH="$T/bin:$PATH" xvfb-run -a -s "-screen 0 1280x1024x24" sh "$SMOKE" "$T/ok"
correr sin_binario 2 "no existe o no es ejecutable" humo "$T/no_existe"

[ "$FALLOS" -eq 0 ]
