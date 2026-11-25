// Vista Agente (Chat IA): proveedores, herramientas, aprobaciones, reglas y sesiones.
import { T } from "../i18n.js";
import { $, $$, esc, showEl, md, toast, fmtBytes, reportarError, migaUI } from "../dom.js";
import { api, LONG_ACTION_TIMEOUT_MS, AGENT_SEND_TIMEOUT_MS } from "../api.js";
import { S, R } from "../state.js";
import { fmtDate, renderPanel } from "./work.js";
import { setView, showScreen } from "../main.js";

export const WAKE_KIND_KEYS = { mention: "wake.mention", task_assigned: "wake.task_assigned", task: "wake.task_assigned", alert: "wake.alert" };
export function wakeKindLabel(kind) { return T(WAKE_KIND_KEYS[kind] || "wake.other"); }

export function agentHomeCard() {
  const a = S.agent;
  if (!a) {
    return '<section class="card home-agent"><div class="ha-head"><span class="ha-dot"></span><div><b>' + esc(T("home.agent")) + "</b><small>" + esc(T("common.loading")) + "</small></div></div></section>";
  }
  const on = !!a.configured;
  const status = a.running ? T("home.agentWorking") : on ? T("home.agentReady") : T("home.agentOff");
  const mode = a.mode === "readonly" ? T("agent.modeRead") : a.mode === "ask" ? T("agent.modeAsk") : T("agent.modeAuto");
  const brain = a.kind === "org" ? T("agent.brainOrg") : (a.model || T("home.agentOff"));
  const log = (a.wake_log || []).slice(-2).reverse();
  const full = a.kind === "org" && on ? '<div class="ha-hint"><button class="link" data-agent-act="settings">' + esc(T("home.agentFull")) + "</button></div>" : "";
  const body = on
    ? '<div class="ha-meta"><span class="pill2">' + esc(brain) + '</span><span class="pill2">' + esc(mode) + "</span></div>" +
      (log.length
        ? '<div class="ha-log">' + log.map((e) => '<div class="ha-line"><small class="mono">' + esc((e.at || "").slice(11, 16)) + "</small><span>" + esc(wakeKindLabel(e.kind)) + " · " + esc(e.result) + "</span></div>").join("") + "</div>"
        : '<p class="small-muted">' + esc(T("panel.noActivity")) + "</p>")
    : '<p class="small-muted">' + esc(T("home.noAgent")) + "</p>";
  return '<section class="card home-agent">' +
    '<div class="ha-head"><span class="ha-dot' + (on ? " on" : "") + '"></span><div><b>' + esc(T("home.agent")) + "</b><small>" + esc(status) + "</small></div>" +
    '<button class="btn sm' + (on ? "" : " primary") + '" data-view="agent">' + esc(on ? T("home.openAgent") : T("home.setupAgent")) + "</button></div>" +
    body + full + "</section>";
}

/* ---------- agente ---------- */
const AGENT_OR = "https://openrouter.ai/api/v1";
// Catálogo de proveedores. Vacío hasta que llegue: mientras tanto el selector
// se queda con las dos opciones que trae el HTML, que es lo que había antes.
let agentProviders = null;

export async function loadAgentProviders() {
  if (agentProviders) return agentProviders;
  let lista;
  try {
    lista = await api("/api/agent/providers");
  } catch {
    // El fallo no se cachea: agentProviders = [] (truthy) bloqueaba el
    // reintento en toda la sesión. Sigue null y el próximo loadAgent pide.
    return [];
  }
  agentProviders = Array.isArray(lista) ? lista : [];
  fillProviderSelect();
  return agentProviders;
}

// Cada opción es texto, no HTML: un nombre del catálogo no debe poder abrir
// una etiqueta dentro del selector.
function fillProviderSelect() {
  const sel = $("#agent-provider");
  if (!agentProviders.length) return;
  const elegido = sel.value;
  const hacer = (p) => {
    const o = document.createElement("option");
    o.value = p.id;
    o.textContent = p.name;
    return o;
  };
  sel.textContent = "";
  for (const p of agentProviders) {
    if (p.supported) sel.appendChild(hacer(p));
  }
  const otro = document.createElement("option");
  otro.value = "other";
  otro.textContent = T("agent.providerOther");
  sel.appendChild(otro);
  // Lo que estaba elegido se conserva si sigue en la lista.
  if ([...sel.options].some((o) => o.value === elegido)) sel.value = elegido;
}

// El desplegable de modelos se llena con el catálogo público del proveedor.
// Se guarda por proveedor en memoria para no repetir la lectura al abrir y
// cerrar Ajustes; si la red falla, el campo sigue libre y queda el enlace.
const agentModelCache = new Map();
export async function loadAgentModels(id) {
  const lista = $("#agent-model-options");
  if (!lista) return;
  if (!id || id === "other") { lista.innerHTML = ""; return; }
  if (agentModelCache.has(id)) { paintAgentModels(agentModelCache.get(id)); return; }
  try {
    const r = await api("/api/agent/models?provider=" + encodeURIComponent(id));
    const modelos = (r && r.models) || [];
    agentModelCache.set(id, modelos);
    // Si la persona cambió de proveedor mientras llegaba la respuesta, no se
    // pinta una lista que ya no corresponde.
    if ($("#agent-provider").value === id) paintAgentModels(modelos);
  } catch { /* el enlace al catálogo sigue disponible */ }
}

function paintAgentModels(modelos) {
  const lista = $("#agent-model-options");
  if (!lista) return;
  lista.innerHTML = modelos.map((m) =>
    '<option value="' + esc(m.id) + '">' + esc(m.name || m.id) + "</option>"
  ).join("");
}

const providerById = (id) => (agentProviders || []).find((p) => p.id === id);
// Una dirección guardada vuelve a su proveedor; una que no está en el
// catálogo se queda como dirección escrita a mano.
const providerByBase = (base) =>
  (agentProviders || []).find((p) => p.supported && p.base_url === base);
// Un Map y no un objeto: un nombre como "constructor" no debe encontrar nada por herencia.
const AGENT_TOOLS = new Map([
  ["get_my_tasks", "agent.tool.tasks"], ["get_my_mentions", "agent.tool.mentions"],
  ["send_chat_message", "agent.tool.send"], ["update_task_status", "agent.tool.update"],
  ["read", "agent.tool.read"], ["write", "agent.tool.write"],
  ["edit", "agent.tool.edit"], ["bash", "agent.tool.bash"],
]);
let agentTimer = null;


// Si la primera carga del agente falla, la vista no puede quedar en blanco:
// sin estado no hay formulario que mostrar, así que se muestra el error con
// una salida clara.
function agentError(msg) {
  const box = $("#agent-error");
  if (!box) return;
  $("#agent-error-msg").textContent = msg || "";
  showEl(box, true);
}
const agentMsg = (el, text, err) => { el.textContent = text; el.classList.toggle("agent-err", !!err); };
const agentBase = () => {
  const id = $("#agent-provider").value;
  if (id === "other") return $("#agent-base").value.trim();
  const p = providerById(id);
  // Sin catálogo cargado queda lo de antes: openrouter a su dirección y el
  // resto escrito a mano.
  if (p) return p.base_url;
  return id === "openrouter" ? AGENT_OR : $("#agent-base").value.trim();
};

// Pregunta cada 400 ms solo mientras el agente trabaja. Una petición en
// vuelo a la vez: sin la guarda, una respuesta lenta apila sondeos y el
// llavero del sistema se lee una vez por cada uno. Guarda la generación de
// `me` de la petición: un cambio de org la invalida y la deja volver a
// arrancar (#125). Cero significa que no hay sondeo en vuelo.

// Como LOGIN_POLL_ERRORS_MAX: un blip de red no es un fallo persistente.
const AGENT_WATCH_ERRORS_MAX = 5;
let agentWatchErrors = 0, agentRetryTimer = null;
export function agentWatch(on) {
  if (on && S.view === "agent" && !agentTimer) agentTimer = setInterval(loadAgent, 400);
  if (!on) {
    if (agentTimer) { clearInterval(agentTimer); agentTimer = null; }
    if (agentRetryTimer !== null) { clearTimeout(agentRetryTimer); agentRetryTimer = null; }
  }
}

export async function loadAgent() {
  if (R.agentLoading) {
    R.agentPendiente = true;
    return;
  }
  const generacion = R.meGeneracion;
  const marca = generacion + 1;
  R.agentLoading = marca;
  try {
    await loadAgentProviders();
    const agente = await api("/api/agent");
    const convs = agente.running ? S.convs : await api("/api/agent/conversations");
    // Un cambio de org mientras esperábamos descarta la respuesta: asignarla
    // después pintaría en la org nueva la conversación de la anterior (#125).
    if (generacion !== R.meGeneracion) return;
    S.agent = agente;
    S.convs = convs;
    agentWatchErrors = 0;
    if (agentRetryTimer !== null) { clearTimeout(agentRetryTimer); agentRetryTimer = null; }
  } catch (err) {
    if (generacion !== R.meGeneracion) return;
    if (err.status === 401) {
      // Sesión vencida: sondear no la arregla; se pide entrar de nuevo.
      agentWatchErrors = 0;
      agentWatch(false);
      if (S.agent) S.agent.running = false;
      agentAskLogin();
      if (S.view === "agent") renderAgent();
      return;
    }
    agentWatchErrors += 1;
    if (S.agent && S.agent.running) {
      // El servidor puede seguir trabajando: un fallo transitorio no apaga
      // el «Trabajando…» ni marca running=false en el cliente. Se reintenta
      // con espera creciente y tope en vez de soltar el turno a la deriva.
      agentWatch(false);
      const espera = Math.min(400 * 2 ** Math.min(agentWatchErrors - 1, 4), 5000);
      agentRetryTimer = setTimeout(() => {
        agentRetryTimer = null;
        if (S.agent && S.agent.running) loadAgent();
      }, espera);
      if (agentWatchErrors >= AGENT_WATCH_ERRORS_MAX) {
        if (S.view === "agent") agentMsg($("#agent-msg"), err.message, true);
      }
      return;
    }
    if (agentWatchErrors < AGENT_WATCH_ERRORS_MAX) return;
    // Fallo persistente sin turno en curso: se detiene y se dice qué pasó.
    agentWatchErrors = 0;
    agentWatch(false);
    if (S.agent) S.agent.running = false;
    if (S.agent) agentMsg($("#agent-msg"), err.message, true);
    else agentError(err.message);
    if (S.view === "agent") renderAgent();
    return;
  } finally {
    if (R.agentLoading === marca) R.agentLoading = 0;
    if (R.agentPendiente && !R.agentLoading) {
      R.agentPendiente = false;
      setTimeout(loadAgent, 0);
    }
  }
  if (S.view === "agent") renderAgent();
  else if (S.view === "panel") renderPanel();
  agentWatch(S.view === "agent" && !!(S.agent && S.agent.running));
}

// La lista de conversaciones guardadas en este equipo.
export function paintConversations() {
  const a = S.agent || {};
  const lista = S.convs || [];
  const sel = $("#agent-convs");
  sel.innerHTML = lista.map((c) => '<option value="' + esc(c.id) + '">' + esc(c.title) + "</option>").join("");
  if (a.conv_id) sel.value = a.conv_id;
  // El select queda como fuente de datos para el resto del código; a la vista
  // va la lista de sesiones, que es lo que hace usable el arnés.
  showEl(sel, false);
  const delUno = document.querySelector('[data-agent-act="delconv"]');
  const delTodas = document.querySelector('[data-agent-act="delconvs"]');
  // Borrar mientras corre pierde la respuesta: el backend también lo rechaza.
  const ocupado = !!a.running;
  if (delUno) showEl(delUno, !!a.conv_id && !ocupado);
  if (delTodas) showEl(delTodas, lista.length > 1 && !ocupado);
  paintSessionList(lista, a);
  $("#agent-tokens").textContent = a.tokens_in || a.tokens_out
    ? T("agent.tokens").replace("{in}", a.tokens_in || 0).replace("{out}", a.tokens_out || 0) : "";
}

// Una fila por sesión guardada. La fecha y el número de mensajes dan contexto
// sin abrirla; la que está en pantalla queda marcada.
let agentSessionKey = "";
function paintSessionList(lista, a) {
  const box = $("#agent-session-list");
  if (!lista.length) {
    agentSessionKey = "";
    if (box.innerHTML) box.innerHTML = '<p class="agent-sessions-empty">' + esc(T("agent.noConvs")) + "</p>";
    return;
  }
  const firma = JSON.stringify([a.conv_id, lista.map((c) => [c.id, c.title, c.messages, c.at])]);
  if (firma === agentSessionKey && box.querySelector(".agent-session")) return;
  agentSessionKey = firma;
  box.innerHTML = lista.map((c) => {
    const title = c.title || T("agent.untitled");
    const count = c.messages === 1 ? T("agent.msgOne") : T("agent.msgMany").replace("{n}", c.messages || 0);
    return '<button class="agent-session' + (c.id === a.conv_id ? " on" : "") + '" data-conv="' + esc(c.id) + '">' +
      "<b>" + esc(title) + "</b><small>" + esc(fmtDate(c.at)) + " · " + esc(count) + "</small></button>";
  }).join("");
}

export async function openConversation(id) {
  const generacion = R.meGeneracion;
  let agente = null;
  try {
    agente = await api("/api/agent/conversations/open", { method: "POST", body: JSON.stringify({ id }) });
  } catch (err) {
    if (generacion !== R.meGeneracion) return;
    agentMsg($("#agent-msg"), err.message, true);
    return;
  }
  if (generacion !== R.meGeneracion) return;
  S.agent = agente;
  S.agentSince = 0;
  S.agentOpen = new Set();
  renderAgent();
}

export async function deleteConversation(all) {
  const id = all ? "" : ((S.agent && S.agent.conv_id) || $("#agent-convs").value || "");
  if (!all && !id) return;
  // Borrar es irreversible: se confirma también la sesión suelta, con su título.
  if (all && !window.confirm(T("agent.deleteConvsConfirm"))) return;
  if (!all) {
    const sesion = (S.convs || []).find((c) => c.id === id);
    const titulo = (sesion && sesion.title) || T("agent.untitled");
    if (!window.confirm(T("agent.deleteConvConfirm").replace("{title}", titulo))) return;
  }
  const generacion = R.meGeneracion;
  let agente = null, convs = null;
  try {
    agente = await api("/api/agent/conversations/delete", { method: "POST", body: JSON.stringify(all ? { all: true } : { id }) });
    if (generacion !== R.meGeneracion) return;
    convs = await api("/api/agent/conversations");
  } catch (err) {
    if (generacion !== R.meGeneracion) return;
    agentMsg($("#agent-msg"), err.message, true);
    return;
  }
  if (generacion !== R.meGeneracion) return;
  S.agent = agente;
  S.convs = convs;
  renderAgent();
}

export function renderAgent() {
  const a = S.agent;
  if (!a) { agentError(T("err.generic")); return; }
  showEl($("#agent-error"), false);
  const setup = !a.configured || S.agentEdit;
  // El formulario se hidrata una vez aunque esté oculto: el select del cerebro
  // es la fuente para adjuntar imágenes y no puede quedarse con la opción
  // "org" del HTML cuando la persona ya usa su clave.
  if (!S.agentFormReady) {
    fillAgentForm();
    S.agentFormReady = true;
  } else if (setup && !S.agentSetupShown) {
    fillAgentForm();
  }
  S.agentSetupShown = setup;
  showEl($("#agent-setup"), setup);
  showEl($("#agent-chat"), !setup);
  showEl($("#agent-cancel"), a.configured);
  $("#agent-setup-title").textContent = T(a.configured ? "agent.settings" : "agent.title");
  $("#agent-send").textContent = T(a.running ? "agent.stop" : "agent.send");
  const brain = a.kind === "org" ? T("agent.brainOrg") : (a.model || "");
  const folders = a.folders || [];
  const activeMode = a.mode === "readonly" || a.mode === "ask" || a.mode === "autonomous" ? a.mode : "autonomous";
  $("#agent-brain").textContent = brain;
  // El selector #agent-mode-quick ya muestra el modo activo en la misma barra:
  // repetirlo al lado saturaba la cabecera en ventanas medianas.
  $("#agent-mode-label").textContent = "";
  const quickMode = $("#agent-mode-quick");
  if (quickMode) {
    quickMode.value = activeMode;
    quickMode.disabled = !!a.running;
  }
  const micBtn = $("#agent-mic");
  if (micBtn) showEl(micBtn, !!(a.voice_stt_url && String(a.voice_stt_url).trim()));
  const cwdPill = $("#agent-cwd-pill");
  if (cwdPill) {
    if (folders.length > 0) {
      const primera = folders[0];
      const base = primera.split(/[\\/]/).filter(Boolean).pop() || primera;
      cwdPill.textContent = base + (folders.length > 1 ? " +" + (folders.length - 1) : "");
      cwdPill.title = folders.join("\n") + "\n\n" + T("agent.piToolsHint");
    } else {
      cwdPill.textContent = T("agent.cwdHome");
      cwdPill.title = T("agent.piToolsHint");
    }
  }
  syncAgentForm();
  if (setup) loadAgentRules();
  // Sin sesión ni timbre el cerebro org no puede responder: se pide entrar en
  // vez de dejar el chat «pensando» hasta que venza el plazo.
  if (!setup && a.needs_login && !a.running) agentAskLogin();
  if (!setup) {
    paintConversations();
    paintAgentApprovals();
    paintWakeLog();
    paintAgentLog();
  }
  updateAgentSend();
}

// El registro de lo que el agente hizo por su cuenta, con la razón de cada línea.
export function paintWakeLog() {
  const a = S.agent || {};
  const box = $("#agent-wake-log");
  const lista = a.wake_log || [];
  showEl(box, !!a.wake_enabled || lista.length > 0);
  if (!lista.length) {
    $("#agent-wake-items").innerHTML = '<p class="fine">' + esc(T("agent.wakeNone")) + "</p>";
    return;
  }
  $("#agent-wake-items").innerHTML = lista.slice().reverse().map((e) =>
    '<div class="agent-wake-item"><small class="mono">' + esc(e.at.slice(11, 16)) + "</small> <b>" + esc(wakeKindLabel(e.kind)) + "</b> " + esc(e.result) + "</div>"
  ).join("");
}

// Una tarjeta por acción que espera el sí o el no. Los argumentos se escapan:
// son datos del modelo, no HTML. La casilla «Recordar» solo aparece cuando el
// servidor dice que la llamada admite una regla (rule_pattern). Aprobar exige
// dos clics deliberados: el primero arma y el segundo confirma; Escape o el
// tiempo desarma. Rechazar es directo, porque va en el sentido seguro.


// Todo desarme cancela el temporizador de la tarjeta: si una tarjeta vieja
// venciera después de armar otra, dejaría la confirmación nueva sin efecto.
export function desarmarAprobacion() {
  if (R.approvalArmed) clearTimeout(R.approvalArmed.timer);
  R.approvalArmed = null;
}

export function repaintApprovals() {
  if (S.view === "panel") renderPanel();
  else renderAgent();
}

export const approvalCard = (p) => {
  const armado = !!R.approvalArmed && R.approvalArmed.id === p.id && Date.now() < R.approvalArmed.hasta;
  return '<div class="agent-approval' + (armado ? " arming" : "") + '"><small class="agent-approval-intro">' + esc(T("agent.approvalIntro")) + "</small><b>" + esc(p.action) + "</b>" +
    (p.text ? "<code>" + esc(p.text) + "</code>" : "") +
    (p.rule_pattern
      ? '<p class="fine">' + esc(T("agent.rememberScope").replace("{rule}", p.rule_pattern)) + "</p>" +
        '<label class="agent-remember"><input type="checkbox" data-remember="' + esc(p.id) + '"> ' + esc(T("agent.remember")) + "</label>"
      : "") +
    '<div class="row"><button class="btn sm" data-approve="' + esc(p.id) + '" data-decision="no">' + esc(T("agent.reject")) + "</button>" +
    '<button class="btn sm' + (armado ? " danger" : " primary") + '" data-approve="' + esc(p.id) + '" data-decision="yes">' + esc(armado ? T("agent.approveConfirm") : T("agent.approve")) + "</button></div></div>";
};

let agentApprovalsKey = "";
export function paintAgentApprovals() {
  const a = S.agent || {};
  const box = $("#agent-approvals");
  const lista = a.approvals || [];
  if (!lista.length) {
    agentApprovalsKey = "";
    if (box.innerHTML) box.innerHTML = "";
    showEl(box, false);
    return;
  }
  // El poll de 400 ms no puede borrar la casilla que la persona acaba de
  // marcar. Si las tarjetas no cambiaron, se dejan como están; si cambió
  // alguna, se reaplica lo marcado por id.
  const firma = JSON.stringify(lista.map((p) => [p.id, p.action, p.text, p.rule_pattern])) + "|" + (R.approvalArmed && R.approvalArmed.id || "");
  if (firma === agentApprovalsKey && box.querySelector(".agent-approval")) return;
  const marcados = new Set(Array.from(box.querySelectorAll("[data-remember]:checked")).map((el) => el.dataset.remember));
  agentApprovalsKey = firma;
  showEl(box, true);
  box.innerHTML = lista.map(approvalCard).join("");
  for (const el of box.querySelectorAll("[data-remember]")) {
    if (marcados.has(el.dataset.remember)) el.checked = true;
  }
}

export async function agentApprove(id, ok, recordar) {
  const generacion = R.meGeneracion;
  let aprobaciones = null;
  try {
    aprobaciones = await api("/api/agent/approve", { method: "POST", body: JSON.stringify({ id, approve: ok, remember: !!recordar }) });
  } catch (err) {
    // El servidor responde 500 con "la acción se aprobó, pero no se pudo
    // recordar…" cuando recordar falla: la aprobación sí pasó; el aviso lo dice.
    if (generacion !== R.meGeneracion) return;
    agentMsg($("#agent-msg"), err.message, true);
    // La acción ya corrió: refrescar la cola para no dejar una tarjeta vieja.
    try {
      const agente = await api("/api/agent");
      if (generacion !== R.meGeneracion) return;
      S.agent = agente;
      renderAgent();
    } catch {}
    return;
  }
  if (generacion !== R.meGeneracion) return;
  S.agent = { ...(S.agent || {}), approvals: aprobaciones };
  if (recordar) agentMsg($("#agent-msg"), T("agent.remembered"), false);
  if (S.view === "panel") renderPanel();
  else renderAgent();
  if (S.agent && S.agent.running) agentWatch(true);
}

// Visor de cambios locales (journal): muestra los archivos que el arnés creó o
// modificó y permite deshacer el último cambio con confirmación. Nunca fuerza:
// si el archivo cambió después, el servidor se niega y el mensaje lo explica.
let agentUndoBusy = false;
let journalEntries = [];

export function closeJournalModal() {
  const modal = $("#journal-modal");
  if (modal) modal.classList.remove("on");
}

function paintJournalList() {
  const list = $("#jn-list");
  const undoBtn = $("#jn-undo-one");
  if (!list) return;
  if (!journalEntries.length) {
    list.innerHTML = '<p class="fine" style="padding:12px 0">' + esc(T("agent.undoNone")) + "</p>";
    if (undoBtn) undoBtn.style.display = "none";
    return;
  }
  if (undoBtn) undoBtn.style.display = "";
  list.innerHTML = '<div class="card">' + journalEntries.slice(0, 30).map((e, i) => {
    const base = String(e.path || "").split(/[\\/]/).filter(Boolean).pop() || e.path || "";
    const kind = e.existed ? T("agent.journalModified") : T("agent.journalCreated");
    const cls = e.existed ? "sig" : "ok";
    const meta = (e.when ? fmtDate(e.when) + " " + String(e.when).slice(11, 16) : "") +
      (e.existed && e.size > 0 ? " · " + fmtBytes(e.size) : "");
    return '<div class="row-i" title="' + esc(e.path) + '">' +
      '<div class="t-body"><b class="mono">' + esc(base) + (i === 0 ? " ★" : "") + "</b>" +
      '<small class="mono" style="word-break:break-all">' + esc(e.path) + "</small>" +
      (meta ? "<small>" + esc(meta) + "</small>" : "") + "</div>" +
      '<span class="pill2 ' + cls + '">' + esc(kind) + "</span></div>";
  }).join("") + "</div>";
}

export async function openJournalModal() {
  const modal = $("#journal-modal");
  const msg = $("#jn-msg");
  const list = $("#jn-list");
  if (!modal || !list) return agentUndoLast();
  if (msg) msg.textContent = "";
  list.innerHTML = '<p class="fine">' + esc(T("common.loading")) + "</p>";
  modal.classList.add("on");
  try {
    const r = await api("/api/agent/journal");
    journalEntries = (r && r.entries) || [];
    paintJournalList();
  } catch (err) {
    journalEntries = [];
    paintJournalList();
    if (msg) msg.textContent = err.message;
  }
}

export async function undoJournalLast() {
  if (agentUndoBusy || !journalEntries.length) return;
  const ultima = journalEntries[0];
  if (!window.confirm(T("agent.undoConfirm") + "\n" + ultima.path)) return;
  agentUndoBusy = true;
  const btn = $("#jn-undo-one");
  const msg = $("#jn-msg");
  if (btn) btn.disabled = true;
  try {
    await api("/api/agent/undo", { method: "POST", body: JSON.stringify({ n: 1 }) });
    const done = T("agent.undoDone") + " " + ultima.path;
    if (msg) msg.textContent = done;
    agentMsg($("#agent-msg"), done, false);
    const r = await api("/api/agent/journal").catch(() => ({ entries: journalEntries.slice(1) }));
    journalEntries = (r && r.entries) || [];
    paintJournalList();
  } catch (err) {
    if (msg) msg.textContent = err.message;
    agentMsg($("#agent-msg"), err.message, true);
  } finally {
    agentUndoBusy = false;
    if (btn) btn.disabled = false;
  }
}

export async function agentUndoLast() {
  return openJournalModal();
}

// Puente 1-clic Tareas → Arnés Pi: carga el contexto estructurado de una tarea
// en el compositor del agente sin hacer llamadas MCP extra.
export function startTaskInAgent(task, goal) {
  if (!task) return;
  const td = $("#taskdetail");
  if (td) td.classList.remove("on");
  S.agentEdit = false;
  setView("agent");
  const partes = [
    T("agent.taskPromptIntro").replace("{title}", task.title || ""),
    "ID: " + (task.id || ""),
    T("td.state") + ": " + T("tasks." + (task.status || "pending")) +
      (task.priority ? " · " + T("td.priority") + ": " + T("prio." + task.priority) : "") +
      (goal ? " · " + T("td.goal") + ": " + goal.title : ""),
  ];
  if (task.description) partes.push("\n" + task.description.trim());
  partes.push("\n" + T("agent.taskPromptOutro"));
  const input = $("#agent-input");
  if (input) {
    input.value = partes.join("\n");
    updateAgentSend();
    input.focus();
    try { input.setSelectionRange(input.value.length, input.value.length); } catch { /* ok */ }
  }
  toast(T("agent.taskLoaded"));
}

// Al abrir la configuración, el formulario toma los valores guardados. Los
// repintados posteriores no tocan lo que la persona escribe.
export function fillAgentForm() {
  const a = S.agent;
  const base = a.base_url || "";
  const guardado = providerByBase(base);
  $("#agent-kind").value = a.kind === "org" ? "org" : "openai";
  $("#agent-provider").value = guardado ? guardado.id : base ? "other" : "openrouter";
  loadAgentModels($("#agent-provider").value);
  $("#agent-base").value = guardado ? "" : base;
  $("#agent-model").value = a.model || "";
  // Sin modelo guardado el modo del servidor no cuenta. El de fábrica es Autónomo (Turbo).
  const modos = ["autonomous", "ask", "readonly"];
  $("#agent-mode").value = a.configured && modos.includes(a.mode) ? a.mode : "autonomous";
  S.agentFolders = (a.folders || []).slice();
  showEl($("#agent-folder-warn"), S.agentFolders.length > 0);
  $("#agent-programs").value = (a.programs || []).join(", ");
  $("#agent-wake").checked = !!a.wake_enabled;
  $("#agent-wake-minute").value = a.wake_per_minute || 3;
  $("#agent-wake-day").value = a.wake_per_day || 20;
  $("#agent-wake-tokens").value = a.wake_tokens_per_day || 20000;
  $("#agent-voice-stt-url").value = (a.voice_stt_url || "").trim();
  $("#agent-voice-stt-model").value = a.voice_stt_model || "";
  $("#agent-voice-tts-url").value = a.voice_tts_url || "";
  $("#agent-voice-tts-model").value = a.voice_tts_model || "";
  $("#agent-voice-tts-voice").value = a.voice_tts_voice || "";
  paintFolders();
  $("#agent-key").value = "";
  agentMsg($("#agent-setup-msg"), "");
}

// Una fila por carpeta permitida, con su botón para quitarla.
export function paintFolders() {
  const box = $("#agent-folders");
  if (!S.agentFolders.length) {
    box.innerHTML = '<p class="fine">' + esc(T("agent.noFolders")) + "</p>";
    return;
  }
  box.innerHTML = S.agentFolders.map((f) =>
    '<div class="agent-folder"><code>' + esc(f) + '</code><button data-folder-remove="' + esc(f) + '" title="' + esc(T("agent.remove")) + '" aria-label="' + esc(T("agent.remove")) + '">✕</button></div>'
  ).join("");
}

export async function loadAgentRules() {
  try {
    const res = await api("/api/agent/rules");
    S.agentRules = (res && res.rules) || [];
  } catch {
    S.agentRules = [];
  }
  paintAgentRules();
}

export function paintAgentRules() {
  const box = $("#agent-rules-list");
  if (!box) return;
  const rules = S.agentRules || [];
  if (!rules.length) {
    box.innerHTML = '<p class="fine">' + esc(T("agent.rulesNone")) + "</p>";
    return;
  }
  box.innerHTML = rules.map((r) =>
    '<div class="agent-rule-row"><code class="mono">' + esc(r.tool) + " · " + esc(r.pattern) +
    '</code><button class="btn sm" data-revoke-rule="' + esc(r.id) + '">' + esc(T("agent.ruleRevoke")) + "</button></div>"
  ).join("");
}

export async function revokeAgentRule(id) {
  if (!id) return;
  try {
    await api("/api/agent/rules/revoke", { method: "POST", body: JSON.stringify({ id }) });
    await loadAgentRules();
  } catch (err) {
    agentMsg($("#agent-setup-msg"), err.message, true);
  }
}

export async function addAgentFolder() {
  try {
    let r;
    try {
      // El selector nativo de Windows bloquea la respuesta hasta que el usuario
      // elige o cancela: un plazo de 40 s abortaba con el diálogo aún abierto y
      // descartaba la carpeta. Acción guiada por el usuario: plazo largo.
      r = await api("/api/agent/pick-folder", { method: "POST", timeout: LONG_ACTION_TIMEOUT_MS });
    } catch (err) {
      // Sin selector nativo (Linux) el borde responde 400: se ofrece escribir
      // la ruta a mano y se manda en body.path, que el borde valida.
      if (err.status !== 400) throw err;
      const escrita = window.prompt(T("agent.folderManual"));
      if (!escrita || !escrita.trim()) return;
      r = await api("/api/agent/pick-folder", { method: "POST", body: JSON.stringify({ path: escrita.trim() }) });
    }
    if (!r.path) return;
    if (!S.agentFolders.includes(r.path)) S.agentFolders.push(r.path);
    if (S.agentFolders.length === 1) showEl($("#agent-folder-warn"), true);
    paintFolders();
  } catch (err) {
    agentMsg($("#agent-setup-msg"), err.message, true);
  }
}

export function syncAgentForm() {
  const a = S.agent || {};
  const org = $("#agent-kind").value === "org";
  const kindNote = $("#agent-kind-note");
  if (kindNote) kindNote.textContent = T(org ? "agent.kindNoteOrg" : "agent.kindNoteOwn");
  // Con el cerebro de la organización sobran proveedor, modelo y clave propia,
  // pero las carpetas, programas, modo y reglas locales sí aplican para el
  // arnés Pi (`read`, `write`, `edit`, `bash`).
  showEl($("#agent-provider-field"), !org);
  showEl($("#agent-base-field"), !org && $("#agent-provider").value === "other");
  showEl($("#agent-model-field"), !org);
  showEl($("#agent-model-link"), !org);
  showEl($("#agent-mode-field"), true);
  showEl($("#agent-folders-field"), true);
  showEl($("#agent-programs-field"), true);
  showEl($("#agent-rules-field"), true);
  showEl($("#agent-sec-permissions"), true);
  // La autonomía también la decide la organización: el servidor la ejecuta y
  // un miembro normal no la cambia desde aquí.
  showEl($("#agent-sec-auto"), !org);
  showEl($("#agent-wake-field"), !org);
  showEl($("#agent-test-btn"), !org);
  showEl($("#agent-key-field"), !org);
  showEl($("#agent-key-note"), !org);
  showEl($("#agent-data-note"), !org);
  showEl($("#agent-org-note"), org);
  showEl($("#agent-mode-warn"), $("#agent-mode").value === "autonomous");
  if (org) return;
  // Si cambia el proveedor o la dirección, la clave guardada no sirve para ese sitio.
  const need = !a.configured || agentBase() !== (a.base_url || "");
  const key = $("#agent-key");
  key.required = need;
  showEl($("#agent-key-req"), need);
  key.placeholder = !a.configured ? T("agent.keyPh") : need ? T("agent.keyNew")
    : a.key_hint ? T("agent.keyHint").replace("{hint}", () => a.key_hint) : T("agent.keySaved");
}

function agentForm() {
  const kind = $("#agent-kind").value === "org" ? "org" : "openai";
  return {
    kind,
    base_url: agentBase(),
    model: $("#agent-model").value.trim(),
    mode: $("#agent-mode").value,
    api_key: $("#agent-key").value.trim(),
    folders: S.agentFolders,
    programs: $("#agent-programs").value.split(",").map((s) => s.trim()).filter(Boolean),
    wake_enabled: $("#agent-wake").checked,
    wake_per_minute: Number($("#agent-wake-minute").value) || 0,
    wake_per_day: Number($("#agent-wake-day").value) || 0,
    wake_tokens_per_day: Number($("#agent-wake-tokens").value) || 0,
    voice_stt_url: $("#agent-voice-stt-url").value.trim(),
    voice_stt_model: $("#agent-voice-stt-model").value.trim(),
    voice_tts_url: $("#agent-voice-tts-url").value.trim(),
    voice_tts_model: $("#agent-voice-tts-model").value.trim(),
    voice_tts_voice: $("#agent-voice-tts-voice").value.trim(),
  };
}

function revisarFormularioAgente(a, valores) {
  if (valores.kind !== "openai") return "";
  const need = !a.configured || valores.base_url !== (a.base_url || "");
  if (!valores.model) return T("agent.errModel");
  if (!valores.api_key && need) return T("agent.errKey");
  return "";
}

export async function testAgentConfig() {
  const a = S.agent || {};
  const msg = $("#agent-setup-msg");
  const valores = agentForm();
  const falta = revisarFormularioAgente(a, valores);
  if (falta) return agentMsg(msg, falta, true);
  agentMsg(msg, T("agent.testing"), false);
  try {
    const res = await api("/api/agent/test", { method: "POST", body: JSON.stringify(valores) });
    if (res.ok) {
      agentMsg(msg, T("agent.testOk") + " (" + res.ms + " ms): " + (res.reply || ""), false);
    } else {
      agentMsg(msg, T("agent.testFail") + ": " + (res.error || T("err.generic")), true);
    }
  } catch (err) {
    agentMsg(msg, T("agent.testFail") + ": " + err.message, true);
  }
}

export async function setAgentModeQuick(mode) {
  const a = S.agent;
  if (!a || a.running || !["autonomous", "ask", "readonly"].includes(mode)) return;
  const generacion = R.meGeneracion;
  let configurado = null;
  try {
    configurado = await api("/api/agent/config", {
      method: "POST",
      body: JSON.stringify({
        kind: a.kind === "org" ? "org" : "openai",
        base_url: a.base_url || "",
        model: a.model || "",
        mode,
        api_key: "",
        folders: a.folders || [],
        programs: a.programs || [],
        wake_enabled: !!a.wake_enabled,
        wake_per_minute: a.wake_per_minute || 0,
        wake_per_day: a.wake_per_day || 0,
        wake_tokens_per_day: a.wake_tokens_per_day || 0,
        voice_stt_url: a.voice_stt_url || "",
        voice_stt_model: a.voice_stt_model || "",
        voice_tts_url: a.voice_tts_url || "",
        voice_tts_model: a.voice_tts_model || "",
        voice_tts_voice: a.voice_tts_voice || "",
      }),
    });
    if (generacion !== R.meGeneracion) return;
    S.agent = configurado;
    renderAgent();
    toast(T("agent.mode") + ": " + T(mode === "autonomous" ? "agent.modeAuto" : mode === "readonly" ? "agent.modeRead" : "agent.modeAsk"));
  } catch (err) {
    if (generacion !== R.meGeneracion) return;
    agentMsg($("#agent-msg"), err.message, true);
    renderAgent();
  }
}

export async function saveAgentConfig() {
  const a = S.agent || {};
  const msg = $("#agent-setup-msg");
  const valores = agentForm();
  const falta = revisarFormularioAgente(a, valores);
  if (falta) return agentMsg(msg, falta, true);
  const generacion = R.meGeneracion;
  let configurado = null;
  try {
    configurado = await api("/api/agent/config", { method: "POST", body: JSON.stringify(valores) });
  } catch (err) {
    if (generacion !== R.meGeneracion) return;
    return agentMsg(msg, err.message, true);
  }
  if (generacion !== R.meGeneracion) return;
  S.agent = configurado;
  S.agentEdit = false;
  $("#agent-key").value = "";
  agentMsg(msg, "");
  renderAgent();
}

const AGENT_DOC_MAX = 32 * 1024;
const AGENT_DOC_TOTAL = 64 * 1024;
const AGENT_DOC_COUNT = 4;
const AGENT_IMAGE_COUNT = 4;
const AGENT_IMAGE_MAX = 2 * 1024 * 1024;
const AGENT_IMAGE_TOTAL = 6 * 1024 * 1024;

// Los documentos viajan como texto dentro del turno; las imágenes viajan como
// partes del mensaje y el backend las valida antes de mandarlas al modelo. En
// ambos casos se lee el archivo aquí y no se envía nada hasta pulsar Enviar.
export function paintAgentDocs() {
  const box = $("#agent-docs");
  if (!box) return;
  showEl(box, S.agentDocs.length > 0);
  box.innerHTML = S.agentDocs.map((d, i) =>
    '<span class="agent-doc"><b>' + esc(d.name) + "</b><small>" + esc(fmtBytes(d.size)) +
    '</small><button data-agent-doc="' + i + '" title="' + esc(T("agent.remove")) + '" aria-label="' + esc(T("agent.remove")) + '">✕</button></span>'
  ).join("");
}

export function paintAgentImages() {
  const box = $("#agent-images");
  if (!box) return;
  showEl(box, S.agentImages.length > 0);
  box.innerHTML = S.agentImages.map((img, i) =>
    '<span class="agent-doc agent-doc-img"><img src="' + esc(img.preview || "data:" + img.mime + ";base64," + img.data) + '" alt="">' +
    "<b>" + esc(img.name) + "</b><small>" + esc(fmtBytes(img.size)) +
    '</small><button data-agent-image="' + i + '" title="' + esc(T("agent.remove")) + '" aria-label="' + esc(T("agent.remove")) + '">✕</button></span>'
  ).join("");
}


// Elige el camino de cada archivo: imagen o documento de texto.
export async function attachAgentFile(file) {
  if (!file) return;
  if (String(file.type || "").startsWith("image/")) return attachAgentImage(file);
  return attachAgentDoc(file);
}

function blobABase64(blob) {
  return new Promise((resolve, reject) => {
    const lector = new FileReader();
    lector.onload = () => resolve(String(lector.result || "").split(",")[1] || "");
    lector.onerror = () => reject(new Error(T("agent.imageRead")));
    lector.readAsDataURL(blob);
  });
}

// La miniatura es lo que queda en el historial visible. Si se mandara la imagen
// entera, cada sondeo del chat repetiría hasta 6 MB de base64 y la ventana se
// pondría pesada.
async function miniaturaImagen(blob) {
  try {
    const mapa = await createImageBitmap(blob);
    const limite = 360;
    const escala = Math.min(1, limite / Math.max(mapa.width, mapa.height));
    const ancho = Math.max(1, Math.round(mapa.width * escala));
    const alto = Math.max(1, Math.round(mapa.height * escala));
    const lienzo = document.createElement("canvas");
    lienzo.width = ancho;
    lienzo.height = alto;
    const ctx = lienzo.getContext("2d");
    ctx.fillStyle = "#fff";
    ctx.fillRect(0, 0, ancho, alto);
    ctx.drawImage(mapa, 0, 0, ancho, alto);
    mapa.close();
    const url = lienzo.toDataURL("image/jpeg", 0.72);
    return url.length <= 240 * 1024 ? url : "";
  } catch {
    return "";
  }
}

// Las imágenes se reducen en la ventana antes de viajar: una foto de teléfono
// de 8 MB no tiene por qué gastar el turno entero. El GIF se deja intacto para
// no perder la animación.
async function comprimirImagen(file) {
  const tipo = String(file.type || "").toLowerCase();
  if (!["image/png", "image/jpeg", "image/webp", "image/gif"].includes(tipo)) {
    throw new Error(T("agent.imageType"));
  }
  if (tipo === "image/gif" || file.size <= 900 * 1024) return { blob: file, mime: tipo };
  const mapa = await createImageBitmap(file);
  const limite = 1600;
  const escala = Math.min(1, limite / Math.max(mapa.width, mapa.height));
  const ancho = Math.max(1, Math.round(mapa.width * escala));
  const alto = Math.max(1, Math.round(mapa.height * escala));
  const lienzo = document.createElement("canvas");
  lienzo.width = ancho;
  lienzo.height = alto;
  lienzo.getContext("2d").drawImage(mapa, 0, 0, ancho, alto);
  mapa.close();
  const blob = await new Promise((resolve, reject) => {
    lienzo.toBlob((b) => (b ? resolve(b) : reject(new Error(T("agent.imageRead")))), "image/jpeg", 0.85);
  });
  return { blob, mime: "image/jpeg" };
}

async function attachAgentImage(file) {
  // Valida contra el cerebro guardado, la misma fuente que el envío mira en
  // `settings_snapshot`: si el cambio a «Mi clave» no se guardó, el servidor
  // rechazaría la imagen aunque el formulario ya mostrara la nueva elección.
  const kind = (S.agent && S.agent.kind) || "";
  if (kind === "org") {
    toast(T("agent.docImage"));
    return;
  }
  if (S.agentImages.length >= AGENT_IMAGE_COUNT) {
    toast(T("agent.imageMany"));
    return;
  }
  const generacion = R.meGeneracion;
  try {
    const { blob, mime } = await comprimirImagen(file);
    if (blob.size > AGENT_IMAGE_MAX) {
      toast(T("agent.imageBig"));
      return;
    }
    const total = S.agentImages.reduce((n, img) => n + img.size, 0) + blob.size;
    if (total > AGENT_IMAGE_TOTAL) {
      toast(T("agent.imageTotal"));
      return;
    }
    const data = await blobABase64(blob);
    const preview = await miniaturaImagen(blob);
    if (generacion !== R.meGeneracion) return;
    S.agentImages.push({ name: file.name, size: blob.size, mime, data, preview });
    paintAgentImages();
    updateAgentSend();
  } catch (err) {
    toast(err.message);
  }
}

async function attachAgentDoc(file) {
  if (!file) return;
  if (file.size > AGENT_DOC_MAX) {
    toast(T("agent.docTooBig").replace("{name}", file.name));
    return;
  }
  if (S.agentDocs.length >= AGENT_DOC_COUNT) {
    toast(T("agent.docMany"));
    return;
  }
  const total = S.agentDocs.reduce((n, d) => n + d.size, 0) + file.size;
  if (total > AGENT_DOC_TOTAL) {
    toast(T("agent.docTotal"));
    return;
  }
  let text = "";
  const generacion = R.meGeneracion;
  try {
    text = await file.text();
  } catch {
    toast(T("agent.docRead"));
    return;
  }
  if (generacion !== R.meGeneracion) return;
  if (text.includes("\u0000")) {
    toast(T("agent.docBinary"));
    return;
  }
  S.agentDocs.push({ name: file.name, size: file.size, text });
  paintAgentDocs();
  updateAgentSend();
}

function agentTextWithDocs(text) {
  if (!S.agentDocs.length) return text;
  const bloques = S.agentDocs.map((d) =>
    "--- Documento adjunto: " + d.name + " ---\n" + d.text + "\n--- Fin de " + d.name + " ---"
  );
  return (text ? text + "\n\n" : "") + bloques.join("\n\n");
}

export function updateAgentSend() {
  $("#agent-send").disabled = !(S.agent && S.agent.running) && !$("#agent-input").value.trim() && !S.agentDocs.length && !S.agentImages.length;
}

let agentSendBusy = false;

// La sesión venció o no existe: el envío no va a funcionar hasta entrar de
// nuevo o usar una clave propia. Se ofrece cada salida en el mismo panel.
export function agentAskLogin() {
  const el = $("#agent-msg");
  if (!el) return;
  el.classList.remove("agent-err");
  el.innerHTML = "<span>" + esc(T("agent.needLogin")) + "</span> " +
    '<button class="btn sm primary" data-agent-act="login">' + esc(T("welcome.login")) + "</button> " +
    '<button class="btn sm" data-agent-act="key">' + esc(T("agent.useKey")) + "</button>";
}

export async function agentSend() {
  if (agentSendBusy) return;
  const input = $("#agent-input");
  const text = input.value.trim();
  if (!text && !S.agentDocs.length && !S.agentImages.length) return;
  agentSendBusy = true;
  const previo = input.value;
  const since = Date.now();
  const payload = agentTextWithDocs(text);
  const docsEnviados = new Set(S.agentDocs);
  const imagenesEnviadas = new Set(S.agentImages);
  const images = S.agentImages.map((img) => ({ mime: img.mime, data: img.data, preview: img.preview }));
  input.value = "";
  updateAgentSend();
  migaUI("chat enviar (" + text.length + " caracteres, " + images.length + " imágenes)");
  try {
    await api("/api/agent/send", { method: "POST", body: JSON.stringify({ text: payload, images }), timeout: AGENT_SEND_TIMEOUT_MS });
  } catch (err) {
    if (!err.status) {
      await loadAgent();
      if (S.agent && S.agent.running) {
        S.agentSince = since;
        updateAgentSend();
        return;
      }
    }
    // Si la persona ya escribio otra cosa mientras esperaba, no se la pisa.
    if (!input.value) input.value = previo;
    updateAgentSend();
    // Un 401 no es la red: la sesión no existe o venció. Decirlo con una
    // salida en vez del texto genérico, que se lee como un problema de conexión.
    if (err.status === 401) agentAskLogin();
    else agentMsg($("#agent-msg"), err.message, true);
    return;
  } finally {
    agentSendBusy = false;
  }
  S.agentSince = since;
  S.agentDocs = S.agentDocs.filter((doc) => !docsEnviados.has(doc));
  S.agentImages = S.agentImages.filter((img) => !imagenesEnviadas.has(img));
  paintAgentDocs();
  paintAgentImages();
  agentMsg($("#agent-msg"), "");
  updateAgentSend();
  await loadAgent();
}

export async function agentStop() {
  try {
    await api("/api/agent/cancel", { method: "POST" });
  } catch (err) {
    agentMsg($("#agent-msg"), err.message, true);
  }
}

const DICTADO_HZ = 16000;
const DICTADO_MAX_MS = 120000;

let agentMicPrep = false;

function pcm16Wav(trozos, tasa) {
  let total = 0;
  for (const trozo of trozos) total += trozo.length;
  const origen = new Float32Array(total);
  let pos = 0;
  for (const trozo of trozos) {
    origen.set(trozo, pos);
    pos += trozo.length;
  }
  const n = Math.max(0, Math.round((total * DICTADO_HZ) / (tasa || DICTADO_HZ)));
  const vista = new DataView(new ArrayBuffer(44 + n * 2));
  const escribir = (desde, texto) => {
    for (let i = 0; i < texto.length; i += 1) vista.setUint8(desde + i, texto.charCodeAt(i));
  };
  escribir(0, "RIFF");
  vista.setUint32(4, 36 + n * 2, true);
  escribir(8, "WAVE");
  escribir(12, "fmt ");
  vista.setUint32(16, 16, true);
  vista.setUint16(20, 1, true);
  vista.setUint16(22, 1, true);
  vista.setUint32(24, DICTADO_HZ, true);
  vista.setUint32(28, DICTADO_HZ * 2, true);
  vista.setUint16(32, 2, true);
  vista.setUint16(34, 16, true);
  escribir(36, "data");
  vista.setUint32(40, n * 2, true);
  for (let i = 0; i < n; i += 1) {
    const x = (i * total) / n;
    const j = Math.floor(x);
    const mezcla = x - j;
    const a = origen[j] || 0;
    const b = j + 1 < total ? origen[j + 1] : a;
    const muestra = Math.max(-1, Math.min(1, a + (b - a) * mezcla));
    vista.setInt16(44 + i * 2, muestra < 0 ? muestra * 0x8000 : muestra * 0x7fff, true);
  }
  return new Blob([vista.buffer], { type: "audio/wav" });
}

async function enviarDictado(blob) {
  const generacion = R.meGeneracion;
  const boton = $("#agent-mic");
  const fd = new FormData();
  fd.append("audio", blob, "dictado.wav");
  boton.classList.add("busy");
  boton.title = T("agent.micTranscribing");
  boton.setAttribute("aria-label", T("agent.micTranscribing"));
  const control = new AbortController();
  const plazo = setTimeout(() => control.abort(), LONG_ACTION_TIMEOUT_MS);
  try {
    let res;
    try {
      res = await fetch("/api/agent/transcribe", { method: "POST", body: fd, signal: control.signal });
    } catch {
      throw new Error(T("load.failed"));
    }
    let datos = null;
    try {
      datos = await res.json();
    } catch {
      datos = null;
    }
    if (!res.ok) throw new Error((datos && datos.error) || res.statusText);
    if (generacion !== R.meGeneracion) return;
    const input = $("#agent-input");
    input.value = (input.value ? input.value.trim() + " " : "") + (datos.text || "");
    input.focus();
    updateAgentSend();
  } catch (err) {
    agentMsg($("#agent-msg"), T("agent.micErr").replace("{error}", () => err.message), true);
  } finally {
    clearTimeout(plazo);
    boton.classList.remove("busy");
    boton.title = T("agent.mic");
    boton.setAttribute("aria-label", T("agent.mic"));
  }
}

export async function agentDictate() {
  const boton = $("#agent-mic");
  if (S.agentMic) {
    S.agentMic.parar();
    return;
  }
  if (agentMicPrep || boton.classList.contains("busy")) return;
  agentMicPrep = true;
  try {
    const AudioCtx = window.AudioContext || window.webkitAudioContext;
    if (!navigator.mediaDevices || !navigator.mediaDevices.getUserMedia || !AudioCtx) {
      toast(T("agent.micNo"));
      return;
    }
    let stream;
    try {
      stream = await navigator.mediaDevices.getUserMedia({
        audio: { channelCount: 1, echoCancellation: true, noiseSuppression: true, autoGainControl: true },
      });
    } catch (err) {
      const nombre = (err && err.name) || "";
      if (nombre !== "NotAllowedError" && nombre !== "PermissionDeniedError" && nombre !== "SecurityError") {
        try {
          stream = await navigator.mediaDevices.getUserMedia({ audio: true });
        } catch (err2) {
          err = err2;
        }
      }
      if (!stream) {
        const fallo = (err && err.name) || "";
        reportarError("ui.mic", fallo || "getUserMedia failed", (err && err.message) || "");
        if (fallo === "NotFoundError" || fallo === "DevicesNotFoundError") {
          toast(T("agent.micNotFound"));
        } else if (fallo === "NotReadableError" || fallo === "TrackStartError") {
          toast(T("agent.micBusy"));
        } else if (fallo === "NotAllowedError" || fallo === "PermissionDeniedError" || fallo === "SecurityError") {
          toast(T("agent.micDenied"));
        } else {
          toast(T("agent.micErr").replace("{error}", () => (err && err.message) || fallo || T("err.generic")));
        }
        return;
      }
    }
    const trozos = [];
    let ctx, fuente, nodo, mudo;
    try {
      ctx = new AudioCtx();
      ctx.resume().catch(() => undefined);
      fuente = ctx.createMediaStreamSource(stream);
      nodo = ctx.createScriptProcessor(4096, 1, 1);
      mudo = ctx.createGain();
      mudo.gain.value = 0;
      nodo.onaudioprocess = (evento) => {
        trozos.push(new Float32Array(evento.inputBuffer.getChannelData(0)));
      };
      fuente.connect(nodo);
      nodo.connect(mudo);
      mudo.connect(ctx.destination);
    } catch (err) {
      stream.getTracks().forEach((pista) => pista.stop());
      if (ctx) ctx.close().catch(() => undefined);
      reportarError("ui.mic", "audio graph failed", (err && err.message) || "");
      toast(T("agent.micErr").replace("{error}", () => (err && err.message) || T("err.generic")));
      return;
    }
    let plazo = 0;
    let cerrado = false;
    const parar = () => {
      if (cerrado) return;
      cerrado = true;
      S.agentMic = null;
      clearTimeout(plazo);
      nodo.onaudioprocess = null;
      try {
        fuente.disconnect();
        nodo.disconnect();
        mudo.disconnect();
      } catch {
        void 0;
      }
      stream.getTracks().forEach((pista) => pista.stop());
      const tasa = ctx.sampleRate;
      ctx.close().catch(() => undefined);
      boton.classList.remove("rec");
      boton.title = T("agent.mic");
      boton.setAttribute("aria-label", T("agent.mic"));
      const wav = pcm16Wav(trozos, tasa);
      if (wav.size > 44) enviarDictado(wav);
    };
    S.agentMic = { parar };
    boton.classList.add("rec");
    boton.title = T("agent.micStop");
    boton.setAttribute("aria-label", T("agent.micStop"));
    plazo = setTimeout(() => {
      parar();
      toast(T("agent.micLimit"));
    }, DICTADO_MAX_MS);
  } finally {
    agentMicPrep = false;
  }
}

// Leer una respuesta con el servidor de voz. Un segundo clic en el mismo botón
// detiene la lectura; otra respuesta distinta cambia de audio.
let speakToken = 0, speakControl = null;
export async function agentSpeak(texto, boton) {
  if (!texto) return;
  const mismo = !!(S.agentSpeak && boton && boton.classList.contains("on"));
  if (S.agentSpeak) {
    S.agentSpeak.pause();
    try { URL.revokeObjectURL(S.agentSpeak.src); } catch { /* ya no existe */ }
    S.agentSpeak = null;
    $$("[data-agent-speak].on").forEach((b) => b.classList.remove("on"));
    if (mismo) return;
  }
  const token = ++speakToken;
  if (speakControl) speakControl.abort();
  // Sin plazo, un servidor de voz colgado dejaba la lectura muda sin salida.
  const control = new AbortController();
  speakControl = control;
  const plazo = setTimeout(() => control.abort(), AGENT_SEND_TIMEOUT_MS);
  let limpiar = null;
  try {
    let res;
    try {
      res = await fetch("/api/agent/speak", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ text: texto }),
        signal: control.signal,
      });
    } catch {
      throw new Error(T("load.failed"));
    }
    if (!res.ok) {
      let datos = null;
      try { datos = await res.json(); } catch { /* sin JSON */ }
      throw new Error((datos && datos.error) || res.statusText);
    }
    const url = URL.createObjectURL(await res.blob());
    if (token !== speakToken) {
      URL.revokeObjectURL(url);
      return;
    }
    const audio = new Audio(url);
    S.agentSpeak = audio;
    if (boton) boton.classList.add("on");
    limpiar = () => {
      URL.revokeObjectURL(url);
      if (S.agentSpeak === audio) S.agentSpeak = null;
      if (boton) boton.classList.remove("on");
    };
    audio.onended = limpiar;
    audio.onerror = limpiar;
    await audio.play();
  } catch (err) {
    if (token !== speakToken) return;
    if (limpiar) limpiar();
    agentMsg($("#agent-msg"), T("agent.speakErr").replace("{error}", () => err.message), true);
  } finally {
    clearTimeout(plazo);
    if (speakControl === control) speakControl = null;
  }
}

export async function agentReset() {
  try {
    await api("/api/agent/reset", { method: "POST" });
  } catch (err) {
    agentMsg($("#agent-msg"), err.message, true);
    return;
  }
  S.agentSince = 0;
  S.agentOpen = new Set();
  S.agentDocs = [];
  S.agentImages = [];
  paintAgentDocs();
  paintAgentImages();
  updateAgentSend();
  await loadAgent();
}

export function agentAction(name) {
  if (name === "send") return S.agent && S.agent.running ? agentStop() : agentSend();
  if (name === "save") return saveAgentConfig();
  if (name === "test") return testAgentConfig();
  if (name === "reset") return agentReset();
  if (name === "addfolder") return addAgentFolder();
  if (name === "delconv") return deleteConversation(false);
  if (name === "delconvs") return deleteConversation(true);
  if (name === "settings") { S.agentEdit = true; setView("agent"); return; }
  if (name === "login") { showScreen("welcome"); return; }
  if (name === "key") {
    agentMsg($("#agent-msg"), "");
    S.agentEdit = true;
    renderAgent();
    const sel = $("#agent-kind");
    if (sel) { sel.value = "openai"; syncAgentForm(); }
    return;
  }
  if (name === "undo") return openJournalModal();
  if (name === "cancel") S.agentEdit = false;
  renderAgent();
}

// Los pasos de herramientas son de la última vuelta: los que llegaron desde que se
// mandó el mensaje. Los de conversaciones anteriores no se muestran.
export function paintAgentLog() {
  const a = S.agent;
  const view = $("#agent-log");
  const atBottom = view.scrollHeight - view.scrollTop - view.clientHeight < 48;
  const stepKinds = (s) => s.kind === "tool_call" || s.kind === "tool_result" || s.kind === "denied";
  const trail = S.agentSince
    ? a.steps.filter((s) => stepKinds(s) && Date.parse(s.at) >= S.agentSince)
    : a.steps.filter(stepKinds);
  const lastUser = a.messages.map((m) => m.role).lastIndexOf("user");
  let html = a.messages.map((m, i) => agentBubble(m, i) + (i === lastUser ? trail.map((s, idx) => agentStepHtml(s, idx)).join("") : "")).join("");
  if (!a.messages.length && !a.draft && !a.running) {
    const sugs = [1, 2, 3, 4].map((n) =>
      '<button class="agent-sug" data-agent-suggest="' + esc(T("agent.sug" + n)) + '">' + esc(T("agent.sug" + n)) + "</button>").join("");
    html = '<div class="agent-hello"><span class="agent-mark"><svg class="i"><use href="#i-zap"/></svg></span>' +
      "<h2>" + esc(T("agent.hello")) + "</h2><p>" + esc(T("agent.helloSub")) + "</p>" +
      (a.kind === "org" ? '<p class="agent-harness-hint">' + esc(T("agent.harnessHint")) + "</p>" : "") +
      '<div class="agent-sugs">' + sugs + "</div></div>";
  }
  if (a.draft) html += agentBubble({ role: "assistant", content: a.draft, draft: true }, -1);
  if (a.running) html += '<div class="agent-step"><span class="spinner"></span> ' + esc(T("agent.working")) + "</div>";
  if (!a.running && a.last_error) html += '<p class="fine agent-err">' + esc(a.last_error) + "</p>";
  if (html === S.agentLogHtml) return;
  const first = !S.agentLogHtml;
  S.agentLogHtml = html;
  $("#agent-log").innerHTML = html;
  if (first || atBottom) view.scrollTop = view.scrollHeight;
}

const agentBubble = (m, i) => {
  const imagenes = (m.images || []).map((img) => {
    const src = img.preview || ("data:" + img.mime + ";base64," + img.data);
    return '<img class="agent-img" src="' + esc(src) + '" alt="">';
  }).join("");
  const etiqueta = esc(T("agent.listen"));
  const tieneVoz = !!(S.agent && S.agent.voice_tts_url && String(S.agent.voice_tts_url).trim());
  const escuchar = m.role === "assistant" && m.content && !m.draft && tieneVoz
    ? '<button class="agent-listen" data-agent-speak="' + i + '" title="' + etiqueta + '" aria-label="' + etiqueta + '"><svg class="i"><use href="#i-speaker"/></svg><span>' + esc(T("agent.listenShort")) + "</span></button>"
    : "";
  const copiar = m.role === "assistant" && m.content && !m.draft
    ? (escuchar ? " " : "") + '<button class="agent-listen" data-agent-copy="' + i + '" title="' + esc(T("agent.copyReply")) + '" aria-label="' + esc(T("agent.copyReply")) + '"><span>' + esc(T("agent.copyShort")) + "</span></button>"
    : "";
  const mine = m.role === "user";
  const role = mine ? T("chat.you") : T("agent.tag");
  return '<div class="agent-turn ' + (mine ? "user" : "agent") + '">' +
    '<span class="turn-glyph" aria-hidden="true">' + (mine ? "›" : "●") + "</span>" +
    '<div class="bb"><div class="turn-head"><b>' + esc(role) + "</b></div>" + imagenes +
    '<div class="tx">' + md(m.content) + "</div>" + escuchar + copiar + "</div></div>";
};

function stepResumenPi(s) {
  if (!s.text || s.kind !== "tool_call") return "";
  try {
    const args = JSON.parse(s.text);
    if (args && typeof args === "object") {
      if (typeof args.path === "string" && args.path) {
        const base = args.path.split(/[\\/]/).filter(Boolean).pop() || args.path;
        return " · " + base;
      }
      if (typeof args.command === "string" && args.command) {
        const cmd = args.command.trim().replace(/\s+/g, " ");
        return " · " + (cmd.length > 56 ? cmd.slice(0, 56) + "…" : cmd);
      }
    }
  } catch { /* argumentos no JSON */ }
  return "";
}

export function agentStepHtml(s, idx = 0) {
  const key = idx + "|" + s.at + "|" + s.kind + "|" + s.tool;
  const baseLabel = s.kind === "denied" ? T("agent.denied")
    : s.kind === "tool_result" ? ("↳ " + (s.tool || "resultado"))
    : AGENT_TOOLS.has(s.tool) ? T(AGENT_TOOLS.get(s.tool))
    : T("agent.usedTool").replace("{tool}", () => s.tool);
  const label = baseLabel + stepResumenPi(s);
  const body = s.text && s.text.length > 1500 ? s.text.slice(0, 1500) + "…" : (s.text || "");
  return '<details class="agent-step" data-step="' + esc(key) + '"' + (S.agentOpen.has(key) ? " open" : "") + "><summary>" + esc(label) +
    '</summary><div class="agent-args"><code>' + esc(s.tool) + "</code>" + (body ? "\n" + esc(body) : "") + "</div></details>";
}

