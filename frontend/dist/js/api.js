// Cliente HTTP local con plazos, paginación y manejo uniforme de errores.
import { T } from "./i18n.js";
import { migaUI, reportarError } from "./dom.js";
import { plazoInicioSesion } from "./estados.mjs";

export const API_TIMEOUT_MS = 40000;
export const SERVER_LOGIN_REQUEST_MS = 30000;
export const LOGIN_START_TIMEOUT_MS = plazoInicioSesion(SERVER_LOGIN_REQUEST_MS, 2, 30000);
// Instalar y actualizar descargan y reemplazan el binario: no pueden cortarse a los 40 s.
export const LONG_ACTION_TIMEOUT_MS = 900000;
export const AGENT_SEND_TIMEOUT_MS = 70000;
export const FILE_UPLOAD_TIMEOUT_MS = 70000;
export const REFRESH_TIMEOUT_MS = 15000;
export const LOAD_RETRY_MS = 4000;
export const LOAD_RETRY_MAX_MS = 30000;

export const api = async (path, opts = {}) => {
  const { timeout = API_TIMEOUT_MS, ...fetchOpts } = opts;
  const metodo = String(fetchOpts.method || "GET").toUpperCase();
  const ruta = String(path).split("?")[0];
  const control = new AbortController();
  const plazo = setTimeout(() => control.abort(), timeout);
  const alAbortar = () => control.abort();
  if (fetchOpts.signal) {
    if (fetchOpts.signal.aborted) control.abort();
    else fetchOpts.signal.addEventListener("abort", alAbortar, { once: true });
  }
  try {
    let res;
    try {
      res = await fetch(path, { headers: { "content-type": "application/json" }, ...fetchOpts, signal: control.signal });
    } catch {
      // Un fetch que cuelga o corta lanza el error crudo del navegador
      // ("Failed to fetch") en inglés. Los errores del servidor (res.ok falso)
      // traen su propio texto y no pasan por aquí.
      const cancelado = fetchOpts.signal && fetchOpts.signal.aborted;
      migaUI("api " + metodo + " " + ruta + " sin respuesta");
      if (!cancelado) reportarError("ui.api", metodo + " " + ruta + " sin respuesta (plazo " + Math.round(timeout / 1000) + " s)", "");
      throw new Error(T("load.failed"));
    }
    const text = await res.text();
    let data = null;
    try { data = text ? JSON.parse(text) : null; } catch { /* not JSON */ }
    if (!res.ok) {
      // El status va en el error para que quien llama distinga una sesión
      // vencida (401) de un fallo de red, que llega sin status.
      migaUI("api " + metodo + " " + ruta + " " + res.status);
      const err = new Error((data && data.error) || text || res.statusText);
      err.status = res.status;
      throw err;
    }
    if (metodo !== "GET") migaUI("api " + metodo + " " + ruta + " ok");
    return data;
  } finally {
    clearTimeout(plazo);
    if (fetchOpts.signal) fetchOpts.signal.removeEventListener("abort", alAbortar);
  }
};

export function paginaDe(cuerpo) {
  return {
    has_more: !!(cuerpo && cuerpo.has_more),
    truncated: !!(cuerpo && cuerpo.truncated),
    next_offset: (cuerpo && cuerpo.next_offset) || 0,
    total: (cuerpo && cuerpo.total) || 0
  };
}
