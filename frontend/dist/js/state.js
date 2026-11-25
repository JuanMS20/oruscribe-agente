// Estado reactivo compartido (S), banderas de ejecución (R) y caché local.
import { T } from "./i18n.js";
import { esc, safeUrl, initials } from "./dom.js";

export const CACHE_KEY = "oruSnap:";
export const CACHE_V = 3;
export const cacheKey = () => CACHE_KEY + S.me.user_id + ":" + ((S.me.organization && S.me.organization.slug) || "");

// Banderas mutables compartidas entre módulos (evita reasignar bindings importados).
export const R = {
  meGeneracion: 0,
  loadBannerState: "off",
  loadWasOffline: false,
  updateInfo: null,
  updateDismissed: "",
  replyTarget: null,
  curNote: null,
  goalsAt: 0,
  loginUrl: "",
  agentLoading: 0,
  approvalArmed: null,
  pollTimer: null,
  connOn: true,
};

export const S = {
  loaded: false, me: null, tasks: [], rooms: [], alerts: [], notes: [], goals: [], mentions: [],
  mentionFilter: "all", messages: new Map(), msgSig: new Map(), msgsIncompletos: new Set(), room: null, view: "panel", lang: "es",
  liveOpen: null, liveChat: null,
  activity: null, decisions: null, teams: null, teamsError: "", cargasFallidas: new Set(), activityAt: 0, decisionsAt: 0, teamsAt: 0, trackAt: 0,
  track: { tab: "kanban", status: "", q: "", member: "", from: "", to: "" }, trackTasks: null, alertFilter: "all",
  taskFilter: "open", taskQuery: "", goalDetail: null, known: [], decisionQuery: "",
  capture: { folder: "", since: "", scanning: false, checked: {} }, digest: null, myContext: null,
  periods: null, periodsAt: 0, periodFilter: "",
  agent: null, agentEdit: false, agentSetupShown: false, agentFormReady: false, agentSince: 0, agentOpen: new Set(), agentLogHtml: "", convs: null, agentFolders: [], agentDocs: [], agentImages: [], agentMic: null, agentSpeak: null, voice: null, settings: null
};

export function knownPeople() {
  const out = new Map();
  const add = (p) => { if (p && p.id && !out.has(p.id)) out.set(p.id, { id: p.id, name: p.name || p.id, is_agent: !!p.is_agent, role: p.role || "worker", avatar_url: p.avatar_url || "", photo_url: p.photo_url || "", picture: p.picture || "", avatar: p.avatar || "" }); };
  if (S.me) add({ id: S.me.user_id, name: S.me.name, is_agent: S.me.is_agent, role: S.me.role, avatar_url: S.me.avatar_url || "", photo_url: S.me.photo_url || "", picture: S.me.picture || "", avatar: S.me.avatar || "" });
  ((S.teams || []).forEach((t) => (t.workers || []).forEach((w) => add(w.user))));
  S.rooms.forEach((r) => add(r.with));
  return out;
}
export function personName(id) {
  const p = knownPeople().get(id);
  return p ? p.name : "";
}

// El arranque pinta primero lo último que se vio y luego pide todo al servidor.
// La copia local es solo para pintar la primera pantalla antes de que llegue
// la red, no un archivo de trabajo. Se recorta lo que más pesa y no se usa en
// los listados: descripciones largas, subtareas y el texto completo de las
// menciones. La actividad, las decisiones y los equipos no entran. La copia se
// borra al cerrar sesión.
const SNAP_TASKS = 200, SNAP_ROOMS = 50, SNAP_ALERTS = 50, SNAP_MENTIONS = 50, SNAP_GOALS = 50, SNAP_PERIODS = 20;

function resumenTarea(t) {
  return {
    id: t.id, title: t.title, status: t.status, priority: t.priority,
    goal_id: t.goal_id, start_date: t.start_date, due_date: t.due_date,
    updated_at: t.updated_at, created_at: t.created_at,
    assigned_to: t.assigned_to, parent_task_id: t.parent_task_id,
    tags: t.tags, points: t.points,
  };
}

function resumenMeta(g) {
  return {
    id: g.id, team_id: g.team_id, title: g.title, due_date: g.due_date,
    weight: g.weight, is_active: g.is_active, created_at: g.created_at,
    period_id: g.period_id, created_by: g.created_by, is_primary: g.is_primary,
    progress_pct: g.progress_pct, cells_done: g.cells_done, cells_total: g.cells_total,
    mode: g.mode, per_worker: g.per_worker, needs_attention: g.needs_attention,
    pct: g.pct, workers_summary: g.workers_summary,
  };
}

export function saveSnapshot() {
  if (!S.me || !S.loaded) return;
  try {
    const snap = {
      v: CACHE_V, at: Date.now(),
      tasks: S.tasks.slice(0, SNAP_TASKS).map(resumenTarea),
      rooms: S.rooms.slice(0, SNAP_ROOMS),
      alerts: S.alerts.slice(0, SNAP_ALERTS),
      mentions: S.mentions.slice(0, SNAP_MENTIONS).map((m) => ({ ...m, body: String(m.body || "").slice(0, 280) })),
      goals: S.goals.slice(0, SNAP_GOALS).map(resumenMeta),
      periods: S.periods.slice(0, SNAP_PERIODS),
    };
    localStorage.setItem(cacheKey(), JSON.stringify(snap));
  } catch { /* sin espacio o sin almacenamiento: se arranca sin caché */ }
}

export function loadSnapshot() {
  if (!S.me) return false;
  try {
    const raw = localStorage.getItem(cacheKey());
    if (!raw) return false;
    const s = JSON.parse(raw);
    if (!s || s.v !== CACHE_V) return false;
    S.tasks = s.tasks || [];
    S.rooms = s.rooms || [];
    S.alerts = s.alerts || [];
    S.mentions = s.mentions || [];
    S.goals = s.goals || [];
    S.periods = s.periods || [];
    S.loaded = true;
    return true;
  } catch {
    return false;
  }
}

export function clearSnapshot() {
  if (!S.me) return;
  try { localStorage.removeItem(cacheKey()); } catch { /* nada que borrar */ }
}

export const fotoDe = (me) => (me && (me.avatar_url || me.photo_url || me.picture || me.avatar)) || "";

export function counts() {
  const open = S.liveOpen !== null ? S.liveOpen : S.tasks.filter((t) => t.status !== "done").length;
  const unread = S.alerts.filter((a) => !a.read).length;
  const chat = S.liveChat !== null ? S.liveChat : S.rooms.reduce((n, r) => n + (r.unread_count || 0) + (r.mention_count || 0), 0);
  return { open, unread, chat };
}


export const roleLabel = (role) => ({ coordinator: T("role.coordinator"), worker: T("role.worker"), manager: T("role.manager") }[role] || role || "—");
export const avatarUrl = (p) => safeUrl((p && (p.avatar_url || p.photo_url || p.picture || p.avatar)) || "");
// Las iniciales van siempre; la foto se pinta encima. Si la foto falla, se
// oculta y queda un rostro legible en vez de un icono roto.
export const faceInner = (name, raw) => {
  const url = safeUrl(raw);
  return esc(initials(name)) + (url ? '<img data-face src="' + esc(url) + '" alt="">' : "");
};


export function invalidateTaskCache() {
  S.trackTasks = null;
  S.trackAt = 0;
}
