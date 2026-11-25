#!/bin/sh
# Prueba install.sh con un ldd falso. No toca el sistema ni la instalación real.
set -eu

aqui=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
caja=$(mktemp -d)
trap 'rm -rf "$caja"' EXIT INT TERM

fallo() {
  echo "FALLO: $*" >&2
  exit 1
}

preparar() {
  paquete=$(mktemp -d "$caja/paquete.XXXXXX")
  cp "$aqui/install.sh" "$paquete/install.sh"
  printf '#!/bin/sh\nexit 0\n' > "$paquete/oruscribe-agente"
  : > "$paquete/icon.png"
  mkdir -p "$caja/bin"
  cat > "$caja/bin/ldd"
  chmod +x "$caja/bin/ldd"
  cat > "$caja/bin/ldconfig" <<'EOF'
#!/bin/sh
if [ "$1" = "-p" ]; then
  echo "	libayatana-appindicator3.so.1 (libc6,x86-64) => /lib/x86_64-linux-gnu/libayatana-appindicator3.so.1"
  echo "	libappindicator3.so.1 (libc6,x86-64) => /lib/x86_64-linux-gnu/libappindicator3.so.1"
fi
exit 0
EOF
  chmod +x "$caja/bin/ldconfig"
  cat > "$caja/bin/objdump" <<'EOF'
#!/bin/sh
echo "0000000000000000      DF *UND*  0000000000000000  GLIBC_2.39  memcpy"
EOF
  cat > "$caja/bin/getconf" <<'EOF'
#!/bin/sh
echo "glibc 2.39"
EOF
  chmod +x "$caja/bin/objdump" "$caja/bin/getconf"
}

correr() {
  PATH="$caja/bin:$PATH" ORUSCRIBE_PREFIX="$1" \
    sh "$paquete/install.sh" > "$caja/salida" 2>&1
}

echo 'Caso: falta libwebkit2gtk-4.1.so.0'
preparar <<'EOF'
#!/bin/sh
echo "	libwebkit2gtk-4.1.so.0 => not found"
EOF
set +e
correr "$caja/prefijo-webkit"
codigo=$?
set -e
[ "$codigo" -eq 1 ] || fallo "salió con $codigo, esperaba 1"
grep -q 'apt install libwebkit2gtk-4.1-0 libayatana-appindicator3-1' "$caja/salida" ||
  fallo 'no nombra el comando de Ubuntu/Debian'
grep -q 'dnf install webkit2gtk4.1 libayatana-appindicator-gtk3' "$caja/salida" ||
  fallo 'no nombra el comando de Fedora'
grep -q 'pacman -S webkit2gtk-4.1 libayatana-appindicator' "$caja/salida" ||
  fallo 'no nombra el comando de Arch'
grep -q 'libwebkit2gtk-4.1.so.0' "$caja/salida" ||
  fallo 'no nombra la biblioteca que falta'
[ ! -e "$caja/prefijo-webkit/bin/oruscribe-agente" ] ||
  fallo 'copió el binario aunque faltaban bibliotecas'

echo 'Caso: falta libayatana-appindicator3.so.1'
preparar <<'EOF'
#!/bin/sh
echo "	libayatana-appindicator3.so.1 => not found"
EOF
set +e
correr "$caja/prefijo-ayatana"
codigo=$?
set -e
[ "$codigo" -eq 1 ] || fallo "salió con $codigo, esperaba 1"
grep -q 'libayatana-appindicator3.so.1' "$caja/salida" ||
  fallo 'no nombra la biblioteca que falta'
[ ! -e "$caja/prefijo-ayatana/bin/oruscribe-agente" ] ||
  fallo 'copió el binario aunque faltaba el indicador'

echo 'Caso: falta libgtk-3.so.0'
preparar <<'EOF'
#!/bin/sh
echo "	libgtk-3.so.0 => not found"
echo "	libgtk-3.so.0 => not found"
EOF
set +e
correr "$caja/prefijo-gtk"
codigo=$?
set -e
[ "$codigo" -eq 1 ] || fallo "salió con $codigo, esperaba 1"
grep -q 'libgtk-3.so.0' "$caja/salida" ||
  fallo 'no nombra la biblioteca que falta'
[ "$(grep -c '^  libgtk-3.so.0$' "$caja/salida")" -eq 1 ] ||
  fallo 'no quitó la biblioteca duplicada'
[ ! -e "$caja/prefijo-gtk/bin/oruscribe-agente" ] ||
  fallo 'copió el binario aunque faltaba GTK'

echo 'Caso: ldd no reporta bibliotecas ausentes'
preparar <<'EOF'
#!/bin/sh
exit 0
EOF
correr "$caja/prefijo-ok" || fallo 'no instaló aunque estaban las bibliotecas'
[ -x "$caja/prefijo-ok/bin/oruscribe-agente" ] || fallo 'falta el binario instalado'
[ -f "$caja/prefijo-ok/share/icons/hicolor/512x512/apps/oruscribe-agente.png" ] ||
  fallo 'falta el icono instalado'
grep -Fq "Exec=\"$caja/prefijo-ok/bin/oruscribe-agente\"" \
  "$caja/prefijo-ok/share/applications/oruscribe-agente.desktop" ||
  fallo 'el lanzador no apunta al binario instalado'

echo 'Caso: ldd limpio pero falta el indicador'
preparar <<'EOF'
#!/bin/sh
exit 0
EOF
cat > "$caja/bin/ldconfig" <<'EOF'
#!/bin/sh
exit 0
EOF
chmod +x "$caja/bin/ldconfig"
set +e
correr "$caja/prefijo-sin-indicador"
codigo=$?
set -e
[ "$codigo" -eq 1 ] || fallo "salió con $codigo, esperaba 1"
grep -q 'Falta la biblioteca de la bandeja' "$caja/salida" ||
  fallo 'no dice que falta la biblioteca de la bandeja'
grep -q 'libayatana-appindicator3.so.1' "$caja/salida" ||
  fallo 'no nombra libayatana-appindicator3.so.1'
grep -q 'apt install libwebkit2gtk-4.1-0 libayatana-appindicator3-1' "$caja/salida" ||
  fallo 'no nombra el comando de Ubuntu/Debian'
grep -q 'dnf install webkit2gtk4.1 libayatana-appindicator-gtk3' "$caja/salida" ||
  fallo 'no nombra el comando de Fedora'
grep -q 'pacman -S webkit2gtk-4.1 libayatana-appindicator' "$caja/salida" ||
  fallo 'no nombra el comando de Arch'
[ ! -e "$caja/prefijo-sin-indicador/bin/oruscribe-agente" ] ||
  fallo 'copió el binario aunque faltaba el indicador'

echo 'Caso: ldconfig lista libappindicator3.so.1 (nombre viejo)'
preparar <<'EOF'
#!/bin/sh
exit 0
EOF
cat > "$caja/bin/ldconfig" <<'EOF'
#!/bin/sh
if [ "$1" = "-p" ]; then
  echo "	libappindicator3.so.1 (libc6,x86-64) => /usr/lib/libappindicator3.so.1"
fi
exit 0
EOF
chmod +x "$caja/bin/ldconfig"
correr "$caja/prefijo-viejo" || fallo 'no instaló aunque el indicador viejo estaba presente'
[ -x "$caja/prefijo-viejo/bin/oruscribe-agente" ] || fallo 'falta el binario instalado'

echo 'Caso: falta OpenSSL 3 en Fedora 45 (esa distro trae OpenSSL 4)'
preparar <<'EOF'
#!/bin/sh
echo "	libssl.so.3 => not found"
echo "	libcrypto.so.3 => not found"
EOF
set +e
correr "$caja/prefijo-openssl3"
codigo=$?
set -e
[ "$codigo" -eq 1 ] || fallo "salió con $codigo, esperaba 1"
grep -q 'libssl.so.3' "$caja/salida" ||
  fallo 'no nombra libssl.so.3'
grep -q 'libcrypto.so.3' "$caja/salida" ||
  fallo 'no nombra libcrypto.so.3'
grep -q 'openssl3-libs' "$caja/salida" ||
  fallo 'no nombra openssl3-libs'
grep -q 'Fedora 45' "$caja/salida" ||
  fallo 'no nombra Fedora 45'
[ ! -e "$caja/prefijo-openssl3/bin/oruscribe-agente" ] ||
  fallo 'copió el binario aunque faltaba OpenSSL 3'

echo 'Caso: el binario pide glibc 2.39 y el sistema tiene 2.36'
preparar <<'EOF'
#!/bin/sh
exit 0
EOF
cat > "$caja/bin/objdump" <<'EOF'
#!/bin/sh
echo "0000000000000000      DF *UND*  0000000000000000  GLIBC_2.39  memcpy"
EOF
cat > "$caja/bin/getconf" <<'EOF'
#!/bin/sh
echo "glibc 2.36"
EOF
chmod +x "$caja/bin/objdump" "$caja/bin/getconf"
set +e
correr "$caja/prefijo-glibc"
codigo=$?
set -e
[ "$codigo" -eq 1 ] || fallo "salió con $codigo, esperaba 1"
grep -q 'glibc 2.39' "$caja/salida" || fallo 'no nombra la glibc que pide el binario'
grep -q '2.36' "$caja/salida" || fallo 'no nombra la glibc del sistema'
[ ! -e "$caja/prefijo-glibc/bin/oruscribe-agente" ] ||
  fallo 'copió el binario aunque la glibc del sistema es más vieja'

echo 'Caso: el sistema tiene glibc más nueva que la que pide el binario'
preparar <<'EOF'
#!/bin/sh
exit 0
EOF
cat > "$caja/bin/objdump" <<'EOF'
#!/bin/sh
echo "0000000000000000      DF *UND*  0000000000000000  GLIBC_2.39  memcpy"
EOF
cat > "$caja/bin/getconf" <<'EOF'
#!/bin/sh
echo "glibc 2.40"
EOF
chmod +x "$caja/bin/objdump" "$caja/bin/getconf"
correr "$caja/prefijo-glibc-nueva" || fallo 'no instaló con una glibc más nueva'
[ -x "$caja/prefijo-glibc-nueva/bin/oruscribe-agente" ] || fallo 'falta el binario instalado'

fallos_nuevos=0

echo 'Caso: ldd detecta una versión de glibc ausente'
preparar <<'EOF'
#!/bin/sh
echo "/bin/sh: /lib/x86_64-linux-gnu/libc.so.6: version \`GLIBC_2.39' not found (required by ./oruscribe-agente)"
exit 1
EOF
set +e
correr "$caja/prefijo-glibc-ldd"
codigo=$?
set -e
[ "$codigo" -eq 1 ] || fallo "salió con $codigo, esperaba 1"
grep -q 'GLIBC_2.39' "$caja/salida" || fallo 'no muestra la versión de glibc que falta'
[ ! -e "$caja/prefijo-glibc-ldd/bin/oruscribe-agente" ] ||
  fallo 'copió el binario aunque ldd no pudo resolverlo'

echo 'Caso: GLIBC_BASELINE.txt manda cuando objdump no da símbolos'
preparar <<'EOF'
#!/bin/sh
exit 0
EOF
printf '2.39\n' > "$paquete/GLIBC_BASELINE.txt"
cat > "$caja/bin/objdump" <<'EOF'
#!/bin/sh
exit 0
EOF
cat > "$caja/bin/getconf" <<'EOF'
#!/bin/sh
echo "glibc 2.36"
EOF
chmod +x "$caja/bin/objdump" "$caja/bin/getconf"
set +e
correr "$caja/prefijo-baseline"
codigo=$?
set -e
[ "$codigo" -eq 1 ] || fallo "salió con $codigo, esperaba 1"
grep -q 'glibc 2.39' "$caja/salida" || fallo 'no leyó la baseline del paquete'
[ ! -e "$caja/prefijo-baseline/bin/oruscribe-agente" ] ||
  fallo 'copió el binario aunque la baseline del paquete era más nueva'

echo 'Caso: sin baseline ni objdump no se instala a ciegas'
preparar <<'EOF'
#!/bin/sh
exit 0
EOF
cat > "$caja/bin/objdump" <<'EOF'
#!/bin/sh
exit 0
EOF
chmod +x "$caja/bin/objdump"
set +e
correr "$caja/prefijo-sin-baseline"
codigo=$?
set -e
[ "$codigo" -eq 1 ] || fallo "salió con $codigo, esperaba 1"
grep -q 'No pude leer la glibc mínima' "$caja/salida" ||
  fallo 'no explicó que no pudo leer la glibc mínima'
[ ! -e "$caja/prefijo-sin-baseline/bin/oruscribe-agente" ] ||
  fallo 'copió el binario sin saber la glibc mínima'

(
  echo 'Caso: reinstalar con el binario destino ya presente'
  preparar <<'EOF'
#!/bin/sh
exit 0
EOF
  instalador_real=$(command -v install)
  cat > "$caja/bin/install" <<EOF
#!/bin/sh
for destino do :; done
if [ -e "\$destino" ]; then
  echo "install: \$destino: Text file busy" >&2
  exit 1
fi
exec "$instalador_real" "\$@"
EOF
  chmod +x "$caja/bin/install"

  destino="$caja/prefijo-reinstalar/bin/oruscribe-agente"
  mkdir -p "$(dirname "$destino")"
  printf '#!/bin/sh\nexit 0\n' > "$destino"
  printf '#!/bin/sh\nexit 7\n' > "$paquete/oruscribe-agente"

  correr "$caja/prefijo-reinstalar" || fallo 'no pudo reinstalar con el binario destino presente'
  grep -q 'exit 7' "$destino" || fallo 'el binario destino no se actualizó'
  [ ! -e "$caja/prefijo-reinstalar/bin/.oruscribe-agente.new" ] ||
    fallo 'quedó el archivo temporal .new'
  rm -f "$caja/bin/install"
) || fallos_nuevos=$((fallos_nuevos + 1))

(
  echo 'Caso: prefijo con espacio'
  preparar <<'EOF'
#!/bin/sh
exit 0
EOF
  prefijo="$caja/prefijo con espacio"
  correr "$prefijo" || fallo 'no instaló con prefijo con espacio'
  [ -x "$prefijo/bin/oruscribe-agente" ] || fallo 'falta el binario instalado'
  grep -Fq "Exec=\"$prefijo/bin/oruscribe-agente\"" \
    "$prefijo/share/applications/oruscribe-agente.desktop" ||
    fallo 'el Exec del lanzador no entrecomilla la ruta con espacio'
) || fallos_nuevos=$((fallos_nuevos + 1))

[ "$fallos_nuevos" -eq 0 ] || fallo "$fallos_nuevos casos nuevos fallaron"

echo 'OK: 14 casos'
