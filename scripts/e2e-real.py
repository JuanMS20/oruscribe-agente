import http.client
import json
import os
import re
import shutil
import socket
import subprocess
import sys
import tempfile
import threading
import time
from http.server import BaseHTTPRequestHandler, HTTPServer
from pathlib import Path

RAIZ = Path(sys.argv[1]) if len(sys.argv) > 1 else Path(__file__).resolve().parent.parent
EXE = ".exe" if os.name == "nt" else ""
SERVER = RAIZ / "target" / "debug" / f"orus-server{EXE}"
FAKE = RAIZ / "target" / "debug" / f"fake-provider{EXE}"


def puerto_libre():
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


PUERTO_APP = 0
PUERTO_PROV = puerto_libre()
SECRETO_CLAVE = "sk-test-0000000000000000000000"

sentry = []
resultados = []


def check(nombre, cond, detalle=""):
    resultados.append((nombre, bool(cond)))
    print(("PASS " if cond else "FAIL ") + nombre + ("" if cond else f"  -> {detalle}"))


class Sentry(BaseHTTPRequestHandler):
    def do_POST(self):
        n = int(self.headers.get("content-length", 0))
        cuerpo = self.rfile.read(n)
        sentry.append({"ruta": self.path, "cuerpo": cuerpo.decode("utf-8", "replace"), "cab": dict(self.headers)})
        self.send_response(200)
        self.end_headers()
        self.wfile.write(b'{"id":"x"}')

    def log_message(self, *a):
        pass


class Cliente:
    def __init__(self):
        self.cookie = ""

    def pedir(self, metodo, ruta, cuerpo=None, cab=None, host=None, origen=True):
        c = http.client.HTTPConnection("127.0.0.1", PUERTO_APP, timeout=60)
        h = {"Host": host or f"127.0.0.1:{PUERTO_APP}", "Cookie": self.cookie}
        if origen is True:
            h["Origin"] = f"http://127.0.0.1:{PUERTO_APP}"
        elif origen:
            h["Origin"] = origen
        if cab:
            h.update(cab)
        datos = None
        if cuerpo is not None:
            datos = json.dumps(cuerpo).encode()
            h["Content-Type"] = "application/json"
        c.request(metodo, ruta, body=datos, headers=h)
        r = c.getresponse()
        crudo = r.read()
        sc = r.getheader("Set-Cookie")
        if sc:
            self.cookie = sc.split(";")[0]
        try:
            j = json.loads(crudo)
        except Exception:
            j = crudo[:300].decode("utf-8", "replace")
        return r.status, j


class Mundo:
    def __init__(self):
        self.tmp = Path(tempfile.mkdtemp(prefix="orus-suite-"))
        self.datos = self.tmp / "datos"
        self.carpeta = self.tmp / "trabajo"
        self.carpeta.mkdir()
        self.fuera = self.tmp / "fuera"
        self.fuera.mkdir()
        (self.carpeta / "hola.txt").write_text("contenido secreto de prueba\n", encoding="utf-8")
        (self.fuera / "ajeno.txt").write_text("DATO-AJENO-NO-LEER\n", encoding="utf-8")
        self.sentry_srv = HTTPServer(("127.0.0.1", 0), Sentry)
        threading.Thread(target=self.sentry_srv.serve_forever, daemon=True).start()
        self.dsn = f"http://abc123def456@127.0.0.1:{self.sentry_srv.server_port}/1"
        self.prov = None
        self.app = None
        self.cli = Cliente()

    def proveedor(self, respuestas):
        if self.prov:
            self.prov.terminate()
            self.prov.wait()
        (self.tmp / "guion.json").write_text(json.dumps({"respuestas": respuestas}), encoding="utf-8")
        reg = self.tmp / "registro.jsonl"
        if reg.exists():
            reg.unlink()
        self.prov = subprocess.Popen([str(FAKE), "--guion", str(self.tmp / "guion.json"), "--puerto", str(PUERTO_PROV), "--registro", str(reg)], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        time.sleep(0.6)

    def arrancar(self, telemetria=True):
        env = dict(os.environ, ORUSCRIBE_ALLOW_FILE_SECRETS="1")
        env.pop("ORUSCRIBE_SENTRY_DSN", None)
        if telemetria:
            env["ORUSCRIBE_SENTRY_DSN"] = self.dsn
        self.app = subprocess.Popen([str(SERVER), "--addr", "127.0.0.1:0", "--data", str(self.datos)], env=env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
        linea = self.app.stdout.readline()
        global PUERTO_APP
        PUERTO_APP = int(re.search(r"127\.0\.0\.1:(\d+)/", linea).group(1))
        token = re.search(r"token=([0-9a-f]+)", linea).group(1)
        threading.Thread(target=lambda: [None for _ in self.app.stdout], daemon=True).start()
        time.sleep(0.8)
        self.cli = Cliente()
        self.cli.pedir("GET", f"/?token={token}")

    def parar(self):
        if self.app:
            self.app.terminate()
            self.app.wait()
            self.app = None

    def configurar(self, modo="autonomous", carpetas=None):
        st, j = self.cli.pedir("POST", "/api/agent/config", {"kind": "openai", "base_url": f"http://127.0.0.1:{PUERTO_PROV}/v1", "model": "fake-1", "mode": modo, "api_key": SECRETO_CLAVE, "folders": [str(c) for c in (carpetas or [self.carpeta])]})
        return st, j

    def turno(self, texto, decidir=None, espera=40):
        self.cli.pedir("POST", "/api/agent/reset")
        st, j = self.cli.pedir("POST", "/api/agent/send", {"text": texto})
        if st != 200:
            return {"send_status": st, "send": j, "aprobaciones": []}
        vistas = []
        t0 = time.time()
        while time.time() - t0 < espera:
            time.sleep(0.3)
            st, j = self.cli.pedir("GET", "/api/agent")
            for a in j.get("approvals") or []:
                if a["id"] not in [v["id"] for v in vistas]:
                    vistas.append(a)
                    self.cli.pedir("POST", "/api/agent/approve", {"id": a["id"], "approve": bool(decidir), "remember": False})
            if st == 200 and not j.get("running"):
                break
        j["aprobaciones"] = vistas
        j["send_status"] = 200
        return j


def llamada(nombre, args, texto="Voy."):
    return {"texto": [texto], "herramienta": {"id": "c1", "nombre": nombre, "argumentos": args}, "final": "tool_calls"}


def cierre(texto="Hecho."):
    return {"texto": [texto]}


def resultados_de_pasos(j):
    return " | ".join(str(p.get("text", "")) for p in j.get("steps", []) if p.get("kind") == "tool_result")


def main():
    m = Mundo()
    print("tmp:", m.tmp)
    try:
        m.arrancar()

        c = Cliente()
        check("API sin cookie -> 401", c.pedir("GET", "/api/state")[0] == 401)
        check("Host ajeno -> 421", m.cli.pedir("GET", "/api/state", host="evil.example.com")[0] == 421)
        check("POST con Origin ajeno -> 403", m.cli.pedir("POST", "/api/agent/reset", origen="http://evil.example.com")[0] == 403)
        check("token incorrecto no da cookie", Cliente().pedir("GET", "/?token=deadbeef")[0] != 303)

        st, j = m.configurar("autonomous")
        check("config proveedor OK", st == 200 and j.get("configured"), j)
        check("la respuesta de config no devuelve la clave", SECRETO_CLAVE not in json.dumps(j))

        m.proveedor([llamada("read", {"path": str(m.carpeta / "hola.txt")}), cierre("Leido.")])
        j = m.turno("lee hola.txt")
        check("read dentro de la carpeta", "contenido secreto" in resultados_de_pasos(j), resultados_de_pasos(j))
        check("el turno termina con respuesta", any(x.get("role") == "assistant" and x.get("content") == "Leido." for x in j.get("messages", [])), j.get("messages"))

        nuevo = m.carpeta / "nuevo.txt"
        m.proveedor([llamada("write", {"path": str(nuevo), "content": "hola mundo\n"}), cierre()])
        j = m.turno("crea nuevo.txt")
        check("write crea el archivo", nuevo.exists() and nuevo.read_text(encoding="utf-8") == "hola mundo\n", resultados_de_pasos(j))
        st, uj = m.cli.pedir("POST", "/api/agent/undo", {"n": 1, "force": False})
        check("deshacer elimina el archivo creado", st == 200 and not nuevo.exists(), (st, uj))

        m.proveedor([llamada("edit", {"path": str(m.carpeta / "hola.txt"), "old_string": "secreto", "new_string": "publico"}), cierre()])
        j = m.turno("edita hola.txt")
        txt = (m.carpeta / "hola.txt").read_text(encoding="utf-8")
        check("edit cambia el contenido", "publico" in txt, txt)
        m.cli.pedir("POST", "/api/agent/undo", {"n": 1, "force": False})
        check("deshacer restaura la edicion", "secreto" in (m.carpeta / "hola.txt").read_text(encoding="utf-8"))

        m.proveedor([llamada("bash", {"command": "echo hola-bash"}), cierre()])
        j = m.turno("corre echo", decidir=True)
        check("bash ejecuta (con o sin aprobacion)", "hola-bash" in resultados_de_pasos(j), (resultados_de_pasos(j), j.get("aprobaciones")))

        m.proveedor([llamada("read", {"path": str(m.fuera / "ajeno.txt")}), cierre()])
        j = m.turno("lee ajeno")
        check("read fuera de la carpeta NO filtra el contenido", "DATO-AJENO" not in resultados_de_pasos(j), resultados_de_pasos(j))
        trav = str(m.carpeta / ".." / "fuera" / "ajeno.txt")
        m.proveedor([llamada("read", {"path": trav}), cierre()])
        j = m.turno("lee ajeno por traversal")
        check("traversal '..' NO filtra el contenido", "DATO-AJENO" not in resultados_de_pasos(j), resultados_de_pasos(j))
        escapa = m.fuera / "creado.txt"
        m.proveedor([llamada("write", {"path": str(escapa), "content": "x"}), cierre()])
        j = m.turno("escribe fuera")
        check("write fuera de la carpeta NO crea el archivo", not escapa.exists(), resultados_de_pasos(j))
        sec = m.datos / "settings.json"
        m.proveedor([llamada("read", {"path": str(sec)}), cierre()])
        j = m.turno("lee los ajustes de la app")
        check("no puede leer los datos internos de la app", SECRETO_CLAVE not in resultados_de_pasos(j), resultados_de_pasos(j)[:200])

        m.configurar("readonly")
        ro = m.carpeta / "ro.txt"
        m.proveedor([llamada("write", {"path": str(ro), "content": "x"}), cierre()])
        j = m.turno("escribe en modo lectura")
        check("modo readonly bloquea write", not ro.exists(), resultados_de_pasos(j))

        m.configurar("ask")
        pre = m.carpeta / "preg.txt"
        m.proveedor([llamada("write", {"path": str(pre), "content": "x"}), cierre()])
        j = m.turno("escribe preguntando", decidir=False)
        check("modo ask pide aprobacion", len(j.get("aprobaciones", [])) >= 1, j.get("aprobaciones"))
        check("rechazar la aprobacion no crea el archivo", not pre.exists())
        m.proveedor([llamada("write", {"path": str(pre), "content": "x"}), cierre()])
        j = m.turno("escribe preguntando", decidir=True)
        check("aceptar la aprobacion crea el archivo", pre.exists(), resultados_de_pasos(j))
        m.configurar("autonomous")

        extra = m.tmp / "efimera"
        extra.mkdir()
        m.configurar("autonomous", [m.carpeta, extra])
        shutil.rmtree(extra)
        m.proveedor([llamada("read", {"path": str(m.carpeta / "hola.txt")}), cierre("ok")])
        j = m.turno("lee con una carpeta que ya no existe")
        check("carpeta guardada que desaparecio NO aborta el turno", "contenido" in resultados_de_pasos(j), (j.get("send_status"), j.get("send"), j.get("error"), resultados_de_pasos(j)))
        m.configurar("autonomous", [m.carpeta])

        antes = len(sentry)
        m.proveedor([])
        j = m.turno("esto va a fallar")
        check("fallo del proveedor se ve en la UI (last_error)", "no se pudo contactar" in (j.get("last_error") or ""), j.get("last_error"))
        time.sleep(2)
        for e in sentry[antes:]:
            pass
        cat = [json.loads(e["cuerpo"]).get("tags", {}).get("category") for e in sentry[antes:]]
        check("fallo del proveedor llega a Sentry (agent.chat)", "agent.chat" in cat, cat)

        antes = len(sentry)
        m.cli.pedir("POST", "/api/diagnostics/report", {"category": "ui.js", "message": f"fallo con {SECRETO_CLAVE} en C:\\Users\\carlos\\secreto\\x.txt y /home/maria/x token=eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjMifQ.abcdefghijk", "detail": "Authorization: Bearer abc.def.ghi"})
        time.sleep(2)
        nuevos = [e["cuerpo"] for e in sentry[antes:]]
        todo = "\n".join(nuevos)
        check("evento de la UI llega a Sentry", len(nuevos) >= 1, len(nuevos))
        check("redaccion: sin clave sk-", "sk-test" not in todo)
        check("redaccion: sin nombre de usuario en rutas", "maria" not in todo and "carlos" not in todo, re.findall(r".{0,30}(?:maria|carlos).{0,30}", todo)[:2])
        check("redaccion: sin JWT", "eyJhbGci" not in todo)
        check("envio con cabecera X-Sentry-Auth o DSN en la ruta", any("abc123def456" in (" ".join(v for k, v in e["cab"].items() if k.lower() == "x-sentry-auth") + e["ruta"]) for e in sentry[antes:]) if nuevos else False)

        m.configurar("autonomous")
        crlf = m.carpeta / "crlf.txt"
        crlf.write_bytes(b"linea uno\r\nlinea dos\r\nlinea tres\r\n")
        m.proveedor([llamada("edit", {"path": str(crlf), "old_string": "linea uno\nlinea dos", "new_string": "UNO\nDOS"}), cierre()])
        j = m.turno("edita un archivo con finales de linea de Windows")
        datos_crlf = crlf.read_bytes()
        check("edit multilinea sobre un archivo CRLF cambia el texto", b"UNO" in datos_crlf, (datos_crlf, resultados_de_pasos(j)))
        check("edit sobre CRLF no deja el archivo con finales mezclados", datos_crlf.count(b"\r\n") == datos_crlf.count(b"\n"), datos_crlf)

        raro = m.tmp / "Mi carpeta ñandú"
        raro.mkdir()
        (raro / "acción.txt").write_text("texto con tildes\n", encoding="utf-8")
        m.configurar("autonomous", [raro])
        m.proveedor([llamada("read", {"path": str(raro / "acción.txt")}), cierre()])
        j = m.turno("lee un archivo con tildes y espacios en la ruta")
        check("rutas con espacios y tildes funcionan", "texto con tildes" in resultados_de_pasos(j), resultados_de_pasos(j))
        m.proveedor([llamada("write", {"path": str(raro / "nuevo_ñ.txt"), "content": "ok"}), cierre()])
        m.turno("crea un archivo nuevo en carpeta con tildes y espacios")
        check("write crea archivo en ruta con tildes y espacios", (raro / "nuevo_ñ.txt").exists())
        m.proveedor([llamada("write", {"path": str(raro / "sub" / "hondo" / "n.txt"), "content": "ok"}), cierre()])
        j = m.turno("intenta crear un archivo en subcarpetas inexistentes")
        check("write en carpeta inexistente avisa sin crear", not (raro / "sub" / "hondo" / "n.txt").exists() and "carpeta" in resultados_de_pasos(j).lower(), resultados_de_pasos(j))
        m.configurar("autonomous")

        enlace = m.carpeta / "enlace"
        try:
            os.symlink(m.fuera, enlace, target_is_directory=True)
            hay_enlace = True
        except (OSError, NotImplementedError):
            hay_enlace = False
        if hay_enlace:
            m.proveedor([llamada("read", {"path": str(enlace / "ajeno.txt")}), cierre()])
            j = m.turno("lee a traves de un enlace simbolico")
            check("symlink hacia afuera NO filtra el contenido", "DATO-AJENO" not in resultados_de_pasos(j), resultados_de_pasos(j))
            m.proveedor([llamada("write", {"path": str(enlace / "escapado.txt"), "content": "x"}), cierre()])
            m.turno("escribe a traves de un enlace simbolico")
            check("symlink hacia afuera NO permite escribir", not (m.fuera / "escapado.txt").exists())
        else:
            print("SKIP symlink (sin permiso para crear enlaces)")

        m.configurar("readonly")
        via_bash = m.carpeta / "via_bash.txt"
        orden = f'echo escapado > "{via_bash}"'
        m.proveedor([llamada("bash", {"command": orden}), cierre()])
        j = m.turno("en modo solo lectura intenta escribir con bash", decidir=False)
        check("readonly NO deja escribir archivos usando bash", not via_bash.exists(), resultados_de_pasos(j))
        m.configurar("autonomous")

        m.proveedor([llamada("bash", {"command": "exit 3"}), cierre()])
        j = m.turno("comando que falla")
        check("bash con codigo de salida distinto de cero lo informa", "3" in resultados_de_pasos(j), resultados_de_pasos(j))

        for ruta, cuerpo, nombre in [("/api/settings", ["no", "es", "objeto"], "settings con un array"), ("/api/agent/config", "texto", "config con una cadena"), ("/api/agent/approve", {}, "approve sin id"), ("/api/agent/undo", {"n": -5}, "undo con n negativo"), ("/api/notes", {"x": 1}, "notes con campos desconocidos")]:
            st, _ = m.cli.pedir("POST", ruta, cuerpo)
            check(f"entrada malformada ({nombre}) responde 4xx y no 5xx", 400 <= st < 500, st)
        c = http.client.HTTPConnection("127.0.0.1", PUERTO_APP, timeout=30)
        c.request("POST", "/api/agent/send", body=b"{no es json", headers={"Host": f"127.0.0.1:{PUERTO_APP}", "Cookie": m.cli.cookie, "Origin": f"http://127.0.0.1:{PUERTO_APP}", "Content-Type": "application/json"})
        check("JSON roto responde 4xx", 400 <= c.getresponse().status < 500)
        st, _ = m.cli.pedir("POST", "/api/agent/send", {"text": "x" * 300000})
        check("mensaje enorme se rechaza con 4xx", 400 <= st < 500, st)

        m.proveedor([llamada("bash", {"command": "sleep 3" if os.name != "nt" else "ping -n 4 127.0.0.1 >nul"}), cierre("fin")])
        m.cli.pedir("POST", "/api/agent/reset")
        st1, _ = m.cli.pedir("POST", "/api/agent/send", {"text": "primero"})
        st2, _ = m.cli.pedir("POST", "/api/agent/send", {"text": "segundo a la vez"})
        check("segundo envio mientras corre el primero -> 409", st1 == 200 and st2 == 409, (st1, st2))
        m.cli.pedir("POST", "/api/agent/cancel")
        t0 = time.time()
        while time.time() - t0 < 20:
            if not m.cli.pedir("GET", "/api/agent")[1].get("running"):
                break
            time.sleep(0.3)
        check("cancelar detiene el turno", not m.cli.pedir("GET", "/api/agent")[1].get("running"))

        st, lista = m.cli.pedir("GET", "/api/agent/conversations")
        n_antes = len(lista) if st == 200 else -1
        m.parar()
        m.arrancar()
        st, lista2 = m.cli.pedir("GET", "/api/agent/conversations")
        check("las conversaciones sobreviven al reinicio", st == 200 and len(lista2) == n_antes and n_antes > 0, (n_antes, lista2 if st != 200 else len(lista2)))
        st, a = m.cli.pedir("GET", "/api/agent")
        check("la config del proveedor sobrevive al reinicio", st == 200 and a.get("kind") == "openai" and a.get("model") == "fake-1", a)
        ajustes = (m.datos / "settings.json").read_text(encoding="utf-8") if (m.datos / "settings.json").exists() else ""
        fugas = [p.name for p in m.datos.rglob("*") if p.is_file() and SECRETO_CLAVE in p.read_text(encoding="utf-8", errors="ignore")]
        check("la clave de API no queda en settings.json", SECRETO_CLAVE not in ajustes, ajustes[:200])
        print("   (archivos del directorio de datos con la clave en claro, con ALLOW_FILE_SECRETS=1):", fugas)

        st, _ = m.cli.pedir("POST", "/api/settings", {"telemetry": False})
        antes = len(sentry)
        m.cli.pedir("POST", "/api/diagnostics/report", {"category": "ui.js", "message": "no deberia salir"})
        time.sleep(2)
        check("con telemetria apagada no sale nada", len(sentry) == antes, len(sentry) - antes)
    finally:
        m.parar()
        if m.prov:
            m.prov.terminate()
        m.sentry_srv.shutdown()

    ok = sum(1 for _, v in resultados if v)
    print(f"\nRESUMEN: {ok}/{len(resultados)} pasan")
    for n, v in resultados:
        if not v:
            print("  FALLA:", n)
    sys.exit(0 if ok == len(resultados) else 1)


main()
