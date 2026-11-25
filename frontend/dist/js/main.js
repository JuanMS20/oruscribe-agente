// Punto de entrada de Oruscribe Agente (ES Module nativo, sin bundler).
window.__oruStart = performance.now();

import { T, getLang, setLang } from "./i18n.js";
import { $, $$, esc, showEl, showJSError, toast, fold, focusablesIn, copiarTexto, reportarError, migaUI } from "./dom.js";
import { api, LONG_ACTION_TIMEOUT_MS, REFRESH_TIMEOUT_MS, LOAD_RETRY_MS, LOAD_RETRY_MAX_MS, paginaDe } from "./api.js";
import { S, R, fotoDe, saveSnapshot, loadSnapshot, clearSnapshot, counts } from "./state.js";
import { clasificarCarga, esEstadoSesionTerminada, siguienteToggle } from "./estados.mjs";
import {
  BRAIN, renderBrain, applyView, brainZoom, brainWheel, brainDown, brainMove, brainUp,
  brainSearch, brainEnter, closeBrainCard, openBrainCard, brainFullscreen,
} from "./views/brain.js";
import {
  loadAgent, renderAgent, agentWatch, agentAction, agentApprove, desarmarAprobacion,
  repaintApprovals, agentSend, agentDictate, agentSpeak, attachAgentFile, openConversation,
  paintFolders, revokeAgentRule, paintAgentDocs, paintAgentImages, updateAgentSend,
  syncAgentForm, loadAgentModels, agentReset, closeJournalModal, undoJournalLast,
  startTaskInAgent, setAgentModeQuick,
} from "./views/agent.js";
import {
  renderRooms, renderRoomPane, renderMessages, openRoom, sendMessage, attachChatFile,
  openAttachment, cargarMasSalas, cargarMensajes, msgSignature, setReplyBar, renderNotes,
  openNote, persistNote, loadNotes, refreshChat, startChatPoll, stopChatPoll,
  refreshChatAlEnfocar, getSaveTimer, setSaveTimer, invalidarNota,
} from "./views/team.js";
import {
  cargaFallo, cargaBien, loadActivity, loadDecisions, loadTeam, loadPeriods, loadTrack,
  loadGoals, paintBadges, renderMe, renderOrgSelector, seleccionarOrg, summary,
  renderPanel, renderTrack, renderGoals, renderPeople, renderMentions, renderTasks,
  cargarMasTareas, openAdvances, closeAdvances, submitAdvance, submitComment,
  openTaskDetail, closeTaskDetail, answerBlockReason, changeTaskStatus, renderAlerts,
  taskById, goalOf,
  setTaskStatus, openTaskModal, closeTaskModal, conBotonUnaVez, submitTask,
  openAlertModal, closeAlertModal, submitAlert, openGoal, closeGoalDetail,
  openDmModal, closeDmModal, sendDm, openGoalModal, closeGoalModal, submitGoal,
  openFocusModal, closeFocusModal, saveFocus, openPeriodModal, closePeriodModal,
  submitPeriod, renderCapture, pickCaptureFolder, scanCapture, applyCapture,
} from "./views/work.js";
import {
  showLoginUrl, startLogin, pollLogin, importarToken, loadSettings, maybeAskAutostart,
  answerAutostart, maybeAutoUpdate, paintUpdateBanner, paintUpdateProgress,
  startUpdatePoll, stopUpdatePoll, settingsClosed, loadSetup, openInstall,
  closeInstall, setSettingsTrigger,
} from "./views/settings.js";

export function applyLang(lang) {
  setLang(lang);
  localStorage.setItem("lang", getLang());
  document.documentElement.lang = getLang();
  $$("[data-i18n]").forEach((el) => { el.textContent = T(el.dataset.i18n); });
  $$("[data-t-placeholder]").forEach((el) => { el.placeholder = T(el.dataset.tPlaceholder); });
  $$("[data-t-title]").forEach((el) => { el.title = T(el.dataset.tTitle); });
  // El aria-label se reescribe siempre: si solo se pone la primera vez, el
  // control queda en el idioma del arranque aunque la persona cambie de idioma.
  $$("[data-t-aria]").forEach((el) => { el.setAttribute("aria-label", T(el.dataset.tAria)); });
  $$("[data-lang]").forEach((b) => b.classList.toggle("on", b.dataset.lang === getLang()));
  // El título de cada sección explica en una línea qué hace; así el menú no
  // depende de que alguien adivine qué significa "Cerebro" o "Resumen".
  const subOf = { panel: "sub.panel", today: "sub.panel", agent: "sub.agent", track: "sub.track", brain: "sub.brain", team: "sub.team", bell: "sub.alerts", capture: "sub.capture", mentions: "sub.mentions", tasks: "sub.tasks", goals: "sub.goals", people: "sub.people", notes: "sub.notes" };
  $$("[data-view]").forEach((b) => {
    const k = subOf[b.dataset.view];
    if (!k) return;
    const navKeyOf = { bell: "nav.alerts", team: "nav.chat" };
    const label = (b.querySelector("span") || {}).textContent || T(navKeyOf[b.dataset.view] || "nav." + b.dataset.view);
    b.title = label + " · " + T(k);
    b.setAttribute("aria-label", b.title);
  });
  render();
  paintLoadBanner(R.loadBannerState, loadBannerDetail, loadBannerLogin);
  paintUpdateBanner(R.updateInfo);
}

export function setTheme(t) {
  if (t === "auto") document.documentElement.removeAttribute("data-theme");
  else document.documentElement.dataset.theme = t;
  localStorage.setItem("theme", t);
  $$("button[data-theme]").forEach((b) => b.classList.toggle("on", b.dataset.theme === t));
}

let loadRetryTimer = null, loadRetryDelay = LOAD_RETRY_MS, refreshInFlight = null, quitting = false;

export function showScreen(name) {
  $$(".screen").forEach((s) => s.classList.toggle("on", s.dataset.screen === name));
  // El aviso de versión nueva no espera a que alguien entre: la bienvenida y el
  // login lo comprueban igual. La guarda de una vez por ejecución lo hace gratis.
  maybeAutoUpdate();
}

/* ---------- boot ---------- */
export async function boot() {
  applyLang(localStorage.getItem("lang") || "es");
  setTheme(localStorage.getItem("theme") || "auto");
  // Antes de saber si hay sesión: la bienvenida también avisa de una versión
  // nueva, y sin ajustes cargados manda el valor por defecto (encendido).
  maybeAutoUpdate();
  try { S.myContext = JSON.parse(localStorage.getItem("myFocus") || "null"); } catch { S.myContext = null; }
  let st = null;
  try { st = await api("/api/state"); } catch { st = null; }
  if (st && st.status === "ready") { S.me = st.me; enterApp(); return; }
  if (st && st.status === "connecting" && !st.flow) { pollRestore(); return; }
  if (st && st.status === "connecting") {
    showScreen("login");
    R.loginUrl = st.login_url || "";
    showLoginUrl(R.loginUrl);
    pollLogin(R.loginUrl);
    hideIntro();
    return;
  }
  if (esEstadoSesionTerminada(st && st.status)) { showScreen("welcome"); noteError(st.error || T("session.ended")); hideIntro(); return; }
  showScreen("welcome");
  hideIntro();
}

// La entrada se va cuando la pantalla ya tiene datos, no a ciegas: así nadie
// ve el armazón con guiones ni un salto de la bienvenida a la app.
const introAt = Date.now();
function hideIntro() {
  // Movimiento reducido: sin animación de salida ni espera de fade, el
  // splash se va de una. El CSS ya congela las transiciones; estos son los
  // temporizadores JS que las rodean.
  const reduce = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
  const left = reduce ? 0 : Math.max(0, 1200 - (Date.now() - introAt));
  setTimeout(() => {
    const i = $("#intro");
    if (!i) return;
    if (reduce) { i.classList.add("hide"); return; }
    i.classList.add("out");
    setTimeout(() => i.classList.add("hide"), 260);
  }, left);
}

// A session that is still opening on a cold start: the splash stays until the
// server answers, so a slow network never looks like a logout. If the session
// never reaches a terminal state, the splash must not wait forever: after
// POLL_RESTORE_MS the poll stops with a visible error and the person can retry.
const POLL_RESTORE_MS = 90000;

function fallarRestauracion() {
  clearInterval(R.pollTimer);
  showScreen("welcome");
  noteError(T("welcome.restoreTimeout"));
  hideIntro();
}

function pollRestore() {
  clearInterval(R.pollTimer);
  const comienzo = Date.now();
  R.pollTimer = setInterval(async () => {
    let st = null;
    try { st = await api("/api/auth/state"); } catch {
      if (Date.now() - comienzo > POLL_RESTORE_MS) fallarRestauracion();
      return;
    }
    if (st.status === "ready") {
      clearInterval(R.pollTimer);
      S.me = st.me;
      enterApp();
    } else if (st.status === "failed" || st.status === "logged_out") {
      // Con un reintento en curso la sesión sigue en disco: el sondeo no se apaga
      // y entra solo cuando la red vuelve.
      if (st.status === "logged_out" || !st.retrying) clearInterval(R.pollTimer);
      showScreen("welcome");
      noteError(st.error || "");
      hideIntro();
    } else if (Date.now() - comienzo > POLL_RESTORE_MS) {
      fallarRestauracion();
    }
  }, 700);
}

export function noteError(msg) {
  const el = $("#welcome-error");
  if (!el) return;
  el.textContent = msg || "";
  el.style.display = msg ? "block" : "none";
}

export async function enterApp() {
  R.connOn = true;
  loadSnapshot();
  render();
  showScreen("app");
  hideIntro();
  startLive();
  startChatPoll();
  await loadSetup();
  await loadSettings();
  maybeAskAutostart();
  maybeAutoUpdate();
  await refreshAll();
  // Al arrancar no hay sala abierta y refreshAll solo pide listas: el panel de
  // mensajes se queda con el estado vacío hasta que la persona hace clic. Abrir
  // la primera deja el chat lleno desde el inicio, sin marcarla como leída para
  // no perder los contadores que la persona todavía no vio.
  if (!S.room && S.rooms.length) await openRoom(S.rooms[0].room_id, false);
}

// El perfil y la foto llegan por una petición aparte. Se reintenta sin
// bloquear la ventana: rápido al principio y luego cada 10 s, porque el
// servidor puede tardar hasta un minuto en reintentar la descarga de fotos
// después de un arranque o una actualización. Sin esta segunda etapa la
// ventana se quedaba con las iniciales toda la sesión.
// Cada foto se considera presente en cualquiera de los cuatro campos que el
// tipo `Me` trae y que el render usa; mirar solo `avatar_url` dejaba reintentos
// eternos cuando la foto venía en `photo_url`, `picture` o `avatar`.

let meTimer = null;
function programarRefreshMe(attempt, espera) {
  if (meTimer !== null) clearTimeout(meTimer);
  meTimer = setTimeout(() => { meTimer = null; refreshMe(attempt); }, espera);
}
export function cancelarReintentosMe() {
  ++R.meGeneracion;
  if (meTimer !== null) { clearTimeout(meTimer); meTimer = null; }
}
async function refreshMe(attempt) {
  const generacion = R.meGeneracion;
  const st = await api("/api/state").catch(() => null);
  if (generacion !== R.meGeneracion) return;
  if (!st) {
    // Un error de red no corta la cadena: antes un solo fallo dejaba las
    // iniciales para siempre si no había foco ni refresco.
    if (attempt < 10) programarRefreshMe(attempt + 1, attempt < 4 ? 1500 : 10000);
    return;
  }
  if (st.status !== "ready" || !st.me) return;
  S.me = st.me;
  renderMe();
  if (!fotoDe(st.me) && attempt < 10) {
    programarRefreshMe(attempt + 1, attempt < 4 ? 1500 : 10000);
  }
}

// Los fallos de carga dejan los datos que ya había en pantalla: la ventana
// muestra lo último que llegó. Sin un aviso por cada ruta (es ruido), un aviso
// único y calmo con reintentar marca que el fallo no es datos viejos.
export async function refreshAll() {
  if (quitting) return;
  const generacion = R.meGeneracion;
  if (refreshInFlight && refreshInFlight.generacion === generacion) return refreshInFlight.promesa;
  const promesa = refreshAllNow(generacion);
  refreshInFlight = { generacion, promesa };
  try {
    return await promesa;
  } finally {
    if (refreshInFlight && refreshInFlight.promesa === promesa) refreshInFlight = null;
  }
}



const LOAD_RETRY_429_MS = 60000;

async function refreshAllNow(generacion) {
  const quick = [
    api("/api/tasks", { timeout: REFRESH_TIMEOUT_MS }),
    api("/api/rooms", { timeout: REFRESH_TIMEOUT_MS }),
    api("/api/alerts", { timeout: REFRESH_TIMEOUT_MS }),
    api("/api/mentions", { timeout: REFRESH_TIMEOUT_MS }),
    api("/api/state", { timeout: REFRESH_TIMEOUT_MS })
  ];
  let rateLimited = null;
  const [tasks, rooms, alerts, mentions, state] = await Promise.all(
    quick.map((p) => p.catch((err) => {
      if (err && err.status === 429 && !rateLimited) rateLimited = err;
      return null;
    }))
  );
  if (generacion !== R.meGeneracion) return;
  const llegaron = [tasks, rooms, alerts, mentions, state].filter(Boolean).length;
  const outcome = clasificarCarga({ state, llegaron, total: quick.length, rateLimited });
  if (outcome === "session") {
    sesionTerminada((state && state.error) || "");
    return;
  } else if (rateLimited) {
    // Límite de llamadas a Oruscribe alcanzado: pausamos el refresco al menos
    // 60 s en lugar de martillar cada 4 s y omitimos las cargas secundarias.
    R.loadWasOffline = true;
    paintLoadBanner("ratelimit");
    toast(rateLimited.message || T("load.rateLimited"));
    scheduleLoadRetry(LOAD_RETRY_429_MS);
  } else if (outcome === "reconnecting") {
    R.loadWasOffline = true;
    paintLoadBanner("reconnecting", (state && state.error) || "", true);
    scheduleLoadRetry();
  } else if (llegaron < quick.length) {
    R.loadWasOffline = true;
    paintLoadBanner("reconnecting");
    scheduleLoadRetry();
  } else {
    const recovered = R.loadWasOffline;
    R.loadWasOffline = false;
    clearLoadRetry();
    paintLoadBanner("off");
    if (recovered) toast(T("load.restored"));
  }
  // La foto y el perfil llegan después del arranque: sin esta relectura la
  // barra lateral se queda con las iniciales toda la sesión.
  if (state && state.status === "ready" && state.me) {
    S.me = state.me;
    if (!fotoDe(state.me) && !rateLimited) programarRefreshMe(0, 1200);
  }
  if (tasks) {
    S.tasks = tasks.tasks || [];
    S.tasksPage = paginaDe(tasks);
    // Los arrays recién leídos mandan sobre las cifras del último `counts`.
    S.liveOpen = null;
  }
  if (rooms) {
    S.rooms = rooms.rooms || [];
    S.roomsPage = paginaDe(rooms);
    S.liveChat = null;
  }
  if (alerts) { S.alerts = alerts.alerts || []; S.alertsPage = paginaDe(alerts); }
  if (mentions) { S.mentions = mentions.mentions || []; S.mentionsPage = paginaDe(mentions); }
  // Sin una sola lectura no hay datos: se queda el esqueleto y el aviso de
  // reconexión, no un panel vacío que parece «nada pendiente».
  if (llegaron) S.loaded = true;
  saveSnapshot();
  render();
  if (rateLimited) return;
  loadGoals(false);
  loadPeriods(false);
  loadActivity(false);
  loadTeam(false);
  loadDecisions(false);
  loadAgent();
  if (S.view === "track") loadTrack(false);
}

let loadBannerDetail = "";
let loadBannerLogin = false;

function limpiarSesionLocal() {
  stopLive();
  stopChatPoll();
  clearLoadRetry();
  cancelarReintentosMe();
  R.loadWasOffline = false;
  paintLoadBanner("off");
  if (S.agentSpeak) {
    S.agentSpeak.pause();
    try { URL.revokeObjectURL(S.agentSpeak.src); } catch { /* ya no existe */ }
    S.agentSpeak = null;
  }
  if (S.agentMic) S.agentMic.parar();
  clearSnapshot();
  try { localStorage.removeItem("myFocus"); } catch { /* nada que borrar */ }
  S.me = null; S.loaded = false; S.tasks = []; S.rooms = []; S.alerts = []; S.goals = []; S.mentions = []; S.notes = []; R.curNote = null; S.messages.clear(); S.msgSig.clear(); S.liveOpen = null; S.liveChat = null;
  S.room = null; S.activity = null; S.decisions = null; S.teams = null; S.periods = null; S.trackTasks = null;
  S.digest = null; S.capture = { folder: "", since: "", scanning: false, checked: {} }; S.myContext = null;
  R.goalsAt = 0; S.activityAt = 0; S.decisionsAt = 0; S.teamsAt = 0; S.trackAt = 0; S.periodsAt = 0;
  R.replyTarget = null; setReplyBar(false);
  S.agent = null; S.convs = null; S.agentLogHtml = ""; S.agentFolders = []; S.agentDocs = []; S.agentImages = []; S.agentEdit = false; S.agentSetupShown = false; S.agentFormReady = false; S.agentSince = 0; S.agentOpen = new Set(); S.settings = null;
  S.view = "panel"; S.track = { tab: "kanban", status: "", q: "", member: "", from: "", to: "" };
  S.alertFilter = "all"; S.taskFilter = "open"; S.taskQuery = ""; S.goalDetail = null; S.decisionQuery = ""; S.mentionFilter = "all"; S.periodFilter = "";
  $$(".view").forEach((v) => v.classList.toggle("on", v.id === "v-panel"));
  $$(".side [data-view]").forEach((b) => b.classList.toggle("on", b.dataset.view === "panel"));
  $("#settings").classList.remove("on");
  renderOrgSelector();
}

function sesionTerminada(mensaje) {
  clearInterval(R.pollTimer);
  R.connOn = false;
  paintConn(false);
  limpiarSesionLocal();
  showScreen("welcome");
  noteError(T("session.ended") + (mensaje ? " " + mensaje : ""));
}

export function paintLoadBanner(state, detalle, conLogin) {
  R.loadBannerState = state;
  loadBannerDetail = typeof detalle === "string" ? detalle : "";
  loadBannerLogin = !!conLogin;
  const el = $("#load-banner");
  if (!el) return;
  el.classList.toggle("on", state !== "off");
  const login = el.querySelector("[data-load-login]");
  if (login) {
    login.textContent = T("load.login");
    showEl(login, state !== "off" && loadBannerLogin);
  }
  if (state === "off") return;
  const text = $("#load-banner-text");
  const key = state === "quitting"
    ? "load.quitting"
    : state === "ratelimit"
      ? "load.rateLimited"
      : state === "session"
        ? "load.session"
        : "load.reconnecting";
  if (text) {
    text.textContent = loadBannerDetail && (state === "session" || state === "reconnecting")
      ? T(key) + " " + loadBannerDetail
      : T(key);
  }
  const retry = el.querySelector("[data-load-retry]");
  if (retry) {
    retry.textContent = T(state === "session" ? "load.login" : "load.retryNow");
    retry.disabled = state === "quitting";
  }
  const quit = el.querySelector("[data-quit]");
  if (quit) quit.disabled = state === "quitting";
}

function scheduleLoadRetry(minDelay = 0) {
  if (loadRetryTimer || quitting) return;
  const delay = Math.max(loadRetryDelay, minDelay);
  loadRetryTimer = setTimeout(probeLoad, delay);
  loadRetryDelay = Math.min(Math.max(loadRetryDelay * 2, minDelay), Math.max(LOAD_RETRY_MAX_MS, minDelay));
}

export function clearLoadRetry() {
  clearTimeout(loadRetryTimer);
  loadRetryTimer = null;
  loadRetryDelay = LOAD_RETRY_MS;
}

async function probeLoad() {
  loadRetryTimer = null;
  if (quitting) return;
  const st = await api("/api/state", { timeout: REFRESH_TIMEOUT_MS }).catch(() => null);
  if (st && st.status === "ready") await refreshAll();
  else if (st && esEstadoSesionTerminada(st.status)) sesionTerminada(st.error || "");
  else scheduleLoadRetry();
}

async function quitApp() {
  if (quitting) return;
  quitting = true;
  clearLoadRetry();
  paintLoadBanner("quitting");
  try {
    await api("/api/quit", { method: "POST", body: "{}", timeout: 5000 });
  } catch {
    quitting = false;
    paintLoadBanner(R.loadWasOffline ? "reconnecting" : "off");
    toast(T("load.quitFail"));
    if (R.loadWasOffline) scheduleLoadRetry();
    return;
  }
  setTimeout(() => {
    if (!quitting) return;
    quitting = false;
    paintLoadBanner(R.loadWasOffline ? "reconnecting" : "off");
    toast(T("load.quitFail"));
    if (R.loadWasOffline) scheduleLoadRetry();
  }, 6000);
}


/* ---------- navigation ---------- */
const HEAD_KEYS = {
  panel: ["head.panel"], track: ["head.track"], brain: ["head.brain"], people: ["head.people"], team: ["head.team"],
  bell: ["head.alerts"], mentions: ["head.team"], tasks: ["head.work"], goals: ["head.org"],
  notes: ["head.local"], capture: ["head.capture"], agent: ["head.agent"],
};

function mentionTabs() {
  const on = (v) => (S.mentionFilter === v ? " on" : "");
  return '<div class="seg2"><button data-mfilter="all" class="' + on("all").trim() + '">' + esc(T("mentions.all")) +
    '</button><button data-mfilter="unread" class="' + on("unread").trim() + '">' + esc(T("mentions.unread")) + "</button></div>";
}

function setHead(name) {
  const c = counts();
  const me = S.me || {};
  const first = (me.name || "").split(" ")[0] || "";
  let sort = T("head.day");
  if (HEAD_KEYS[name]) sort = T(HEAD_KEYS[name][0]);
  let title = "", sub = "", act = "";
  switch (name) {
    case "panel":
      title = "<span>" + esc(T("today.greet")) + ",</span> " + esc(first);
      sub = summary(c.open, c.unread);
      act = (S.setup && S.setup.portable && S.setup.platform === "windows"
        ? '<button class="notice-chip" id="open-install" title="' + esc(T("install.cardSub")) + '"><svg class="i"><use href="#i-download"/></svg><span>' + esc(T("install.open")) + "</span></button>"
        : "") +
        '<button class="btn sm primary" data-new-task="1">' + esc(T("track.newTask")) + "</button>";
      break;
    case "track":
      title = esc(T("head.track"));
      sub = T("sub.track");
      act = '<button class="btn sm primary" data-new-task="1">' + esc(T("track.newTask")) + "</button>";
      break;
    case "people":
      title = esc(T("head.people"));
      sub = T("sub.people");
      act = '<button class="btn sm" data-open-focus="1">' + esc(T("fm.update")) + "</button>";
      break;
    case "brain":
      title = esc(T("head.brain"));
      sub = T("sub.brain");
      break;
    case "tasks":
      title = esc(T("tasks.title"));
      sub = T("sub.tasks");
      act = '<button class="btn sm primary" data-new-task="1">' + esc(T("tasks.new")) + "</button>";
      break;
    case "goals":
      title = esc(T("goals.title"));
      sub = T("sub.goals");
      act = '<button class="btn sm" data-open-period="1">' + esc(T("periods.new")) + "</button>" +
        '<button class="btn sm primary" data-open-goal="1">' + esc(T("goals.new")) + "</button>";
      break;
    case "capture":
      title = esc(T("head.capture"));
      sub = T("sub.capture");
      break;
    case "team":
      title = esc(T("nav.chat"));
      sub = T("sub.team");
      break;
    case "bell":
      title = esc(T("alerts.title"));
      sub = T("sub.alerts");
      act = '<button class="btn sm" id="mark-alerts">' + esc(T("alerts.markAll")) + "</button>" +
        '<button class="btn sm primary" data-send-alert="1">' + esc(T("alerts.send")) + "</button>";
      break;
    case "mentions":
      title = esc(T("mentions.title"));
      sub = T("sub.mentions");
      act = mentionTabs() + (S.mentions.some((m) => !m.read) ? '<button class="btn sm" data-mentions-read="1">' + esc(T("mentions.mark")) + "</button>" : "");
      break;
    case "agent":
      title = esc(T("nav.agent"));
      sub = T("sub.agent");
      break;
    case "notes":
      title = esc(T("nav.notes"));
      sub = T("sub.notes");
      break;
    default:
      title = esc(T("nav.panel"));
  }
  $("#page-kicker").textContent = sort;
  $("#page-title").innerHTML = title;
  $("#page-sub").textContent = S.loaded ? sub : "";
  $("#page-actions").innerHTML = act;
}

export function setView(name) {
  if (name === "today") name = "panel";
  S.view = name;
  migaUI("vista " + name);
  $$(".view").forEach((v) => v.classList.toggle("on", v.id === "v-" + name));
  $$("#v-team, #v-notes").forEach((v) => v.classList.remove("open"));
  $$(".side [data-view]").forEach((b) => {
    const on = b.dataset.view === name;
    b.classList.toggle("on", on);
    // La selección del menú no puede vivir solo en el color de fondo: los
    // lectores de pantalla la leen por aria-current.
    if (on) b.setAttribute("aria-current", "page"); else b.removeAttribute("aria-current");
  });
  const more = $("#side-more");
  if (more && more.querySelector('[data-view="' + name + '"]')) more.open = true;
  $("#side").classList.remove("open");
  // La vista también queda en el body: el botón flotante se aparta del
  // compositor del chat del equipo, que ocupa la esquina inferior derecha.
  document.body.dataset.currentView = name;
  // El botón flotante sobra dentro del propio chat; en el resto de pantallas
  // mantiene el acceso a un clic y no deja el chat escondido tras un icono.
  showEl($("#chat-fab"), name !== "agent");
  if (name === "notes") loadNotes();
  if (name === "agent") loadAgent(); else agentWatch(false);
  if (name === "track") loadTrack(false);
  if (name === "people") loadTeam(false);
  if (name === "brain") { loadTeam(false); loadDecisions(false); }
  if (name === "goals") { loadGoals(false); loadPeriods(false); }
  render();
  // Al volver al chat, la sala abierta se pone al día con el mismo tope que el
  // foco: el repaso periódico no corre mientras la persona está en otra vista,
  // pero entrar y salir de Chat tampoco puede disparar una lectura por clic.
  if (name === "team") refreshChatAlEnfocar();
}

/* ---------- paleta de comandos ---------- */
// Ctrl+K abre un índice de todo el arnés: cada pantalla con una línea que dice
// qué hace, y las acciones que antes estaban escondidas en un icono.
const CMDK = { items: [], sel: 0, open: false };
function cmdkViews() {
  return [
    { label: T("nav.panel"), sub: T("sub.panel"), icon: "i-panel", run: () => setView("panel") },
    { label: T("nav.agent"), sub: T("sub.agent"), icon: "i-zap", run: () => setView("agent") },
    { label: T("nav.track"), sub: T("sub.track"), icon: "i-columns", run: () => setView("track") },
    { label: T("nav.brain"), sub: T("sub.brain"), icon: "i-brain", run: () => setView("brain") },
    { label: T("nav.chat"), sub: T("sub.team"), icon: "i-hash", run: () => setView("team") },
    { label: T("nav.alerts"), sub: T("sub.alerts"), icon: "i-bell", run: () => setView("bell") },
    { label: T("nav.capture"), sub: T("sub.capture"), icon: "i-refresh", run: () => setView("capture") },
    { label: T("nav.mentions"), sub: T("sub.mentions"), icon: "i-at", run: () => setView("mentions") },
    { label: T("nav.tasks"), sub: T("sub.tasks"), icon: "i-tasks", run: () => setView("tasks") },
    { label: T("nav.goals"), sub: T("sub.goals"), icon: "i-target", run: () => setView("goals") },
    { label: T("nav.people"), sub: T("sub.people"), icon: "i-users", run: () => setView("people") },
    { label: T("nav.notes"), sub: T("sub.notes"), icon: "i-notebook", run: () => setView("notes") }
  ];
}
function cmdkActions() {
  return [
    { label: T("track.newTask"), sub: T("cmdk.newTaskSub"), icon: "i-plus", run: openTaskModal },
    { label: T("agent.newChat"), sub: T("cmdk.newChatSub"), icon: "i-zap", run: () => { setView("agent"); agentReset(); } },
    { label: T("common.refresh"), sub: T("cmdk.refreshSub"), icon: "i-refresh", run: () => { refreshAll(); loadSetup(); } },
    { label: T("set.theme"), sub: T("cmdk.themeSub"), icon: "i-moon", run: () => $("#tb-theme").click() },
    { label: T("set.title"), sub: T("cmdk.settingsSub"), icon: "i-settings", run: () => $("#open-settings").click() }
  ];
}
function cmdkRender() {
  const q = ($("#cmdk-q").value || "").trim().toLowerCase();
  const todos = [
    ...cmdkViews().map((it) => ({ ...it, group: T("cmdk.views") })),
    ...cmdkActions().map((it) => ({ ...it, group: T("cmdk.actions") }))
  ];
  CMDK.items = todos.filter((it) => !q || (it.label + " " + it.sub).toLowerCase().includes(q));
  CMDK.sel = 0;
  const list = $("#cmdk-list");
  if (!CMDK.items.length) {
    list.innerHTML = '<p class="cmdk-empty">' + esc(T("cmdk.empty")) + "</p>";
    return;
  }
  let lastGroup = "";
  list.innerHTML = CMDK.items.map((it, i) => {
    const head = it.group !== lastGroup ? '<p class="cmdk-group">' + esc(it.group) + "</p>" : "";
    lastGroup = it.group;
    return head + '<button class="cmdk-it' + (i === 0 ? " on" : "") + '" role="option" aria-selected="' + (i === 0) + '" data-cmdk="' + i + '">' +
    '<span class="cmdk-ic"><svg class="i"><use href="#' + it.icon + '"/></svg></span>' +
    '<span class="cmdk-txt"><b>' + esc(it.label) + "</b><small>" + esc(it.sub) + "</small></span>" +
    (i === 0 ? '<kbd>↵</kbd>' : "") + "</button>";
  }).join("");
}
function cmdkSelect(i) {
  const it = CMDK.items[i];
  if (!it) return;
  cmdkClose();
  it.run();
}
function cmdkMove(d) {
  if (!CMDK.items.length) return;
  CMDK.sel = Math.max(0, Math.min(CMDK.items.length - 1, CMDK.sel + d));
  $$("#cmdk-list .cmdk-it").forEach((el, i) => {
    el.classList.toggle("on", i === CMDK.sel);
    el.setAttribute("aria-selected", i === CMDK.sel ? "true" : "false");
    if (i === CMDK.sel) el.scrollIntoView({ block: "nearest" });
  });
}
function cmdkOpen() {
  CMDK.open = true;
  $("#cmdk").classList.add("on");
  $("#cmdk-q").value = "";
  cmdkRender();
  $("#cmdk-q").focus();
}
function cmdkClose() {
  CMDK.open = false;
  $("#cmdk").classList.remove("on");
}

export function render() {
  if (!S.me) return;
  renderMe();
  const view = S.view || "panel";
  if (view === "panel") renderPanel();
  else if (view === "track") renderTrack();
  else if (view === "brain") renderBrain();
  else if (view === "goals") renderGoals();
  else if (view === "people") renderPeople();
  else if (view === "tasks") renderTasks();
  else if (view === "bell") renderAlerts();
  else if (view === "mentions") renderMentions();
  else if (view === "team") { renderRooms(); if (S.room) renderMessages(); }
  else if (view === "notes") renderNotes();
  else if (view === "agent") renderAgent();
  else if (view === "capture") renderCapture();
  setHead(view);
  paintBadges();
}


/* ---------- wiring ---------- */
$("#btn-login").onclick = startLogin;
$("#login-retry").onclick = startLogin;
// Reutiliza la URL ya recibida: llamar a /api/auth/start otra vez reemplazaría
// el flujo OAuth en curso. Reintenta abrir el navegador y mantiene el enlace
// visible para copiarlo si el sistema no abre pestañas solo.
$("#open-browser").onclick = () => {
  if (!R.loginUrl) return;
  showLoginUrl(R.loginUrl);
  try { window.open(R.loginUrl, "_blank", "noopener"); } catch { /* respaldo manual */ }
  const input = $("#login-url");
  if (input) input.select();
};
$("#copy-login-url").onclick = async () => {
  const msg = $("#login-url-msg");
  if (!R.loginUrl) return;
  try {
    await copiarTexto(R.loginUrl);
    if (msg) msg.textContent = T("login.copied");
  } catch (err) {
    reportarError("ui.clipboard", "copy-login-url failed", err && err.message);
    const input = $("#login-url");
    if (input) { input.focus(); input.select(); }
    if (msg) msg.textContent = err.message;
  }
};
$("#login-continue").onclick = () => enterApp();
// Cerebro: el lienzo es estático; estos oyentes se cuelgan una sola vez.
$("#brain-zin").onclick = () => brainZoom(1.25);
$("#brain-reset").onclick = () => { BRAIN.zoom = 1; BRAIN.panx = 0; BRAIN.pany = 0; applyView(); };
$("#brain-zout").onclick = () => brainZoom(0.8);
$("#brain-fs").onclick = brainFullscreen;
$("#brain-q").addEventListener("input", brainSearch);
$("#brain-q").addEventListener("keydown", (e) => {
  const q = $("#brain-q");
  if (e.key === "Enter") { e.preventDefault(); brainEnter(); }
  if (e.key === "Escape") { q.value = ""; brainSearch({ target: q }); q.blur(); }
});
$("#brain-svg").addEventListener("pointerdown", brainDown);
$("#brain-svg").addEventListener("pointermove", brainMove);
$("#brain-svg").addEventListener("pointerup", brainUp);
$("#brain-svg").addEventListener("wheel", brainWheel, { passive: false });
$("#tb-theme").onclick = () => {
  // Un clic siempre cambia lo que se ve: en automático manda el tema del sistema.
  const seen = document.documentElement.dataset.theme ||
    (window.matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light");
  setTheme(seen === "dark" ? "light" : "dark");
};
document.addEventListener("click", (e) => {
  const quit = e.target.closest("[data-quit]");
  if (quit) { quitApp(); return; }
  const llogin = e.target.closest("[data-load-login]");
  if (llogin) { startLogin(); return; }
  const lretry = e.target.closest("[data-load-retry]");
  if (lretry) {
    if (R.loadBannerState === "session") startLogin();
    else { clearLoadRetry(); refreshAll(); }
    return;
  }
  const nretry = e.target.closest("[data-notes-retry]");
  if (nretry) { loadNotes(); return; }
  const bclose = e.target.closest("[data-bclose]");
  if (bclose) { closeBrainCard(); return; }
  // Solo botones: el <html> también lleva data-theme, y si entra aquí se come
  // todos los clics de la app en cuanto alguien elige tema claro u oscuro.
  const lang = e.target.closest("button[data-lang]");
  if (lang) { applyLang(lang.dataset.lang); return; }
  const th = e.target.closest("button[data-theme]");
  if (th) { setTheme(th.dataset.theme); return; }
  const back = e.target.closest("[data-back]");
  if (back) { showScreen(back.dataset.back); return; }
  const att = e.target.closest("[data-att-url]");
  if (att) {
    openAttachment(att.dataset.attUrl, att.dataset.attName || "", att.dataset.attMime || "");
    return;
  }
  const view = e.target.closest("[data-view]");
  if (view) { setView(view.dataset.view); return; }
  const room = e.target.closest("[data-room]");
  if (room) { setView("team"); openRoom(room.dataset.room); return; }
  if (e.target.closest("#tasks-more")) { cargarMasTareas(); return; }
  if (e.target.closest("#rooms-more")) { cargarMasSalas(); return; }
  const mfilter = e.target.closest("[data-mfilter]");
  if (mfilter) { S.mentionFilter = mfilter.dataset.mfilter; render(); return; }
  const task = e.target.closest("[data-task]");
  if (task) { setTaskStatus(task.dataset.task, task.dataset.to); return; }
  const adv = e.target.closest("[data-advance]");
  if (adv) { openAdvances(adv.dataset.advance, adv.dataset.taskTitle || ""); return; }
  const tdst = e.target.closest("[data-td-status]");
  if (tdst) { changeTaskStatus(tdst.dataset.tdStatus); return; }
  const taskAgent = e.target.closest("[data-task-agent]");
  if (taskAgent) {
    const t = taskById(taskAgent.dataset.taskAgent);
    if (t) startTaskInAgent(t, t.goal_id ? goalOf(t.goal_id) : null);
    return;
  }
  const openT = e.target.closest("[data-task-open]");
  if (openT) { openTaskDetail(openT.dataset.taskOpen); return; }
  const sc = e.target.closest("[data-send-comment]");
  if (sc) {
    if (sc.disabled) return;
    const inp = document.querySelector('input[data-comment="' + sc.dataset.sendComment + '"]');
    sc.disabled = true;
    submitComment(sc.dataset.sendComment, inp ? inp.value.trim() : "").finally(() => { sc.disabled = false; });
    return;
  }
  const goal = e.target.closest("[data-goal]");
  if (goal) { openGoal(goal.dataset.goal); return; }
  const tt = e.target.closest("[data-track-tab]");
  if (tt) { S.track.tab = tt.dataset.trackTab; renderTrack(); return; }
  if (e.target.closest("[data-track-clear]")) {
    S.track = { tab: S.track.tab, status: "", q: "", member: "", from: "", to: "" };
    renderTrack();
    return;
  }
  if (e.target.closest("[data-mentions-read]")) {
    const ids = S.mentions.filter((m) => !m.read).map((m) => m.message_id || m.id).filter(Boolean);
    if (ids.length) {
      api("/api/mentions/read", { method: "POST", body: JSON.stringify({ ids }) })
        .then(() => refreshAll())
        .catch((err) => toast(err.message));
    }
    return;
  }
  const af = e.target.closest("[data-alert-filter]");
  if (af) { S.alertFilter = af.dataset.alertFilter; renderAlerts(); return; }
  if (e.target.closest("[data-new-task]")) { openTaskModal(); return; }
  if (e.target.closest("[data-send-alert]")) { openAlertModal(); return; }
  if (e.target.closest("[data-open-goal]")) { openGoalModal(); return; }
  if (e.target.closest("[data-open-focus]")) { openFocusModal(); return; }
  if (e.target.closest("[data-open-period]")) { openPeriodModal(); return; }
  const pf = e.target.closest("[data-period-filter]");
  if (pf) { S.periodFilter = pf.dataset.periodFilter; renderGoals(); return; }
  if (e.target.closest("[data-cap-pick]")) { pickCaptureFolder(); return; }
  if (e.target.closest("[data-cap-scan]")) { scanCapture(); return; }
  if (e.target.closest("[data-cap-apply]")) { applyCapture(); return; }
  if (e.target.closest("[data-cap-all]")) {
    (S.digest && S.digest.candidates || []).forEach((c) => { S.capture.checked[c.id] = true; });
    renderCapture();
    return;
  }
  if (e.target.closest("[data-cap-none]")) { S.capture.checked = {}; renderCapture(); return; }
  if (e.target.closest("[data-cap-reset]")) { S.digest = null; S.capture.checked = {}; renderCapture(); return; }
  const pane = e.target.closest("[data-pane]");
  if (pane) {
    $$(".sheet nav .nav-it").forEach((b) => b.classList.toggle("on", b === pane));
    $$("[data-p]").forEach((d) => { d.style.display = d.dataset.p === pane.dataset.pane ? "block" : "none"; });
  }
});

$("#adv-close").onclick = closeAdvances;
$("#attach-close").onclick = () => {
  $("#attach-view").classList.remove("on");
  $("#attach-body").textContent = "";
};
$("#advances").addEventListener("mousedown", (e) => { if (e.target === e.currentTarget) closeAdvances(); });
$("#adv-upload").onclick = () => conBotonUnaVez($("#adv-upload"), submitAdvance);
$("#close-settings").onclick = settingsClosed;
$("#settings").addEventListener("mousedown", (e) => { if (e.target === e.currentTarget) settingsClosed(); });
$("#org-select").addEventListener("change", (e) => { seleccionarOrg(e.target.value); });
$("#logout").onclick = async () => {
  // Si el servidor no revoca el permiso, borrar lo local dejaría la ventana
  // pidiendo entrar de nuevo con la sesión todavía viva: se avisa y no se toca.
  try {
    await api("/api/auth/logout", { method: "POST", body: "{}" });
  } catch (err) {
    toast(err.message);
    return;
  }
  limpiarSesionLocal();
  showScreen("welcome");
};

$("#w-token-save").onclick = () => importarToken("w-token", "w-token-msg");
$("#token-save").onclick = () => importarToken("token-input", "token-msg", () => $("#settings").classList.remove("on"));
$("#menu").onclick = () => $("#side").classList.toggle("open");
$("#reload").onclick = () => {
  S.trackTasks = null;
  loadGoals(true); loadActivity(true); loadTeam(true); loadDecisions(true);
  refreshAll();
  if (S.room) openRoom(S.room);
};

/* ---------- modales nuevos ---------- */
$("#nt-close").onclick = closeTaskModal;
$("#nt-cancel").onclick = closeTaskModal;
$("#nt-create").onclick = () => conBotonUnaVez($("#nt-create"), submitTask);
$("#newtask").addEventListener("mousedown", (e) => { if (e.target === e.currentTarget) closeTaskModal(); });
$("#al-close").onclick = closeAlertModal;
$("#al-cancel").onclick = closeAlertModal;
$("#al-send").onclick = () => conBotonUnaVez($("#al-send"), submitAlert);
$("#alertbox").addEventListener("mousedown", (e) => { if (e.target === e.currentTarget) closeAlertModal(); });
$("#gd-close").onclick = closeGoalDetail;
$("#goaldetail").addEventListener("mousedown", (e) => { if (e.target === e.currentTarget) closeGoalDetail(); });
$("#new-dm").onclick = openDmModal;
$("#dm-close").onclick = closeDmModal;
$("#dm-cancel").onclick = closeDmModal;
$("#dm-send").onclick = () => conBotonUnaVez($("#dm-send"), sendDm);
$("#newdm").addEventListener("mousedown", (e) => { if (e.target === e.currentTarget) closeDmModal(); });
$("#ng-close").onclick = closeGoalModal;
$("#ng-cancel").onclick = closeGoalModal;
$("#ng-create").onclick = () => conBotonUnaVez($("#ng-create"), submitGoal);
$("#newgoal").addEventListener("mousedown", (e) => { if (e.target === e.currentTarget) closeGoalModal(); });
$("#td-close").onclick = closeTaskDetail;
$("#taskdetail").addEventListener("mousedown", (e) => { if (e.target === e.currentTarget) closeTaskDetail(); });
$("#jn-close").onclick = closeJournalModal;
$("#jn-cancel").onclick = closeJournalModal;
$("#jn-undo-one").onclick = undoJournalLast;
$("#journal-modal").addEventListener("mousedown", (e) => { if (e.target === e.currentTarget) closeJournalModal(); });
$("#fm-close").onclick = closeFocusModal;
$("#fm-cancel").onclick = closeFocusModal;
$("#fm-save").onclick = () => conBotonUnaVez($("#fm-save"), saveFocus);
$("#focusmodal").addEventListener("mousedown", (e) => { if (e.target === e.currentTarget) closeFocusModal(); });
$("#np-close").onclick = closePeriodModal;
$("#np-cancel").onclick = closePeriodModal;
$("#np-create").onclick = () => conBotonUnaVez($("#np-create"), submitPeriod);
$("#newperiod").addEventListener("mousedown", (e) => { if (e.target === e.currentTarget) closePeriodModal(); });
$("#bw-close").onclick = () => answerBlockReason(false);
$("#bw-cancel").onclick = () => answerBlockReason(false);
$("#bw-send").onclick = () => answerBlockReason(true);
$("#blockwhy").addEventListener("mousedown", (e) => { if (e.target === e.currentTarget) answerBlockReason(false); });
$("#bw-note").addEventListener("keydown", (e) => {
  if (e.key === "Enter") { e.preventDefault(); answerBlockReason(true); }
  else if (e.key === "Escape") { e.preventDefault(); answerBlockReason(false); }
});

function refocus(sel) {
  const el = $(sel);
  if (!el) return;
  el.focus();
  try { el.setSelectionRange(el.value.length, el.value.length); } catch { /* selects no aceptan selección */ }
}
let decTimer = null;
document.addEventListener("input", (e) => {
  const t = e.target;
  if (t.id === "tr-q") { S.track.q = t.value; renderTrack(); refocus("#tr-q"); }
  else if (t.id === "task-q") { S.taskQuery = t.value; renderTasks(); refocus("#task-q"); }
  else if (t.id === "cap-folder") { S.capture.folder = t.value; }
  else if (t.id === "cap-since") { S.capture.since = t.value; }
  else if (t.id === "dec-q") {
    clearTimeout(decTimer);
    decTimer = setTimeout(async () => { S.decisionQuery = t.value; await loadDecisions(true); refocus("#dec-q"); }, 350);
  }
});
document.addEventListener("change", (e) => {
  const t = e.target;
  if (t.id === "tr-status") { S.track.status = t.value; renderTrack(); }
  else if (t.id === "tr-member") { S.track.member = t.value; renderTrack(); }
  else if (t.id === "tr-from") { S.track.from = t.value; renderTrack(); }
  else if (t.id === "tr-to") { S.track.to = t.value; renderTrack(); }
  else if (t.id === "task-filter") { S.taskFilter = t.value; renderTasks(); }
  else if (t.matches("[data-cap-cand]")) { S.capture.checked[t.dataset.capCand] = t.checked; renderCapture(); }
});
document.addEventListener("click", async (e) => {
  const one = e.target.closest("[data-alert-one]");
  if (one) {
    await api("/api/alerts/read", { method: "POST", body: JSON.stringify({ ids: [one.dataset.alertOne] }) }).catch(() => {});
    await refreshAll();
    return;
  }
  if (e.target.closest("#mark-alerts")) {
    const ids = S.alerts.filter((a) => !a.read).map((a) => a.id);
    if (!ids.length) return;
    await api("/api/alerts/read", { method: "POST", body: JSON.stringify({ ids }) }).catch(() => {});
    await refreshAll();
  }
});

$("#room-q").oninput = (e) => {
  const q = fold(e.target.value);
  $$("#room-list .room").forEach((r) => { r.style.display = fold(r.textContent).includes(q) ? "" : "none"; });
};
$("#room-input").oninput = (e) => { $("#room-send").disabled = !e.target.value.trim(); };
$("#room-input").onkeydown = (e) => { if (e.key === "Enter" && !e.shiftKey) { e.preventDefault(); sendMessage(); } };
$("#room-send").onclick = sendMessage;
$("#room-attach").onclick = () => $("#room-file").click();

// Emojis del compositor: una lista curada y fija, sin dependencias. El que se
// elige entra donde esté el cursor y el popover se cierra con un clic afuera.
const EMOJIS = "😀 😂 🥹 😍 😎 🤔 😅 😢 😤 😴 🤖 👍 👎 👏 🙌 🤝 💪 🙏 🔥 🚀 ✅ ❌ 💡 📌 📎 ⏰ 🎉 ❤️ 👀 🤩".split(" ");
const emojiWrap = $("#emoji-wrap");
function insertEmoji(e) {
  const input = $("#room-input");
  const ini = input.selectionStart ?? input.value.length;
  const fin = input.selectionEnd ?? ini;
  input.value = input.value.slice(0, ini) + e + input.value.slice(fin);
  const pos = ini + e.length;
  input.setSelectionRange(pos, pos);
  input.focus();
  input.dispatchEvent(new Event("input", { bubbles: true }));
}
$("#room-emoji").onclick = (ev) => {
  ev.stopPropagation();
  if (emojiWrap.style.display === "grid") { emojiWrap.style.display = "none"; return; }
  if (!emojiWrap.dataset.ready) {
    emojiWrap.innerHTML = EMOJIS.map((e) => '<button class="emoji" data-emoji="' + e + '">' + e + "</button>").join("");
    emojiWrap.dataset.ready = "1";
  }
  emojiWrap.style.display = "grid";
};
emojiWrap.onclick = (ev) => {
  const b = ev.target.closest("[data-emoji]");
  if (!b) return;
  insertEmoji(b.dataset.emoji);
  emojiWrap.style.display = "none";
};
document.addEventListener("mousedown", (e) => {
  if (emojiWrap.style.display === "grid" && !e.target.closest("#emoji-wrap") && !e.target.closest("#room-emoji")) emojiWrap.style.display = "none";
});
$("#room-file").onchange = (e) => { const f = e.target.files[0]; e.target.value = ""; if (f) attachChatFile(f); };
$("#room-back").onclick = () => setView("team");
document.addEventListener("click", (e) => {
  if (!e.target.closest("[data-retry-cargas]")) return;
  S.cargasFallidas.clear();
  loadGoals(true);
  loadActivity(true);
  loadDecisions(true);
  renderPanel();
});

$("#room-reply-x") && ($("#room-reply-x").onclick = () => { R.replyTarget = null; setReplyBar(false); });
document.addEventListener("click", (e) => {
  const reply = e.target.closest("[data-reply]");
  if (reply) {
    R.replyTarget = reply.dataset.reply;
    const list = S.messages.get(S.room) || [];
    const msg = list.find((x) => x.id === R.replyTarget);
    $("#room-reply-t").textContent = msg ? ((msg.author ? msg.author.name : "") + ": " + (msg.body || "").slice(0, 60)) : R.replyTarget;
    setReplyBar(true);
    $("#room-input").focus();
    return;
  }
  const react = e.target.closest("[data-react]");
  if (react) {
    react.disabled = true;
    api("/api/messages/react", { method: "POST", body: JSON.stringify({ message: react.dataset.react, emoji: react.dataset.emoji || "👍" }) })
      .then(() => {
        const emoji = react.dataset.emoji || "👍";
        const lista = S.messages.get(S.room) || [];
        const msg = lista.find((x) => x.id === react.dataset.react);
        if (msg) {
          if (!Array.isArray(msg.reactions)) msg.reactions = [];
          const r = msg.reactions.find((x) => x && x.emoji === emoji);
          if (r) r.count = (r.count || 0) + 1;
          else msg.reactions.push({ emoji, count: 1 });
          if ($("#room-msgs")) renderMessages();
        }
        toast(emoji);
      })
      .catch((err) => { toast(err.message); react.disabled = false; });
    return;
  }
});

$("#note-q").oninput = renderNotes;
$("#note-new").onclick = () => {
  persistNote();
  openNote("n" + Date.now());
  $("#note-title").focus();
};
$("#note-back").onclick = () => { persistNote(); R.curNote = null; $("#v-notes").classList.remove("open"); renderNotes(); };
$("#note-del").onclick = () => $("#note-confirm").classList.add("on");
$("#note-del-no").onclick = () => $("#note-confirm").classList.remove("on");
$("#note-del-yes").onclick = async () => {
  const id = R.curNote;
  if (!id) return;
  const generacion = R.meGeneracion;
  invalidarNota();
  try {
    await api("/api/notes?id=" + encodeURIComponent(id), { method: "DELETE" });
  } catch (err) {
    // El servidor no borró la nota: se queda en la lista y se dice el error;
    // quitarla igual dejaba un estado que mentía hasta la próxima recarga.
    toast(err.message);
    return;
  }
  if (generacion !== R.meGeneracion) return;
  S.notes = S.notes.filter((n) => n.id !== id);
  R.curNote = null;
  $("#note-confirm").classList.remove("on");
  $("#v-notes").classList.remove("open");
  renderNotes();
};
$("#notes-items").addEventListener("click", (e) => {
  const card = e.target.closest("[data-note]");
  if (card) { persistNote(); openNote(card.dataset.note); }
});
["#note-title", "#note-body"].forEach((sel) => {
  $(sel).addEventListener("input", () => {
    $("#note-saved").textContent = T("notes.saving");
    clearTimeout(getSaveTimer());
    setSaveTimer(setTimeout(persistNote, 600));
  });
});

// Enter y Espacio activan lo que responde al clic pero no es un <button>:
// filas de tarea, tarjetas kanban y nodos del mapa. Sin esto, una persona solo
// con teclado no puede abrir el detalle de su trabajo.
document.addEventListener("keydown", (e) => {
  if (e.key !== "Enter" && e.key !== " ") return;
  const t = e.target;
  if (/^(INPUT|TEXTAREA|SELECT|BUTTON|A)$/.test(t.tagName)) return;
  const node = t.closest("[data-bnode]");
  if (node) { e.preventDefault(); openBrainCard(node.dataset.bnode); return; }
  const act = t.closest("[data-task-open]");
  if (act) { e.preventDefault(); act.click(); }
});
document.addEventListener("keydown", (e) => {
  if (e.key === "Escape") {
    if (R.approvalArmed) { desarmarAprobacion(); repaintApprovals(); }
    const cerrabaSettings = $("#settings").classList.contains("on");
    $("#settings").classList.remove("on");
    if (cerrabaSettings) settingsClosed();
    ["#newtask", "#alertbox", "#goaldetail", "#advances", "#newdm", "#newgoal", "#taskdetail", "#focusmodal", "#newperiod", "#install-modal", "#journal-modal"].forEach((sel) => { const el = $(sel); if (el) el.classList.remove("on"); });
    if ($("#attach-view").classList.contains("on")) {
      $("#attach-view").classList.remove("on");
      $("#attach-body").textContent = "";
    }
    if ($("#blockwhy").classList.contains("on")) answerBlockReason(false);
    closeBrainCard();
  }
  if (e.key === "/" && S.view === "brain" && !/^(INPUT|TEXTAREA)$/.test(e.target.tagName)) {
    const bq = $("#brain-q");
    if (bq) { e.preventDefault(); bq.focus(); }
  }
});

/* ---------- agente: cableado ---------- */
document.addEventListener("click", (e) => {
  const act = e.target.closest("[data-agent-act]");
  if (act) agentAction(act.dataset.agentAct);
  const aretry = e.target.closest("[data-agent-retry]");
  if (aretry) loadAgent();
  const conv = e.target.closest("[data-conv]");
  if (conv) openConversation(conv.dataset.conv);
  const sug = e.target.closest("[data-agent-suggest]");
  if (sug) {
    const input = $("#agent-input");
    input.value = sug.dataset.agentSuggest;
    input.focus();
    updateAgentSend();
  }
  const quitar = e.target.closest("[data-folder-remove]");
  if (quitar) {
    S.agentFolders = S.agentFolders.filter((f) => f !== quitar.dataset.folderRemove);
    paintFolders();
  }
  const rev = e.target.closest("[data-revoke-rule]");
  if (rev) revokeAgentRule(rev.dataset.revokeRule);
  const doc = e.target.closest("[data-agent-doc]");
  if (doc) {
    S.agentDocs.splice(Number(doc.dataset.agentDoc), 1);
    paintAgentDocs();
    updateAgentSend();
  }
  const img = e.target.closest("[data-agent-image]");
  if (img) {
    S.agentImages.splice(Number(img.dataset.agentImage), 1);
    paintAgentImages();
    updateAgentSend();
  }
  const speak = e.target.closest("[data-agent-speak]");
  if (speak) {
    const mensaje = (S.agent && S.agent.messages && S.agent.messages[Number(speak.dataset.agentSpeak)]) || null;
    if (mensaje) agentSpeak(mensaje.content, speak);
  }
  const copyReply = e.target.closest("[data-agent-copy]");
  if (copyReply) {
    const mensaje = (S.agent && S.agent.messages && S.agent.messages[Number(copyReply.dataset.agentCopy)]) || null;
    if (mensaje && mensaje.content) {
      copiarTexto(mensaje.content).then(() => toast(T("agent.copied"))).catch((err) => toast(err.message));
    }
  }
  const ap = e.target.closest("[data-approve]");
  if (ap) {
    const id = ap.dataset.approve;
    if (ap.dataset.decision !== "yes") { desarmarAprobacion(); agentApprove(id, false, false); return; }
    if (R.approvalArmed && R.approvalArmed.id === id && Date.now() < R.approvalArmed.hasta) {
      const tarjeta = ap.closest(".agent-approval");
      const recordar = !!(tarjeta && tarjeta.querySelector("[data-remember]:checked"));
      desarmarAprobacion();
      agentApprove(id, true, recordar);
    } else {
      // Armar no aprueba nada: repinta la tarjeta pidiendo confirmación y se
      // desarma sola, para que un segundo clic distraído no escriba nada. La
      // casilla de Recordar se lee al confirmar, no al armar.
      desarmarAprobacion();
      R.approvalArmed = { id, hasta: Date.now() + 6000, timer: setTimeout(() => { desarmarAprobacion(); repaintApprovals(); }, 6000) };
      repaintApprovals();
    }
  }
});
document.addEventListener("keydown", (e) => {
  if (e.target.id !== "agent-input" || e.key !== "Enter" || e.shiftKey || e.isComposing) return;
  e.preventDefault();
  if (!S.agent || !S.agent.running) agentSend();
});
document.addEventListener("input", (e) => {
  if (e.target.id === "agent-input") updateAgentSend();
  else if (e.target.closest("#agent-setup")) syncAgentForm();
});
document.addEventListener("change", (e) => {
  if (e.target.closest("#agent-setup")) syncAgentForm();
  if (e.target.id === "agent-convs" && e.target.value) openConversation(e.target.value);
  if (e.target.id === "agent-mode-quick") setAgentModeQuick(e.target.value);
});
$("#agent-provider").addEventListener("change", () => loadAgentModels($("#agent-provider").value));
$("#agent-attach").onclick = () => $("#agent-file").click();
$("#agent-file").onchange = async (e) => {
  const files = [...(e.target.files || [])];
  e.target.value = "";
  for (const f of files) await attachAgentFile(f);
};
$("#agent-mic").onclick = () => agentDictate();
// "toggle" no sube por el árbol: se escucha en captura para recordar qué pasos están abiertos.
$("#agent-log").addEventListener("toggle", (e) => {
  const d = e.target;
  if (!d.dataset || !d.dataset.step) return;
  if (d.open) S.agentOpen.add(d.dataset.step); else S.agentOpen.delete(d.dataset.step);
}, true);

/* ---------- timbre ---------- */


export function paintConn(on) {
  R.connOn = on;
  for (const id of ["#conn-pill", "#ac-pill"]) {
    const el = $(id);
    if (!el) continue;
    el.classList.toggle("off", !on);
    el.innerHTML = '<span class="dot"></span><span>' + esc(T(on ? "conn.ok" : "conn.off")) + "</span>";
  }
}

export function paintDoorbell(on) {
  const el = $("#ac-doorbell");
  if (!el) return;
  el.innerHTML = '<span class="dot"' + (on ? "" : ' style="background:var(--muted)"') + "></span><span>" + esc(T(on ? "set.doorbellOn" : "set.doorbellOff")) + "</span>";
  el.style.opacity = on ? "" : ".6";
}

// El timbre ya trae las cifras de tareas y chat (`counts`). Los avisos se
// muestran al instante y el badge se pinta con esas cifras: no hace falta un
// refresco completo por cada uno. Las demás listas se cargan al abrir su vista
// o con una acción de la persona. Así un equipo activo no convierte el timbre
// en sondeo de la API ni gasta la cuota del plan.
const EVENTOS_DE_CHAT = new Set(["chat_message", "chat_mention", "message", "mention"]);
const EVENTOS_DE_ALERTA = new Set(["alert"]);
const EVENTOS_DE_MENCION = new Set(["mention", "chat_mention"]);
let chatLiveTimer = null;
let listsLiveTimer = null;

function applyLiveEvent(e) {
  if (e.kind === "counts") {
    if (typeof e.open === "number") S.liveOpen = e.open;
    if (typeof e.chat === "number") S.liveChat = e.chat;
    paintBadges();
    return;
  }
  if (document.hidden) return;
  if (EVENTOS_DE_CHAT.has(e.kind) && S.view === "team" && S.room) scheduleChatLive();
  if (EVENTOS_DE_ALERTA.has(e.kind) || EVENTOS_DE_MENCION.has(e.kind)) scheduleListsLive();
}

// Alerta y mención son listas cortas: una lectura dirigida mantiene el badge
// al día. El tope de 15 s aplana la ráfaga, igual que el chat abierto.
function scheduleListsLive() {
  if (listsLiveTimer) return;
  listsLiveTimer = setTimeout(async () => {
    listsLiveTimer = null;
    if (document.hidden) return;
    const [alerts, mentions] = await Promise.all([
      api("/api/alerts").catch(() => null),
      api("/api/mentions").catch(() => null),
    ]);
    if (alerts) { S.alerts = alerts.alerts || []; S.alertsPage = paginaDe(alerts); }
    if (mentions) { S.mentions = mentions.mentions || []; S.mentionsPage = paginaDe(mentions); }
    paintBadges();
    if (S.view === "bell") renderAlerts();
    else if (S.view === "mentions") renderMentions();
  }, 15000);
}

function scheduleChatLive() {
  if (chatLiveTimer) return;
  chatLiveTimer = setTimeout(async () => {
    chatLiveTimer = null;
    if (document.hidden || S.view !== "team" || !S.room) return;
    const sala = S.room;
    const list = await cargarMensajes(sala).catch(() => null);
    if (!list || S.room !== sala) return;
    const sig = msgSignature(list);
    if (sig === S.msgSig.get(sala)) return;
    S.msgSig.set(sala, sig);
    S.messages.set(sala, list);
    renderMessages();
  }, 15000);
}

// One request at a time, parked on the server until something happens. A plain
// request instead of a stream because the window hands pages whole responses.
let liveOn = false, liveControl = null, liveGeneracion = 0;

async function liveLoop(generacion, control) {
  while (liveOn && generacion === liveGeneracion) {
    let e = null;
    try {
      e = await api("/api/events", { signal: control.signal });
      if (generacion !== liveGeneracion) return;
      paintConn(true);
    } catch {
      if (generacion !== liveGeneracion) return;
      paintConn(false);
      await new Promise((r) => setTimeout(r, 3000));
      continue;
    }
    if (!e || e.kind === "silencio") continue;
    const text = T("live." + e.kind);
    if (text !== "live." + e.kind) toast(text);
    applyLiveEvent(e);
  }
}

export function startLive() {
  if (liveOn) return;
  liveOn = true;
  const generacion = ++liveGeneracion;
  liveControl = new AbortController();
  liveLoop(generacion, liveControl);
}

export function stopLive() {
  liveOn = false;
  ++liveGeneracion;
  if (liveControl) { liveControl.abort(); liveControl = null; }
  if (chatLiveTimer) { clearTimeout(chatLiveTimer); chatLiveTimer = null; }
  if (listsLiveTimer) { clearTimeout(listsLiveTimer); listsLiveTimer = null; }
}

// El timbre avisa al instante; este repaso es solo la red por si el canal en
// vivo pierde un aviso o se cae. Sondear rápido quema la cuota del backend (una
// llamada de salas son 10-20 consultas en el servidor): 5 minutos basta.

$("#open-settings").addEventListener("click", () => {
  setSettingsTrigger(document.activeElement);
  $("#settings").classList.add("on");
  loadSettings();
  const primera = $("#settings .sheet").querySelector("button, [href], input, select, textarea");
  if (primera) primera.focus();
});

document.addEventListener("keydown", (e) => {
  if (e.key !== "Tab") return;
  const abiertos = $$(".modal.on");
  if (!abiertos.length) return;
  const modal = abiertos[abiertos.length - 1];
  const todos = focusablesIn(modal);
  if (!todos.length) return;
  e.preventDefault();
  const idx = todos.indexOf(document.activeElement);
  const next = e.shiftKey
    ? (idx <= 0 ? todos.length - 1 : idx - 1)
    : (idx === -1 || idx === todos.length - 1 ? 0 : idx + 1);
  todos[next].focus();
});

// Al cerrar un modal, el foco vuelve al control que lo abrió. Sin esto, quien
// usa el teclado aterriza en el cuerpo de la página y pierde su lugar.
const modalTriggers = new Map();
const modalFocusObserver = new MutationObserver((cambios) => {
  for (const cambio of cambios) {
    const el = cambio.target;
    if (!(el instanceof Element) || !el.classList.contains("modal")) continue;
    if (el.classList.contains("on")) {
      if (!modalTriggers.has(el)) modalTriggers.set(el, document.activeElement);
      continue;
    }
    const disparador = modalTriggers.get(el);
    modalTriggers.delete(el);
    if (disparador && disparador.isConnected && typeof disparador.focus === "function") disparador.focus();
  }
});
$$(".modal").forEach((m) => modalFocusObserver.observe(m, { attributes: true, attributeFilter: ["class"] }));
$("#autostart-toggle").onclick = async () => {
  const next = siguienteToggle(S.settings && S.settings.autostart);
  if (next === null) { toast(T("set.unknown")); loadSettings(); return; }
  try {
    await api("/api/settings", { method: "POST", body: JSON.stringify({ autostart: next }) });
    await loadSettings();
    toast(T("set.autostart") + ": " + T(next ? "set.on" : "set.off"));
  } catch (err) {
    toast(err.message);
  }
};
$$("[data-autostart-answer]").forEach((b) => {
  b.onclick = () => answerAutostart(b.dataset.autostartAnswer === "yes");
});
$("#autoupdate-toggle").onclick = async () => {
  const next = siguienteToggle(S.settings && S.settings.autoUpdate);
  if (next === null) { toast(T("set.unknown")); loadSettings(); return; }
  try {
    await api("/api/settings", { method: "POST", body: JSON.stringify({ autoUpdate: next }) });
    await loadSettings();
    toast(T("set.autoUpdate") + ": " + T(next ? "set.on" : "set.off"));
  } catch (err) {
    toast(err.message);
  }
};
const telBtn = $("#telemetry-toggle");
if (telBtn) {
  telBtn.onclick = async () => {
    const next = siguienteToggle(S.settings && S.settings.telemetry);
    if (next === null) { toast(T("set.unknown")); loadSettings(); return; }
    try {
      await api("/api/settings", { method: "POST", body: JSON.stringify({ telemetry: next }) });
      await loadSettings();
      toast(T("set.telemetry") + ": " + T(next ? "set.on" : "set.off"));
    } catch (err) {
      toast(err.message);
    }
  };
}
$("#update-banner-dismiss").onclick = () => {
  R.updateDismissed = (R.updateInfo && R.updateInfo.latest) || "";
  stopUpdatePoll();
  paintUpdateProgress(null);
  $("#update-banner").classList.remove("on");
};
$("#update-banner-go").onclick = async () => {
  const el = $("#update-banner-text");
  const go = $("#update-banner-go");
  go.disabled = true;
  el.textContent = T("update.working");
  // El sondeo arranca antes del POST: la descarga ya está en marcha cuando el
  // servidor empieza a mandar bytes, y el primer dato llega en el primer tick.
  paintUpdateProgress({ phase: "checking" });
  startUpdatePoll();
  try {
    await api("/api/update/apply", { method: "POST", body: "{}", timeout: LONG_ACTION_TIMEOUT_MS });
    stopUpdatePoll();
    paintUpdateProgress(null);
    el.textContent = T("update.restarting");
  } catch (err) {
    stopUpdatePoll();
    el.textContent = T("update.failed");
    paintUpdateProgress({ phase: "error", error: err.message });
    go.disabled = false;
  }
};
$("#copy-diag").onclick = async () => {
  const msg = $("#diag-msg");
  try {
    const d = await api("/api/diagnostics");
    await copiarTexto(d.text || "");
    msg.textContent = T("set.diagDone");
  } catch (err) {
    msg.textContent = err.message;
  }
};

$("#check-update").onclick = async () => {
  const msg = $("#update-msg");
  msg.textContent = T("set.checking");
  try {
    const r = await api("/api/update");
    if (!r.available) {
      msg.textContent = r.configured === false ? T("set.checkFail") : T("set.upToDate");
      return;
    }
    msg.textContent = T("set.updateReady") + " " + (r.latest || "") + " ";
    const go = document.createElement("button");
    go.className = "btn sm primary";
    go.textContent = T("set.doUpdate");
    go.onclick = async () => {
      go.disabled = true;
      msg.textContent = T("update.working");
      paintUpdateProgress({ phase: "checking" });
      startUpdatePoll();
      try {
        await api("/api/update/apply", { method: "POST", body: "{}", timeout: LONG_ACTION_TIMEOUT_MS });
        stopUpdatePoll();
        msg.textContent = T("update.restarting");
      } catch (err) {
        stopUpdatePoll();
        msg.textContent = err.message + (r.url ? " (" + r.url + ")" : "");
        go.disabled = false;
        msg.appendChild(document.createTextNode(" "));
        msg.appendChild(go);
      }
    };
    msg.appendChild(go);
  } catch {
    msg.textContent = T("set.checkFail");
  }
};

document.addEventListener("click", (e) => {
  if (e.target.closest("#open-install")) openInstall();
});
$("#install-close").onclick = closeInstall;
$("#install-cancel").onclick = closeInstall;
$("#install-terms-ok").onchange = (e) => { $("#install-go").disabled = !e.target.checked; };
$("#install-go").onclick = async () => {
  const msg = $("#install-msg");
  msg.textContent = T("install.working");
  try {
    await api("/api/setup/install", {
      method: "POST",
      body: JSON.stringify({ autostart: $("#install-auto").checked, termsVersion: S.setup.termsVersion }),
      timeout: LONG_ACTION_TIMEOUT_MS,
    });
    msg.textContent = T("install.done");
  } catch (err) {
    msg.textContent = err.message;
  }
};

if ($("#uninstall")) {
  $("#uninstall").onclick = async () => {
    const sure = window.confirm(T("set.uninstallSub") + "\n\n" + T("set.uninstall") + "?");
    if (!sure) return;
    try {
      await api("/api/setup/uninstall", { method: "POST", body: JSON.stringify({ keepData: true }), timeout: LONG_ACTION_TIMEOUT_MS });
    } catch (err) {
      toast(err.message);
    }
  };
}

/* ---------- paleta de comandos: eventos ---------- */
$("#open-cmdk").addEventListener("click", cmdkOpen);
$("#cmdk-q").addEventListener("input", cmdkRender);
$("#cmdk-q").addEventListener("keydown", (e) => {
  if (e.key === "ArrowDown") { e.preventDefault(); cmdkMove(1); }
  else if (e.key === "ArrowUp") { e.preventDefault(); cmdkMove(-1); }
  else if (e.key === "Enter") { e.preventDefault(); cmdkSelect(CMDK.sel); }
  else if (e.key === "Escape") { e.preventDefault(); cmdkClose(); }
});
$("#cmdk-list").addEventListener("click", (e) => {
  const it = e.target.closest("[data-cmdk]");
  if (it) cmdkSelect(Number(it.dataset.cmdk));
});
$("#cmdk").addEventListener("mousedown", (e) => { if (e.target === e.currentTarget) cmdkClose(); });
document.addEventListener("keydown", (e) => {
  if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "k") {
    e.preventDefault();
    if (CMDK.open) cmdkClose(); else cmdkOpen();
  } else if (e.key === "Escape" && CMDK.open) {
    cmdkClose();
  }
});
if (/Mac|iPhone|iPad/.test(navigator.platform || "")) $("#cmdk-key").textContent = "⌘K";
// Un foco no puede iniciar una cadena nueva de reintentos: con la foto ya
// visible no hace falta, y con la foto ausente basta una cadena a la vez.
let ultimoMeFoco = 0;
window.addEventListener("focus", () => {
  if (!S.me || fotoDe(S.me)) return;
  const ahora = Date.now();
  if (ahora - ultimoMeFoco < 60000) return;
  ultimoMeFoco = ahora;
  programarRefreshMe(0, 0);
});

boot();
