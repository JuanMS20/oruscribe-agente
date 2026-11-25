// Utilidades puras de DOM, seguridad de URL, markdown y formato.
import { T } from "./i18n.js";

// Rastro de pasos de la pantalla (últimos 30): solo vistas, acciones y fallos
// de red, nunca texto escrito por la persona. Viaja con cada reporte de error.
const RASTRO_MAX = 30;
const rastro = [];
export function migaUI(paso) {
  rastro.push(new Date().toISOString().slice(11, 19) + " " + String(paso || "").slice(0, 120));
  if (rastro.length > RASTRO_MAX) rastro.shift();
}

// Envía un error técnico redactado al backend local (que aplica tope 10/h,
// deduplicación y respeta el interruptor de telemetría en Ajustes).
export function reportarError(category, message, detail) {
  try {
    const body = JSON.stringify({
      category: String(category || "ui.error").slice(0, 64),
      message: String(message || "").slice(0, 400),
      detail: String(detail || "").slice(0, 400),
      trail: rastro.slice(-16),
    });
    fetch("/api/diagnostics/report", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body,
    }).catch(() => {});
  } catch { /* nunca rompe la UI */ }
}

export function showJSError(linea) {
  reportarError("ui.js", linea, "");
  let d = document.getElementById("js-error");
  if (!d) {
    d = document.createElement("div");
    d.id = "js-error";
    d.style.cssText = "position:fixed;left:0;right:0;bottom:0;z-index:999;background:#b8281f;color:#fff;font:12px/1.4 monospace;padding:6px;white-space:pre-wrap";
    document.body.appendChild(d);
  }
  const lineas = d.textContent ? d.textContent.split("\n") : [];
  lineas.push(linea);
  d.textContent = lineas.slice(-4).join("\n");
}
window.addEventListener("error", (e) => {
  showJSError("JS: " + e.message + " @ " + (e.filename || "") + ":" + e.lineno);
});
window.addEventListener("unhandledrejection", (e) => {
  showJSError("PROMESA: " + String(e.reason && (e.reason.stack || e.reason.message || e.reason)));
});
export const $ = (s) => document.querySelector(s);
export const $$ = (s) => [...document.querySelectorAll(s)];
export const esc = (s) => String(s == null ? "" : s).replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));
// Solo direcciones que el CSP permite: https, rutas del mismo origen y fotos
// en base64. Un javascript:, un data:text/html, un http:// o un //externo no
// debe llegar al DOM. safeUrl era más estricto que el CSP y descartaba avatares
// legítimos que venían como ruta relativa o como data:image.
export const safeUrl = (u) => {
  const s = String(u || "");
  // El navegador normaliza `\` a `/`: `/\host` se volvería `//host` (externo).
  if (s.includes("\\")) return "";
  if (/^https:\/\//i.test(s)) return s;
  if (/^\/(?!\/)/.test(s)) return s;
  if (/^data:image\/(?:png|jpe?g|gif|webp);base64,/i.test(s)) return s;
  return "";
};
// Una foto que no carga no deja un icono roto: se oculta y quedan las iniciales
// que ya están debajo. El oyente va en captura porque las imágenes se crean
// después de este punto, en cada repintado.
document.addEventListener("error", (e) => {
  const img = e.target;
  if (img && img.tagName === "IMG" && img.hasAttribute("data-face")) img.style.display = "none";
}, true);

export const showEl = (el, on) => { el.style.display = on ? "" : "none"; };

export const enMiB = (bytes) => (bytes / 1048576).toFixed(1) + " MiB";

export const initials = (name) => (name || "··").split(/\s+/).filter(Boolean).slice(0, 2).map((w) => w[0]).join("").toUpperCase();

export function fmtBytes(n) {
  if (n < 1024) return n + " B";
  if (n < 1024 * 1024) return (n / 1024).toFixed(1).replace(".", ",") + " KB";
  return (n / (1024 * 1024)).toFixed(1).replace(".", ",") + " MB";
}

export function toast(msg) {
  const el = $("#toast");
  if (!el) return;
  el.querySelector("div:nth-child(2)").textContent = msg;
  el.classList.add("on");
  setTimeout(() => el.classList.remove("on"), 6000);
}

export const fold = (s) => String(s || "").toLowerCase().normalize("NFD").replace(/\p{Mn}/gu, "");

export function trunc(s, n) {
  s = String(s || "");
  return s.length > n ? s.slice(0, n - 1) + "…" : s;
}

// Temblor determinístico por id. El hash anterior repetía valores para ids
// parecidos: "d0" y "d12" caían a 4.9 de distancia y los rombos rosas quedaban

export function md(text) {
  const s = esc(text);
  const bloques = [];
  const conCodigo = s.replace(/```([\s\S]*?)```/g, (_, code) => {
    bloques.push("<pre><code>" + code.replace(/^\n+|\n+$/g, "") + "</code></pre>");
    return "\u0000B" + (bloques.length - 1) + "\u0000";
  });
  const mdInline = (t) => t
    .replace(/\[([^\]]+)\]\((https:[^)\s]+)\)/gi, '<a href="$2" target="_blank" rel="noreferrer">$1</a>')
    .replace(/\*\*([^*]+)\*\*/g, "<b>$1</b>")
    .replace(/(^|[\s(>])\*([^*\n]+)\*/g, "$1<i>$2</i>")
    .replace(/`([^`]+)`/g, "<code>$1</code>")
    .replace(/(^|[\s(])@([\p{L}\p{N}_.-]+)/gu, '$1<span class="mention">@$2</span>');
  // Cada pieza es un párrafo, una lista entera, un encabezado o un bloque de
  // código pendiente de reponer. Entre piezas va un salto; dentro de una lista
  // no puede haber saltos, por eso las piezas no se mezclan con líneas sueltas.
  const piezas = [];
  let lista = "";
  let items = [];
  let parrafo = [];
  const cerrarLista = () => {
    if (lista) piezas.push("<" + lista + ">" + items.join("") + "</" + lista + ">");
    lista = "";
    items = [];
  };
  const volcarParrafo = () => {
    if (parrafo.length) piezas.push(parrafo.join("<br>"));
    parrafo = [];
  };
  for (const linea of conCodigo.split("\n")) {
    const t = linea.trim();
    if (!t) { volcarParrafo(); cerrarLista(); continue; }
    if (/^\u0000B\d+\u0000$/.test(t)) { volcarParrafo(); cerrarLista(); piezas.push(t); continue; }
    const h = t.match(/^#{1,6}\s+(.*)$/);
    if (h) { volcarParrafo(); cerrarLista(); piezas.push("<b>" + mdInline(h[1]) + "</b>"); continue; }
    const ul = t.match(/^[-*]\s+(.*)$/);
    const ol = t.match(/^\d+[.)]\s+(.*)$/);
    if (ul || ol) {
      volcarParrafo();
      const tipo = ul ? "ul" : "ol";
      if (lista !== tipo) cerrarLista();
      if (!lista) { lista = tipo; items = []; }
      items.push("<li>" + mdInline(ul ? ul[1] : ol[1]) + "</li>");
      continue;
    }
    cerrarLista();
    parrafo.push(mdInline(t));
  }
  volcarParrafo();
  cerrarLista();
  return piezas.join("<br>").replace(/\u0000B(\d+)\u0000/g, (_, i) => bloques[Number(i)] || "");
}

const PALETTE = ["#3aa9c4", "#7c6cf2", "#e0803a", "#4f9d69", "#c4553f", "#8a6fbf", "#2f8fa8", "#b5883a"];
export function colorFor(name) {
  let h = 0;
  for (const ch of String(name || "")) h = (h * 31 + ch.codePointAt(0)) % 9973;
  return PALETTE[h % PALETTE.length];
}

// Avatar of a message author: the photo when the profile has one, initials on
// their color when it does not.
export function avatarHTML(author) {
  const who = author ? author.name : "";
  const raw = author ? (author.avatar_url || author.photo_url || author.picture || author.avatar || "") : "";
  const url = safeUrl(raw);
  return '<span class="rav" style="background:' + colorFor(who) + '">' + esc(initials(who)) +
    (url ? '<img data-face src="' + esc(url) + '" alt="' + esc(who) + '">' : "") + "</span>";
}

// Un adjunto se abre dentro de la ventana, no en el navegador: el chat no
// pierde el hilo. La dirección pasa por safeUrl y el texto se escapa al pintar.
export function attachmentHTML(f) {
  const url = safeUrl(f.download_url);
  const nombre = f.file_name || "";
  if (!url) return '<span class="chip"><svg class="i"><use href="#i-clip"/></svg> ' + esc(nombre) + "</span>";
  const attrs = ' data-att-url="' + esc(url) + '" data-att-name="' + esc(nombre) + '" data-att-mime="' + esc(f.mime_type || "") + '"';
  if (String(f.mime_type || "").startsWith("image/")) {
    return '<button type="button" class="rimg-open" title="' + esc(nombre) + '"' + attrs + '><img class="rimg" src="' + esc(url) + '" alt="' + esc(nombre) + '" loading="lazy"></button>';
  }
  return '<button type="button" class="chip"' + attrs + '><svg class="i"><use href="#i-clip"/></svg> ' + esc(nombre) + "</button>";
}

export const ATTACH_TEXT = /^(text\/|application\/(json|xml|x-yaml|yaml|javascript|x-sh|x-powershell|sql))/i;
export function attachmentType(mime) {
  const m = String(mime || "").toLowerCase();
  if (m.startsWith("image/")) return T("attach.typeImage");
  if (m === "application/pdf") return T("attach.typePdf");
  if (ATTACH_TEXT.test(m)) return T("attach.typeText");
  return T("attach.typeFile");
}

export function focusablesIn(root) {
  return [...root.querySelectorAll('button, [href], input, select, textarea, [tabindex]:not([tabindex="-1"])')]
    .filter((el) => !el.disabled && (el.offsetWidth || el.offsetHeight || el.getClientRects().length));
}

// navigator.clipboard puede faltar en una vista embebida; el respaldo usa un
// cuadro temporal y el comando viejo.
export async function copiarTexto(texto) {
  try {
    if (navigator.clipboard && navigator.clipboard.writeText) {
      await navigator.clipboard.writeText(texto);
      return;
    }
  } catch (e) { /* respaldo */ }
  const area = document.createElement("textarea");
  area.value = texto;
  area.style.position = "fixed";
  area.style.opacity = "0";
  document.body.appendChild(area);
  area.select();
  let copiado = false;
  try {
    copiado = document.execCommand("copy");
  } finally {
    area.remove();
  }
  if (!copiado) throw new Error(T("copy.failed"));
}
