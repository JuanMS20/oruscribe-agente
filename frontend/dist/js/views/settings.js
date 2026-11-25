// Login OAuth/token, Configuración, Autostart, Actualizaciones e Instalador.
import { T, getLang } from "../i18n.js";
import { $, $$, esc, showEl, toast, enMiB, reportarError } from "../dom.js";
import { api, LOGIN_START_TIMEOUT_MS } from "../api.js";
import { S, R, roleLabel } from "../state.js";
import { esTokenCompatible, clasificarSondeoLogin, estadoToggle } from "../estados.mjs";
import { enterApp, showScreen, render, noteError, paintDoorbell } from "../main.js";

export function setSettingsTrigger(el) { settingsTrigger = el; }

/* ---------- login ---------- */
const LOGIN_POLL_TIMEOUT_MS = 5 * 60 * 1000;
const LOGIN_POLL_ERRORS_MAX = 5;

let loginStarting = false;


export function showLoginUrl(url) {
  const box = $("#login-url-box");
  const input = $("#login-url");
  if (!box || !input) return;
  input.value = url || "";
  box.style.display = url ? "block" : "none";
}

export function showLoginFail(msg, keepUrl) {
  clearInterval(R.pollTimer);
  if (!keepUrl) showLoginUrl("");
  $("#login-wait").style.display = "none";
  $("#login-ok").style.display = "none";
  $("#login-fail").style.display = "block";
  $("#login-fail-text").textContent = msg || T("err.generic");
}

function loginTimeout() {
  showLoginFail(T("login.timeout"), true);
}

function loginPollError(err) {
  reportarError("ui.login", "login poll failed", err && err.message);
  showLoginFail((err && err.message) || T("login.pollError"), true);
}

export async function startLogin() {
  if (loginStarting) return;
  loginStarting = true;
  noteError("");
  try {
    const res = await api("/api/auth/start", { method: "POST", body: "{}", timeout: LOGIN_START_TIMEOUT_MS });
    showScreen("login");
    R.loginUrl = (res && res.url) || "";
    showLoginUrl(R.loginUrl);
    if (!R.loginUrl) { showLoginFail(T("login.urlMissing")); return; }
    $("#login-wait").style.display = "block";
    $("#login-ok").style.display = "none";
    $("#login-fail").style.display = "none";
    pollLogin(R.loginUrl);
  } catch (err) {
    noteError(err.message);
    toast(err.message || T("err.generic"));
    reportarError("ui.login", "start login failed", err && err.message);
  } finally {
    loginStarting = false;
  }
}

export function pollLogin(url) {
  clearInterval(R.pollTimer);
  const started = Date.now();
  let errores = 0;
  R.pollTimer = setInterval(async () => {
    if (Date.now() - started >= LOGIN_POLL_TIMEOUT_MS) {
      clearInterval(R.pollTimer);
      loginTimeout();
      return;
    }
    let st = null;
    try { st = await api("/api/auth/state"); }
    catch (err) {
      errores++;
      if (errores >= LOGIN_POLL_ERRORS_MAX) {
        clearInterval(R.pollTimer);
        loginPollError(err);
      }
      return;
    }
    errores = 0;
    const clase = clasificarSondeoLogin(st);
    if (clase === "ok") {
      clearInterval(R.pollTimer);
      showLoginUrl("");
      S.me = st.me;
      $("#login-wait").style.display = "none";
      $("#login-ok").style.display = "block";
      $("#login-ok-text").innerHTML = esc(st.me.name) + " · " + esc(st.me.organization ? st.me.organization.name : "");
      await enterApp();
    } else if (clase === "fail") {
      clearInterval(R.pollTimer);
      showLoginFail(st.error || T(st.status === "logged_out" ? "session.ended" : "err.generic"));
    }
  }, 1200);
}


// Un token se importa igual desde la bienvenida y desde ajustes: pide el
// token, importa, y quien llama decide qué cerrar antes de entrar a la app.
export async function importarToken(inputId, msgId, antesDeEntrar) {
  const token = $("#" + inputId).value.trim();
  const msg = $("#" + msgId);
  if (!token) { msg.textContent = "—"; return; }
  if (!esTokenCompatible(token)) {
    msg.textContent = T("token.incompatible");
    reportarError("auth.token", "token sin forma JWT rechazado antes de importar", "");
    return;
  }
  msg.textContent = T("common.loading");
  try {
    const st = await api("/api/auth/import", { method: "POST", body: JSON.stringify({ token }) });
    S.me = st.me;
    $("#" + inputId).value = "";
    msg.textContent = "";
    if (antesDeEntrar) antesDeEntrar();
    await enterApp();
  } catch (err) {
    msg.textContent = err.message;
  }
}

/* ---------- ajustes ---------- */
function pintarToggle(id, valor) {
  const btn = $(id);
  if (!btn) return;
  const estado = estadoToggle(valor);
  btn.textContent = estado === "unknown" ? "—" : T(estado === "on" ? "set.on" : "set.off");
  btn.dataset.on = estado === "unknown" ? "?" : estado === "on" ? "1" : "0";
}

export async function loadSettings() {
  try {
    const st = await api("/api/settings");
    S.settings = st;
    // El aviso se comprobó antes de tener ajustes. Si la persona apagó la
    // búsqueda automática, manda su ajuste y el banner se va.
    if (st.autoUpdate === false) $("#update-banner").classList.remove("on");
    $("#ac-version").textContent = st.version || "—";
    $("#ac-datadir").textContent = st.dataDir || "—";
    pintarToggle("#autostart-toggle", st.autostart);
    const btn = $("#autostart-toggle");
    const filaAutostart = btn.parentElement;
    if (filaAutostart) filaAutostart.style.display = st.platform === "windows" ? "" : "none";
    pintarToggle("#autoupdate-toggle", st.autoUpdate);
    pintarToggle("#telemetry-toggle", st.telemetry);
    paintDoorbell(!!st.doorbell);
    const qRow = $("#ac-quota-row");
    const qVal = $("#ac-quota");
    if (qRow && qVal) {
      if (st.mcpQuota) {
        const q = st.mcpQuota;
        qVal.textContent = q.usedMinute + "/" + q.limitMinute + " min · " + q.usedHour + "/" + q.limitHour + " h";
        qVal.className = "pill2 mono" + (q.usedMinute >= q.limitMinute * 0.8 || q.usedHour >= q.limitHour * 0.8 ? " warn" : "");
        qRow.style.display = "";
      } else {
        qRow.style.display = "none";
      }
    }
    return st;
  } catch (err) {
    S.settings = null;
    for (const id of ["#autostart-toggle", "#autoupdate-toggle", "#telemetry-toggle"]) pintarToggle(id, undefined);
    toast((err && err.message) || T("err.load"));
    reportarError("ui.settings", "loadSettings failed", err && err.message);
    return null;
  }
}

// La pregunta de arranque se hace una vez por instalación y se recuerda en
// settings.json. En Linux el paquete nativo no pasaba por el instalador y nadie
// la hacía; por eso Osman no pudo confirmar si abría sola.
let autostartAskedThisRun = false;
export function maybeAskAutostart() {
  if (autostartAskedThisRun || !S.settings || !S.setup || S.settings.platform !== "windows") return;
  if (!S.setup.installed || S.settings.autostart || S.settings.autostartAsked) return;
  autostartAskedThisRun = true;
  showEl($("#autostart-prompt"), true);
}

export async function answerAutostart(yes) {
  const body = { autostartAsked: true };
  if (yes) body.autostart = true;
  try {
    S.settings = await api("/api/settings", { method: "POST", body: JSON.stringify(body) });
    showEl($("#autostart-prompt"), false);
    toast(T("set.autostart") + ": " + T(yes ? "set.on" : "set.off"));
  } catch (err) {
    toast(err.message);
  }
}

// El aviso de versión nueva corre aunque no haya sesión: en la bienvenida
// todavía no hay ajustes cargados y manda el valor por defecto (encendido),
// porque apagarlo es una decisión explícita que el servidor ya tendrá guardada.
// Se repite cada 6 h sin recargar la ventana, y sin red no dice nada: no
// convierte un arranque normal en un error.
let updateCheckedThisRun = false;


let updatePollOn = false;
let updatePollTimer = null;

const UPDATE_RECHECK_MS = 30 * 60 * 1000;
const UPDATE_FOCUS_MIN_MS = 15 * 60 * 1000;
const UPDATE_POLL_MS = 500;
let lastUpdateCheckAt = 0;

export async function maybeAutoUpdate() {
  if (updateCheckedThisRun) return;
  if (S.settings && S.settings.autoUpdate === false) return;
  updateCheckedThisRun = true;
  lastUpdateCheckAt = Date.now();
  try {
    paintUpdateBanner(await api("/api/update"));
  } catch { /* sin red: se reintenta a los 30 min o al volver a enfocar */ }
}

setInterval(() => {
  updateCheckedThisRun = false;
  maybeAutoUpdate();
}, UPDATE_RECHECK_MS);

document.addEventListener("visibilitychange", () => {
  if (document.visibilityState !== "visible") return;
  if (Date.now() - lastUpdateCheckAt < UPDATE_FOCUS_MIN_MS) return;
  updateCheckedThisRun = false;
  maybeAutoUpdate();
});

// Un solo sitio decide qué se ve del aviso: texto, notas y progreso. Al
// cambiar de idioma se repinta con el último dato recibido.
export function paintUpdateBanner(r) {
  R.updateInfo = r || null;
  const banner = $("#update-banner");
  if (!banner) return;
  const latest = (r && r.latest) || "";
  // La clase `.on` es la que lo hace visible: el estilo base es display:none.
  if (!r || !r.available || (latest && latest === R.updateDismissed)) {
    banner.classList.remove("on");
    return;
  }
  const texto = $("#update-banner-text");
  if (texto) texto.textContent = T("set.updateReady") + (latest ? " " + latest : "");
  const caja = $("#update-notes-box");
  const notas = $("#update-notes");
  const cuerpo = String((r && r.notes) || "").trim();
  if (caja && notas) {
    notas.textContent = cuerpo;
    caja.hidden = !cuerpo;
  }
  if (!updatePollOn) paintUpdateProgress(null);
  banner.classList.add("on");
}

// Un solo formateador para todas las fases: porcentaje cuando el servidor
// manda Content-Length, MiB recibidos cuando no, y el texto fijo mientras
// verifica, aplica o se reinicia. Nadie más escribe en #update-progress.
export function paintUpdateProgress(p) {
  const el = $("#update-progress");
  if (!el) return;
  const fase = (p && p.phase) || "idle";
  if (fase === "idle") {
    el.textContent = "";
    el.hidden = true;
    return;
  }
  const recibido = Number(p.received) || 0;
  const total = Number(p.total) || 0;
  let texto;
  if (fase === "downloading") {
    texto = total > 0
      ? Math.floor((recibido * 100) / total) + " % · " + enMiB(recibido) + " / " + enMiB(total)
      : enMiB(recibido);
  } else if (fase === "verifying") texto = T("update.verifying");
  else if (fase === "applying") texto = T("update.applying");
  else if (fase === "restarting") texto = T("update.restarting");
  else if (fase === "error") texto = String((p && p.error) || "").trim() || T("update.failed");
  else texto = T("update.pending");
  el.textContent = texto;
  el.hidden = false;
}

// El sondeo se encadena con setTimeout: si el servidor tarda más que el
// intervalo, no se apilan peticiones. Informa, no manda: si falla, el banner
// sigue con el texto del POST.
async function leerProgresoActualizacion() {
  try {
    paintUpdateProgress(await api("/api/update/progress", { timeout: 8000 }));
  } catch (err) {
    // Un servidor sin la ruta no tiene progreso que dar: se apaga el sondeo en
    // vez de repetir un 404 cada 500 ms durante toda la descarga.
    if (err && err.status === 404) stopUpdatePoll();
  }
}

export function startUpdatePoll() {
  if (updatePollOn) return;
  updatePollOn = true;
  const paso = async () => {
    if (!updatePollOn) return;
    await leerProgresoActualizacion();
    if (updatePollOn) updatePollTimer = setTimeout(paso, UPDATE_POLL_MS);
  };
  paso();
}

export function stopUpdatePoll() {
  updatePollOn = false;
  clearTimeout(updatePollTimer);
  updatePollTimer = null;
}

let settingsTrigger = null;

export function settingsClosed() {
  $("#settings").classList.remove("on");
  if (settingsTrigger) { settingsTrigger.focus(); settingsTrigger = null; }
}

/* ---------- instalación ---------- */
export async function loadSetup() {
  try {
    S.setup = await api("/api/setup");
  } catch {
    S.setup = null;
  }
  const row = $("#uninstall-row");
  if (row) row.style.display = S.setup && S.setup.installed && S.setup.platform === "windows" ? "flex" : "none";
  if (S.setup && S.setup.portable) render();
}

export function openInstall() {
  if (!S.setup) return;
  $("#install-title").textContent = S.setup.termsTitle || T("install.terms");
  $("#install-terms").textContent = S.setup.termsBody || "";
  $("#install-terms-ok").checked = false;
  $("#install-go").disabled = true;
  $("#install-msg").textContent = "";
  $("#install-modal").classList.add("on");
}

export const closeInstall = () => $("#install-modal").classList.remove("on");
