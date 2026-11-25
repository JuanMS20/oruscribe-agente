// Vista Cerebro: mapa interactivo SVG de metas, equipos, personas y tareas.
import { T } from "../i18n.js";
import { $, $$, esc, trunc } from "../dom.js";
import { S, knownPeople, personName } from "../state.js";
import { skeleton, pctOf, dayText, ownerOf, goalOf, openTaskDetail, openGoal } from "./work.js";
import { setView } from "../main.js";

/* ---------- cerebro: grafo de la organización ---------- */
// Un nodo por pieza de la organización y un enlace por actividad (hacia quien
// la hizo). Física propia sin librerías, como la de la web: repulsión entre
// nodos, resortes en los enlaces y atracción al anillo de su tipo; las
// decisiones quedan fijas en el anillo exterior.
export const BRAIN = { nodes: [], links: [], idx: new Map(), els: new Map(), pos: new Map(),
  off: new Set([]), q: "", zoom: 1, panx: 0, pany: 0, sel: null, drag: null };
const BW = 1000, BH = 700, BCX = BW / 2, BCY = BH / 2;
const BRAIN_RINGS = { teams: 60, people: 115, goals: 165, tasks: 210, subs: 240, activity: 275, decisions: 315 };
// Etiquetas que solo aparecen al pasar por encima o al buscarlas. Con quince
// metas y treinta decisiones en pantalla, treinta textos recortados a la vez
// se pisaban entre sí y con los nodos, y era lo que hacía la vista más casera.
const BRAIN_QUIET = { goals: 1, tasks: 1, subs: 1, decisions: 1, activity: 1 };
const BRAIN_TKEY = { teams: "brain.kTeam", people: "brain.kPerson", goals: "brain.kGoal", tasks: "brain.kTask",
  subs: "brain.kSub", decisions: "brain.kDecision", activity: "brain.kActivity" };

// apilados. Se mezcla con avalancha para que cada id dé su propia posición.
function brainJitter(id, amp) {
  let h = 2166136261 >>> 0;
  for (let i = 0; i < id.length; i++) { h ^= id.charCodeAt(i); h = Math.imul(h, 16777619) >>> 0; }
  h ^= h >>> 15; h = Math.imul(h, 2246822507) >>> 0;
  h ^= h >>> 13; h = Math.imul(h, 3266489909) >>> 0;
  h = (h ^ (h >>> 16)) >>> 0;
  return ((h % 10000) / 10000 - 0.5) * 2 * amp;
}

function brainEventLabel(e) {
  const base = e.task_title || e.title || e.current_focus || e.notes || e.type || "";
  if (e.type === "task_status_changed" && e.to_status) return base + " → " + (T("tasks." + e.to_status) || e.to_status);
  return base;
}

function buildBrainData() {
  const nodes = [], links = [], idx = new Map();
  const push = (n) => { idx.set(n.id, n); nodes.push(n); };
  (S.teams || []).forEach((tm) => push({ id: "t:" + tm.id, type: "teams", label: tm.name || "", sub: (tm.workers || []).length + " " + T("brain.people") }));
  const brainPeople = new Map(knownPeople());
  (S.activity || []).forEach((e) => {
    const id = e.user_id || e.actor_id || "";
    const name = e.actor_name || e.actor || "";
    if (id && !brainPeople.has(id)) brainPeople.set(id, { id, name: name || id, is_agent: false, role: "" });
    if (!id && name && !brainPeople.has("name:" + name)) brainPeople.set("name:" + name, { id: "name:" + name, name, is_agent: false, role: "" });
  });
  brainPeople.forEach((p, id) => push({ id: "p:" + id, type: "people", label: p.name || id, sub: p.is_agent ? T("brain.agent") : (p.role || "") }));
  (S.goals || []).forEach((g) => push({ id: "g:" + g.id, type: "goals", label: g.title || "", sub: pctOf(g) + "%", goal: g.id }));
  (S.tasks || []).forEach((t) => push({ id: "k:" + t.id, type: "tasks", label: t.title || "", sub: T("tasks." + t.status) || "", task: t.id, status: t.status, owner: ownerOf(t) }));
  (S.tasks || []).forEach((t) => (Array.isArray(t.subtasks) ? t.subtasks : []).forEach((s, i) =>
    push({ id: "s:" + t.id + ":" + i, type: "subs", label: s.title || "", sub: T("tasks." + s.status) || "", task: t.id, parent: t.title || "", status: s.status })));
  (S.decisions || []).forEach((d) => push({ id: "d:" + d.id, type: "decisions", label: d.title || "", sub: (d.decision || "").slice(0, 400), text: d.decision || "" }));
  (S.activity || []).forEach((e, i) => push({ id: "a:" + i, type: "activity", label: brainEventLabel(e),
    sub: (e.actor_name || "") + " · " + dayText(e.timestamp), actor: e.actor_name || "", actorId: e.user_id || e.actor_id || "" }));
  // Enlaces de id exacto: equipo con sus miembros, tarea con su meta y con
  // quien la lleva, subtarea con su tarea. Sin estos, la mayoría de los nodos
  // quedaba flotando sola y el grafo no enseñaba nada.
  (S.teams || []).forEach((tm) => {
    const equipo = idx.get("t:" + tm.id);
    if (!equipo) return;
    (tm.workers || []).forEach((w) => {
      const p = w && w.user && idx.get("p:" + w.user.id);
      if (p) links.push({ s: equipo, t: p });
    });
  });
  (S.tasks || []).forEach((t) => {
    const tarea = idx.get("k:" + t.id);
    if (!tarea) return;
    if (t.goal_id) {
      const meta = idx.get("g:" + t.goal_id);
      if (meta) links.push({ s: tarea, t: meta });
    }
    if (t.assigned_to) {
      const persona = idx.get("p:" + t.assigned_to);
      if (persona) links.push({ s: tarea, t: persona });
    }
  });
  nodes.forEach((n) => {
    if (n.type !== "subs") return;
    const padre = idx.get("k:" + n.task);
    if (padre) links.push({ s: n, t: padre });
  });
  // Enlaces: uno por actividad, hacia el miembro que la hizo (por id y, si no
  // vino id, por nombre). Sin actor conocido, la actividad queda suelta.
  nodes.forEach((n) => {
    if (n.type !== "activity") return;
    let target = n.actorId ? idx.get("p:" + n.actorId) : null;
    if (!target && n.actor) target = nodes.find((m) => m.type === "people" && m.label === n.actor);
    if (target) links.push({ s: n, t: target });
  });
  // Cada tipo en su aro, con el temblor sobre el radio para que el orden
  // angular se conserve: los vecinos no se cruzan aunque el aro esté lleno.
  // Si la posición choca con una ya colocada, se ensancha el aro hasta que
  // quepa, así ningún nodo nace encima de otro sin importar cuántos entren.
  const per = {};
  nodes.forEach((n) => { per[n.type] = (per[n.type] || 0) + 1; });
  const colocados = [];
  const dentro = (x, y) => x >= 18 && x <= BW - 18 && y >= 18 && y <= BH - 18;
  const libre = (x, y) => {
    for (let i = 0; i < colocados.length; i++) {
      const p = colocados[i], dx = p.x - x, dy = p.y - y;
      if (dx * dx + dy * dy < 625) return false;
    }
    return true;
  };
  const guardar = (n, x, y) => { n.x = x; n.y = y; n.hidden = BRAIN.off.has(n.type); colocados.push(n); };
  const seen = {};
  nodes.forEach((n) => {
    if (n.type === "decisions") return;
    const prev = BRAIN.pos.get(n.id);
    if (prev) { guardar(n, prev.x, prev.y); return; }
    seen[n.type] = (seen[n.type] || 0) + 1;
    const R = BRAIN_RINGS[n.type] || 200;
    const cuantos = Math.max(per[n.type], 1);
    const a = ((seen[n.type] - 0.5) / cuantos) * Math.PI * 2 + (R % 7) * 0.45;
    const paso = (Math.PI * 2 * R) / cuantos;
    const rr = brainJitter(n.id, Math.min(16, paso * 0.3));
    let elegido = null;
    for (let t = 0; t <= 24; t++) {
      const dr = t === 0 ? 0 : (t % 2 ? 1 : -1) * Math.ceil(t / 2) * 7;
      const da = t === 0 ? 0 : ((t % 3) - 1) * 0.06;
      const xx = BCX + Math.cos(a + da) * (R + rr + dr);
      const yy = BCY + Math.sin(a + da) * (R + rr + dr);
      if (!dentro(xx, yy)) continue;
      if (libre(xx, yy)) { elegido = { x: xx, y: yy }; break; }
      if (!elegido) elegido = { x: xx, y: yy };
    }
    if (!elegido) elegido = { x: BCX + Math.cos(a) * (R + rr), y: BCY + Math.sin(a) * (R + rr) };
    guardar(n, elegido.x, elegido.y);
  });
  // Las decisiones no cuelgan de nadie: se reparten por el hueco que queda,
  // probando posiciones hasta que ninguna se pise con un aro ni con otra.
  nodes.forEach((n) => {
    if (n.type !== "decisions") return;
    const prev = BRAIN.pos.get(n.id);
    if (prev) { guardar(n, prev.x, prev.y); return; }
    let elegido = null;
    for (let t = 0; t < 400 && !elegido; t++) {
      const xx = BCX + brainJitter(n.id + "x" + t, BW / 2 - 22);
      const yy = BCY + brainJitter(n.id + "y" + t, BH / 2 - 22);
      if (dentro(xx, yy) && libre(xx, yy)) elegido = { x: xx, y: yy };
    }
    if (!elegido) elegido = { x: BCX, y: BCY };
    guardar(n, elegido.x, elegido.y);
  });
  BRAIN.pos.forEach((v, k) => { if (!idx.has(k)) BRAIN.pos.delete(k); });
  BRAIN.nodes = nodes;
  BRAIN.links = links;
  BRAIN.idx = idx;
}

function paintBrain() {
  BRAIN.nodes.forEach((n) => {
    if (n.hidden) return;
    const g = BRAIN.els.get(n.id);
    if (g) g.setAttribute("transform", "translate(" + n.x.toFixed(1) + "," + n.y.toFixed(1) + ")");
    BRAIN.pos.set(n.id, { x: n.x, y: n.y });
  });
  BRAIN.links.forEach((l) => {
    if (!l.el || l.s.hidden || l.t.hidden) return;
    l.el.setAttribute("x1", l.s.x.toFixed(1)); l.el.setAttribute("y1", l.s.y.toFixed(1));
    l.el.setAttribute("x2", l.t.x.toFixed(1)); l.el.setAttribute("y2", l.t.y.toFixed(1));
  });
}

export function applyView() {
  const w = document.getElementById("brain-world");
  if (w) w.setAttribute("transform", "translate(" + BRAIN.panx.toFixed(1) + "," + BRAIN.pany.toFixed(1) + ") scale(" + BRAIN.zoom.toFixed(3) + ")");
  const svg = document.getElementById("brain-svg");
  if (svg) svg.classList.toggle("zoom-sm", BRAIN.zoom < 0.75);
}

function brainView(e) {
  const svg = document.getElementById("brain-svg");
  const r = svg.getBoundingClientRect();
  const s = Math.min(r.width / BW, r.height / BH) || 1;
  return { x: (e.clientX - r.left - (r.width - BW * s) / 2) / s, y: (e.clientY - r.top - (r.height - BH * s) / 2) / s };
}

export function brainZoom(f) {
  const z2 = Math.max(0.35, Math.min(3, BRAIN.zoom * f));
  const k = z2 / BRAIN.zoom;
  BRAIN.panx = BCX - (BCX - BRAIN.panx) * k;
  BRAIN.pany = BCY - (BCY - BRAIN.pany) * k;
  BRAIN.zoom = z2;
  applyView();
}

export function brainWheel(e) {
  e.preventDefault();
  const v = brainView(e);
  const z2 = Math.max(0.35, Math.min(3, BRAIN.zoom * (e.deltaY < 0 ? 1.15 : 0.87)));
  const wx = (v.x - BRAIN.panx) / BRAIN.zoom, wy = (v.y - BRAIN.pany) / BRAIN.zoom;
  BRAIN.zoom = z2;
  BRAIN.panx = v.x - wx * z2;
  BRAIN.pany = v.y - wy * z2;
  applyView();
}

export function brainDown(e) {
  if (e.button !== 0) return;
  const g = e.target.closest("[data-bnode]");
  if (g) BRAIN.drag = { id: g.dataset.bnode, sx: e.clientX, sy: e.clientY, moved: false };
  else BRAIN.drag = { pan: true, sx: e.clientX, sy: e.clientY, px: BRAIN.panx, py: BRAIN.pany };
  try { document.getElementById("brain-svg").setPointerCapture(e.pointerId); } catch { /* sin captura: igual arrastra dentro del lienzo */ }
}

export function brainMove(e) {
  const d = BRAIN.drag;
  if (!d) return;
  if (d.pan) {
    BRAIN.panx = d.px + (e.clientX - d.sx);
    BRAIN.pany = d.py + (e.clientY - d.sy);
    applyView();
    return;
  }
  if (!d.moved && Math.hypot(e.clientX - d.sx, e.clientY - d.sy) > 4) d.moved = true;
  if (d.moved) {
    const n = BRAIN.idx.get(d.id);
    if (!n) return;
    const v = brainView(e);
    n.x = (v.x - BRAIN.panx) / BRAIN.zoom;
    n.y = (v.y - BRAIN.pany) / BRAIN.zoom;
    BRAIN.pos.set(n.id, { x: n.x, y: n.y });
    paintBrain();
  }
}

export function brainUp() {
  const d = BRAIN.drag;
  BRAIN.drag = null;
  if (d && !d.pan && !d.moved && d.id) openBrainCard(d.id);
}

export function brainToggle(type) {
  if (BRAIN.off.has(type)) BRAIN.off.delete(type); else BRAIN.off.add(type);
  const b = document.querySelector('[data-bchip="' + type + '"]');
  if (b) b.classList.toggle("on", !BRAIN.off.has(type));
  applyFilters();
}

function applyFilters() {
  BRAIN.nodes.forEach((n) => {
    n.hidden = BRAIN.off.has(n.type);
    const g = BRAIN.els.get(n.id);
    if (g) g.style.display = n.hidden ? "none" : "";
  });
  let vn = 0, vl = 0;
  BRAIN.nodes.forEach((n) => { if (!n.hidden) vn++; });
  BRAIN.links.forEach((l) => {
    const off = l.s.hidden || l.t.hidden;
    if (l.el) l.el.style.display = off ? "none" : "";
    if (!off) vl++;
  });
  const c = document.getElementById("brain-count");
  if (c) c.textContent = vn + " " + T("brain.nodes") + " · " + vl + " " + T("brain.links");
  applyHighlight();
}

function applyHighlight() {
  const q = BRAIN.q;
  const svg = document.getElementById("brain-svg");
  if (svg) svg.classList.toggle("searching", !!q);
  BRAIN.nodes.forEach((n) => {
    const g = BRAIN.els.get(n.id);
    if (!g) return;
    const m = !q || (n.label || "").toLowerCase().includes(q);
    g.classList.toggle("dim", !!q && !m);
    g.classList.toggle("hit", !!q && m);
  });
}

export function brainSearch(e) { BRAIN.q = (e.target.value || "").trim().toLowerCase(); applyHighlight(); }

export function brainEnter() {
  const n = BRAIN.nodes.find((x) => !x.hidden && (x.label || "").toLowerCase().includes(BRAIN.q));
  if (n) openBrainCard(n.id);
}

export function closeBrainCard() {
  const card = document.getElementById("brain-card");
  if (card) card.classList.remove("on");
  if (BRAIN.sel) { const g = BRAIN.els.get(BRAIN.sel); if (g) g.classList.remove("sel"); }
  BRAIN.sel = null;
}

export function openBrainCard(id) {
  const n = BRAIN.idx.get(id);
  if (!n) return;
  if (BRAIN.sel) { const g = BRAIN.els.get(BRAIN.sel); if (g) g.classList.remove("sel"); }
  BRAIN.sel = id;
  const g = BRAIN.els.get(id);
  if (g) g.classList.add("sel");
  const pc = (st) => (st === "done" ? "ok" : st === "blocked" ? "bad" : st === "in_progress" ? "sig" : "");
  let body = "";
  if (n.type === "teams") body = "<b>" + esc(n.label) + "</b><small>" + esc(n.sub) +
    '</small><button class="btn sm" data-view="people">' + esc(T("brain.fPeople")) + "</button>";
  else if (n.type === "people") body = "<b>" + esc(n.label) + "</b>" + (n.sub ? "<small>" + esc(n.sub) + "</small>" : "");
  else if (n.type === "goals") body = "<b>" + esc(n.label) + '</b><div class="bar"><i style="width:' + pctOf(goalOf(n.goal) || {}) +
    '%"></i></div><small>' + esc(n.sub) + '</small><button class="btn sm" data-goal="' + esc(n.goal) + '">' + esc(T("brain.openGoal")) + "</button>";
  else if (n.type === "tasks") body = "<b>" + esc(n.label) + '</b><span class="pill2 ' + pc(n.status) + '">' + esc(n.sub) + "</span>" +
    (n.owner ? "<small>" + esc(T("td.owner")) + ": " + esc(n.owner) + "</small>" : "") +
    '<button class="btn sm" data-task-open="' + esc(n.task) + '">' + esc(T("brain.openTask")) + "</button>";
  else if (n.type === "subs") body = "<b>" + esc(n.label) + '</b><span class="pill2 ' + pc(n.status) + '">' + esc(n.sub) + "</span>" +
    "<small>" + esc(T("brain.kTask")) + ": " + esc(n.parent) + '</small><button class="btn sm" data-task-open="' + esc(n.task) + '">' + esc(T("brain.openTask")) + "</button>";
  else if (n.type === "decisions") body = "<b>" + esc(n.label) + "</b><p>" + esc(n.text || n.sub) + "</p>";
  else body = "<b>" + esc(n.label) + "</b><small>" + esc(n.sub) + "</small>";
  const card = document.getElementById("brain-card");
  card.innerHTML = '<button class="bc-x" data-bclose aria-label="' + esc(T("common.close")) + '">×</button>' +
    '<small class="bc-type">' + esc(T(BRAIN_TKEY[n.type])) + "</small>" + body;
  card.classList.add("on");
}

export function brainFullscreen() {
  const c = document.getElementById("brain-canvas");
  if (!c) return;
  if (document.fullscreenElement) document.exitFullscreen().catch(() => {});
  else if (c.requestFullscreen) c.requestFullscreen().catch(() => {});
}

export function renderBrain() {
  const shell = $("#brain-shell"), loading = $("#brain-loading");
  if (!S.loaded) { shell.style.display = "none"; loading.innerHTML = skeleton(); return; }
  loading.innerHTML = "";
  shell.style.display = "";
  BRAIN.drag = null;
  buildBrainData();
  const shape = (n) => {
    if (n.type === "teams") return '<rect class="bn-shape" x="-15" y="-15" width="30" height="30" rx="9"/>';
    if (n.type === "people") return '<circle class="bn-shape" r="13"/>';
    if (n.type === "goals") return '<circle class="bn-shape" r="15"/><circle class="bn-in" r="5.5"/>';
    if (n.type === "tasks") return '<circle class="bn-shape" r="9"/>';
    if (n.type === "subs") return '<circle class="bn-shape" r="6.5"/>';
    if (n.type === "decisions") return '<rect class="bn-shape" x="-9" y="-9" width="18" height="18" rx="3.5" transform="rotate(45)"/>';
    return '<circle class="bn-shape" r="7.5" stroke-dasharray="3 3"/>';
  };
  const labelY = { teams: 31, people: 29, goals: 31, tasks: 21, subs: 18, decisions: 20, activity: 20 };
  const links = BRAIN.links.map((l) => '<line class="blink"/>').join("");
  const nodes = BRAIN.nodes.map((n) =>
    '<g class="bnode b-' + n.type + '" data-bnode="' + esc(n.id) + '" tabindex="0" role="button" aria-label="' + esc(n.label) + '">' +
    '<circle class="bn-hit" r="20"/>' + shape(n) +
    '<text class="bn-label' + (BRAIN_QUIET[n.type] ? " bn-quiet" : "") + '" y="' + (labelY[n.type] || 20) + '">' + esc(trunc(n.label, 22)) + "</text></g>").join("");
  const counts = {};
  BRAIN.nodes.forEach((n) => { counts[n.type] = (counts[n.type] || 0) + 1; });
  const chip = (k, key, color) => '<button class="bchip' + (BRAIN.off.has(k) ? "" : " on") + '" data-bchip="' + k + '">' +
    '<i class="bsw" style="background:' + color + '"></i>' + esc(T(key)) + " <small>" + (counts[k] || 0) + "</small></button>";
  const emptyEl = $("#brain-empty");
  if (emptyEl) emptyEl.hidden = BRAIN.nodes.length > 0;
  $("#brain-chips").innerHTML =
    chip("teams", "brain.fTeams", "#7c6cff") + chip("people", "brain.fPeople", "#4d8dff") + chip("goals", "brain.fGoals", "#ff9d3c") +
    chip("tasks", "brain.fTasks", "#31b573") + chip("subs", "brain.fSubs", "#e7c93f") + chip("decisions", "brain.fDecisions", "#f27ba7") +
    chip("activity", "brain.fActivity", "var(--muted)") +
    '<span class="brain-help">' + esc(T("brain.help")) + "</span>";
  $("#brain-links").innerHTML = links;
  $("#brain-nodes").innerHTML = nodes;
  // Referencias para pintar cada cuadro sin reconstruir el SVG.
  BRAIN.els = new Map();
  document.querySelectorAll("#brain-nodes .bnode").forEach((g) => BRAIN.els.set(g.dataset.bnode, g));
  document.querySelectorAll("#brain-links .blink").forEach((el, i) => { if (BRAIN.links[i]) BRAIN.links[i].el = el; });
  $$("#brain-chips [data-bchip]").forEach((b) => (b.onclick = () => brainToggle(b.dataset.bchip)));
  closeBrainCard();
  applyFilters();
  applyView();
  // Sin el bucle de la simulación, nadie coloca los nodos: hay que pintarlos
  // acá con su posición final.
  paintBrain();
}

