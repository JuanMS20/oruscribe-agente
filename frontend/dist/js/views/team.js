// Chat del equipo (salas, directos, adjuntos, reacciones, sondeo) y Notas locales.
import { T, getLang } from "../i18n.js";
import { $, $$, esc, safeUrl, showEl, md, colorFor, initials, avatarHTML, attachmentHTML, ATTACH_TEXT, attachmentType, toast, fold } from "../dom.js";
import { api, API_TIMEOUT_MS, FILE_UPLOAD_TIMEOUT_MS, paginaDe } from "../api.js";
import { S, R, counts, avatarUrl, faceInner } from "../state.js";
import { fmtDate, emptyState, skeleton, badge, paintBadges } from "./work.js";

export function setSaveTimer(t) { saveTimer = t; }
export function getSaveTimer() { return saveTimer; }

/* ---------- chat ---------- */
export function renderRooms() {
  if (!S.loaded) { $("#room-list").innerHTML = skeleton(); renderRoomPane(); return; }
  const groups = [
    ["team", T("chat.teams")],
    ["room", T("chat.rooms")],
    ["direct", T("chat.directs")],
  ];
  let html = "";
  for (const [kind, label] of groups) {
    const items = S.rooms.filter((r) => r.kind === kind);
    if (!items.length) continue;
    html += '<p class="rooms-sec"><span>' + esc(label) + "</span><b>" + items.length + "</b></p>";
    html += items.map((r) => {
      const n = (r.unread_count || 0) + (r.mention_count || 0);
      const rurl = kind === "direct" ? avatarUrl(r.with) : "";
      const face = kind === "direct"
        ? '<span class="rav" style="background:' + colorFor(r.name) + '">' + esc(initials(r.name)) +
          (rurl ? '<img data-face src="' + esc(rurl) + '" alt="' + esc(r.name) + '">' : '<svg class="i"><use href="#i-user"/></svg>') + "</span>"
        : '<span class="rav"><svg class="i"><use href="#i-hash"/></svg></span>';
      return (
        '<button class="room' + (r.room_id === S.room ? " on" : "") + '" data-room="' + esc(r.room_id) + '">' +
        face +
        '<span class="rt"><b>' + esc(r.name) + (r.with && r.with.is_agent ? ' <span class="tag">' + esc(T("agent.tag")) + "</span>" : "") + "</b>" +
        "<small>" + esc(roomPreview(r) || T("chat.noMessages")) + "</small></span>" +
        '<span class="rmeta">' + (n ? '<span class="count">' + n + "</span>" : "") + "</span></button>"
      );
    }).join("");
  }
  if (S.roomsPage && (S.roomsPage.has_more || S.roomsPage.truncated)) {
    html += '<div class="fine">' + esc(T("tasks.moreNote")) +
      (S.roomsPage.next_offset > 0 ? ' <button class="btn sm" id="rooms-more">' + esc(T("chat.moreRooms")) + "</button>" : "") + "</div>";
  }
  $("#room-list").innerHTML = html || '<p class="notes-none">' + esc(T("chat.emptyList")) + "</p>";
  renderRoomPane();
}

export async function cargarMasSalas() {
  if (!S.roomsPage || !S.roomsPage.has_more || S.roomsPage.next_offset <= 0) return;
  const pagina = await api("/api/rooms?offset=" + encodeURIComponent(S.roomsPage.next_offset)).catch(() => null);
  if (!pagina) { toast(T("load.failed")); return; }
  const vistos = new Set((S.rooms || []).map((r) => r.room_id));
  (pagina.rooms || []).forEach((r) => { if (!vistos.has(r.room_id)) S.rooms.push(r); });
  S.roomsPage = paginaDe(pagina);
  renderRooms();
}

export function roomSub(r) {
  if (r.kind === "direct") return r.with && r.with.is_agent ? T("agent.tag") : T("chat.directs");
  if (r.kind === "team") return T("chat.teams");
  return T("chat.rooms");
}

export function renderRoomPane() {
  const room = S.rooms.find((r) => r.room_id === S.room);
  const head = $("#v-team .room-head");
  const comp = $("#v-team .room-comp");
  if (head) {
    head.style.display = room ? "flex" : "none";
    head.dataset.kind = room ? room.kind : "";
  }
  if (comp) comp.style.display = room ? "flex" : "none";
  if (!room) {
    $("#room-title").textContent = "";
    $("#room-sub").textContent = "";
    $("#room-msgs").innerHTML = '<div style="padding-top:8vh">' + emptyState("i-at", T("chat.pick"), T("chat.pickSub")) + "</div>";
    return;
  }
  $("#room-title").textContent = room.name;
  $("#room-sub").textContent = roomSub(room);
}

export function roomPreview(r) {
  if (!r.last_message_at) return "";
  const d = new Date(r.last_message_at);
  return d.toLocaleString(getLang() === "en" ? "en-GB" : "es-ES", { day: "numeric", month: "short", hour: "2-digit", minute: "2-digit" });
}

// Markdown seguro: primero se escapa todo y después se le da formato, así que
// el texto del servidor nunca puede meter etiquetas propias. Los bloques de
// código se apartan antes para que su contenido no se transforme; los
// encabezados van en negrita para no romper la maqueta; solo los enlaces
// https se vuelven enlaces de verdad.

export async function openAttachment(url, nombre, mime) {
  const safe = safeUrl(url);
  const descarga = $("#attach-download");
  showEl(descarga, false);
  if (!safe) return toast(T("attach.fail"));
  $("#attach-title").textContent = nombre || T("attach.title");
  $("#attach-kind").textContent = attachmentType(mime) + (mime ? " · " + mime : "");
  descarga.href = safe;
  descarga.download = nombre || "";
  showEl(descarga, true);
  $("#attach-msg").textContent = "";
  const body = $("#attach-body");
  body.textContent = "";
  if (String(mime || "").startsWith("image/")) {
    // La pista se ve mientras carga y se queda si la imagen no llega; la
    // descarga sigue disponible en el pie del visor.
    $("#attach-msg").textContent = T("attach.previewHint");
    const img = document.createElement("img");
    img.onload = () => {
      if ($("#attach-msg").textContent === T("attach.previewHint")) $("#attach-msg").textContent = "";
    };
    img.onerror = () => { $("#attach-msg").textContent = T("attach.fail"); };
    img.src = safe;
    img.alt = nombre || "";
    body.appendChild(img);
  } else if (mime === "application/pdf") {
    // Un iframe no siempre avisa cuando el PDF no carga: la pista queda a la
    // vista y la descarga sigue disponible.
    $("#attach-msg").textContent = T("attach.previewHint");
    const frame = document.createElement("iframe");
    frame.src = safe;
    frame.title = nombre || "PDF";
    body.appendChild(frame);
  } else if (ATTACH_TEXT.test(mime || "")) {
    const pre = document.createElement("pre");
    pre.textContent = T("common.loading");
    body.appendChild(pre);
    // Sin plazo, un servidor de adjuntos colgado dejaba «Cargando…» para
    // siempre; al vencer se muestra el error y la descarga sigue disponible.
    const control = new AbortController();
    const plazo = setTimeout(() => control.abort(), API_TIMEOUT_MS);
    try {
      let res;
      try {
        res = await fetch(safe, { signal: control.signal });
      } catch {
        throw new Error(T("load.failed"));
      }
      if (!res.ok) throw new Error(String(res.status));
      const texto = (await res.text()).slice(0, 512 * 1024);
      pre.textContent = texto;
    } catch {
      pre.textContent = "";
      $("#attach-msg").textContent = T("attach.fail");
    } finally {
      clearTimeout(plazo);
    }
  } else {
    const a = document.createElement("a");
    a.className = "btn sm";
    a.href = safe;
    a.target = "_blank";
    a.rel = "noreferrer";
    a.textContent = T("attach.download");
    body.appendChild(a);
  }
  $("#attach-view").classList.add("on");
}

export function renderMessages() {
  const list = S.messages.get(S.room) || [];
  const msgs = $("#room-msgs");
  // Refreshing a room must not throw a reader back to the bottom of a
  // conversation they were scrolling through.
  const atBottom = msgs.scrollHeight - msgs.scrollTop - msgs.clientHeight < 48;
  const keep = msgs.scrollTop;
  const aviso = S.msgsIncompletos.has(S.room)
    ? '<p class="fine" style="text-align:center;padding:8px">' + esc(T("chat.partial")) + "</p>"
    : "";
  if (!list.length) { msgs.innerHTML = aviso + '<p class="fine" style="text-align:center;padding:24px">' + esc(T("chat.empty")) + "</p>"; return; }
  let lastDay = "";
  const authorKey = (m) => m && ((m.author && (m.author.id || m.author.name)) || (m.is_mine ? "me" : ""));
  const html = list.map((m, i) => {
    const d = new Date(m.created_at);
    const day = d.toDateString();
    let head = "";
    if (day !== lastDay) {
      lastDay = day;
      head = '<div class="day"><span>' + esc(dayLabel(d)) + "</span></div>";
    }
    const prev = i > 0 ? list[i - 1] : null;
    const prevDate = prev ? new Date(prev.created_at) : null;
    // Un mismo autor seguido se lee como un solo turno: se oculta su avatar y
    // su nombre, no su texto. Las respuestas siempre abren turno nuevo.
    const cont = !!prev && !!prevDate && day === prevDate.toDateString()
      && authorKey(prev) === authorKey(m) && !m.reply_to_id
      && d.getTime() - prevDate.getTime() < 5 * 60 * 1000;
    const mine = m.is_mine;
    const who = m.author ? m.author.name : "";
    const time = d.toLocaleTimeString(getLang() === "en" ? "en-GB" : "es-ES", { hour: "2-digit", minute: "2-digit" });
    // Una imagen se ve como imagen; lo demás queda como su chip de descarga.
    // La dirección pasa por safeUrl: si el servidor manda algo que no es una
    // dirección segura, la imagen no se pinta.
    const files = (m.attachments || []).map(attachmentHTML).join(" ");
    const parent = m.reply_to_id ? list.find((x) => x.id === m.reply_to_id) : null;
    const quote = parent
      ? '<div class="q"><b>' + esc(parent.author ? parent.author.name : "") + "</b> " + esc((parent.body || "").slice(0, 90)) + "</div>"
      : m.reply_to_id
        ? '<div class="q">' + esc(T("chat.reply")) + "</div>"
        : "";
    const acts = '<div class="acts">' + ["👍", "❤️", "😂", "😮", "😢", "🙏"].map((em) => '<button class="mini" data-react="' + esc(m.id) + '" data-emoji="' + esc(em) + '" title="' + esc(em) + '">' + esc(em) + "</button>").join("") + '<button class="mini" data-reply="' + esc(m.id) + '">' + esc(T("chat.reply")) + "</button></div>";
    const seen = (m.reactions || []).filter((r) => r && (r.emoji || r.count)).map((r) => '<span class="chip">' + esc(r.emoji || "") + (r.count > 1 ? " " + esc(String(r.count)) : "") + "</span>").join(" ");
    const label = mine ? esc(T("chat.you")) : esc(who);
    const role = !mine && m.author && m.author.is_agent ? ' <span class="tag">' + esc(T("agent.tag")) + "</span>" : "";
    return head + '<div class="rm' + (mine ? " me" : "") + (cont ? " cont" : "") + '">' + avatarHTML(m.author) +
      '<div class="bb"><div class="who"><b>' + label + "</b>" + role + "<small>" + esc(time) + "</small></div>" + acts + quote +
      '<div class="tx">' + md(m.body) + "</div>" + (files ? '<div class="reacts">' + files + "</div>" : "") + (seen ? '<div class="reacts">' + seen + "</div>" : "") + "</div></div>";
  });
  msgs.innerHTML = aviso + html.join("");
  msgs.scrollTop = atBottom ? msgs.scrollHeight : keep;
}

export function dayLabel(d) {
  const today = new Date();
  if (d.toDateString() === today.toDateString()) return T("chat.today");
  const y = new Date(); y.setDate(y.getDate() - 1);
  if (d.toDateString() === y.toDateString()) return T("chat.yesterday");
  return d.toLocaleDateString(getLang() === "en" ? "en-GB" : "es-ES", { weekday: "short", day: "numeric", month: "short" });
}

// Una firma barata de la conversación: si no cambió nada, no se vuelve a pintar
// y la sala no parpadea en cada repaso.
export function msgSignature(list) {
  // El texto completo, no su largo: una edición que mantiene el tamaño y un
  // cambio de autor o de respuesta deben repintar. Mirar solo el id hacía que
  // un mensaje editado se viera viejo hasta recargar a mano.
  return (list || []).map((m) => [
    m.id, m.body || "", m.is_mine ? "1" : "0", m.reply_to_id || "",
    (m.author && (m.author.id || m.author.name)) || "",
    // La foto del autor también entra: si un contacto cambia su avatar, la
    // firma cambia y el mensaje se repinta con la imagen nueva.
    (m.author && (m.author.avatar_url || m.author.photo_url || m.author.picture || m.author.avatar)) || "",
    (m.reactions || []).map((r) => (r.emoji || "") + "=" + (r.count || 0)).join(","),
    (m.attachments || []).map((f) => f.id || f.file_name || "").join(","),
  ].join("\u0001")).join("\u0002");
}

export function setReplyBar(visible) {
  const bar = $("#room-reply");
  if (bar) bar.style.display = visible ? "flex" : "none";
  document.body.classList.toggle("replying", visible);
}

const MSG_PAGES_MAX = 20;

// shortcut: 20 páginas (1000 mensajes), cargar por tramos bajo demanda si una sala pasa de ahí
export async function cargarMensajes(room) {
  const paginas = [];
  S.msgsIncompletos.delete(room);
  let offset = 0;
  for (let n = 0; n < MSG_PAGES_MAX; n++) {
    let data;
    try {
      data = await api("/api/messages?room=" + encodeURIComponent(room) + "&offset=" + offset);
    } catch (err) {
      // Si ya hay páginas, se conservan y se avisa; perder todo lo cargado por
      // una página que falló obliga a empezar de cero.
      if (!paginas.length) throw err;
      S.msgsIncompletos.add(room);
      break;
    }
    const page = (data && data.messages) || [];
    if (page.length) paginas.push(page);
    if (!data || !data.has_more || !page.length) break;
    const next = Number(data.next_offset);
    if (!Number.isFinite(next) || next <= offset) break;
    offset = next;
    if (n === MSG_PAGES_MAX - 1) S.msgsIncompletos.add(room);
  }
  // El servidor entrega las páginas de la más nueva a la más vieja y cada
  // página ya viene en orden de lectura: se invierte el orden de las páginas.
  return paginas.reverse().flat();
}

export async function openRoom(id, marcarLeido = true) {
  R.replyTarget = null;
  setReplyBar(false);
  S.room = id;
  // Con 900 px o menos el panel de la sala vive oculto hasta que la vista lleva
  // `open` (app.css:892-895). Sin esto, elegir una sala cargaba los mensajes y
  // dejaba a la persona en la lista; el botón de volver está dentro del panel.
  $("#v-team").classList.add("open");
  const room = S.rooms.find((r) => r.room_id === id);
  $("#room-title").textContent = room ? room.name : "—";
  $("#room-sub").textContent = room ? roomSub(room) : "";
  renderRooms();
  $("#room-msgs").innerHTML = '<p class="fine" style="text-align:center;padding:24px">' + esc(T("chat.loading")) + "</p>";
  try {
    const list = await cargarMensajes(id);
    S.messages.set(id, list);
    S.msgSig.set(id, msgSignature(list));
    renderMessages();
    if (marcarLeido && room && ((room.unread_count || 0) + (room.mention_count || 0)) > 0) {
      try {
        await api("/api/rooms/read", { method: "POST", body: JSON.stringify({ room: id }) });
        room.unread_count = 0; room.mention_count = 0;
        S.liveChat = null;
        renderRooms(); badge("#c-chat", counts().chat);
      } catch {
        // El POST falló: la sala no quedó leída en el servidor y los
        // contadores conservan la cifra real en vez de bajar a cero.
      }
    }
  } catch (err) {
    $("#room-msgs").innerHTML = '<p class="fine" style="text-align:center;padding:24px">' + esc(err.message) + "</p>";
  }
}

export async function sendMessage() {
  const input = $("#room-input");
  const body = input.value.trim();
  if (!body || !S.room) return;
  const room = S.room;
  const replyTo = R.replyTarget;
  input.value = "";
  $("#room-send").disabled = true;
  try {
    await api("/api/messages", { method: "POST", body: JSON.stringify({ room, body, replyTo: replyTo || "" }) });
  } catch (err) {
    // El POST falló: el mensaje no salió, se devuelve el borrador.
    if (S.room === room) {
      if (!input.value) input.value = body;
      input.dispatchEvent(new Event("input", { bubbles: true }));
    }
    toast(err.message);
    return;
  }
  if (S.room === room && R.replyTarget === replyTo) {
    R.replyTarget = null;
    setReplyBar(false);
  }
  // El mensaje ya está en el servidor. Si falla la recarga no se devuelve el
  // borrador: eso invitaba a mandarlo otra vez y quedaba duplicado.
  try {
    const list = await cargarMensajes(room);
    S.messages.set(room, list);
    S.msgSig.set(room, msgSignature(list));
    if (S.room === room) renderMessages();
  } catch (err) {
    toast(err.message);
  }
}

export async function attachChatFile(file) {
  if (!file || !S.room) return;
  // La sala se captura al inicio, como sendMessage: si cambias de sala
  // durante la subida, el adjunto es de la original y esa se recarga.
  const room = S.room;
  const replyTo = R.replyTarget;
  const input = $("#room-input");
  const body = input.value.trim();
  const fd = new FormData();
  fd.append("room", room);
  fd.append("body", body);
  fd.append("file", file);
  input.value = "";
  $("#room-send").disabled = true;
  toast(T("chat.uploading"));
  const control = new AbortController();
  const plazo = setTimeout(() => control.abort(), FILE_UPLOAD_TIMEOUT_MS);
  try {
    let res;
    try {
      res = await fetch("/api/chat/files", { method: "POST", body: fd, signal: control.signal });
    } catch {
      throw new Error(T("load.failed"));
    }
    if (!res.ok) {
      let msg = res.statusText;
      try { msg = (await res.json()).error || msg; } catch { /* sin JSON */ }
      throw new Error(msg);
    }
    if (S.room === room && R.replyTarget === replyTo) {
      R.replyTarget = null;
      setReplyBar(false);
    }
    try {
      const list = await cargarMensajes(room);
      S.messages.set(room, list);
      S.msgSig.set(room, msgSignature(list));
      if (S.room === room) renderMessages();
    } catch (err) {
      toast(err.message);
    }
  } catch (err) {
    // Solo se devuelve el borrador si el campo sigue vacio: escribir mientras
    // espera no puede perder lo nuevo.
    if (S.room === room) {
      if (!input.value) input.value = body;
      input.dispatchEvent(new Event("input", { bubbles: true }));
    }
    toast(err.message);
  } finally {
    clearTimeout(plazo);
  }
}

/* ---------- notes ---------- */
let saveTimer = null, notesLoading = false, notaVersion = 0;
export function invalidarNota() { notaVersion += 1; }



export function renderNotes() {
  if (notesLoading) { $("#notes-items").innerHTML = skeleton(); return; }
  const q = fold($("#note-q") ? $("#note-q").value.trim() : "");
  const list = [...S.notes].sort((a, b) => (b.updated_at || "").localeCompare(a.updated_at || ""));
  const vis = list.filter((n) => !q || fold(n.title).includes(q) || fold(n.body).includes(q));
  $("#notes-items").innerHTML = vis.length
    ? vis.map((n) => '<button class="ncard' + (n.id === R.curNote ? " on" : "") + '" data-note="' + esc(n.id) + '"><b>' + esc(n.title || T("notes.untitled")) + "</b>" +
        (n.body && n.body.trim() ? "<span>" + esc(n.body.trim().slice(0, 90)) + "</span>" : "") + "<small>" + esc(noteDate(n.updated_at)) + "</small></button>").join("")
    : '<div class="notes-none">' + esc(S.notes.length ? T("notes.noMatch") : T("notes.none")) + "</div>";
  $("#note-ed").classList.toggle("blank", !R.curNote);
}

export function noteDate(s) {
  const d = new Date(s);
  if (Number.isNaN(d.getTime())) return "";
  return d.toLocaleString(getLang() === "en" ? "en-GB" : "es-ES", { day: "numeric", month: "short", hour: "2-digit", minute: "2-digit" });
}

export function openNote(id) {
  R.curNote = id;
  const n = S.notes.find((x) => x.id === id);
  $("#note-title").value = n ? n.title : "";
  $("#note-body").value = n ? n.body : "";
  $("#note-saved").textContent = T("notes.saved");
  $("#note-confirm").classList.remove("on");
  $("#v-notes").classList.add("open");
  renderNotes();
}

export async function persistNote() {
  if (!R.curNote) return;
  const id = R.curNote;
  const title = $("#note-title").value;
  const body = $("#note-body").value;
  const existing = S.notes.find((n) => n.id === id);
  if ((!existing && !title.trim() && !body.trim()) ||
      (existing && existing.title === title && existing.body === body)) {
    $("#note-saved").textContent = T("notes.saved");
    return;
  }
  const generacion = R.meGeneracion;
  const version = ++notaVersion;
  try {
    const saved = await api("/api/notes", { method: "POST", body: JSON.stringify({ id, title, body }) });
    if (generacion !== R.meGeneracion || version !== notaVersion) return;
    const i = S.notes.findIndex((n) => n.id === saved.id);
    if (i >= 0) S.notes[i] = saved; else S.notes.push(saved);
    $("#note-saved").textContent = T("notes.saved");
    renderNotes();
  } catch (err) {
    if (generacion !== R.meGeneracion || version !== notaVersion) return;
    $("#note-saved").textContent = err.message;
  }
}

export async function loadNotes() {
  const generacion = R.meGeneracion;
  notesLoading = true;
  renderNotes();
  try {
    const data = await api("/api/notes");
    if (generacion !== R.meGeneracion) return;
    S.notes = data.notes || [];
  } catch {
    if (generacion !== R.meGeneracion) return;
    $("#notes-items").innerHTML = '<div class="notes-none">' + esc(T("notes.loadFail")) +
      ' <button class="btn sm" data-notes-retry>' + esc(T("common.retry")) + "</button></div>";
    return;
  } finally {
    notesLoading = false;
  }
  renderNotes();
}


const CHAT_POLL_MS = 900000;
let chatPoll = null;
export async function refreshChat() {
  // La guarda va antes del /api/rooms: esa ruta cuesta 10-20 consultas en el
  // servidor, y enfocar estando en otra vista no debe pagarla.
  if (S.view !== "team" || !S.room) return;
  const room = S.room;
  const generacion = R.meGeneracion;
  const rooms = await api("/api/rooms").catch(() => null);
  if (generacion !== R.meGeneracion) return;
  if (rooms) {
    S.rooms = rooms.rooms || [];
    S.liveChat = null;
    renderRooms();
    // La lista de salas trae sus contadores; el badge de la barra se quedaría
    // viejo hasta el próximo refresco completo si no se actualiza aquí.
    badge("#c-chat", counts().chat);
  }
  const list = await cargarMensajes(room).catch(() => null);
  if (!list) return;
  if (generacion !== R.meGeneracion || S.room !== room) return;
  const sig = msgSignature(list);
  if (sig === S.msgSig.get(room)) return;
  S.msgSig.set(room, sig);
  S.messages.set(room, list);
  renderMessages();
}
export function startChatPoll() {
  if (chatPoll) return;
  // Con el timbre conectado (R.connOn), los avisos llegan en vivo y no se
  // sondea el servidor; el respaldo de 15 min solo corre si el timbre cayó.
  chatPoll = setInterval(() => { if (!document.hidden && !R.connOn) refreshChat(); }, CHAT_POLL_MS);
}
export function stopChatPoll() {
  if (!chatPoll) return;
  clearInterval(chatPoll);
  chatPoll = null;
}
const CHAT_FOCO_MS = 180000;
let ultimoChatFoco = 0;
export function refreshChatAlEnfocar() {
  if (!chatPoll || R.connOn) return;
  const ahora = Date.now();
  if (ahora - ultimoChatFoco < CHAT_FOCO_MS) return;
  ultimoChatFoco = ahora;
  refreshChat();
}
window.addEventListener("focus", refreshChatAlEnfocar);
document.addEventListener("visibilitychange", () => { if (!document.hidden) refreshChatAlEnfocar(); });

