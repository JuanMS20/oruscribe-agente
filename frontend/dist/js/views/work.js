// Vistas de trabajo: Hoy/Panel, Seguimiento, Mis tareas, Metas, Equipos, Avisos, Menciones, Puesta al día y modales.
import { T, getLang } from "../i18n.js";
import { $, $$, esc, safeUrl, showEl, md, colorFor, initials, toast, fold } from "../dom.js";
import { api, LONG_ACTION_TIMEOUT_MS, FILE_UPLOAD_TIMEOUT_MS, paginaDe } from "../api.js";
import { S, R, cacheKey, knownPeople, personName, counts, roleLabel, avatarUrl, faceInner, invalidateTaskCache, clearSnapshot, loadSnapshot, saveSnapshot } from "../state.js";
import { slugActualOrganizacion, opcionesOrganizacion, estadoOrganizacion, firmaOrganizacion } from "../estados.mjs";
import { agentHomeCard, approvalCard, agentWatch, loadAgent, paintAgentDocs, paintAgentImages, updateAgentSend } from "./agent.js";
import { setReplyBar, stopChatPoll, startChatPoll, openRoom, getSaveTimer, setSaveTimer } from "./team.js";
import { closeBrainCard } from "./brain.js";
import { loadSettings } from "./settings.js";
import { refreshAll, render, setView, stopLive, startLive, cancelarReintentosMe, clearLoadRetry, paintLoadBanner, paintConn } from "../main.js";

/* ---------- datos de panel, seguimiento y equipos ---------- */
const DAY = 86400000;

// Un fallo de metas, actividad o decisiones no puede pintarse como un cero:
// se apunta qué sección falló y el panel lo dice con un reintento.
export function cargaFallo(clave, error) {
  S.cargasFallidas.add(clave);
  if (S.me) render();
}

export function cargaBien(clave) {
  if (S.cargasFallidas.delete(clave) && S.me) render();
}

export async function loadActivity(force) {
  if (!force && S.activity && Date.now() - S.activityAt < 60000) return;
  const generacion = R.meGeneracion;
  let data = null;
  try {
    data = await api("/api/activity?days=14");
  } catch (err) {
    if (generacion !== R.meGeneracion) return;
    cargaFallo("activity", err);
    return;
  }
  if (generacion !== R.meGeneracion) return;
  cargaBien("activity");
  S.activity = data.events || [];
  S.activityAt = Date.now();
  if (S.me) render();
}

export async function loadDecisions(force) {
  if (!force && S.decisions && Date.now() - S.decisionsAt < 60000) return;
  const generacion = R.meGeneracion;
  let data = null;
  try {
    data = await api("/api/decisions" + (S.decisionQuery ? "?q=" + encodeURIComponent(S.decisionQuery) : ""));
  } catch (err) {
    if (generacion !== R.meGeneracion) return;
    cargaFallo("decisions", err);
    return;
  }
  if (generacion !== R.meGeneracion) return;
  cargaBien("decisions");
  S.decisions = data.decisions || [];
  S.decisionsAt = Date.now();
  if (S.me) render();
}

export async function loadTeam(force) {
  if (!force && S.teams && Date.now() - S.teamsAt < 60000) return;
  const generacion = R.meGeneracion;
  let data = null;
  try {
    data = await api("/api/team");
  } catch (err) {
    if (generacion !== R.meGeneracion) return;
    S.teamsError = err.message;
    if (S.me) render();
    return;
  }
  if (generacion !== R.meGeneracion) return;
  S.teamsError = "";
  S.teams = data.teams || [];
  S.teamsAt = Date.now();
  saveSnapshot();
  if (S.me) render();
}

export async function loadPeriods(force) {
  if (!force && S.periods && Date.now() - S.periodsAt < 60000) return;
  const generacion = R.meGeneracion;
  const data = await api("/api/periods").catch(() => null);
  if (generacion !== R.meGeneracion) return;
  if (!data) return;
  S.periods = data.periods || [];
  S.periodsAt = Date.now();
  saveSnapshot();
  if (S.me) render();
}

export async function loadTrack(force) {
  if (!force && S.trackTasks && Date.now() - S.trackAt < 30000) return;
  // El tablero comparte actividad, decisiones y equipos con el panel. Cada uno
  // tiene su propio plazo y su propia caché: pedirlos otra vez aquí duplicaba
  // cuatro viajes en cada refresco.
  const generacion = R.meGeneracion;
  const tasks = await api("/api/tasks/search?limit=50").catch(() => null);
  if (generacion !== R.meGeneracion) return;
  if (tasks) {
    S.trackTasks = tasks.tasks || [];
    // Solo una lectura que llegó es fresca: marcar trackAt con tasks null
    // dejaba el tablero viejo como fresco 30 s y escondía el fallo.
    S.trackAt = Date.now();
  }
  loadActivity(force);
  loadDecisions(force);
  loadTeam(force);
  saveSnapshot();
  if (S.me) render();
}

// Las metas tardan segundos en el servidor. Se piden aparte para que el resto
// de la app no espere por ellas, y no más de una vez por minuto: cada aviso del
// timbre vuelve a refrescar.

export async function loadGoals(force) {
  if (!force && Date.now() - R.goalsAt < 60000) return;
  const generacion = R.meGeneracion;
  let data = null;
  try {
    data = await api("/api/goals");
  } catch (err) {
    if (generacion !== R.meGeneracion) return;
    cargaFallo("goals", err);
    return;
  }
  if (generacion !== R.meGeneracion) return;
  cargaBien("goals");
  S.goals = data.goals || [];
  R.goalsAt = Date.now();
  saveSnapshot();
  render();
}


export function badge(id, n) {
  const el = $(id);
  if (!el) return;
  el.textContent = n > 99 ? "99+" : String(n);
  el.style.display = n ? "inline-grid" : "none";
}

export function paintBadges() {
  const c = counts();
  const mUnread = S.mentions.filter((m) => !m.read).length;
  badge("#c-today", c.unread + c.open);
  badge("#c-tasks", c.open);
  badge("#c-chat", c.chat);
  badge("#c-bell", c.unread);
  badge("#c-mentions", mUnread);
}


export function renderMe() {
  const me = S.me;
  if (!me) return;
  const face = faceInner(me.name, me.avatar_url || me.photo_url || me.picture || me.avatar || "");
  $("#me-av").innerHTML = face;
  $("#me-name").textContent = me.name;
  $("#me-sub").textContent = roleLabel(me.role);
  $("#ac-av").innerHTML = face;
  $("#ac-name").textContent = me.name;
  $("#ac-email").textContent = me.email || "";
  $("#ac-org").textContent = me.organization ? me.organization.name + " (" + me.organization.slug + ")" : "—";
  $("#ac-role").textContent = roleLabel(me.role);
  $("#ac-server").textContent = T("set.conn");
  paintConn(R.connOn);
  renderOrgSelector();
}

let orgCambiando = false;
let firmaOrgRender = "";

function restaurarSeleccionOrg() {
  const sel = $("#org-select");
  if (sel) sel.value = slugActualOrganizacion(S.me);
}

export function renderOrgSelector() {
  const caja = $("#org-switch");
  const sel = $("#org-select");
  if (!caja || !sel) return;
  const me = S.me;
  const estado = estadoOrganizacion(me);
  const nota = $("#org-switch-note");
  if (nota) showEl(nota, estado === "sin-lista");
  const notaAjustes = $("#org-note");
  if (notaAjustes) showEl(notaAjustes, estado === "sin-lista");
  if (estado === "sin-sesion") {
    showEl(caja, false);
    firmaOrgRender = "";
    return;
  }
  const etiqueta = T("org.current");
  const firma = firmaOrganizacion(me, etiqueta);
  if (firma !== firmaOrgRender) {
    const ops = opcionesOrganizacion(me);
    sel.innerHTML = ops.map((o) => '<option value="' + esc(o.slug) + '">' + esc(o.name) +
      (o.current ? " · " + esc(etiqueta) : "") + "</option>").join("");
    sel.value = slugActualOrganizacion(me);
    firmaOrgRender = firma;
  }
  sel.disabled = orgCambiando || estado !== "con-opciones";
  showEl(caja, true);
}

export async function seleccionarOrg(slug) {
  if (orgCambiando || !S.me) return;
  const actual = (S.me.organization && S.me.organization.slug) || "";
  if (!slug || slug === actual) return;
  const ofrecidas = Array.isArray(S.me.other_organizations) ? S.me.other_organizations : [];
  const elegida = ofrecidas.find((o) => o && o.slug === slug);
  if (!elegida) {
    restaurarSeleccionOrg();
    toast(T("org.error"));
    return;
  }
  if (!window.confirm(T("org.confirm").replace("{name}", elegida.name || slug))) {
    restaurarSeleccionOrg();
    return;
  }
  orgCambiando = true;
  if (S.agentMic) S.agentMic.parar();
  clearTimeout(getSaveTimer());
  setSaveTimer(null);
  cancelarReintentosMe();
  // La caché de la organización anterior no puede revivir en la nueva.
  const viejaClave = cacheKey();
  try { localStorage.removeItem(viejaClave); } catch { /* sin almacenamiento */ }
  renderOrgSelector();
  try {
    const st = await api("/api/org/seleccionar", {
      method: "POST",
      body: JSON.stringify({ slug })
    });
    stopLive();
    stopChatPoll();
    clearLoadRetry();
    R.loadWasOffline = false;
    paintLoadBanner("off");
    // Ni un rastro de la organización anterior: datos, contadores, bandeja,
    // conversación del agente y título. El servidor ya canceló el turno y abrió
    // una conversación nueva conservando las anteriores en disco.
    S.loaded = false;
    S.tasks = []; S.rooms = []; S.alerts = []; S.mentions = []; S.goals = []; S.periods = [];
    S.tasksPage = null; S.roomsPage = null; S.alertsPage = null; S.mentionsPage = null;
    S.activity = null; S.decisions = null; S.teams = null; S.trackTasks = null;
    S.digest = null; S.capture = { folder: "", since: "", scanning: false, checked: {} };
    S.messages.clear(); S.msgSig.clear(); S.liveOpen = null; S.liveChat = null; S.room = null;
    S.notes = []; R.curNote = null; R.replyTarget = null; setReplyBar(false);
    const tituloNota = $("#note-title");
    if (tituloNota) tituloNota.value = "";
    const cuerpoNota = $("#note-body");
    if (cuerpoNota) cuerpoNota.value = "";
    const guardadoNota = $("#note-saved");
    if (guardadoNota) guardadoNota.textContent = T("notes.saved");
    const entradaSala = $("#room-input");
    if (entradaSala) {
      entradaSala.value = "";
      entradaSala.dispatchEvent(new Event("input", { bubbles: true }));
    }
    S.agent = null; S.convs = null; S.agentSince = 0; S.agentOpen = new Set(); S.agentLogHtml = "";
    // Los adjuntos y el borrador del cerebro anterior no pueden viajar al de
    // la org nueva: sin esto «Enviar» sigue habilitado y manda el archivo de
    // A al cerebro de B (#125). Se invalida también la carga en vuelo para
    // que `refreshAll` pueda pedir el agente de la org nueva.
    agentWatch(false);
    R.agentLoading = 0;
    S.agentDocs = []; S.agentImages = []; S.agentFolders = [];
    S.agentEdit = false; S.agentSetupShown = false; S.agentFormReady = false;
    const entradaAgente = $("#agent-input");
    if (entradaAgente) entradaAgente.value = "";
    paintAgentDocs(); paintAgentImages(); updateAgentSend();
    S.me = st.me || S.me;
    S.settings = null;
    S.view = "panel";
    try { localStorage.removeItem("myFocus"); } catch { /* nada que borrar */ }
    S.myContext = null;
    render();
    startLive();
    startChatPoll();
    await refreshAll();
    await loadSettings();
    toast((S.me.organization && S.me.organization.name) || slug);
  } catch (err) {
    toast(err.message || T("org.error"));
    const estado = await api("/api/state").catch(() => null);
    if (estado && estado.me) S.me = estado.me;
    render();
  } finally {
    orgCambiando = false;
    renderOrgSelector();
    restaurarSeleccionOrg();
  }
}

/* ---------- today ---------- */
export function fmtDate(d) {
  if (!d) return "";
  // Las fechas sin hora (YYYY-MM-DD, due_date) se leen como fecha local a
  // mano: new Date() las lee en UTC y en Bogotá eso pinta el día anterior.
  // Un timestamp completo (el at de una sesión) ya trae hora y zona: se lee
  // entero y toLocaleDateString lo pasa al día local; tomar solo el prefijo
  // UTC pintaba el día siguiente en UTC-5.
  const s = String(d);
  const m = /^(\d{4})-(\d{2})-(\d{2})$/.exec(s);
  const dt = m ? new Date(+m[1], +m[2] - 1, +m[3]) : new Date(s);
  if (Number.isNaN(dt.getTime())) return "";
  return dt.toLocaleDateString(getLang() === "en" ? "en-GB" : "es-ES", { day: "numeric", month: "short" });
}

export function summary(open, unread) {
  return open + " " + (open === 1 ? T("today.tOne") : T("today.tMany")) + " · " + unread + " " + (unread === 1 ? T("today.aOne") : T("today.aMany"));
}

export function emptyState(icon, title, sub) {
  return '<div class="empty"><svg class="i"><use href="#' + icon + '"/></svg><b>' + esc(title) + "</b><p>" + esc(sub) + "</p></div>";
}
const rowIcon = (icon) => '<div class="t-ic"><svg class="i"><use href="#' + icon + '"/></svg></div>';
const cardOf = (rows) => '<div class="card">' + rows.join("") + "</div>";
export const skeleton = () => '<div class="card"><div class="row-i"><span class="spinner"></span><div class="t-body"><b>' + esc(T("common.loading")) + "</b></div></div></div>";

/* ---------- piezas compartidas ---------- */
const statusPill = (st) => {
  const cls = st === "done" ? "ok" : st === "blocked" ? "bad" : st === "in_progress" ? "sig" : "";
  return '<span class="pill2 ' + cls + '">' + esc(T("tasks." + st)) + "</span>";
};
// La columna y el filtro hablan en plural; la píldora de una tarea, en singular.
const statusColumn = (st) => st === "done" ? T("track.doneMany") : st === "blocked" ? T("track.blockedMany") : T("tasks." + st);
export const pctOf = (g) => Math.round(Math.max(0, Math.min(100, g.progress_pct || g.pct || 0)));
const workersOf = (g) => (g.workers_summary && g.workers_summary.length ? g.workers_summary : (g.per_worker || []));
const dayKey = (ts) => { const d = new Date(ts); return Number.isNaN(d.getTime()) ? "" : d.toDateString(); };
export const dayText = (ts) => {
  const d = new Date(ts);
  if (Number.isNaN(d.getTime())) return "";
  const today = new Date(); today.setHours(0, 0, 0, 0);
  const that = new Date(d); that.setHours(0, 0, 0, 0);
  const diff = Math.round((today - that) / DAY);
  if (diff === 0) return T("chat.today");
  if (diff === 1) return T("chat.yesterday");
  return d.toLocaleDateString(getLang() === "en" ? "en-GB" : "es-ES", { weekday: "short", day: "numeric", month: "short" });
};
export const ownerOf = (t) => (t.assigned_to ? personName(t.assigned_to) : "");
export const goalOf = (id) => (S.goals || []).find((g) => g.id === id);

function sectionHead(title, sub, view) {
  return '<div class="section-h"><h2>' + esc(title) + "</h2>" +
    (sub ? "<small>" + esc(String(sub)) + "</small>" : "") +
    (view ? '<button class="link" data-view="' + view + '">' + esc(T("panel.viewAll")) + "</button>" : "") + "</div>";
}

function goalCard(g) {
  const pct = pctOf(g);
  const workers = workersOf(g);
  return '<button class="goal-card" data-goal="' + esc(g.id) + '">' +
    '<div class="gc-top"><h3>' + esc(g.title) + "</h3><span class=\"gc-pct\">" + pct + "%</span></div>" +
    '<div class="bar"><i style="width:' + pct + '%"></i></div>' +
    '<div class="gc-meta">' +
      (g.cells_total ? '<span class="pill2">' + (g.cells_done || 0) + "/" + g.cells_total + " " + esc(T("goals.tasks")) + "</span>" : "") +
      (g.due_date ? '<span class="pill2 warn">' + esc(T("goals.due")) + " " + esc(fmtDate(g.due_date)) + "</span>" : "") +
      (g.needs_attention ? '<span class="pill2 bad">' + esc(T("goals.attention")) + "</span>" : "") +
      workers.slice(0, 4).map((w) => '<span class="pill2">' + esc(w.name) + " " + Math.round(w.pct || 0) + "%</span>").join("") +
    "</div></button>";
}

function feedItem(e) {
  const icons = { decision_recorded: "i-flag", goal_created: "i-target", task_status_changed: "i-tasks", context_updated: "i-clock" };
  const cls = { decision_recorded: "decision", goal_created: "goal", task_status_changed: "task", context_updated: "context" };
  const ic = icons[e.type] || "i-bell";
  const who = e.actor_name || "";
  const actor = knownPeople().get(e.user_id || e.actor_id || "");
  const faceUrl = avatarUrl(actor);
  const face = actor
    ? '<div class="feed-ic face" style="background:' + colorFor(actor.name) + '">' + esc(initials(actor.name)) +
      (faceUrl ? '<img data-face src="' + esc(faceUrl) + '" alt="">' : "") + "</div>"
    : '<div class="feed-ic ' + (cls[e.type] || "") + '"><svg class="i"><use href="#' + ic + '"/></svg></div>';
  let text = "";
  if (e.type === "decision_recorded") text = "<b>" + esc(who) + "</b> · " + esc(e.title || "");
  else if (e.type === "goal_created") text = "<b>" + esc(who) + "</b> · " + esc(e.title || "");
  else if (e.type === "task_status_changed") text = "<b>" + esc(who) + "</b> · " + esc(e.task_title || "") + (e.to_status ? " → " + esc(T("tasks." + e.to_status)) : "");
  else if (e.type === "context_updated") text = "<b>" + esc(who) + "</b> · " + esc(e.current_focus || e.notes || "");
  else text = "<b>" + esc(who) + "</b> · " + esc(e.title || T("panel.activity"));
  return '<div class="feed-item">' + face +
    '<div class="feed-body">' + text + "<small>" + esc(dayText(e.timestamp)) + "</small></div></div>";
}

function feedBlock(events, limit) {
  const list = (events || []).slice(0, limit || 8);
  if (!list.length) return '<p class="small-muted">' + esc(T("panel.noActivity")) + "</p>";
  return '<div class="feed">' + list.map(feedItem).join("") + "</div>";
}

function decisionBlock(items, limit) {
  const list = (items || []).slice(0, limit || 5);
  if (!list.length) return '<p class="small-muted">' + esc(T("panel.noDecisions")) + "</p>";
  return list.map((d) => '<div class="feed-item"><div class="feed-ic decision"><svg class="i"><use href="#i-flag"/></svg></div>' +
    '<div class="feed-body"><b>' + esc(d.title) + "</b><small>" + esc((d.decision || "").slice(0, 150)) + " · " + esc(dayText(d.created_at)) + "</small></div></div>").join("");
}

/* ---------- panel ---------- */
// La pantalla de entrada es del arnés, no del tablero corporativo: primero lo
// que espera una decisión, después el día de la persona, después el agente y al
// final el pulso de la organización.
function attentionRow(icon, text, view, action) {
  return '<button class="att-row" data-view="' + view + '">' +
    '<span class="att-ic"><svg class="i"><use href="#' + icon + '"/></svg></span>' +
    '<span class="att-tx"><b>' + esc(text) + "</b></span>" +
    '<span class="att-go">' + esc(action) + ' <svg class="i"><use href="#i-chev"/></svg></span></button>';
}


export function renderPanel() {
  if (!S.loaded) { $("#v-panel").innerHTML = skeleton(); return; }
  const c = counts();
  const goals = S.goals || [];
  const open = S.tasks.filter((t) => t.status !== "done");
  const blocked = S.tasks.filter((t) => t.status === "blocked").length;
  const mUnread = S.mentions.filter((m) => !m.read).length;
  const approvals = (S.agent && S.agent.approvals) || [];
  const avg = goals.length ? Math.round(goals.reduce((n, g) => n + pctOf(g), 0) / goals.length) : 0;
  const primary = goals.find((g) => g.is_primary) || goals.slice().sort((a, b) => (b.cells_total || 0) - (a.cells_total || 0))[0];
  let html = "";

  const fallidas = [...S.cargasFallidas];
  if (fallidas.length) {
    html += '<div style="display:flex;align-items:center;gap:8px;padding:10px 0"><span class="fine">' +
      esc(T("load.partial")) + " " + esc(fallidas.map((clave) => T("panel." + clave)).join(", ")) +
      '</span><button class="btn sm" data-retry-cargas="1">' + esc(T("common.retry")) + "</button></div>";
  }

  // 1. Lo que espera una decisión. Si no hay nada, una línea tranquila.
  const rows = [];
  if (c.unread) rows.push(attentionRow("i-bell", c.unread + " " + T(c.unread === 1 ? "home.alertOne" : "home.alertMany"), "bell", T("home.review")));
  if (mUnread) rows.push(attentionRow("i-at", mUnread + " " + T(mUnread === 1 ? "home.mentionOne" : "home.mentionMany"), "mentions", T("home.review")));
  if (blocked) rows.push(attentionRow("i-alert", blocked + " " + T(blocked === 1 ? "home.blockedOne" : "home.blockedMany"), "tasks", T("home.review")));
  // La decisión sobre el agente no se toma desde el inicio: el panel enlaza a
  // la vista del agente, donde aprobar exige dos clics.
  if (approvals.length) rows.push(attentionRow("i-zap", approvals.length + " " + T(approvals.length === 1 ? "home.approvalOne" : "home.approvalMany"), "agent", T("home.review")));
  if (rows.length) {
    html += '<section class="card attention"><div class="section-h in-card"><h2>' + esc(T("home.attention")) + "</h2><small>" + esc(T("home.attentionSub")) + "</small></div><div class=\"attention-body\">";
    html += '<div class="att-list">' + rows.join("") + "</div>";
    html += "</div></section>";
  } else {
    html += '<section class="card attention clear"><div class="att-clear"><span class="att-ic ok"><svg class="i"><use href="#i-check-circle"/></svg></span><div><b>' + esc(T("home.clear")) + "</b><small>" + esc(T("home.clearSub")) + "</small></div></div></section>";
  }

  // 2. El día de la persona y el pulso de la organización.
  html += '<div class="home-grid"><div class="home-col">';
  html += '<section class="card home-day"><div class="section-h in-card"><h2>' + esc(T("home.day")) + "</h2><small>" +
    open.length + " " + esc(T(open.length === 1 ? "today.tOne" : "today.tMany")) +
    '</small><button class="link" data-view="tasks">' + esc(T("panel.viewAll")) + "</button></div>";
  if (!open.length) {
    html += '<div class="empty slim"><svg class="i"><use href="#i-check-circle"/></svg><b>' + esc(T("today.emptyT")) + "</b><p>" + esc(T("today.emptyS")) + "</p></div>";
  } else {
    html += open.slice(0, 6).map((t) => '<div class="row-i st ' + esc(t.status) + '" data-task-open="' + esc(t.id) + '" tabindex="0">' + rowIcon("i-tasks") +
      '<div class="t-body"><b>' + esc(t.title) + "</b><small>" + statusText(t) + "</small></div>" +
      '<div class="t-act"><button class="btn sm" data-advance="' + esc(t.id) + '" data-task-title="' + esc(t.title) + '">' + esc(T("tasks.advances")) + "</button></div></div>").join("");
  }
  html += "</section>";

  html += '<section class="home-pulse"><div class="section-h"><h2>' + esc(T("home.pulse")) + "</h2><small>" + esc(T("home.pulseSub")) +
    '</small><button class="link" data-view="goals">' + esc(T("panel.viewAll")) + "</button></div>";
  if (primary) {
    const pct = pctOf(primary);
    const periodo = (S.periods || []).find((p) => p.is_current);
    html += '<div class="pulse-main card"><div class="pm-head"><span class="kicker">' + esc(T("panel.mainGoal")) + '</span><span class="pm-pct">' + pct + '%</span></div>' +
      "<b>" + esc(primary.title) + "</b>" +
      '<div class="bar"><i style="width:' + pct + '%"></i></div>' +
      '<div class="pm-foot"><small>' + (primary.cells_done || 0) + " " + esc(T("panel.doneOf")) + " " + (primary.cells_total || 0) + " · " + esc(T("panel.tasksDone")) + "</small>" +
      (periodo ? '<span class="pill2 sig">' + esc(periodo.name) + "</span>" : "") +
      '<button class="btn sm" data-goal="' + esc(primary.id) + '">' + esc(T("goals.detail")) + "</button></div></div>";
  }
  html += '<div class="pulse-stats">' +
    '<div class="pstat"><b>' + goals.length + "</b><span>" + esc(T("panel.stats.goals")) + "</span></div>" +
    '<div class="pstat"><b>' + avg + "%</b><span>" + esc(T("panel.stats.avg")) + "</span></div>" +
    '<div class="pstat"><b>' + knownPeople().size + "</b><span>" + esc(T("panel.stats.people")) + "</span></div>" +
    "</div>";
  html += goals.length ? '<div class="goal-list">' + goals.slice(0, 3).map(goalCard).join("") + "</div>" : '<p class="small-muted">' + esc(T("panel.noGoals")) + "</p>";
  html += "</section></div>";

  // 3. El arnés: qué está haciendo el agente y qué movió al equipo.
  html += '<div class="home-col">';
  html += agentHomeCard();
  html += '<section class="card home-feed"><div class="section-h in-card"><h2>' + esc(T("panel.activity")) + '</h2><button class="link" data-view="track">' + esc(T("panel.viewAll")) + "</button></div>" + feedBlock(S.activity, 6) + "</section>";
  html += '<section class="card home-feed"><div class="section-h in-card"><h2>' + esc(T("panel.decisions")) + '</h2><button class="link" data-view="goals">' + esc(T("panel.viewAll")) + "</button></div>" + decisionBlock(S.decisions, 4) + "</section>";
  html += "</div></div>";

  $("#v-panel").innerHTML = html;
  if (!window.__oruReadyAt) window.__oruReadyAt = performance.now();
}

function statusText(t) {
  let s = T("tasks." + t.status);
  if (t.priority === "high" || t.priority === "critical") s += " · " + T("prio." + t.priority);
  if (t.due_date) s += " · " + T("goals.due") + " " + fmtDate(t.due_date);
  const owner = ownerOf(t);
  if (owner) s += " · " + owner;
  return esc(s);
}

/* ---------- seguimiento ---------- */
function trackTasks() {
  const all = S.trackTasks || S.tasks || [];
  const q = fold(S.track.q || "");
  const from = S.track.from ? dateMs(S.track.from) : null;
  const to = S.track.to ? dateMs(S.track.to) : null;
  return all.filter((t) => {
    if (S.track.status && t.status !== S.track.status) return false;
    if (S.track.member && t.assigned_to !== S.track.member) return false;
    if (q && !fold(t.title + " " + (t.description || "")).includes(q)) return false;
    if (from || to) {
      const s = dateMs(t.start_date || t.due_date);
      const e = dateMs(t.due_date || t.start_date);
      if (!Number.isFinite(s) && !Number.isFinite(e)) return false;
      if (Number.isFinite(from) && Math.max(s, e) < from) return false;
      if (Number.isFinite(to) && Math.min(s, e) > to) return false;
    }
    return true;
  });
}

function taskCard(t) {
  const owner = ownerOf(t);
  return '<div class="kcard" data-task-open="' + esc(t.id) + '" role="button" tabindex="0">' +
    "<b>" + esc(t.title) + "</b>" +
    (t.due_date ? "<small>" + esc(T("goals.due")) + " " + esc(fmtDate(t.due_date)) + "</small>" : "") +
    '<div class="kc-foot">' + statusPill(t.status) +
    (t.priority === "high" || t.priority === "critical" ? '<span class="pill2 warn">' + esc(T("prio." + t.priority)) + "</span>" : "") +
    (owner ? '<span class="av" title="' + esc(owner) + '">' + esc(initials(owner)) + "</span>" : "") +
    '<button class="btn sm" data-advance="' + esc(t.id) + '" data-task-title="' + esc(t.title) + '" style="margin-left:auto">' + esc(T("tasks.advances")) + "</button>" +
    "</div></div>";
}

function dateMs(d) {
  const m = /^(\d{4})-(\d{2})-(\d{2})/.exec(String(d || ""));
  if (!m) return NaN;
  return new Date(+m[1], +m[2] - 1, +m[3]).getTime();
}

function ganttHtml() {
  const all = S.trackTasks || S.tasks || [];
  const dated = all.filter((t) => t.due_date || t.start_date);
  if (!dated.length) return emptyState("i-clock", T("gantt.noDates"), T("gantt.noDatesSub"));
  const starts = dated.map((t) => dateMs(t.start_date || t.due_date));
  const ends = dated.map((t) => dateMs(t.due_date || t.start_date));
  const min = Math.min.apply(null, starts.concat(ends)) - DAY;
  const max = Math.max.apply(null, starts.concat(ends)) + DAY;
  const total = Math.max(DAY, max - min);
  const days = Math.round(total / DAY);
  const step = days <= 10 ? 1 : Math.ceil(days / 6);
  let head = "";
  for (let i = 0; i <= days; i += step) {
    const d = new Date(min + i * DAY);
    head += '<span class="gtick" style="left:' + ((i / days) * 100).toFixed(2) + '%">' +
      esc(d.toLocaleDateString(getLang() === "en" ? "en-GB" : "es-ES", { day: "numeric", month: "short" })) + "</span>";
  }
  const rows = dated.slice().sort((a, b) => dateMs(a.start_date || a.due_date) - dateMs(b.start_date || b.due_date)).map((t) => {
    const s = dateMs(t.start_date || t.due_date);
    const e = dateMs(t.due_date || t.start_date);
    let left, width;
    if (t.start_date) {
      left = ((Math.min(s, e) - min) / total) * 100;
      width = Math.max(2, ((Math.abs(e - s) + DAY) / total) * 100);
    } else {
      // Solo hay fecha de entrega: una marca en ese día, no una barra de un día
      // entero que se lea como si el trabajo empezara al día siguiente.
      width = 2.5;
      left = ((e - min) / total) * 100 - width / 2;
    }
    return '<div class="grow"><div class="glabel" title="' + esc(t.title) + '">' + esc(t.title) + "</div>" +
      '<div class="gtrack"><div class="gbar ' + esc(t.status) + '" style="left:' + left.toFixed(2) + "%;width:" + width.toFixed(2) + '%" title="' + esc(t.title) + " · " + esc(T("tasks." + t.status)) + '"></div></div></div>';
  }).join("");
  const undated = all.length - dated.length;
  const startLabel = new Date(min + DAY).toLocaleDateString(getLang() === "en" ? "en-GB" : "es-ES", { day: "numeric", month: "short" });
  const endLabel = new Date(max - DAY).toLocaleDateString(getLang() === "en" ? "en-GB" : "es-ES", { day: "numeric", month: "short" });
  const rangeText = startLabel === endLabel ? startLabel : esc(T("gantt.range")) + " " + esc(startLabel) + " " + esc(T("gantt.to")) + " " + esc(endLabel);
  return '<div class="section-h"><h2>' + esc(T("track.gantt")) + "</h2><small>" + rangeText +
    (undated ? " · " + undated + " " + esc(T("gantt.undated")) : "") + "</small></div>" +
    '<div class="gantt"><div class="ghead">' + head + "</div>" + rows + "</div>";
}

function stateHtml() {
  const all = S.trackTasks || S.tasks || [];
  if (!all.length) return emptyState("i-tasks", T("state.none"), T("tasks.emptyS"));
  const statuses = ["pending", "in_progress", "blocked", "done"];
  const people = new Map();
  const bucket = (id, name) => {
    if (!people.has(id)) people.set(id, { name, counts: { pending: 0, in_progress: 0, blocked: 0, done: 0 }, total: 0 });
    return people.get(id);
  };
  for (const t of all) {
    const id = t.assigned_to || "";
    const name = id ? (personName(id) || id) : T("track.unassigned");
    const p = bucket(id, name);
    p.counts[t.status] = (p.counts[t.status] || 0) + 1;
    p.total++;
  }
  const rows = [...people.values()].sort((a, b) => b.total - a.total);
  const totals = { pending: 0, in_progress: 0, blocked: 0, done: 0 };
  rows.forEach((p) => statuses.forEach((s) => { totals[s] += p.counts[s] || 0; }));
  const head = '<div class="st-head"><span>' + esc(T("state.member")) + "</span>" +
    statuses.map((s) => "<span>" + esc(T("tasks." + s)) + "</span>").join("") + "<span>" + esc(T("state.total")) + "</span></div>";
  const body = rows.map((p) => '<div class="st-row"><span class="st-name">' + esc(p.name) + "</span>" +
    statuses.map((s) => '<span class="st-n ' + s + '">' + (p.counts[s] || 0) + "</span>").join("") +
    '<span class="st-total">' + p.total + "</span></div>").join("");
  const foot = '<div class="st-row st-foot"><span class="st-name">' + esc(T("state.summary")) + "</span>" +
    statuses.map((s) => '<span class="st-n ' + s + '">' + (totals[s] || 0) + "</span>").join("") +
    '<span class="st-total">' + all.length + "</span></div>";
  return '<div class="section-h"><h2>' + esc(T("track.state")) + "</h2><small>" + rows.length + " " + esc(rows.length === 1 ? T("track.person") : T("track.people")) + "</small></div>" +
    '<div class="st-table">' + head + body + foot + "</div>";
}

export function renderTrack() {
  if (!S.loaded) { $("#v-track").innerHTML = skeleton(); return; }
  const tab = S.track.tab;
  const tasks = trackTasks();
  const activeFilters = !!(S.track.status || S.track.q || S.track.member || S.track.from || S.track.to);
  const people = [...knownPeople().values()].sort((a, b) => (a.name || "").localeCompare(b.name || ""));
  let html = '<div class="toolbar"><div class="tabs">' +
    ["kanban", "list", "state", "gantt", "activity", "decisions"].map((k) => '<button data-track-tab="' + k + '" class="' + (tab === k ? "on" : "") + '">' + esc(T("track." + k)) + "</button>").join("") +
    "</div>" +
    ((tab === "kanban" || tab === "list")
      ? '<input class="input" id="tr-q" placeholder="' + esc(T("track.search")) + '" value="' + esc(S.track.q) + '">' +
        '<select class="input sel" id="tr-status"><option value="">' + esc(T("track.all")) + "</option>" +
        ["pending", "in_progress", "blocked", "done"].map((s) => '<option value="' + s + '"' + (S.track.status === s ? " selected" : "") + ">" + esc(statusColumn(s)) + "</option>").join("") +
        "</select>" +
        '<select class="input sel" id="tr-member"><option value="">' + esc(T("track.anyone")) + "</option>" +
        people.map((p) => '<option value="' + esc(p.id) + '"' + (S.track.member === p.id ? " selected" : "") + ">" + esc(p.name) + "</option>").join("") +
        "</select>" +
        '<label class="date-field"><span>' + esc(T("track.from")) + '</span><span class="date-wrap"><input class="input date' + (S.track.from ? " has-value" : "") + '" id="tr-from" type="date" aria-label="' + esc(T("track.from")) + '" value="' + esc(S.track.from) + '"><span class="date-ph" aria-hidden="true">' + esc(T("track.noDate")) + '</span></span></label>' +
        '<label class="date-field"><span>' + esc(T("track.to")) + '</span><span class="date-wrap"><input class="input date' + (S.track.to ? " has-value" : "") + '" id="tr-to" type="date" aria-label="' + esc(T("track.to")) + '" value="' + esc(S.track.to) + '"><span class="date-ph" aria-hidden="true">' + esc(T("track.noDate")) + '</span></span></label>' +
        (activeFilters ? '<button class="btn sm" data-track-clear="1">' + esc(T("track.clear")) + "</button>" : "")
      : tab === "decisions"
        ? '<input class="input" id="dec-q" placeholder="' + esc(T("track.decisionSearch")) + '" value="' + esc(S.decisionQuery) + '">'
        : "") +
    "</div>";

  if (tab === "kanban" || tab === "list") {
    const all = S.trackTasks || S.tasks || [];
    const blocked = all.filter((t) => t.status === "blocked").length;
    const done = all.filter((t) => t.status === "done").length;
    const owners = new Set(all.map((t) => t.assigned_to).filter(Boolean));
    const tope = all.length >= 50;
    html += '<div class="brain-stats"><span class="pill2">' + (tope ? "50+" : all.length) + " " + esc(T("brain.tasks").toLowerCase()) + "</span>" +
      '<span class="pill2 ' + (blocked ? "bad" : "") + '">' + blocked + " " + esc(T(blocked === 1 ? "track.blockedOne" : "track.blockedMany")) + "</span>" +
      '<span class="pill2">' + done + " " + esc(T(done === 1 ? "track.doneOne" : "track.doneMany")) + "</span>" +
      '<span class="pill2">' + owners.size + " " + esc(owners.size === 1 ? T("track.person") : T("track.people")) + "</span></div>";
  }

  if (tab === "kanban") {
    html += '<div class="kanban">' + ["pending", "in_progress", "blocked", "done"].map((st) => {
      const items = tasks.filter((t) => t.status === st);
      return '<div class="kcol"><div class="kh">' + esc(statusColumn(st)) + '<span class="count">' + items.length + "</span></div>" +
        (items.length ? items.map(taskCard).join("") : '<p class="empty-col">' + esc(T("track.emptyCol")) + "</p>") + "</div>";
    }).join("") + "</div>";
    if (!tasks.length) html += '<p class="small-muted" style="margin-top:12px">' + esc(T("track.none")) + "</p>";
  } else if (tab === "list") {
    if (!tasks.length) html += emptyState("i-tasks", T("track.none"), T("tasks.emptyS"));
    else html += cardOf(tasks.map((t) => '<div class="row-i st ' + esc(t.status) + '" data-task-open="' + esc(t.id) + '" tabindex="0">' + rowIcon("i-tasks") +
      '<div class="t-body"><b>' + esc(t.title) + "</b><small>" + statusText(t) + "</small></div>" +
      '<div class="t-act"><button class="btn sm" data-advance="' + esc(t.id) + '" data-task-title="' + esc(t.title) + '">' + esc(T("tasks.advances")) + "</button></div></div>"));
  } else if (tab === "gantt") {
    html += ganttHtml();
  } else if (tab === "state") {
    html += stateHtml();
  } else if (tab === "activity") {
    html += sectionHead(T("track.activity"), T("track.activitySub"));
    html += feedBlock(S.activity, 40);
  } else {
    html += sectionHead(T("track.decisions"), T("track.decisionsSub"));
    if (!S.decisions || !S.decisions.length) html += '<p class="small-muted">' + esc(T("panel.noDecisions")) + "</p>";
    else {
      let last = "";
      html += S.decisions.map((d) => {
        const k = dayKey(d.created_at);
        let head = "";
        if (k && k !== last) { last = k; head = '<p class="day-sec">' + esc(dayText(d.created_at)) + "</p>"; }
        return head + '<div class="card" style="margin-bottom:10px;padding:14px 16px"><b>' + esc(d.title) + "</b>" +
          '<p class="gd-desc" style="margin:6px 0 0">' + esc((d.decision || "").slice(0, 420)) + "</p></div>";
      }).join("");
    }
  }
  $("#v-track").innerHTML = html;
}

/* ---------- metas ---------- */
export function renderGoals() {
  if (!S.loaded) { $("#v-goals").innerHTML = skeleton(); return; }
  const all = S.goals || [];
  const periods = S.periods || [];
  const list = (S.periodFilter ? all.filter((g) => g.period_id === S.periodFilter) : all).slice().sort((a, b) => pctOf(b) - pctOf(a));
  let html = "";
  if (periods.length) {
    html += '<div class="toolbar"><div class="tabs"><button data-period-filter="" class="' + (S.periodFilter ? "" : "on") + '">' + esc(T("periods.all")) + "</button>" +
      periods.map((p) => '<button data-period-filter="' + esc(p.id) + '" class="' + (S.periodFilter === p.id ? "on" : "") + '">' +
        esc(p.name) + (p.is_current ? " · " + esc(T("periods.current")) : "") + "</button>").join("") + "</div></div>";
  }
  if (list.length) {
    const avg = Math.round(list.reduce((n, g) => n + pctOf(g), 0) / list.length);
    const risk = list.filter((g) => g.needs_attention).length;
    html += '<div class="brain-stats"><span class="pill2">' + list.length + " " + esc(T("goals.count")) + "</span>" +
      '<span class="pill2 ok">' + avg + "% " + esc(T("panel.stats.avg")) + "</span>" +
      (risk ? '<span class="pill2 bad">' + risk + " " + esc(T("goals.attention")) + "</span>" : "") + "</div>";
  }
  if (!list.length) {
    html += S.periodFilter
      ? emptyState("i-target", T("periods.noGoals"), T("periods.noGoalsSub"))
      : emptyState("i-target", T("goals.emptyT"), T("goals.emptyS"));
  } else {
    html += list.map(goalCard).join("");
  }
  $("#v-goals").innerHTML = html;
}

/* ---------- equipos ---------- */
export function renderPeople() {
  if (!S.loaded) { $("#v-people").innerHTML = skeleton(); return; }
  const teams = S.teams;
  // Sin equipos cargados todavía (la caché ya no guarda las marcas por
  // persona): skeleton hasta que llegue el primer /api/team, no el estado
  // vacío — "todavía no" no es "no hay nadie".
  if (!Array.isArray(teams) || (teams.length === 0 && !S.teamsAt)) {
    if (S.teamsError) {
      $("#v-people").innerHTML = emptyState("i-users", T("people.errT"), S.teamsError) +
        '<div style="text-align:center;margin-top:10px"><button class="btn sm" id="people-retry">' + esc(T("common.retry")) + "</button></div>";
      $("#people-retry").onclick = () => { S.teamsError = ""; loadTeam(true); renderPeople(); };
      return;
    }
    $("#v-people").innerHTML = skeleton();
    return;
  }
  if (!teams.length) {
    $("#v-people").innerHTML = emptyState("i-users", T("people.noTeams"), T("sub.people"));
    return;
  }
  const people = new Set();
  teams.forEach((t) => (t.workers || []).forEach((w) => { if (w.user && w.user.id) people.add(w.user.id); }));
  const teamLabel = T(teams.length === 1 ? "people.teamOne" : "people.teamMany");
  const peopleLabel = T(people.size === 1 ? "people.personOne" : "people.personMany");
  $("#v-people").innerHTML = '<div class="brain-stats"><span class="pill2">' + teams.length + " " + esc(teamLabel) + "</span>" +
    '<span class="pill2">' + people.size + " " + esc(peopleLabel) + "</span></div><div class=\"people-grid\">" + teams.map((tm) => {
    let workers = tm.workers || [];
    const me = S.me;
    const mine = me && (me.teams || []).some((t) => t.id === tm.id) && !workers.some((w) => w.user && w.user.id === me.user_id);
    if (mine) {
      workers = [{ user: { id: me.user_id, name: me.name, role: me.role, is_agent: me.is_agent, avatar_url: me.avatar_url || "", photo_url: me.photo_url || "", picture: me.picture || "", avatar: me.avatar || "" }, context: S.myContext || null, tasks: S.tasks }, ...workers];
    }
    return '<div class="team-card"><div class="tc-head"><b>' + esc(tm.name) + "</b><small>" + workers.length + " " + esc(T(workers.length === 1 ? "people.memberOne" : "people.memberMany")) + "</small></div>" +
      (workers.length ? workers.map((w) => {
        const u = w.user || {};
        const ctx = w.context || {};
        const tasks = (w.tasks || []).filter((t) => t.status !== "done");
        const moodLabel = { on_track: T("fm.onTrack"), at_risk: T("fm.atRisk"), blocked: T("fm.blocked") }[ctx.mood_signal] || T("fm.unknown");
        const purl = avatarUrl(u);
        const pface = '<div class="av" style="background:' + colorFor(u.name) + '">' + esc(initials(u.name)) +
          (purl ? '<img data-face src="' + esc(purl) + '" alt="' + esc(u.name) + '">' : "") + "</div>";
        return '<div class="person">' + pface + '<div class="pbody">' +
          "<b>" + esc(u.name || "") + (u.is_agent ? ' <span class="tag">' + esc(T("agent.tag")) + "</span>" : "") + "</b>" +
          '<small>' + esc(roleLabel(u.role)) + (ctx.current_focus ? " · " + esc(T("people.focus")) + ": " + esc(ctx.current_focus) : "") + "</small>" +
          (ctx.mood_signal ? '<span class="mood ' + esc(ctx.mood_signal) + '"><i></i>' + esc(moodLabel) + "</span>" : "") +
          '<div class="ptasks">' + (tasks.length
            ? tasks.slice(0, 5).map((t) => '<div class="pt ' + esc(t.status) + '"><i></i><span>' + esc(t.title) + "</span></div>").join("")
            : '<div class="pt"><i></i><span class="small-muted">' + esc(T("people.noTasks")) + "</span></div>") + "</div>" +
          "</div></div>";
      }).join("") : '<p class="small-muted" style="padding:12px 16px">' + esc(T("people.noTeams")) + "</p>") +
      "</div>";
  }).join("") + "</div>";
}

/* ---------- mentions ---------- */
export function renderMentions() {
  if (!S.loaded) { $("#v-mentions").innerHTML = skeleton(); return; }
  const all = S.mentions;
  const vis = S.mentionFilter === "unread" ? all.filter((m) => !m.read) : all;
  if (!all.length) {
    $("#v-mentions").innerHTML = emptyState("i-at", T("mentions.emptyT"), T("mentions.emptyS"));
    return;
  }
  const rows = vis.map((m) => {
    const when = new Date(m.created_at);
    const stamp = Number.isNaN(when.getTime()) ? "" : when.toLocaleString(getLang() === "en" ? "en-GB" : "es-ES", { day: "numeric", month: "short", hour: "2-digit", minute: "2-digit" });
    const who = m.author ? m.author.name : "";
    return (
      '<div class="row-i"' + (m.read ? ' style="opacity:.6"' : "") + ">" + rowIcon("i-at") +
      '<div class="t-body"><b>' + esc(who) + (m.room_name ? " · " + esc(m.room_name) : "") + "</b><small>" + esc(m.body || "") + (stamp ? " · " + esc(stamp) : "") + "</small></div>" +
      (m.room_id ? '<div class="t-act"><button class="btn sm" data-room="' + esc(m.room_id) + '">' + esc(T("mentions.open")) + "</button></div>" : "") + "</div>"
    );
  });
  let html = rows.length ? cardOf(rows) : emptyState("i-at", T("mentions.emptyT"), T("mentions.emptyS"));
  if (S.mentionsPage && (S.mentionsPage.has_more || S.mentionsPage.truncated)) {
    html += '<div class="fine">' + esc(T("tasks.moreNote")) + "</div>";
  }
  $("#v-mentions").innerHTML = html;
}

/* ---------- tasks ---------- */
export function renderTasks() {
  if (!S.loaded) { $("#v-tasks").innerHTML = skeleton(); return; }
  const q = fold(S.taskQuery || "");
  let list = S.tasks || [];
  if (S.taskFilter === "open") list = list.filter((t) => t.status !== "done");
  else if (S.taskFilter === "done") list = list.filter((t) => t.status === "done");
  if (q) list = list.filter((t) => fold(t.title + " " + (t.description || "")).includes(q));
  let html = '<div class="toolbar"><input class="input" id="task-q" placeholder="' + esc(T("tasks.search")) + '" value="' + esc(S.taskQuery) + '">' +
    '<select class="input sel" id="task-filter">' +
    ["open", "all", "done"].map((k) => '<option value="' + k + '"' + (S.taskFilter === k ? " selected" : "") + ">" + esc(statusColumn(k)) + "</option>").join("") +
    "</select></div>";
  if (!list.length) {
    html += emptyState("i-tasks", T("tasks.emptyT"), T("tasks.emptyS"));
  } else {
    html += cardOf(list.map((t) => {
      const st = t.status;
      const next = st === "pending" || st === "blocked" ? "in_progress" : "done";
      const label = st === "done" ? "" : (st === "pending" || st === "blocked" ? T("tasks.start") : T("tasks.finish"));
      return '<div class="row-i st ' + esc(st) + '" data-task-open="' + esc(t.id) + '" tabindex="0">' + rowIcon("i-tasks") +
        '<div class="t-body"><b>' + esc(t.title) + "</b><small>" + statusText(t) + "</small></div>" +
        '<div class="t-act">' + (label ? '<button class="btn sm" data-task="' + esc(t.id) + '" data-to="' + next + '">' + esc(label) + "</button>" : "") +
        (st !== "done" ? ' <button class="btn sm" data-task-agent="' + esc(t.id) + '" title="' + esc(T("td.workAgent")) + '">' + esc(T("agent.tag")) + "</button>" : "") +
        ' <button class="btn sm" data-advance="' + esc(t.id) + '" data-task-title="' + esc(t.title) + '">' + esc(T("tasks.advances")) + "</button></div></div>";
    }));
  }
  if (S.tasksPage && (S.tasksPage.has_more || S.tasksPage.truncated)) {
    html += '<div class="fine">' + esc(T("tasks.moreNote")) +
      (S.tasksPage.next_offset > 0 ? ' <button class="btn sm" id="tasks-more">' + esc(T("tasks.more")) + "</button>" : "") + "</div>";
  }
  $("#v-tasks").innerHTML = html;
}

export async function cargarMasTareas() {
  if (!S.tasksPage || !S.tasksPage.has_more || S.tasksPage.next_offset <= 0) return;
  const pagina = await api("/api/tasks?offset=" + encodeURIComponent(S.tasksPage.next_offset)).catch(() => null);
  if (!pagina) { toast(T("load.failed")); return; }
  const vistos = new Set((S.tasks || []).map((t) => t.id));
  (pagina.tasks || []).forEach((t) => { if (!vistos.has(t.id)) S.tasks.push(t); });
  S.tasksPage = paginaDe(pagina);
  renderTasks();
}

let ADV_TASK = null;
export function openAdvances(taskId, title) {
  ADV_TASK = taskId;
  $("#taskdetail").classList.remove("on");
  $("#adv-title").textContent = title ? (T("adv.title") + ": " + title) : T("adv.title");
  $("#adv-list").innerHTML = '<div class="fine">' + T("common.loading") + '</div>';
  $("#adv-status").textContent = "";
  $("#advances").classList.add("on");
  loadAdvances();
}
export function closeAdvances() { $("#advances").classList.remove("on"); ADV_TASK = null; }
async function loadAdvances() {
  if (!ADV_TASK) return;
  try {
    const data = await api("/api/tasks/files?task=" + encodeURIComponent(ADV_TASK));
    const files = data.files || [];
    const comments = data.comments || {};
    if (!files.length) { $("#adv-list").innerHTML = '<div class="fine">' + T("adv.empty") + '</div>'; return; }
    // group by update_id, newest first (files already in attached order; reverse)
    const groups = new Map();
    files.forEach((f) => {
      const key = f.update_id || "";
      if (!groups.has(key)) groups.set(key, { note: f.note, files: [], update_id: key });
      groups.get(key).files.push(f);
    });
    const arr = [...groups.values()].reverse();
    const html = arr.map((g) => {
      const cs = comments[g.update_id] || [];
      const filesHtml = g.files.map((f) => '<div class="row-i"><div class="t-body"><b>' + esc(f.file_name) + '</b><small>' + esc(f.uploaded_by && f.uploaded_by.name) + ' · ' + esc(f.attached_at ? new Date(f.attached_at).toLocaleString() : "") + '</small>' +
        (safeUrl(f.download_url) ? ' <a href="' + esc(safeUrl(f.download_url)) + '" target="_blank">' + esc(T("adv.file")) + '</a>' : '') + '</div></div>').join("");
      const commentsHtml = cs.map((c) => '<div class="fine" style="margin:4px 0 4px 12px"><b>' + esc(c.author && c.author.name) + ':</b> ' + esc(c.body) + '</div>').join("");
      return '<div class="card" style="margin-bottom:10px"><div class="card-head"><b>' + esc(g.note || "") + '</b></div>' + filesHtml +
        commentsHtml +
        '<div class="row" style="margin-top:6px"><input class="input" data-comment="' + esc(g.update_id) + '" placeholder="' + esc(T("adv.comment")) + '" style="flex:1"><button class="btn sm" data-send-comment="' + esc(g.update_id) + '">' + esc(T("adv.reply")) + '</button></div></div>';
    }).join("");
    $("#adv-list").innerHTML = html;
  } catch (err) { $("#adv-list").innerHTML = '<div class="fine">' + esc(err.message) + '</div>'; }
}
export async function submitAdvance() {
  if (!ADV_TASK) return;
  const fileInput = $("#adv-file");
  const note = $("#adv-note").value.trim();
  if (!fileInput.files || !fileInput.files.length) { $("#adv-status").textContent = T("err.generic"); return; }
  const fd = new FormData();
  fd.append("task", ADV_TASK);
  fd.append("note", note);
  fd.append("file", fileInput.files[0]);
  $("#adv-status").textContent = T("adv.uploading");
  // Sin plazo, un servidor colgado dejaba «Subiendo» y el botón muerto para
  // siempre: conBotonUnaVez espera la promesa. Mismo patrón que attachChatFile.
  const control = new AbortController();
  const plazo = setTimeout(() => control.abort(), FILE_UPLOAD_TIMEOUT_MS);
  try {
    let res;
    try {
      res = await fetch("/api/tasks/files", { method: "POST", body: fd, signal: control.signal });
    } catch {
      throw new Error(T("load.failed"));
    }
    if (!res.ok) {
      let msg = T("err.generic");
      try { msg = (await res.json()).error || msg; } catch { /* sin JSON */ }
      throw new Error(msg);
    }
    $("#adv-status").textContent = T("tasks.updated");
    $("#adv-note").value = "";
    fileInput.value = "";
    await loadAdvances();
  } catch (err) { $("#adv-status").textContent = err.message; }
  finally {
    clearTimeout(plazo);
  }
}
export async function submitComment(updateId, body) {
  if (!ADV_TASK || !body) return;
  try {
    await api("/api/tasks/files/comment", { method: "POST", body: JSON.stringify({ task: ADV_TASK, update: updateId, body }) });
    await loadAdvances();
  } catch (err) { $("#adv-status").textContent = err.message; }
}

/* ---------- detalle de tarea ---------- */
let TD_TASK = null;

export function taskById(id) {
  // La lista propia trae la descripción completa; la del tablero llega
  // recortada del servidor, así que para abrir el detalle gana la propia.
  return (S.tasks || []).find((t) => t.id === id) || (S.trackTasks || []).find((t) => t.id === id);
}

// Las subtareas vienen anidadas en la tarea padre. Se pintan bajo ella, con su
// estado y la etiqueta, sin botones propios: una subtarea no crea tareas hijas.
// Se arman con el DOM para que el título del servidor no pase por innerHTML.
function paintSubtasks(t) {
  const box = document.getElementById("td-subs");
  if (!box) return;
  box.textContent = "";
  const subs = Array.isArray(t.subtasks) ? t.subtasks : [];
  if (!subs.length) return;

  const head = document.createElement("div");
  head.className = "section-h";
  const title = document.createElement("h2");
  title.textContent = T("td.subtasks");
  head.appendChild(title);
  box.appendChild(head);

  const card = document.createElement("div");
  card.className = "card";
  subs.forEach((s) => {
    const row = document.createElement("div");
    row.className = "row-i st " + s.status;
    const body = document.createElement("div");
    body.className = "t-body";
    const name = document.createElement("b");
    name.textContent = s.title;
    const tag = document.createElement("small");
    tag.textContent = T("td.subtask");
    body.append(name, tag);
    const pill = document.createElement("span");
    pill.className = "pill2 " + (s.status === "done" ? "ok" : s.status === "blocked" ? "bad" : s.status === "in_progress" ? "sig" : "");
    pill.textContent = T("tasks." + s.status);
    row.append(body, pill);
    card.appendChild(row);
  });
  box.appendChild(card);
}

export function openTaskDetail(id) {
  const t = taskById(id);
  if (!t) return;
  TD_TASK = id;
  const owner = ownerOf(t);
  const g = t.goal_id ? goalOf(t.goal_id) : null;
  const states = ["pending", "in_progress", "blocked", "done"];
  $("#td-body").innerHTML = '<div class="gd-head"><h3>' + esc(t.title) + "</h3>" + statusPill(t.status) + "</div>" +
    '<div class="gd-stats">' +
      (t.priority ? '<span class="pill2">' + esc(T("td.priority")) + ": " + esc(T("prio." + t.priority)) + "</span>" : "") +
      (t.due_date ? '<span class="pill2 warn">' + esc(T("td.due")) + ": " + esc(fmtDate(t.due_date)) + "</span>" : "") +
      (owner ? '<span class="pill2">' + esc(T("td.owner")) + ": " + esc(owner) + "</span>" : "") +
      (g ? '<span class="pill2 sig">' + esc(T("td.goal")) + ": " + esc(g.title) + "</span>" : '<span class="pill2">' + esc(T("td.noGoal")) + "</span>") +
    "</div>" +
    '<div class="section-h"><h2>' + esc(T("td.state")) + "</h2></div>" +
    '<div class="row" style="justify-content:flex-start">' + states.map((st) =>
      '<button class="btn sm' + (t.status === st ? " primary" : "") + '" data-td-status="' + st + '">' + esc(T("tasks." + st)) + "</button>").join("") +
      '<button class="btn sm" data-advance="' + esc(t.id) + '" data-task-title="' + esc(t.title) + '">' + esc(T("td.advances")) + "</button>" +
      '<button class="btn sm primary" data-task-agent="' + esc(t.id) + '">' + esc(T("td.workAgent")) + "</button></div>" +
    '<div id="td-subs"></div>' +
    '<div class="section-h"><h2>' + esc(T("td.description")) + "</h2></div>" +
    '<p class="gd-desc">' + md(t.description || T("td.noDesc")) + "</p>" +
    '<p class="fine" id="td-status"></p>';
  paintSubtasks(t);
  $("#taskdetail").classList.add("on");
}

export function closeTaskDetail() { $("#taskdetail").classList.remove("on"); }

/* ---------- motivo al bloquear ---------- */
let blockWhy = null;

// Devuelve el motivo escrito, o "" si la persona cancela. Sin motivo no se envía.
export function askBlockReason() {
  return new Promise((resolve) => {
    blockWhy = resolve;
    $("#bw-note").value = "";
    $("#bw-status").textContent = "";
    $("#blockwhy").classList.add("on");
    $("#bw-note").focus();
  });
}

export function answerBlockReason(send) {
  if (!blockWhy) return;
  if (send) {
    const note = $("#bw-note").value.trim();
    if (!note) {
      $("#bw-status").textContent = T("bw.need");
      $("#bw-note").focus();
      return;
    }
    const resolve = blockWhy;
    blockWhy = null;
    $("#blockwhy").classList.remove("on");
    resolve(note);
    return;
  }
  const resolve = blockWhy;
  blockWhy = null;
  $("#blockwhy").classList.remove("on");
  resolve("");
}

export async function changeTaskStatus(st) {
  if (!TD_TASK) return;
  let note = "";
  if (st === "blocked") {
    note = await askBlockReason();
    if (!note) return;
  }
  const msg = $("#td-status");
  if (msg) msg.textContent = T("common.loading");
  try {
    await api("/api/tasks/status", { method: "POST", body: JSON.stringify({ id: TD_TASK, status: st, note }) });
    toast(T("td.changed"));
    invalidateTaskCache();
    await refreshAll();
    if (S.trackTasks) loadTrack(true);
    openTaskDetail(TD_TASK);
  } catch (err) {
    if (msg) msg.textContent = err.message;
  }
}

/* ---------- alerts ---------- */
export function renderAlerts() {
  if (!S.loaded) { $("#v-bell").innerHTML = skeleton(); return; }
  const types = ["blocker", "misalignment", "milestone", "info"];
  const unread = S.alerts.filter((a) => !a.read).length;
  let list = S.alerts;
  if (S.alertFilter === "unread") list = list.filter((a) => !a.read);
  else if (S.alertFilter !== "all") list = list.filter((a) => a.type === S.alertFilter);
  const count = (f) => S.alerts.filter(f).length;
  const tabs = [["all", T("alerts.all"), S.alerts.length], ["unread", T("alerts.unread"), unread]]
    .concat(types.map((t) => [t, T("alerts.type." + t), count((a) => a.type === t)]));
  let html = '<div class="toolbar"><div class="tabs">' + tabs.map(([k, label, n]) =>
    '<button data-alert-filter="' + k + '" class="' + (S.alertFilter === k ? "on" : "") + '">' + esc(label) + " " + n + "</button>").join("") + "</div></div>";
  if (!list.length) {
    html += emptyState("i-bell", T("alerts.emptyT"), S.alertFilter === "all" ? T("alerts.emptyS") : T("alerts.none"));
  } else {
    let last = "";
    html += list.map((a) => {
      const k = dayKey(a.created_at);
      let head = "";
      if (k && k !== last) { last = k; head = '<p class="day-sec">' + esc(dayText(a.created_at)) + "</p>"; }
      const cls = a.type === "blocker" ? "bad" : a.type === "misalignment" ? "warn" : a.type === "milestone" ? "sig" : "";
      return head + '<div class="row-i' + (a.read ? "" : "") + '" style="' + (a.read ? "opacity:.62" : "") + '">' +
        '<div class="feed-ic ' + (a.type === "blocker" ? "context" : "") + '"><svg class="i"><use href="#' + (a.type === "blocker" ? "i-alert" : "i-bell") + '"/></svg></div>' +
        '<div class="t-body"><b>' + esc(a.message) + '</b><small><span class="pill2 ' + cls + '">' + esc(T("alerts.type." + a.type)) + "</span> · " +
        esc(a.created_at ? new Date(a.created_at).toLocaleString(getLang() === "en" ? "en-GB" : "es-ES", { hour: "2-digit", minute: "2-digit" }) : "") + "</small></div>" +
        (a.read ? "" : '<div class="t-act"><button class="btn sm" data-alert-one="' + esc(a.id) + '">' + esc(T("alerts.markOne")) + "</button></div>") + "</div>";
    }).join("");
  }
  if (S.alertsPage && (S.alertsPage.has_more || S.alertsPage.truncated)) {
    html += '<div class="fine">' + esc(T("tasks.moreNote")) + "</div>";
  }
  $("#v-bell").innerHTML = html;
}


export async function setTaskStatus(id, to) {
  let note = "";
  if (to === "blocked") {
    note = await askBlockReason();
    if (!note) return;
  }
  try {
    const res = await api("/api/tasks/status", { method: "POST", body: JSON.stringify({ id, status: to, note }) });
    const reminder = res && res.progress && res.progress.progress_reminder;
    invalidateTaskCache();
    await refreshAll();
    if (reminder) toast(reminder);
  } catch (err) {
    toast(err.message);
  }
}

/* ---------- crear tarea, alerta y detalle de meta ---------- */
export function openTaskModal() {
  $("#nt-name").value = "";
  $("#nt-desc").value = "";
  $("#nt-due").value = "";
  $("#nt-priority").value = "medium";
  const sel = $("#nt-goal");
  sel.innerHTML = '<option value="">' + esc(T("nt.noGoal")) + "</option>" +
    (S.goals || []).map((g) => '<option value="' + esc(g.id) + '">' + esc(g.title) + "</option>").join("");
  $("#nt-status").textContent = "";
  $("#newtask").classList.add("on");
  $("#nt-name").focus();
}

export function closeTaskModal() { $("#newtask").classList.remove("on"); }

// Un doble clic en "crear" mandaba dos POST iguales: el boton seguia activo
// mientras esperaba. Queda deshabilitado hasta que la peticion termina.
export async function conBotonUnaVez(boton, accion) {
  if (!boton || boton.disabled) return;
  boton.disabled = true;
  try { await accion(); } finally { boton.disabled = false; }
}

export async function submitTask() {
  const title = $("#nt-name").value.trim();
  const desc = $("#nt-desc").value.trim();
  if (!title || !desc) { $("#nt-status").textContent = T("nt.required"); return; }
  const body = {
    title, description: desc,
    priority: $("#nt-priority").value,
    due_date: $("#nt-due").value || "",
    goal_id: $("#nt-goal").value || ""
  };
  $("#nt-status").textContent = T("common.loading");
  try {
    await api("/api/tasks", { method: "POST", body: JSON.stringify(body) });
    closeTaskModal();
    toast(T("nt.created"));
    invalidateTaskCache();
    await refreshAll();
    if (S.view === "track") loadTrack(true);
  } catch (err) {
    $("#nt-status").textContent = err.message;
  }
}

export function openAlertModal() {
  const sel = $("#al-target");
  const opts = (S.teams || []).map((t) => '<option value="team:' + esc(t.id) + '">' + esc(t.name) + "</option>");
  const people = [...knownPeople().values()].filter((p) => p.id !== (S.me && S.me.user_id));
  opts.push.apply(opts, people.map((p) => '<option value="user:' + esc(p.id) + '">' + esc(p.name) + "</option>"));
  sel.innerHTML = opts.join("") || '<option value="">' + esc(T("al.choose")) + "</option>";
  $("#al-msg").value = "";
  $("#al-type").value = "info";
  $("#al-status").textContent = "";
  $("#alertbox").classList.add("on");
  $("#al-msg").focus();
}

export function closeAlertModal() { $("#alertbox").classList.remove("on"); }

export async function submitAlert() {
  const msg = $("#al-msg").value.trim();
  const target = $("#al-target").value;
  if (!msg) { $("#al-status").textContent = T("al.choose"); return; }
  if (!target) { $("#al-status").textContent = T("al.choose"); return; }
  const [kind, id] = target.split(":");
  const body = { type: $("#al-type").value, message: msg, team: "", user: "" };
  if (kind === "team") body.team = id; else body.user = id;
  $("#al-status").textContent = T("common.loading");
  try {
    await api("/api/alerts/send", { method: "POST", body: JSON.stringify(body) });
    closeAlertModal();
    toast(T("al.created"));
    // Sin este refresco el aviso queda guardado pero la vista no lo muestra
    // hasta que llegue otro aviso o un timbre: el mismo fallo silencioso del chat.
    await refreshAll();
  } catch (err) {
    $("#al-status").textContent = err.message;
  }
}

export async function openGoal(id) {
  const body = $("#gd-body");
  body.innerHTML = '<p class="small-muted">' + esc(T("common.loading")) + "</p>";
  $("#goaldetail").classList.add("on");
  const d = await api("/api/goals/detail?id=" + encodeURIComponent(id)).catch((err) => { body.innerHTML = '<p class="small-muted">' + esc(err.message) + "</p>"; return null; });
  if (!d) return;
  const g = d.goal || {};
  const tasks = d.linked_tasks || [];
  const pct = pctOf(g);
  const workers = workersOf(g);
  body.innerHTML = '<div class="gd-head"><h3>' + esc(g.title || "") + "</h3></div>" +
    '<p class="small-muted">' + esc(T("goals.progress")) + " · " + (g.cells_total ? (g.cells_done || 0) + "/" + g.cells_total + " " + esc(T("goals.tasks")) : "") + "</p>" +
    '<div class="gd-stats"><span class="pill2 sig">' + pct + "%</span>" +
    (g.due_date ? '<span class="pill2 warn">' + esc(T("goals.due")) + " " + esc(fmtDate(g.due_date)) + "</span>" : "") +
    (g.needs_attention ? '<span class="pill2 bad">' + esc(T("goals.attention")) + "</span>" : "") +
    workers.map((w) => '<span class="pill2">' + esc(w.name) + " " + Math.round(w.pct || 0) + "%</span>").join("") + "</div>" +
    '<div class="bar" style="margin:0 0 14px"><i style="width:' + pct + '%"></i></div>' +
    '<p class="gd-desc">' + md(g.description || T("goals.noDesc")) + "</p>" +
    '<div class="section-h"><h2>' + esc(T("goals.linked")) + "</h2><small>" + tasks.length + " " + esc(T("goals.tasks")) + "</small></div>" +
    (tasks.length ? '<ul class="gd-tasks">' + tasks.map((t) => '<li>' + statusPill(t.status) +
      '<span title="' + esc(t.title) + '">' + esc(t.title) + "</span>" +
      ((t.assignees || []).length ? '<span class="small-muted">' + esc(t.assignees.join(", ")) + "</span>" : "") + "</li>").join("") + "</ul>"
      : '<p class="small-muted">' + esc(T("track.none")) + "</p>");
}

export function closeGoalDetail() { $("#goaldetail").classList.remove("on"); }

export function openDmModal() {
  const sel = $("#dm-to");
  const people = [...knownPeople().values()].filter((p) => p.id !== (S.me && S.me.user_id));
  sel.innerHTML = people.length
    ? people.map((p) => '<option value="' + esc(p.id) + '">' + esc(p.name) + (p.is_agent ? " · " + esc(T("agent.tag")) : "") + "</option>").join("")
    : '<option value="">' + esc(T("dm.choosePerson")) + "</option>";
  $("#dm-msg").value = "";
  $("#dm-status").textContent = "";
  $("#newdm").classList.add("on");
  $("#dm-msg").focus();
}

export function closeDmModal() { $("#newdm").classList.remove("on"); }

export async function sendDm() {
  const to = $("#dm-to").value;
  const msg = $("#dm-msg").value.trim();
  if (!to || !msg) { $("#dm-status").textContent = T("al.choose"); return; }
  $("#dm-status").textContent = T("common.loading");
  try {
    await api("/api/messages", { method: "POST", body: JSON.stringify({ to, body: msg }) });
    const rooms = await api("/api/rooms").catch(() => null);
    if (rooms) S.rooms = rooms.rooms || [];
    if (rooms) S.liveChat = null;
    closeDmModal();
    const room = S.rooms.find((r) => r.with && r.with.id === to);
    setView("team");
    if (room) await openRoom(room.room_id);
    else render();
    toast(T("dm.sent"));
  } catch (err) {
    $("#dm-status").textContent = err.message;
  }
}

export function openGoalModal() {
  $("#ng-name").value = "";
  $("#ng-desc").value = "";
  $("#ng-due").value = "";
  const sel = $("#ng-team");
  const teams = (S.me && S.me.teams) || [];
  sel.innerHTML = '<option value="">' + esc(T("ng.org")) + "</option>" +
    teams.map((t) => '<option value="' + esc(t.id) + '">' + esc(t.name) + "</option>").join("");
  const per = S.periods || [];
  $("#ng-period").innerHTML = '<option value="">' + esc(T("periods.none")) + "</option>" +
    per.map((p) => '<option value="' + esc(p.id) + '"' + (p.is_current ? " selected" : "") + ">" + esc(p.name) + "</option>").join("");
  $("#ng-status").textContent = "";
  $("#newgoal").classList.add("on");
  $("#ng-name").focus();
}

export function closeGoalModal() { $("#newgoal").classList.remove("on"); }

export async function submitGoal() {
  const title = $("#ng-name").value.trim();
  if (!title) { $("#ng-status").textContent = T("ng.required"); return; }
  const body = {
    title,
    description: $("#ng-desc").value.trim(),
    due_date: $("#ng-due").value || "",
    team_id: $("#ng-team").value || "",
    period_id: $("#ng-period").value || ""
  };
  $("#ng-status").textContent = T("common.loading");
  try {
    await api("/api/goals", { method: "POST", body: JSON.stringify(body) });
    closeGoalModal();
    toast(T("ng.created"));
    loadGoals(true);
    loadActivity(true);
  } catch (err) {
    $("#ng-status").textContent = err.message;
  }
}

/* ---------- mi foco ---------- */
export function openFocusModal() {
  let focus = "", mood = "on_track";
  const me = S.me;
  if (me) {
    for (const tm of S.teams || []) {
      for (const w of tm.workers || []) {
        if (w.user && w.user.id === me.user_id) {
          if (w.context) {
            focus = w.context.current_focus || "";
            mood = w.context.mood_signal || "on_track";
          }
        }
      }
    }
  }
  if (!focus && S.myContext) {
    focus = S.myContext.current_focus || S.myContext.focus || "";
    mood = S.myContext.mood_signal || S.myContext.mood || "on_track";
  }
  $("#fm-focus").value = focus;
  $("#fm-mood").value = mood;
  $("#fm-notes").value = "";
  $("#fm-status").textContent = "";
  $("#focusmodal").classList.add("on");
  $("#fm-focus").focus();
}

export function closeFocusModal() { $("#focusmodal").classList.remove("on"); }

export async function saveFocus() {
  const focus = $("#fm-focus").value.trim();
  const notes = $("#fm-notes").value.trim();
  if (!focus && !notes) { $("#fm-status").textContent = T("fm.ph"); return; }
  $("#fm-status").textContent = T("common.loading");
  try {
    await api("/api/context", { method: "POST", body: JSON.stringify({ focus, mood: $("#fm-mood").value, notes }) });
    S.myContext = { current_focus: focus, mood_signal: $("#fm-mood").value };
    try { localStorage.setItem("myFocus", JSON.stringify(S.myContext)); } catch { /* sin almacenamiento local */ }
    toast(T("fm.saved"));
    closeFocusModal();
    loadTeam(true);
    loadActivity(true);
  } catch (err) {
    $("#fm-status").textContent = err.message;
  }
}

/* ---------- periodos ---------- */
function monthRange() {
  const now = new Date();
  const first = new Date(now.getFullYear(), now.getMonth(), 1);
  const last = new Date(now.getFullYear(), now.getMonth() + 1, 0);
  const iso = (d) => d.getFullYear() + "-" + String(d.getMonth() + 1).padStart(2, "0") + "-" + String(d.getDate()).padStart(2, "0");
  const name = now.toLocaleDateString(getLang() === "en" ? "en-GB" : "es-ES", { month: "long", year: "numeric" }).replace(/ de /i, " ");
  return { name: name.charAt(0).toUpperCase() + name.slice(1), start: iso(first), end: iso(last) };
}

export function openPeriodModal() {
  const r = monthRange();
  $("#np-name").value = r.name;
  $("#np-start").value = r.start;
  $("#np-end").value = r.end;
  $("#np-kind").value = "month";
  const sel = $("#np-team");
  const teams = (S.me && S.me.teams) || [];
  const isManager = S.me && S.me.role === "manager";
  sel.innerHTML = (isManager ? '<option value="">' + esc(T("periods.org")) + "</option>" : "") +
    teams.map((t) => '<option value="' + esc(t.id) + '">' + esc(t.name) + "</option>").join("");
  if (!isManager && teams.length) sel.value = teams[0].id;
  $("#np-status").textContent = "";
  $("#newperiod").classList.add("on");
  $("#np-name").focus();
}

export function closePeriodModal() { $("#newperiod").classList.remove("on"); }

export async function submitPeriod() {
  const name = $("#np-name").value.trim();
  const start = $("#np-start").value;
  const end = $("#np-end").value;
  if (!name || !start || !end) { $("#np-status").textContent = T("periods.required"); return; }
  if (end < start) { $("#np-status").textContent = T("periods.required"); return; }
  const body = { name, start_date: start, end_date: end, kind: $("#np-kind").value, team_id: $("#np-team").value || "" };
  $("#np-status").textContent = T("common.loading");
  try {
    await api("/api/periods", { method: "POST", body: JSON.stringify(body) });
    closePeriodModal();
    toast(T("periods.created"));
    S.periodsAt = 0;
    await loadPeriods(true);
  } catch (err) {
    $("#np-status").textContent = err.message;
  }
}

/* ---------- puesta al día ---------- */
function isoDaysAgo(n) {
  const d = new Date();
  d.setDate(d.getDate() - n);
  return d.toISOString().slice(0, 10);
}

export function renderCapture() {
  const st = S.capture;
  if (!st.since) st.since = isoDaysAgo(7);
  if (st.scanning) {
    $("#v-capture").innerHTML = '<div class="card" style="padding:28px;text-align:center"><span class="spinner"></span> <b>' + esc(T("cap.scanning")) + "</b></div>";
    return;
  }
  const form = '<div class="card" style="padding:16px 18px;margin-bottom:14px">' +
    '<p class="small-muted" style="margin:0 0 14px">' + esc(T("cap.explain")) + "</p>" +
    '<div class="field2"><div class="field"><label class="lbl">' + esc(T("cap.folder")) + '</label>' +
    '<div class="row" style="gap:8px"><input class="input mono" style="flex:1" id="cap-folder" placeholder="' + esc(T("cap.folderPh")) + '" value="' + esc(st.folder || "") + '">' +
    '<button class="btn sm" data-cap-pick="1">' + esc(T("cap.pick")) + "</button></div>" +
    '<p class="fine" style="margin:6px 0 0">' + esc(T("cap.folderHelp")) + "</p></div>" +
    '<div class="field"><label class="lbl">' + esc(T("cap.since")) + '</label><input class="input" id="cap-since" type="date" value="' + esc(st.since) + '"></div></div>' +
    '<div class="row" style="margin-top:12px"><button class="btn sm primary" data-cap-scan="1">' + esc(T("cap.scan")) + "</button></div></div>";

  if (!S.digest) {
    $("#v-capture").innerHTML = form + emptyState("i-refresh", T("cap.empty"), T("cap.emptySub"));
    return;
  }
  const cands = S.digest.candidates || [];
  const checked = st.checked || {};
  const n = cands.filter((c) => checked[c.id]).length;
  let html = form;
  html += '<div class="section-h"><h2>' + esc(T("cap.draft")) + "</h2><small>" + cands.length + " " + esc(T("cap.candidates")) + " · " + n + " " + esc(T("cap.checked")) + "</small>" +
    '<button class="link" data-cap-reset="1">' + esc(T("cap.scanAnother")) + "</button></div>";
  html += cands.map((c) => '<label class="cand"><input type="checkbox" data-cap-cand="' + esc(c.id) + '"' + (checked[c.id] ? " checked" : "") + ">" +
    '<div class="cand-body"><b>' + esc(c.title) + "</b>" +
    '<p class="small-muted">' + md((c.description || "").slice(0, 260)) + "</p>" +
    '<div class="gc-meta">' +
      (c.suggested_goal ? '<span class="pill2 sig">' + esc(T("cap.suggested")) + ": " + esc(c.suggested_goal.title) + "</span>" : "") +
      ((c.sources || []).length ? '<span class="pill2">' + esc(T("cap.source")) + ": " + esc(c.sources.join(", ")) + "</span>" : "") +
      ((c.refs || []).length ? '<span class="pill2">' + esc(c.refs.join(", ")) + "</span>" : "") +
    "</div></div></label>").join("");
  html += '<div class="row" style="justify-content:flex-start;margin-top:14px">' +
    '<button class="btn sm" data-cap-all="1">' + esc(T("cap.selectAll")) + "</button>" +
    '<button class="btn sm" data-cap-none="1">' + esc(T("cap.none")) + "</button>" +
    '<button class="btn sm primary" data-cap-apply="1"' + (n ? "" : " disabled") + ">" + esc(T("cap.apply")) + "</button></div>";
  html += '<p class="fine" id="cap-status"></p>';
  $("#v-capture").innerHTML = html;
}

export async function pickCaptureFolder() {
  try {
    const r = await api("/api/agent/pick-folder", { method: "POST", timeout: LONG_ACTION_TIMEOUT_MS });
    if (!r.path) return;
    S.capture.folder = r.path;
    const el = $("#cap-folder");
    if (el) el.value = r.path;
  } catch (err) {
    toast(err.message);
  }
}

export async function scanCapture() {
  const folderEl = $("#cap-folder");
  const sinceEl = $("#cap-since");
  const folder = folderEl ? folderEl.value.trim() : (S.capture.folder || "");
  const since = sinceEl ? sinceEl.value : (S.capture.since || isoDaysAgo(7));
  if (!folder) { toast(T("cap.needFolder")); return; }
  S.capture.folder = folder;
  S.capture.since = since;
  S.capture.scanning = true;
  renderCapture();
  try {
    const d = await api("/api/capture/propose", { method: "POST", body: JSON.stringify({ folder, since }) });
    S.digest = d;
    S.capture.checked = {};
    (d.candidates || []).forEach((c) => { S.capture.checked[c.id] = true; });
  } catch (err) {
    toast(err.message);
  }
  S.capture.scanning = false;
  renderCapture();
}

export async function applyCapture() {
  if (!S.digest) return;
  const checked = S.capture.checked || {};
  const ids = Object.keys(checked).filter((k) => checked[k]);
  if (!ids.length) return;
  const status = $("#cap-status");
  if (status) status.textContent = T("common.loading");
  try {
    await api("/api/capture/apply", { method: "POST", body: JSON.stringify({ digest: S.digest.digest_id, accept: ids }) });
    S.digest = null;
    S.capture.checked = {};
    toast(T("cap.applied"));
    S.trackTasks = null;
    refreshAll();
  } catch (err) {
    if (status) status.textContent = err.message;
  }
}

