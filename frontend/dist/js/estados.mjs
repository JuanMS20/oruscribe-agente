export function esEstadoSesionTerminada(status) {
  return status === "logged_out";
}

export function esTokenCompatible(token) {
  const t = String(token == null ? "" : token).trim();
  if (!t || /[\s\u0000-\u001f\u007f-\u009f]/.test(t)) return false;
  const partes = t.split(".");
  return partes.length === 3 && partes.every((p) => p.length > 0);
}

export function clasificarSondeoLogin(st) {
  if (!st) return "wait";
  if (st.status === "ready") return "ok";
  if (esEstadoSesionTerminada(st.status)) return "fail";
  return "wait";
}

export function estadoToggle(valor) {
  if (valor === true) return "on";
  if (valor === false) return "off";
  return "unknown";
}

export function siguienteToggle(valor) {
  const estado = estadoToggle(valor);
  if (estado === "unknown") return null;
  return estado === "on" ? false : true;
}

export function slugActualOrganizacion(me) {
  return (me && me.organization && me.organization.slug) || "";
}

export function opcionesOrganizacion(me) {
  const actual = slugActualOrganizacion(me);
  const ofrecidas = me && Array.isArray(me.other_organizations) ? me.other_organizations : [];
  const ops = [{ slug: actual, name: (me && me.organization && me.organization.name) || actual, current: true }];
  for (const o of ofrecidas) {
    if (!o || !o.slug || o.slug === actual || ops.some((x) => x.slug === o.slug)) continue;
    ops.push({ slug: o.slug, name: o.name || o.slug, current: false });
  }
  return ops;
}

export function estadoOrganizacion(me) {
  if (!me) return "sin-sesion";
  if (!Array.isArray(me.other_organizations)) return "sin-lista";
  return opcionesOrganizacion(me).length > 1 ? "con-opciones" : "sin-opciones";
}

export function firmaOrganizacion(me, etiquetaActual) {
  return JSON.stringify([estadoOrganizacion(me), opcionesOrganizacion(me), etiquetaActual]);
}

export function plazoInicioSesion(plazoPeticionMs, peticiones, margenMs) {
  const plazo = Number(plazoPeticionMs);
  const n = Number(peticiones);
  const margen = Number(margenMs);
  if (![plazo, n, margen].every(Number.isFinite) || plazo <= 0 || n < 1 || margen < 0) return 0;
  return Math.ceil(plazo) * Math.ceil(n) + Math.ceil(margen);
}

export function clasificarCarga({ state, llegaron, total, rateLimited }) {
  if (esEstadoSesionTerminada(state && state.status)) return "session";
  if (rateLimited) return "rateLimited";
  if (state && state.status === "failed") return "reconnecting";
  if (state && state.status === "ready" && llegaron >= total) return "ready";
  return "offline";
}
