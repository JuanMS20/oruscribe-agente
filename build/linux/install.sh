#!/bin/sh
# Instala el binario y el lanzador en ~/.local (o en $ORUSCRIBE_PREFIX).
# No pide root y no toca los archivos del paquete del sistema.
#
# Antes de copiar nada comprueba que el binario encuentre todas las bibliotecas
# con las que se compiló. Si falta alguna, no instala a medias: sale con 1 y
# dice las que faltan y el paquete de Ubuntu/Debian, Fedora y Arch.
set -eu

origen=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
prefix=${ORUSCRIBE_PREFIX:-"$HOME/.local"}
binario="$origen/oruscribe-agente"

if ! ldd_salida=$(ldd "$binario" 2>&1); then
  printf '%s\n' "$ldd_salida" >&2
  echo "ldd no pudo resolver las bibliotecas de $binario (revisa las versiones GLIBC). No instalo a ciegas." >&2
  exit 1
fi

faltan=$(printf '%s\n' "$ldd_salida" | awk '
  /=> not found$/ {
    lib = $0
    sub(/[ \t]*=> not found$/, "", lib)
    sub(/^[ \t]+/, "", lib)
    if (!(lib in visto)) {
      visto[lib] = 1
      print "  " lib
    }
  }
')

if [ -n "$faltan" ]; then
  cat >&2 <<EOF
Faltan bibliotecas del sistema y Oruscribe Agente no va a arrancar:

$faltan

Instálalas y vuelve a ejecutar ./install.sh:

  Ubuntu/Debian:         sudo apt install libwebkit2gtk-4.1-0 libayatana-appindicator3-1
  Fedora 44 o anterior:  sudo dnf install webkit2gtk4.1 libayatana-appindicator-gtk3
  Fedora 45 o posterior: sudo dnf install webkit2gtk4.1 libayatana-appindicator-gtk3 openssl3-libs
  Arch:                  sudo pacman -S webkit2gtk-4.1 libayatana-appindicator
  Arch (falta libssl.so.3): sudo pacman -S openssl-3
EOF
  exit 1
fi

# ldd solo comprueba que la biblioteca aparezca, no la versión de glibc. El
# binario pide símbolos GLIBC_x.y y en una distro más vieja instala y después
# no arranca: se mira antes de copiar nada.
base_linea="$origen/GLIBC_BASELINE.txt"
if [ -f "$base_linea" ]; then
  pide=$(tr -d '[:space:]' < "$base_linea")
else
  pide=""
fi
if [ -z "$pide" ] && command -v objdump >/dev/null 2>&1; then
  pide=$(objdump -T "$binario" 2>/dev/null |
    sed -n 's/.*GLIBC_\([0-9][0-9]*\.[0-9][0-9]*\).*/\1/p' |
    sort -u -t. -k1,1n -k2,2n | tail -n 1)
fi
if [ -z "$pide" ]; then
  echo "No pude leer la glibc mínima del binario (falta GLIBC_BASELINE.txt y objdump). No instalo a ciegas: compila el binario en este equipo." >&2
  exit 1
fi
if ! command -v getconf >/dev/null 2>&1; then
  echo "No pude consultar la glibc del sistema con getconf. No instalo a ciegas: revisa la versión y vuelve a ejecutar ./install.sh." >&2
  exit 1
fi
tiene=$(getconf GNU_LIBC_VERSION 2>/dev/null | awk '{print $2}')
if [ -z "$tiene" ]; then
  echo "getconf no reportó la glibc del sistema. No instalo a ciegas." >&2
  exit 1
fi
if ! awk -v a="$tiene" -v b="$pide" 'BEGIN {
  split(a, x, "."); split(b, y, ".");
  exit (x[1] + 0 < y[1] + 0 || (x[1] + 0 == y[1] + 0 && x[2] + 0 < y[2] + 0)) ? 1 : 0
}'; then
  cat >&2 <<EOF
Este binario pide glibc $pide y este sistema tiene $tiene: se instalaría,
pero al abrir la app fallaría con "GLIBC_$pide not found".

Usa una distro con glibc $pide o más nueva, o compila el binario en este equipo.
EOF
  exit 1
fi

# La bandeja se abre con dlopen en runtime: ldd no la ve. Se comprueba con ldconfig.
ldconfig_bin=$(command -v ldconfig || { [ -x /sbin/ldconfig ] && echo /sbin/ldconfig; } || { [ -x /usr/sbin/ldconfig ] && echo /usr/sbin/ldconfig; } || echo ldconfig)
if ! ldconfig_salida=$("$ldconfig_bin" -p 2>/dev/null); then
  echo "No pude leer la cache de bibliotecas con ldconfig -p (puede que no exista ldconfig o no haya cache). No instalo a ciegas: revisa el sistema y vuelve a ejecutar ./install.sh." >&2
  exit 1
fi

indicador=$(printf '%s\n' "$ldconfig_salida" | awk '
  $1 == "libayatana-appindicator3.so.1" || $1 == "libappindicator3.so.1" { print $1; exit }
')

if [ -z "$indicador" ]; then
  cat >&2 <<EOF
Falta la biblioteca de la bandeja: Oruscribe Agente no va a arrancar.

ldconfig -p no lista libayatana-appindicator3.so.1 ni libappindicator3.so.1.
La bandeja se carga en runtime y ldd no la reporta.

Instálala y vuelve a ejecutar ./install.sh:

  Ubuntu/Debian:  sudo apt install libwebkit2gtk-4.1-0 libayatana-appindicator3-1
  Fedora:         sudo dnf install webkit2gtk4.1 libayatana-appindicator-gtk3
  Arch:           sudo pacman -S webkit2gtk-4.1 libayatana-appindicator
EOF
  exit 1
fi

if command -v gst-inspect-1.0 >/dev/null 2>&1; then
  if ! gst-inspect-1.0 pulsesrc >/dev/null 2>&1 &&
    ! gst-inspect-1.0 pipewiresrc >/dev/null 2>&1; then
    echo "Aviso: no hay elementos GStreamer de captura (pulsesrc/pipewiresrc). La app instalará, pero getUserMedia puede fallar con NotFoundError/NotReadableError; instala los plugins GStreamer de captura de audio de tu distro antes de usar el micrófono." >&2
  fi
fi

mkdir -p "$prefix/bin" "$prefix/share/applications" \
  "$prefix/share/icons/hicolor/512x512/apps"

install -m 755 "$origen/oruscribe-agente" "$prefix/bin/.oruscribe-agente.new"
mv -f "$prefix/bin/.oruscribe-agente.new" "$prefix/bin/oruscribe-agente"
install -m 644 "$origen/icon.png" \
  "$prefix/share/icons/hicolor/512x512/apps/oruscribe-agente.png"

exec_ruta=$(printf '%s' "$prefix/bin/oruscribe-agente" | sed 's/\\/\\\\/g; s/"/\\"/g; s/%/%%/g')

cat > "$prefix/share/applications/oruscribe-agente.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=Oruscribe Agente
Exec="$exec_ruta"
Icon=oruscribe-agente
Categories=Office;
Terminal=false
EOF

echo "Oruscribe Agente instalado en $prefix/bin/oruscribe-agente"
