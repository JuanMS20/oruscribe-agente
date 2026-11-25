import test from "node:test";
import assert from "node:assert/strict";
import {
  clasificarCarga, esEstadoSesionTerminada, esTokenCompatible, plazoInicioSesion,
  slugActualOrganizacion, opcionesOrganizacion, estadoOrganizacion, firmaOrganizacion,
  clasificarSondeoLogin, estadoToggle, siguienteToggle,
} from "../dist/js/estados.mjs";

test("solo logged_out es terminal; failed sigue siendo reconexión", () => {
  assert.equal(esEstadoSesionTerminada("logged_out"), true);
  assert.equal(esEstadoSesionTerminada("failed"), false);
  assert.equal(esEstadoSesionTerminada("connecting"), false);
  assert.equal(esEstadoSesionTerminada("ready"), false);
  assert.equal(esEstadoSesionTerminada(null), false);
});

test("un estado logged_out gana sobre cargas parciales y límite de cuota", () => {
  const resultado = clasificarCarga({
    state: { status: "logged_out", error: "revocado" },
    llegaron: 1,
    total: 5,
    rateLimited: true,
  });
  assert.equal(resultado, "session");
});

test("failed mantiene la reconexión y ofrece la salida a login", () => {
  assert.equal(clasificarCarga({ state: { status: "failed" }, llegaron: 4, total: 5 }), "reconnecting");
  assert.equal(clasificarCarga({ state: { status: "failed", error: "red" }, llegaron: 5, total: 5 }), "reconnecting");
  assert.equal(clasificarCarga({ state: { status: "failed" }, llegaron: 1, total: 5, rateLimited: {} }), "rateLimited");
});

test("el límite de cuota no se disfraza de sesión caída", () => {
  const resultado = clasificarCarga({
    state: { status: "ready" },
    llegaron: 2,
    total: 5,
    rateLimited: new Error("429"),
  });
  assert.equal(resultado, "rateLimited");
});

test("sin estado y con lecturas incompletas sigue siendo reconexión", () => {
  assert.equal(clasificarCarga({ state: null, llegaron: 4, total: 5 }), "offline");
  assert.equal(clasificarCarga({ state: { status: "connecting" }, llegaron: 5, total: 5 }), "offline");
});

test("solo ready con todas las lecturas cierra la reconexión", () => {
  assert.equal(clasificarCarga({ state: { status: "ready" }, llegaron: 5, total: 5 }), "ready");
  assert.equal(clasificarCarga({ state: { status: "ready" }, llegaron: 4, total: 5 }), "offline");
});

test("el sondeo de login solo es terminal en logged_out", () => {
  assert.equal(clasificarSondeoLogin(null), "wait");
  assert.equal(clasificarSondeoLogin({ status: "connecting" }), "wait");
  assert.equal(clasificarSondeoLogin({ status: "failed", error: "red" }), "wait");
  assert.equal(clasificarSondeoLogin({ status: "ready" }), "ok");
  assert.equal(clasificarSondeoLogin({ status: "logged_out" }), "fail");
});

test("un ajuste desconocido no se invierte", () => {
  assert.equal(estadoToggle(true), "on");
  assert.equal(estadoToggle(false), "off");
  assert.equal(estadoToggle(undefined), "unknown");
  assert.equal(estadoToggle(null), "unknown");
  assert.equal(siguienteToggle(true), false);
  assert.equal(siguienteToggle(false), true);
  assert.equal(siguienteToggle(undefined), null);
  assert.equal(siguienteToggle(null), null);
});

test("el selector no inventa opciones cuando el servidor no manda la lista", () => {
  const me = { organization: { slug: "a", name: "A" } };
  assert.equal(slugActualOrganizacion(me), "a");
  assert.deepEqual(opcionesOrganizacion(me), [{ slug: "a", name: "A", current: true }]);
  assert.equal(estadoOrganizacion(me), "sin-lista");
  assert.equal(estadoOrganizacion(null), "sin-sesion");
});

test("las opciones descartan slug actual, inválidos y repetidos", () => {
  const me = {
    organization: { slug: "a", name: "A" },
    other_organizations: [
      { slug: "b", name: "B" }, { slug: "a", name: "A otra" },
      null, { name: "sin slug" }, { slug: "b", name: "B repetida" }, { slug: "c" },
    ],
  };
  assert.deepEqual(opcionesOrganizacion(me).map((o) => o.slug), ["a", "b", "c"]);
  assert.equal(estadoOrganizacion(me), "con-opciones");
  assert.equal(estadoOrganizacion({ organization: { slug: "a" }, other_organizations: [] }), "sin-opciones");
});

test("la firma del selector es estable entre repintados y cambia con los datos", () => {
  const me = { organization: { slug: "a", name: "A" }, other_organizations: [{ slug: "b", name: "B" }] };
  const firma = firmaOrganizacion(me, "Actual");
  assert.equal(firma, firmaOrganizacion({ ...me }, "Actual"));
  assert.notEqual(firma, firmaOrganizacion(me, "Current"));
  assert.notEqual(firma, firmaOrganizacion({ ...me, organization: { slug: "c", name: "C" } }, "Actual"));
  assert.notEqual(firma, firmaOrganizacion({ ...me, other_organizations: [] }, "Actual"));
});

test("el plazo del cliente cubre dos peticiones del servidor y margen", () => {
  const servidor = 30000 * 2;
  const cliente = plazoInicioSesion(30000, 2, 30000);
  assert.equal(cliente, 90000);
  assert.ok(cliente > servidor);
  assert.ok(plazoInicioSesion(30000, 3, 30000) > cliente);
});

test("un plazo inválido no se convierte en espera infinita", () => {
  assert.equal(plazoInicioSesion(0, 2, 30000), 0);
  assert.equal(plazoInicioSesion(30000, 0, 30000), 0);
  assert.equal(plazoInicioSesion(NaN, 2, 30000), 0);
  assert.equal(plazoInicioSesion(30000, 2, -1), 0);
});

test("un token opaco de la API se rechaza antes de enviarse", () => {
  assert.equal(esTokenCompatible("syn_abc123"), false);
  assert.equal(esTokenCompatible("oru_abc"), false);
  assert.equal(esTokenCompatible(""), false);
  assert.equal(esTokenCompatible(null), false);
});

test("un JWT con tres partes no vacías es compatible", () => {
  assert.equal(esTokenCompatible("eyJhbGciOiJSUzI1NiJ9.eyJzdWIiOiIxIn0.c2ln"), true);
  assert.equal(esTokenCompatible("  eyJ.eyJ.c2ln  "), true);
});

test("un JWT partido, con huecos o con control se rechaza", () => {
  assert.equal(esTokenCompatible("eyJ.eyJ"), false);
  assert.equal(esTokenCompatible("eyJ..c2ln"), false);
  assert.equal(esTokenCompatible("eyJ.eyJ.c2ln.extra"), false);
  assert.equal(esTokenCompatible("eyJ.eyJ.c2 ln"), false);
  assert.equal(esTokenCompatible("eyJ.eyJ.c2\nsln"), false);
});
