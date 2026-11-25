import test from "node:test";
import assert from "node:assert/strict";

const fakeEl = () => ({
  value: "", textContent: "", innerHTML: "", disabled: false, style: {}, dataset: {},
  scrollHeight: 0, scrollTop: 0, clientHeight: 0,
  classList: { add() {}, remove() {}, toggle() {}, contains() { return false; } },
  addEventListener() {}, dispatchEvent() {}, focus() {}, setAttribute() {}, removeAttribute() {},
  querySelector() { return fakeEl(); }, select() {}, setSelectionRange() {}, appendChild() {}, closest() { return null; },
});
const elements = new Map();
globalThis.document = {
  documentElement: fakeEl(), body: fakeEl(), hidden: false,
  querySelector(s) { if (!elements.has(s)) elements.set(s, fakeEl()); return elements.get(s); },
  querySelectorAll() { return []; }, getElementById(s) { return this.querySelector("#" + s); },
  createElement() { return fakeEl(); }, addEventListener() {},
};
globalThis.window = {
  addEventListener() {}, matchMedia() { return { matches: false }; }, open() {}, confirm() { return true; },
};
globalThis.localStorage = { getItem: () => null, setItem() {}, removeItem() {} };
Object.defineProperty(globalThis, "navigator", { value: { mediaDevices: {} }, configurable: true });
globalThis.MutationObserver = class { observe() {} disconnect() {} takeRecords() { return []; } };

const setIntervalReal = globalThis.setInterval;
let pollModule = null;
globalThis.setInterval = (...args) => { pollModule = setIntervalReal(...args); return pollModule; };
const { S, R } = await import("../dist/js/state.js");
const { persistNote, refreshChat } = await import("../dist/js/views/team.js");
globalThis.setInterval = setIntervalReal;
if (pollModule !== null) clearInterval(pollModule);

function defer() {
  let resolve;
  const promise = new Promise((res) => { resolve = res; });
  return { promise, resolve };
}

test("el autosave vacío borra el contenido y una respuesta vieja no revive la nota", async () => {
  const original = { id: "n1", title: "Antes", body: "Texto" };
  S.notes = [{ ...original }];
  R.curNote = "n1";
  document.querySelector("#note-title").value = "";
  document.querySelector("#note-body").value = "";
  const espera = defer();
  const enviado = [];
  globalThis.fetch = async (url, opts) => {
    enviado.push(JSON.parse(opts.body));
    await espera.promise;
    return { ok: true, status: 200, text: async () => JSON.stringify({ id: "n1", title: "", body: "", updated_at: "x" }) };
  };
  const promesa = persistNote();
  R.meGeneracion += 1;
  espera.resolve();
  await promesa;
  assert.equal(enviado.length, 1);
  assert.deepEqual([enviado[0].title, enviado[0].body], ["", ""]);
  assert.deepEqual(S.notes, [original]);
});

test("refreshChat no guarda en la sala nueva los mensajes de la sala anterior", async () => {
  S.view = "team";
  S.room = "A";
  S.rooms = [{ room_id: "A" }, { room_id: "B" }];
  S.messages.clear();
  S.msgSig.clear();
  const espera = defer();
  globalThis.fetch = async (url) => {
    if (url === "/api/rooms") {
      return { ok: true, status: 200, text: async () => JSON.stringify({ rooms: S.rooms }) };
    }
    if (String(url).startsWith("/api/messages?room=A")) {
      await espera.promise;
      return { ok: true, status: 200, text: async () => JSON.stringify({ messages: [{ id: "m1", body: "de A" }], has_more: false }) };
    }
    return { ok: true, status: 200, text: async () => "{}" };
  };
  const promesa = refreshChat();
  S.room = "B";
  espera.resolve();
  await promesa;
  assert.equal(S.messages.has("A"), false);
  assert.equal(S.messages.has("B"), false);
});
