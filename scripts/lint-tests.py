#!/usr/bin/env python3
"""Bloquea los tests que se saltan con un `return` mudo (#82)."""

import re
import sys
from pathlib import Path

RAIZ = Path(__file__).resolve().parent.parent
COMIENZO_TEST = re.compile(r"^\s*#\[(?:tokio::)?test\]\s*$")
FIRMA = re.compile(r"^\s*(?:pub\s+)?(?:async\s+)?fn\s+([A-Za-z0-9_]+)")
SALIDA_RUIDOSA = ("eprintln!", "println!", "panic!", "assert!", "unreachable!")
MECANISMO = ("std::env::var_os", "std::env::args")


def cuerpo(lines, inicio):
    profundidad = 0
    abierto = False
    for i in range(inicio, len(lines)):
        for ch in lines[i]:
            if ch == "{":
                profundidad += 1
                abierto = True
            elif ch == "}":
                profundidad -= 1
                if abierto and profundidad == 0:
                    return i
    return len(lines) - 1


def revisar(archivo):
    lines = archivo.read_text(encoding="utf-8", errors="replace").splitlines()
    fallos = []
    i = 0
    while i < len(lines):
        if not COMIENZO_TEST.match(lines[i]):
            i += 1
            continue
        j = i + 1
        while j < len(lines) and lines[j].lstrip().startswith("#["):
            j += 1
        if j >= len(lines) or not FIRMA.match(lines[j]):
            i += 1
            continue
        fin = cuerpo(lines, j)
        for k in range(j, fin + 1):
            if lines[k].strip() != "return;":
                continue
            contexto = "\n".join(lines[max(j, k - 8):k])
            if any(marca in contexto for marca in SALIDA_RUIDOSA):
                continue
            if any(marca in contexto for marca in MECANISMO):
                continue
            fallos.append((k + 1, lines[j].strip()))
        i = fin + 1
    return fallos


def main():
    fallos = []
    for archivo in sorted(RAIZ.glob("crates/**/*.rs")):
        if archivo.is_file():
            for linea, firma in revisar(archivo):
                fallos.append((archivo.relative_to(RAIZ), linea, firma))
    if not fallos:
        return 0
    print(
        "Arregla: un test que no puede correr se marca #[ignore = \"motivo\"] o imprime "
        "el motivo con eprintln! antes de salir. No dejes un return mudo: el gate lo "
        "cuenta como verde."
    )
    print(
        "Por qué: un test que se salta sin decirlo reporta cobertura que no existe (#82)."
    )
    for archivo, linea, firma in fallos:
        print(f"{archivo}:{linea}: {firma}")
    return 1


if __name__ == "__main__":
    sys.exit(main())
