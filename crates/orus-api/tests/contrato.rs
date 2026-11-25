//! Contrato HTTP inmune: una comprobación por cada ruta que registra el
//! router, más una guarda que falla si una ruta queda sin caso.
//!
//! Port de `internal/app/immune_contract_test.go:190-680`. Los cuerpos y las
//! aserciones son los del Go; los casos con varias comprobaciones se conservan
//! completos. Ola 7 conectó las cinco rutas de setup y update: ya no queda
//! ninguna ruta fuera de alcance.

mod soporte;

use std::collections::HashSet;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Local, Utc};
use orus_api::events::Hub;
use orus_api::{Api, GitLog, RUTAS, Setup, Update};
use orus_app::{
    App, Backend, ChatMessage, ChatReply, ChatRequest, Connector, DigestSource, Event, ProveedorFn,
    Provider, ProviderError, SessionError, Status, TaskStatus,
};
use orus_config::Settings;
use orus_install::{SetupInfo, TERMS_BODY, TERMS_TITLE, TERMS_VERSION, UpdateInfo, platform};
use orus_org::DigestItem;
use serde_json::Value;

use orus_auth::{Error as AuthError, Store, Token};
use soporte::{
    AppDePrueba, BackendDePrueba, RunnerOk, con_runtime, conversaciones_en, en_datos_aislados,
};

// ---------------------------------------------------------------------------
// Piezas de la tabla
// ---------------------------------------------------------------------------

/// El cuerpo que manda cada caso.
enum Cuerpo {
    Vacio,
    Json(&'static str),
    /// `capture/propose`: la carpeta se arma en `pedir` con un `tempdir`,
    /// porque `/tmp/repo` no existe en Windows. El `GitLogFalso` no la toca.
    Captura,
    Multipart {
        campo: &'static str,
        campos: &'static [(&'static str, &'static str)],
        archivo: (&'static str, &'static str, &'static str),
    },
}

/// Cómo se comprueba la respuesta.
enum Chequeo {
    OkClave(&'static str),
    OkClaves(&'static [&'static str]),
    Codigo(u16),
    Personalizado(fn(&BackendDePrueba, &Respuesta, &Arc<App>)),
}

struct Extra {
    method: &'static str,
    path: &'static str,
    body: Cuerpo,
    chequeo: Chequeo,
}

struct Caso {
    name: &'static str,
    method: &'static str,
    path: &'static str,
    body: Cuerpo,
    setup: Option<fn(&mut App)>,
    assert: Chequeo,
    extra: Option<Extra>,
}

fn simple(
    name: &'static str,
    method: &'static str,
    path: &'static str,
    body: Cuerpo,
    chequeo: Chequeo,
) -> Caso {
    Caso {
        name,
        method,
        path,
        body,
        setup: None,
        assert: chequeo,
        extra: None,
    }
}

/// Ya no hay comprobaciones de Go fuera de alcance: Ola 7 conectó las cinco
/// rutas de setup y update. La guarda se queda para que el día que aparezca
/// una ruta nueva sin caso, el contrato no la deje pasar en silencio.
const FUERA_DE_ALCANCE: &[(&str, &str, &str)] = &[];

fn casos() -> Vec<Caso> {
    vec![
        // ---------- lectura ----------
        simple(
            "GET /api/state",
            "GET",
            "/api/state",
            Cuerpo::Vacio,
            Chequeo::OkClave("status"),
        ),
        simple(
            "GET /api/auth/state",
            "GET",
            "/api/auth/state",
            Cuerpo::Vacio,
            Chequeo::OkClave("status"),
        ),
        simple(
            "GET /api/tasks",
            "GET",
            "/api/tasks",
            Cuerpo::Vacio,
            Chequeo::OkClave("tasks"),
        ),
        simple(
            "GET /api/tasks/search",
            "GET",
            "/api/tasks/search?q=factura&status=blocked",
            Cuerpo::Vacio,
            Chequeo::OkClave("tasks"),
        ),
        simple(
            "GET /api/tasks/files",
            "GET",
            "/api/tasks/files?task=t1",
            Cuerpo::Vacio,
            Chequeo::OkClaves(&[
                "files",
                "comments",
                "count",
                "total",
                "has_more",
                "truncated",
                "next_offset",
            ]),
        ),
        simple(
            "GET /api/rooms",
            "GET",
            "/api/rooms",
            Cuerpo::Vacio,
            Chequeo::OkClave("rooms"),
        ),
        simple(
            "GET /api/messages",
            "GET",
            "/api/messages?room=r1",
            Cuerpo::Vacio,
            Chequeo::OkClaves(&["messages", "has_more", "next_offset"]),
        ),
        simple(
            "GET /api/alerts",
            "GET",
            "/api/alerts",
            Cuerpo::Vacio,
            Chequeo::OkClaves(&["alerts", "unread"]),
        ),
        simple(
            "GET /api/mentions",
            "GET",
            "/api/mentions",
            Cuerpo::Vacio,
            Chequeo::OkClaves(&["mentions", "unread"]),
        ),
        simple(
            "GET /api/goals",
            "GET",
            "/api/goals",
            Cuerpo::Vacio,
            Chequeo::OkClave("goals"),
        ),
        simple(
            "GET /api/goals/detail",
            "GET",
            "/api/goals/detail?id=g1",
            Cuerpo::Vacio,
            Chequeo::OkClave("goal"),
        ),
        simple(
            "GET /api/periods",
            "GET",
            "/api/periods",
            Cuerpo::Vacio,
            Chequeo::OkClave("periods"),
        ),
        simple(
            "GET /api/activity",
            "GET",
            "/api/activity?days=14",
            Cuerpo::Vacio,
            Chequeo::OkClave("events"),
        ),
        simple(
            "GET /api/decisions",
            "GET",
            "/api/decisions?q=token",
            Cuerpo::Vacio,
            Chequeo::OkClave("decisions"),
        ),
        simple(
            "GET /api/team",
            "GET",
            "/api/team",
            Cuerpo::Vacio,
            Chequeo::OkClave("teams"),
        ),
        simple(
            "GET /api/settings",
            "GET",
            "/api/settings",
            Cuerpo::Vacio,
            Chequeo::Personalizado(ajustes_leidos),
        ),
        simple(
            "GET /api/setup",
            "GET",
            "/api/setup",
            Cuerpo::Vacio,
            Chequeo::Personalizado(setup_completo),
        ),
        simple(
            "GET /api/update",
            "GET",
            "/api/update",
            Cuerpo::Vacio,
            Chequeo::Personalizado(actualizacion_sin_configurar),
        ),
        simple(
            "GET /api/update/progress",
            "GET",
            "/api/update/progress",
            Cuerpo::Vacio,
            Chequeo::Personalizado(actualizacion_en_reposo),
        ),
        simple(
            "GET /api/agent",
            "GET",
            "/api/agent",
            Cuerpo::Vacio,
            Chequeo::OkClave("kind"),
        ),
        simple(
            "GET /api/agent/providers",
            "GET",
            "/api/agent/providers",
            Cuerpo::Vacio,
            Chequeo::Personalizado(proveedores),
        ),
        simple(
            "GET /api/agent/models (sin proveedor)",
            "GET",
            "/api/agent/models",
            Cuerpo::Vacio,
            Chequeo::Codigo(400),
        ),
        simple(
            "POST /api/agent/pick-folder (sin selector)",
            "POST",
            "/api/agent/pick-folder",
            Cuerpo::Vacio,
            Chequeo::Codigo(400),
        ),
        simple(
            "GET /api/agent/approvals",
            "GET",
            "/api/agent/approvals",
            Cuerpo::Vacio,
            Chequeo::Personalizado(aprobaciones),
        ),
        simple(
            "GET /api/agent/conversations",
            "GET",
            "/api/agent/conversations",
            Cuerpo::Vacio,
            Chequeo::Personalizado(conversaciones),
        ),
        simple(
            "GET /api/events",
            "GET",
            "/api/events",
            Cuerpo::Vacio,
            Chequeo::Personalizado(eventos),
        ),
        // ---------- escritura ----------
        simple(
            "POST /api/tasks",
            "POST",
            "/api/tasks",
            Cuerpo::Json(r#"{"title":"Tarea inmune","description":"desc","priority":"high"}"#),
            Chequeo::Personalizado(tarea_creada),
        ),
        simple(
            "POST /api/tasks/status",
            "POST",
            "/api/tasks/status",
            Cuerpo::Json(r#"{"id":"t1","status":"blocked","note":"espero firma"}"#),
            Chequeo::Personalizado(estado_cambiado),
        ),
        simple(
            "POST /api/tasks/files/comment",
            "POST",
            "/api/tasks/files/comment",
            Cuerpo::Json(r#"{"task":"t1","update":"u1","body":"comentario inmune"}"#),
            Chequeo::Personalizado(comentario_creado),
        ),
        simple(
            "POST /api/messages",
            "POST",
            "/api/messages",
            Cuerpo::Json(r#"{"room":"r1","body":"hola inmune"}"#),
            Chequeo::Codigo(200),
        ),
        simple(
            "POST /api/messages (directo)",
            "POST",
            "/api/messages",
            Cuerpo::Json(r#"{"to":"u2","body":"directo inmune"}"#),
            Chequeo::Personalizado(mensaje_directo),
        ),
        simple(
            "POST /api/rooms/read",
            "POST",
            "/api/rooms/read",
            Cuerpo::Json(r#"{"room":"r1"}"#),
            Chequeo::Personalizado(sala_leida),
        ),
        simple(
            "POST /api/alerts/read",
            "POST",
            "/api/alerts/read",
            Cuerpo::Json(r#"{"ids":["a1","a2"]}"#),
            Chequeo::Personalizado(alertas_leidas),
        ),
        simple(
            "POST /api/mentions/read",
            "POST",
            "/api/mentions/read",
            Cuerpo::Json(r#"{"ids":["m1","m2"]}"#),
            Chequeo::Personalizado(menciones_leidas),
        ),
        simple(
            "POST /api/goals",
            "POST",
            "/api/goals",
            Cuerpo::Json(r#"{"title":"Meta inmune","description":"d"}"#),
            Chequeo::Personalizado(meta_creada),
        ),
        simple(
            "POST /api/periods",
            "POST",
            "/api/periods",
            Cuerpo::Json(
                r#"{"name":"Periodo inmune","start_date":"2026-11-01","end_date":"2026-11-30","kind":"month"}"#,
            ),
            Chequeo::Personalizado(periodo_creado),
        ),
        simple(
            "POST /api/capture/apply",
            "POST",
            "/api/capture/apply",
            Cuerpo::Json(r#"{"digest":"d1","accept":["c1"]}"#),
            Chequeo::Personalizado(captura_aplicada),
        ),
        simple(
            "POST /api/alerts/send",
            "POST",
            "/api/alerts/send",
            Cuerpo::Json(r#"{"type":"blocker","message":"aviso inmune","team":"tm1"}"#),
            Chequeo::Personalizado(aviso_enviado),
        ),
        simple(
            "POST /api/messages/react",
            "POST",
            "/api/messages/react",
            Cuerpo::Json(r#"{"message":"m1","emoji":"👍"}"#),
            Chequeo::Personalizado(reaccion_enviada),
        ),
        simple(
            "POST /api/context",
            "POST",
            "/api/context",
            Cuerpo::Json(r#"{"focus":"foco inmune","mood":"at_risk","notes":"nota"}"#),
            Chequeo::Personalizado(contexto_actualizado),
        ),
        Caso {
            setup: None,
            assert: Chequeo::Personalizado(ajustes_cambiados),
            extra: Some(Extra {
                method: "POST",
                path: "/api/settings",
                body: Cuerpo::Json(r#"{"autostart":true}"#),
                chequeo: Chequeo::Codigo(500),
            }),
            ..simple(
                "POST /api/settings",
                "POST",
                "/api/settings",
                Cuerpo::Json(r#"{"locale":"en"}"#),
                Chequeo::Codigo(200),
            )
        },
        simple(
            "POST /api/update/apply",
            "POST",
            "/api/update/apply",
            Cuerpo::Json("{}"),
            Chequeo::Personalizado(actualizacion_sin_version),
        ),
        simple(
            "POST /api/setup/install",
            "POST",
            "/api/setup/install",
            Cuerpo::Json(r#"{"termsVersion":"otra","autostart":false}"#),
            Chequeo::Personalizado(instalacion_rechaza_terminos),
        ),
        simple(
            "POST /api/setup/uninstall (cuerpo vacío conserva los datos)",
            "POST",
            "/api/setup/uninstall",
            Cuerpo::Json("{}"),
            Chequeo::Personalizado(desinstalacion_conserva_datos),
        ),
        simple(
            "POST /api/setup/uninstall (keepData falso borra)",
            "POST",
            "/api/setup/uninstall",
            Cuerpo::Json(r#"{"keepData":false}"#),
            Chequeo::Personalizado(desinstalacion_borra_datos),
        ),
        simple(
            "POST /api/quit",
            "POST",
            "/api/quit",
            Cuerpo::Vacio,
            Chequeo::Codigo(500),
        ),
        simple(
            "POST /api/agent/config",
            "POST",
            "/api/agent/config",
            Cuerpo::Json(
                r#"{"kind":"openai","base_url":"https://ejemplo.test/v1","model":"modelo-inmune","mode":"readonly","api_key":"clave-inmune"}"#,
            ),
            Chequeo::Personalizado(agente_configurado),
        ),
        simple(
            "POST /api/agent/test",
            "POST",
            "/api/agent/test",
            Cuerpo::Json(
                r#"{"kind":"openai","base_url":"https://ejemplo.test/v1","model":"modelo-inmune","mode":"readonly","api_key":"clave-inmune"}"#,
            ),
            Chequeo::Personalizado(agente_probado),
        ),
        simple(
            "POST /api/agent/transcribe",
            "POST",
            "/api/agent/transcribe",
            Cuerpo::Multipart {
                campo: "audio",
                campos: &[],
                archivo: ("audio.webm", "audio/webm", "audio-inmune"),
            },
            Chequeo::Codigo(400),
        ),
        simple(
            "POST /api/agent/speak",
            "POST",
            "/api/agent/speak",
            Cuerpo::Json(r#"{"text":"hola"}"#),
            Chequeo::Codigo(400),
        ),
        simple(
            "POST /api/agent/send (vacío)",
            "POST",
            "/api/agent/send",
            Cuerpo::Json(r#"{"text":"   "}"#),
            Chequeo::Codigo(400),
        ),
        simple(
            "POST /api/agent/cancel",
            "POST",
            "/api/agent/cancel",
            Cuerpo::Json("{}"),
            Chequeo::Codigo(200),
        ),
        simple(
            "GET /api/agent/journal",
            "GET",
            "/api/agent/journal",
            Cuerpo::Vacio,
            Chequeo::Personalizado(diario_es_arreglo),
        ),
        simple(
            "GET /api/agent/rules",
            "GET",
            "/api/agent/rules",
            Cuerpo::Vacio,
            Chequeo::OkClave("rules"),
        ),
        simple(
            "POST /api/agent/undo (diario vacío)",
            "POST",
            "/api/agent/undo",
            Cuerpo::Json(r#"{"n":1}"#),
            Chequeo::Codigo(400),
        ),
        simple(
            "POST /api/agent/undo (sin n)",
            "POST",
            "/api/agent/undo",
            Cuerpo::Json("{}"),
            Chequeo::Codigo(400),
        ),
        simple(
            "POST /api/agent/rules/revoke (sin id)",
            "POST",
            "/api/agent/rules/revoke",
            Cuerpo::Json("{}"),
            Chequeo::Codigo(400),
        ),
        simple(
            "POST /api/agent/rules/revoke (desconocida)",
            "POST",
            "/api/agent/rules/revoke",
            Cuerpo::Json(r#"{"id":"no-existe-0000"}"#),
            Chequeo::Codigo(404),
        ),
        Caso {
            setup: None,
            assert: Chequeo::Codigo(400),
            extra: Some(Extra {
                method: "POST",
                path: "/api/agent/approve",
                body: Cuerpo::Json(r#"{"id":"no-existe","approve":true}"#),
                chequeo: Chequeo::Codigo(200),
            }),
            ..simple(
                "POST /api/agent/approve",
                "POST",
                "/api/agent/approve",
                Cuerpo::Json("{}"),
                Chequeo::Codigo(400),
            )
        },
        simple(
            "POST /api/agent/conversations/open",
            "POST",
            "/api/agent/conversations/open",
            Cuerpo::Json(r#"{"id":"no-existe"}"#),
            Chequeo::Codigo(500),
        ),
        simple(
            "POST /api/agent/conversations/delete",
            "POST",
            "/api/agent/conversations/delete",
            Cuerpo::Json(r#"{"all":true}"#),
            Chequeo::Codigo(200),
        ),
        simple(
            "POST /api/agent/conversations/delete (sin id ni all)",
            "POST",
            "/api/agent/conversations/delete",
            Cuerpo::Json("{}"),
            Chequeo::Codigo(400),
        ),
        simple(
            "POST /api/agent/reset",
            "POST",
            "/api/agent/reset",
            Cuerpo::Json("{}"),
            Chequeo::Codigo(200),
        ),
        // ---------- cuenta ----------
        Caso {
            setup: Some(cerrar_sesion),
            assert: Chequeo::Personalizado(importar_sesion),
            extra: None,
            ..simple(
                "POST /api/auth/import",
                "POST",
                "/api/auth/import",
                Cuerpo::Json(r#"{"token":"syn_inmune"}"#),
                Chequeo::Codigo(200),
            )
        },
        // Issue #129: un token vacío es entrada inválida, no fallo del servidor.
        simple(
            "POST /api/auth/import (token vacío)",
            "POST",
            "/api/auth/import",
            Cuerpo::Json("{}"),
            Chequeo::Personalizado(importar_token_vacio),
        ),
        simple(
            "POST /api/auth/logout",
            "POST",
            "/api/auth/logout",
            Cuerpo::Json("{}"),
            Chequeo::Personalizado(cerrar_sesion_comprobada),
        ),
        simple(
            "POST /api/auth/start (sin red)",
            "POST",
            "/api/auth/start",
            Cuerpo::Json("{}"),
            Chequeo::Codigo(500),
        ),
        simple(
            "POST /api/org/seleccionar (org ajena)",
            "POST",
            "/api/org/seleccionar",
            Cuerpo::Json(r#"{"slug":"otra-org"}"#),
            Chequeo::Personalizado(org_ajena_rechazada),
        ),
        // Issue #129: sin slug no se puede elegir organización: 400 accionable.
        simple(
            "POST /api/org/seleccionar (sin slug)",
            "POST",
            "/api/org/seleccionar",
            Cuerpo::Json("{}"),
            Chequeo::Personalizado(org_sin_slug),
        ),
        // ---------- notas y diagnóstico ----------
        simple(
            "GET /api/notes (lista vacía)",
            "GET",
            "/api/notes",
            Cuerpo::Vacio,
            Chequeo::Personalizado(notas_vacias),
        ),
        simple(
            "POST /api/notes",
            "POST",
            "/api/notes",
            Cuerpo::Json(r#"{"id":"n-inmune","title":"Nota inmune","body":"cuerpo"}"#),
            Chequeo::Personalizado(nota_creada),
        ),
        simple(
            "DELETE /api/notes",
            "DELETE",
            "/api/notes?id=n-inmune",
            Cuerpo::Vacio,
            Chequeo::Personalizado(nota_borrada),
        ),
        simple(
            "GET /api/diagnostics",
            "GET",
            "/api/diagnostics",
            Cuerpo::Vacio,
            Chequeo::Personalizado(diagnostico),
        ),
        simple(
            "POST /api/diagnostics/report",
            "POST",
            "/api/diagnostics/report",
            Cuerpo::Json(r#"{"category":"ui.test","message":"fallo de prueba"}"#),
            Chequeo::OkClave("ok"),
        ),
        // ---------- archivos ----------
        simple(
            "POST /api/tasks/files",
            "POST",
            "/api/tasks/files",
            Cuerpo::Multipart {
                campo: "file",
                campos: &[("task", "t1"), ("note", "avance inmune")],
                archivo: ("prueba.txt", "text/plain", "hola"),
            },
            Chequeo::Personalizado(archivo_de_tarea),
        ),
        simple(
            "POST /api/chat/files",
            "POST",
            "/api/chat/files",
            Cuerpo::Multipart {
                campo: "file",
                campos: &[("room", "r1"), ("body", "adjunto inmune")],
                archivo: ("prueba.txt", "text/plain", "hola"),
            },
            Chequeo::Personalizado(archivo_de_chat),
        ),
        // ---------- puesta al día ----------
        simple(
            "POST /api/capture/propose",
            "POST",
            "/api/capture/propose",
            Cuerpo::Captura,
            Chequeo::Personalizado(captura_propuesta),
        ),
    ]
}

// ---------------------------------------------------------------------------
// Comprobaciones
// ---------------------------------------------------------------------------

struct Respuesta {
    status: u16,
    headers: reqwest::header::HeaderMap,
    texto: String,
    body: Value,
}

fn cabecera(respuesta: &Respuesta, nombre: &str) -> String {
    respuesta
        .headers
        .get(nombre)
        .and_then(|valor| valor.to_str().ok())
        .unwrap_or_default()
        .to_owned()
}

fn base(respuesta: &Respuesta, status: u16) {
    assert_eq!(
        respuesta.status, status,
        "código inesperado; cuerpo: {}",
        respuesta.texto
    );
    assert_eq!(
        cabecera(respuesta, "content-type"),
        "application/json; charset=utf-8",
        "content-type; cuerpo: {}",
        respuesta.texto
    );
    assert_eq!(
        cabecera(respuesta, "cache-control"),
        "no-store",
        "cache-control; cuerpo: {}",
        respuesta.texto
    );
    assert!(
        respuesta.texto.ends_with('\n'),
        "el cuerpo no termina en \\n: {:?}",
        respuesta.texto
    );
    assert!(
        respuesta.body != Value::Null,
        "la respuesta no es JSON: {}",
        respuesta.texto
    );
}

fn clave(respuesta: &Respuesta, nombre: &str) {
    assert!(
        respuesta.body.get(nombre).is_some(),
        "la respuesta no trae {nombre:?}: {}",
        respuesta.texto
    );
}

fn comprobar(chequeo: &Chequeo, backend: &BackendDePrueba, respuesta: &Respuesta, app: &Arc<App>) {
    match chequeo {
        Chequeo::OkClave(nombre) => {
            base(respuesta, 200);
            clave(respuesta, nombre);
        }
        Chequeo::OkClaves(nombres) => {
            base(respuesta, 200);
            for nombre in *nombres {
                clave(respuesta, nombre);
            }
        }
        Chequeo::Codigo(codigo) => base(respuesta, *codigo),
        Chequeo::Personalizado(f) => f(backend, respuesta, app),
    }
}

fn ajustes_leidos(_: &BackendDePrueba, respuesta: &Respuesta, _: &Arc<App>) {
    base(respuesta, 200);
    assert_eq!(respuesta.body["version"], "0.0.0-test");
    assert_eq!(respuesta.body["platform"], std::env::consts::OS);
    assert!(
        !respuesta.body["dataDir"]
            .as_str()
            .unwrap_or_default()
            .is_empty(),
        "dataDir vacío: {}",
        respuesta.texto
    );
}

fn proveedores(_: &BackendDePrueba, respuesta: &Respuesta, _: &Arc<App>) {
    base(respuesta, 200);
    assert!(
        respuesta
            .body
            .as_array()
            .is_some_and(|lista| !lista.is_empty()),
        "el catálogo de proveedores vino vacío: {}",
        respuesta.texto
    );
}

fn aprobaciones(_: &BackendDePrueba, respuesta: &Respuesta, _: &Arc<App>) {
    base(respuesta, 200);
    assert!(
        respuesta.body.is_array(),
        "las aprobaciones no son un arreglo: {}",
        respuesta.texto
    );
}

fn conversaciones(_: &BackendDePrueba, respuesta: &Respuesta, _: &Arc<App>) {
    base(respuesta, 200);
    assert!(
        respuesta.body.is_array(),
        "las conversaciones no son un arreglo: {}",
        respuesta.texto
    );
}

fn eventos(_: &BackendDePrueba, respuesta: &Respuesta, _: &Arc<App>) {
    base(respuesta, 200);
    clave(respuesta, "kind");
    assert_eq!(
        respuesta.body["kind"], "chat_mention",
        "el evento publicado no llegó: {}",
        respuesta.texto
    );
}

fn tarea_creada(backend: &BackendDePrueba, respuesta: &Respuesta, _: &Arc<App>) {
    base(respuesta, 200);
    clave(respuesta, "task");
    assert_eq!(backend.texto(&backend.creado_titulo), "Tarea inmune");
}

fn estado_cambiado(backend: &BackendDePrueba, respuesta: &Respuesta, _: &Arc<App>) {
    base(respuesta, 200);
    assert_eq!(backend.estado_calls.load(Ordering::Relaxed), 1);
    assert_eq!(
        *backend.ultimo_estado.lock().expect("estado"),
        Some(TaskStatus::Blocked)
    );
    assert_eq!(backend.texto(&backend.ultima_nota), "espero firma");
}

fn comentario_creado(backend: &BackendDePrueba, respuesta: &Respuesta, _: &Arc<App>) {
    base(respuesta, 200);
    assert_eq!(backend.comentario_calls.load(Ordering::Relaxed), 1);
    assert_eq!(
        backend.texto(&backend.ultimo_comentario),
        "comentario inmune"
    );
}

fn mensaje_directo(backend: &BackendDePrueba, respuesta: &Respuesta, _: &Arc<App>) {
    base(respuesta, 200);
    assert_eq!(backend.directos.load(Ordering::Relaxed), 1);
}

fn sala_leida(backend: &BackendDePrueba, respuesta: &Respuesta, _: &Arc<App>) {
    base(respuesta, 200);
    assert_eq!(backend.texto(&backend.sala_leida), "r1");
}

fn alertas_leidas(backend: &BackendDePrueba, respuesta: &Respuesta, _: &Arc<App>) {
    base(respuesta, 200);
    assert_eq!(backend.alertas_leidas.load(Ordering::Relaxed), 2);
}

fn menciones_leidas(backend: &BackendDePrueba, respuesta: &Respuesta, _: &Arc<App>) {
    base(respuesta, 200);
    assert_eq!(backend.menciones_leidas.load(Ordering::Relaxed), 2);
}

fn meta_creada(backend: &BackendDePrueba, respuesta: &Respuesta, _: &Arc<App>) {
    base(respuesta, 200);
    clave(respuesta, "goal");
    assert_eq!(backend.texto(&backend.meta_titulo), "Meta inmune");
}

fn periodo_creado(backend: &BackendDePrueba, respuesta: &Respuesta, _: &Arc<App>) {
    base(respuesta, 200);
    clave(respuesta, "period");
    assert_eq!(backend.texto(&backend.periodo_titulo), "Periodo inmune");
}

fn captura_aplicada(backend: &BackendDePrueba, respuesta: &Respuesta, _: &Arc<App>) {
    base(respuesta, 200);
    clave(respuesta, "ok");
    assert_eq!(backend.texto(&backend.aplicado), "d1:c1");
}

fn aviso_enviado(backend: &BackendDePrueba, respuesta: &Respuesta, _: &Arc<App>) {
    base(respuesta, 200);
    assert_eq!(backend.texto(&backend.aviso_tipo), "blocker");
    assert_eq!(backend.texto(&backend.aviso_texto), "aviso inmune");
}

fn reaccion_enviada(backend: &BackendDePrueba, respuesta: &Respuesta, _: &Arc<App>) {
    base(respuesta, 200);
    assert_eq!(backend.texto(&backend.reaccion), "m1:👍");
}

fn contexto_actualizado(backend: &BackendDePrueba, respuesta: &Respuesta, _: &Arc<App>) {
    base(respuesta, 200);
    assert_eq!(backend.texto(&backend.foco), "foco inmune");
    assert_eq!(backend.texto(&backend.animo), "at_risk");
}

fn ajustes_cambiados(_: &BackendDePrueba, respuesta: &Respuesta, _: &Arc<App>) {
    base(respuesta, 200);
    assert_eq!(
        respuesta.body["locale"], "en",
        "idioma guardado: {}",
        respuesta.texto
    );
}

fn setup_completo(_: &BackendDePrueba, respuesta: &Respuesta, _: &Arc<App>) {
    base(respuesta, 200);
    for nombre in [
        "installed",
        "portable",
        "dir",
        "termsTitle",
        "termsBody",
        "termsVersion",
        "acceptedTerms",
        "platform",
    ] {
        clave(respuesta, nombre);
    }
    assert_eq!(
        respuesta.body["platform"],
        platform(),
        "la plataforma no es la del host: {}",
        respuesta.texto
    );
    assert!(
        respuesta.body["termsVersion"]
            .as_str()
            .is_some_and(|version| !version.is_empty()),
        "la versión de los términos vino vacía: {}",
        respuesta.texto
    );
}

fn actualizacion_sin_configurar(_: &BackendDePrueba, respuesta: &Respuesta, _: &Arc<App>) {
    base(respuesta, 200);
    assert_eq!(
        respuesta.body["configured"], false,
        "sin manifiesto configurado no debe decir que está configurado: {}",
        respuesta.texto
    );
    assert_eq!(
        respuesta.body["available"], false,
        "sin manifiesto no puede haber versión disponible: {}",
        respuesta.texto
    );
    assert!(
        respuesta.body.get("current").is_some(),
        "la respuesta tiene que traer la versión actual: {}",
        respuesta.texto
    );
    for omitida in ["latest", "notes", "url", "sha256"] {
        assert!(
            respuesta.body.get(omitida).is_none(),
            "sin URL no debe aparecer {omitida}: {}",
            respuesta.texto
        );
    }
}

/// `GET /api/update/progress` en reposo: fase `idle`, contadores en cero y
/// sin texto de error. Es lo que la ventana lee al abrir.
fn actualizacion_en_reposo(_: &BackendDePrueba, respuesta: &Respuesta, _: &Arc<App>) {
    base(respuesta, 200);
    assert_eq!(
        respuesta.body["phase"], "idle",
        "sin actualización en curso la fase tiene que ser idle: {}",
        respuesta.texto
    );
    assert_eq!(respuesta.body["received"], 0, "{}", respuesta.texto);
    assert_eq!(respuesta.body["total"], 0, "{}", respuesta.texto);
    assert_eq!(
        respuesta.body["error"], "",
        "en reposo no hay error que mostrar: {}",
        respuesta.texto
    );
}

fn actualizacion_sin_version(_: &BackendDePrueba, respuesta: &Respuesta, _: &Arc<App>) {
    base(respuesta, 500);
    assert!(
        respuesta.body["error"]
            .as_str()
            .is_some_and(|error| error.contains("versión nueva")),
        "el error no explica que no hay versión nueva: {}",
        respuesta.texto
    );
}

fn instalacion_rechaza_terminos(_: &BackendDePrueba, respuesta: &Respuesta, _: &Arc<App>) {
    base(respuesta, 500);
    assert!(
        respuesta.body["error"]
            .as_str()
            .is_some_and(|error| error.contains("términos")),
        "el error no explica que los términos cambiaron: {}",
        respuesta.texto
    );
}

fn desinstalacion_conserva_datos(_: &BackendDePrueba, respuesta: &Respuesta, _: &Arc<App>) {
    base(respuesta, 200);
    assert_eq!(
        *DESINSTALADO.lock().expect("estado de desinstalación"),
        Some(false),
        "un cuerpo vacío no puede pedir borrar los datos: {}",
        respuesta.texto
    );
}

fn desinstalacion_borra_datos(_: &BackendDePrueba, respuesta: &Respuesta, _: &Arc<App>) {
    base(respuesta, 200);
    assert_eq!(
        *DESINSTALADO.lock().expect("estado de desinstalación"),
        Some(true),
        "keepData falso tiene que borrar los datos: {}",
        respuesta.texto
    );
}

fn agente_configurado(_: &BackendDePrueba, respuesta: &Respuesta, _: &Arc<App>) {
    base(respuesta, 200);
    assert_eq!(respuesta.body["configured"], true);
    assert_eq!(respuesta.body["model"], "modelo-inmune");
    assert_eq!(respuesta.body["mode"], "readonly");
}

fn agente_probado(_: &BackendDePrueba, respuesta: &Respuesta, _: &Arc<App>) {
    base(respuesta, 200);
    assert_eq!(respuesta.body["ok"], true);
    assert_eq!(respuesta.body["reply"], "ok");
}

fn diario_es_arreglo(_: &BackendDePrueba, respuesta: &Respuesta, _: &Arc<App>) {
    base(respuesta, 200);
    assert!(
        respuesta.body["entries"].is_array(),
        "la lista del diario debe venir como arreglo, nunca null: {}",
        respuesta.texto
    );
}

fn importar_sesion(_: &BackendDePrueba, respuesta: &Respuesta, _: &Arc<App>) {
    base(respuesta, 200);
    clave(respuesta, "me");
}

/// Issue #129: el token vacío se rechaza antes de tocar la sesión.
fn importar_token_vacio(_: &BackendDePrueba, respuesta: &Respuesta, _: &Arc<App>) {
    base(respuesta, 400);
    assert_eq!(respuesta.body["error"], "el token está vacío");
}

fn org_ajena_rechazada(_: &BackendDePrueba, respuesta: &Respuesta, app: &Arc<App>) {
    base(respuesta, 400);
    assert!(
        respuesta.body["error"]
            .as_str()
            .is_some_and(|error| error.contains("syncra") && error.contains("otra-org")),
        "el error debe nombrar la organización recibida y la pedida: {}",
        respuesta.texto
    );
    assert_eq!(
        app.settings_snapshot().org_slug,
        "syncra",
        "un rechazo no puede guardar la organización pedida"
    );
    assert!(
        !app.settings_snapshot().org_fijada,
        "un rechazo no puede fijar una organización que el servidor no confirmó"
    );
}

/// Issue #129: sin slug la ruta no puede elegir nada y lo dice con un 400.
fn org_sin_slug(_: &BackendDePrueba, respuesta: &Respuesta, _: &Arc<App>) {
    base(respuesta, 400);
    assert_eq!(respuesta.body["error"], "falta el slug de la organización");
}

fn cerrar_sesion_comprobada(_: &BackendDePrueba, respuesta: &Respuesta, app: &Arc<App>) {
    base(respuesta, 200);
    assert_eq!(
        app.state().status,
        Status::LoggedOut,
        "el estado tras cerrar la sesión no es logged_out"
    );
}

fn notas_vacias(_: &BackendDePrueba, respuesta: &Respuesta, _: &Arc<App>) {
    base(respuesta, 200);
    assert!(
        respuesta.body["notes"]
            .as_array()
            .is_some_and(Vec::is_empty),
        "sin notas la lista tiene que ser [] y nunca null: {}",
        respuesta.texto
    );
}

fn nota_creada(_: &BackendDePrueba, respuesta: &Respuesta, _: &Arc<App>) {
    base(respuesta, 200);
    assert_eq!(
        respuesta.body["id"], "n-inmune",
        "el POST devuelve la nota guardada: {}",
        respuesta.texto
    );
    assert_eq!(respuesta.body["title"], "Nota inmune");
    assert_eq!(respuesta.body["body"], "cuerpo");
    assert!(
        respuesta.body["updated_at"]
            .as_str()
            .is_some_and(|fecha| fecha.ends_with('Z')),
        "updated_at tiene que ser RFC3339 del servidor: {}",
        respuesta.texto
    );
}

fn nota_borrada(_: &BackendDePrueba, respuesta: &Respuesta, _: &Arc<App>) {
    base(respuesta, 200);
    assert_eq!(
        respuesta.body["ok"], true,
        "el DELETE devuelve ok aunque el id no exista: {}",
        respuesta.texto
    );
}

fn diagnostico(_: &BackendDePrueba, respuesta: &Respuesta, _: &Arc<App>) {
    base(respuesta, 200);
    let texto = respuesta.body["text"].as_str().unwrap_or_default();
    assert!(
        texto.contains("Motor: Rust"),
        "el diagnóstico no dice Motor: Rust: {}",
        respuesta.texto
    );
    assert!(
        texto.contains("Estado de la sesión: ready"),
        "el diagnóstico no trae el estado: {}",
        respuesta.texto
    );
    assert!(
        !texto.contains("syn_"),
        "el diagnóstico incluye un token: {}",
        respuesta.texto
    );
}

fn archivo_de_tarea(backend: &BackendDePrueba, respuesta: &Respuesta, _: &Arc<App>) {
    base(respuesta, 200);
    clave(respuesta, "update_id");
    assert_eq!(backend.subidas.load(Ordering::Relaxed), 1);
    assert_eq!(backend.texto(&backend.subida_tarea), "t1");
}

fn archivo_de_chat(backend: &BackendDePrueba, respuesta: &Respuesta, _: &Arc<App>) {
    base(respuesta, 200);
    clave(respuesta, "ok");
    assert_eq!(backend.subidas_chat.load(Ordering::Relaxed), 1);
    assert_eq!(backend.texto(&backend.subida_sala), "r1");
}

fn captura_propuesta(_: &BackendDePrueba, respuesta: &Respuesta, _: &Arc<App>) {
    base(respuesta, 200);
    assert!(
        !respuesta.body["digest_id"]
            .as_str()
            .unwrap_or_default()
            .is_empty(),
        "propuesta sin digest_id: {}",
        respuesta.texto
    );
    assert!(
        respuesta.body["candidates"]
            .as_array()
            .is_some_and(|lista| !lista.is_empty()),
        "propuesta sin candidatos: {}",
        respuesta.texto
    );
}

// ---------------------------------------------------------------------------
// App y costuras
// ---------------------------------------------------------------------------

/// `Store` que arranca sin sesión y guarda el token importado, como el
/// `FileStore` temporal que usa Go en `newImmuneApp`.
struct StoreDeContrato(Mutex<Option<Token>>);

impl Store for StoreDeContrato {
    fn save(&self, token: Token) -> Result<(), AuthError> {
        *self.0.lock().expect("token de prueba") = Some(token);
        Ok(())
    }

    fn load(&self) -> Result<Token, AuthError> {
        self.0
            .lock()
            .expect("token de prueba")
            .clone()
            .ok_or(AuthError::NoSession)
    }

    fn clear(&self) -> Result<(), AuthError> {
        *self.0.lock().expect("token de prueba") = None;
        Ok(())
    }
}

/// Clave del proveedor con memoria: `agent/config` la escribe y la vista la lee.
struct SecretosMemoria(Mutex<Option<String>>);

impl orus_app::Secrets for SecretosMemoria {
    fn get(&self) -> Result<Option<String>, String> {
        Ok(self.0.lock().expect("clave de prueba").clone())
    }

    fn set(&self, value: &str) -> Result<(), String> {
        *self.0.lock().expect("clave de prueba") = Some(value.to_owned());
        Ok(())
    }

    fn delete(&self) -> Result<(), String> {
        *self.0.lock().expect("clave de prueba") = None;
        Ok(())
    }
}

struct ConectorFijo(Arc<BackendDePrueba>);

#[async_trait]
impl Connector for ConectorFijo {
    async fn open(&self, _: &str, _: &str, _: &str) -> Result<Arc<dyn Backend>, SessionError> {
        Ok(self.0.clone())
    }
}

struct ProveedorEco;

#[async_trait]
impl Provider for ProveedorEco {
    async fn chat(&self, _: ChatRequest) -> Result<ChatReply, ProviderError> {
        Ok(ChatReply {
            message: ChatMessage {
                role: "assistant".to_owned(),
                content: "ok".to_owned(),
                ..ChatMessage::default()
            },
            prompt_tokens: 0,
            completion_tokens: 0,
        })
    }
}

fn proveedor_eco() -> ProveedorFn {
    Arc::new(|_, _| Ok(Arc::new(ProveedorEco) as Arc<dyn Provider>))
}

struct GitLogFalso;

impl GitLog for GitLogFalso {
    fn log(&self, dir: &str, _: Option<DateTime<Utc>>) -> Result<DigestSource, SessionError> {
        Ok(DigestSource {
            name: dir.to_owned(),
            items: Some(vec![DigestItem {
                kind: "commit".to_owned(),
                title: "Commit de prueba inmune".to_owned(),
                ..DigestItem::default()
            }]),
            truncated: false,
        })
    }
}

/// `true` si el último `uninstall` pidió borrar los datos; `None` si todavía
/// no se llamó. El caso de contrato la resetea antes de cada petición.
static DESINSTALADO: Mutex<Option<bool>> = Mutex::new(None);

/// Las acciones que Go resuelve dentro de `App`; acá se aíslan para probar el
/// borde sin instalar, desinstalar ni descargar nada.
struct AccionesFalsas;

impl Setup for AccionesFalsas {
    fn setup_info(&self) -> Result<SetupInfo, String> {
        Ok(SetupInfo {
            installed: false,
            portable: true,
            dir: r"C:\Oruscribe".to_owned(),
            terms_title: TERMS_TITLE.to_owned(),
            terms_body: TERMS_BODY.to_owned(),
            terms_version: TERMS_VERSION.to_owned(),
            accepted_terms: String::new(),
            platform: platform().to_owned(),
        })
    }

    fn install(&self, _: bool, terms_version: &str) -> Result<(), String> {
        if terms_version != TERMS_VERSION {
            return Err(
                "los términos cambiaron: vuelve a leerlos y acepta la versión vigente".to_owned(),
            );
        }
        Ok(())
    }

    fn uninstall(&self, remove_data: bool) -> Result<(), String> {
        *DESINSTALADO.lock().expect("estado de desinstalación") = Some(remove_data);
        Ok(())
    }
}

#[async_trait]
impl Update for AccionesFalsas {
    async fn check_update(&self) -> Result<UpdateInfo, String> {
        Ok(UpdateInfo {
            configured: false,
            available: false,
            current: "0.0.0-test".to_owned(),
            ..UpdateInfo::default()
        })
    }

    async fn apply_update(&self) -> Result<(), String> {
        Err("no hay una versión nueva para descargar".to_owned())
    }

    fn progreso(&self) -> orus_api::Progreso {
        orus_api::Progreso::default()
    }
}

fn cerrar_sesion(app: &mut App) {
    app.logout().expect("cerrar la sesión de prueba");
}

fn aislar_ajustes() {
    let dir = orus_config::data_dir();
    std::fs::create_dir_all(&dir).expect("carpeta de datos");
    std::fs::write(dir.join("settings.json"), b"{}").expect("ajustes base");
}

fn app_de_contrato(backend: Arc<BackendDePrueba>, conversaciones: orus_app::Conversations) -> App {
    let mut app = AppDePrueba::nuevo()
        .con_store(Box::new(StoreDeContrato(Mutex::new(None))))
        .con_connector(Arc::new(ConectorFijo(backend)))
        .con_settings(Settings::default())
        .con_secretos(Arc::new(SecretosMemoria(Mutex::new(None))))
        .con_conversaciones(conversaciones)
        .con_runner(Arc::new(RunnerOk))
        .construir()
        .with_proveedor(proveedor_eco());
    // Sin red para `/api/auth/start`: el puerto 1 no escucha.
    app.set_login_issuer("http://127.0.0.1:1");
    app
}

async fn levantar_api(api: Api) -> (String, tokio::task::JoinHandle<std::io::Result<()>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("escuchar");
    let addr = listener.local_addr().expect("dirección local");
    let router = api.router();
    let server = tokio::spawn(async move { axum::serve(listener, router).await });
    (format!("http://{addr}"), server)
}

async fn pedir(base_url: &str, method: &str, path: &str, cuerpo: &Cuerpo) -> Respuesta {
    // El `tempdir` tiene que vivir hasta que termine la petición: si se
    // creara dentro del `match`, la carpeta desaparecería antes del envío.
    let repo =
        matches!(cuerpo, Cuerpo::Captura).then(|| tempfile::tempdir().expect("repo de prueba"));
    let cliente = reqwest::Client::new();
    let url = format!("{base_url}{path}");
    let peticion = match method {
        "GET" => cliente.get(&url),
        "POST" => cliente.post(&url),
        "DELETE" => cliente.delete(&url),
        otro => panic!("método no esperado: {otro}"),
    };
    let peticion = match cuerpo {
        Cuerpo::Vacio => peticion,
        Cuerpo::Json(texto) => peticion
            .header("content-type", "application/json")
            .body(*texto),
        Cuerpo::Captura => {
            let carpeta = repo.as_ref().expect("repo de prueba").path();
            let cuerpo = serde_json::json!({
                "folder": carpeta.to_string_lossy(),
                "since": "2020-01-01",
            });
            peticion
                .header("content-type", "application/json")
                .body(cuerpo.to_string())
        }
        Cuerpo::Multipart {
            campo,
            campos,
            archivo,
        } => {
            let mut formulario = reqwest::multipart::Form::new();
            for (nombre, valor) in *campos {
                formulario = formulario.text(*nombre, *valor);
            }
            let (nombre, tipo, contenido) = *archivo;
            let parte = reqwest::multipart::Part::bytes(contenido.as_bytes().to_vec())
                .file_name(nombre.to_owned())
                .mime_str(tipo)
                .expect("tipo de archivo");
            peticion.multipart(formulario.part(*campo, parte))
        }
    };
    let response = peticion.send().await.expect("petición HTTP");
    let status = response.status().as_u16();
    let headers = response.headers().clone();
    let texto = response.text().await.expect("leer el cuerpo");
    let body = serde_json::from_str(&texto).unwrap_or(Value::Null);
    Respuesta {
        status,
        headers,
        texto,
        body,
    }
}

async fn ejecutar(caso: &Caso) {
    aislar_ajustes();
    *DESINSTALADO.lock().expect("estado de desinstalación") = None;
    let conversaciones = tempfile::tempdir().expect("conversaciones");
    let backend = Arc::new(BackendDePrueba::default());
    let mut app = app_de_contrato(
        Arc::clone(&backend),
        conversaciones_en(conversaciones.path()),
    );
    app.import_token("syn_inmune")
        .await
        .expect("importar token");
    if let Some(setup) = caso.setup {
        setup(&mut app);
    }
    let app = Arc::new(app);
    let hub = Arc::new(Hub::new());
    let instalador: Arc<dyn Setup> = Arc::new(AccionesFalsas);
    let actualizador: Arc<dyn Update> = Arc::new(AccionesFalsas);
    let api = if caso.path.starts_with("/api/capture/propose") {
        Api::with_git_log(Arc::clone(&app), Arc::new(GitLogFalso))
    } else if caso.path == "/api/events" {
        Api::with_hub(Arc::clone(&app), Arc::clone(&hub))
    } else {
        Api::new(Arc::clone(&app))
    }
    .with_version("0.0.0-test".to_owned())
    .with_wait(Duration::from_millis(100))
    .with_setup(instalador)
    .with_update(actualizador);
    let (base_url, server) = levantar_api(api).await;

    // Go publica a los 120 ms; aquí esperamos a que el handler se suscriba
    // para que el evento no se pierda si el arranque del servidor tarda más.
    if caso.path == "/api/events" {
        let hub = Arc::clone(&hub);
        tokio::spawn(async move {
            let suscrito = async {
                while hub.len() == 0 {
                    tokio::task::yield_now().await;
                }
            };
            if tokio::time::timeout(Duration::from_secs(5), suscrito)
                .await
                .is_ok()
            {
                hub.publish(Event {
                    kind: "chat_mention".to_owned(),
                    at: Local::now(),
                    open: 0,
                    chat: 0,
                });
            }
        });
    }

    let respuesta = pedir(&base_url, caso.method, caso.path, &caso.body).await;
    comprobar(&caso.assert, &backend, &respuesta, &app);
    if let Some(extra) = &caso.extra {
        let respuesta = pedir(&base_url, extra.method, extra.path, &extra.body).await;
        comprobar(&extra.chequeo, &backend, &respuesta, &app);
    }

    server.abort();
    let _ = server.await;
}

#[test]
fn contrato_http_inmune() {
    en_datos_aislados("contrato_http_inmune", || {
        con_runtime(async {
            for caso in &casos() {
                println!("CASO {}", caso.name);
                ejecutar(caso).await;
            }
        });
    });
}

// ---------------------------------------------------------------------------
// Guardas de inventario
// ---------------------------------------------------------------------------

fn sin_query(path: &str) -> &str {
    path.split('?').next().unwrap_or(path)
}

#[test]
fn inventario_de_rutas() {
    let casos = casos();
    assert!(!casos.is_empty(), "CASOS no puede estar vacío");
    assert!(
        FUERA_DE_ALCANCE.is_empty(),
        "ya no quedan rutas fuera de alcance: {FUERA_DE_ALCANCE:?}"
    );

    let mut rutas_casos: HashSet<(&str, &str)> = HashSet::new();
    for caso in &casos {
        rutas_casos.insert((caso.method, sin_query(caso.path)));
    }

    let rutas_router: HashSet<(&str, &str)> = RUTAS.iter().copied().collect();
    assert_eq!(
        rutas_router.len(),
        RUTAS.len(),
        "RUTAS no puede tener duplicados"
    );
    assert_eq!(
        rutas_casos, rutas_router,
        "CASOS y RUTAS no coinciden por (método, ruta)"
    );
}

#[test]
fn cada_ruta_responde_distinto_de_404() {
    en_datos_aislados("cada_ruta_responde_distinto_de_404", || {
        con_runtime(async {
            for (method, path) in RUTAS {
                aislar_ajustes();
                let conversaciones = tempfile::tempdir().expect("conversaciones");
                let backend = Arc::new(BackendDePrueba::default());
                let app = app_de_contrato(
                    Arc::clone(&backend),
                    conversaciones_en(conversaciones.path()),
                );
                app.import_token("syn_inmune")
                    .await
                    .expect("importar token");
                let api = Api::new(Arc::new(app))
                    .with_version("0.0.0-test".to_owned())
                    .with_wait(Duration::from_millis(100));
                let (base_url, server) = levantar_api(api).await;
                let respuesta = pedir(&base_url, method, path, &Cuerpo::Vacio).await;
                assert_ne!(
                    respuesta.status, 404,
                    "{method} {path} no está registrada (cuerpo: {})",
                    respuesta.texto
                );
                server.abort();
                let _ = server.await;
            }
        });
    });
}
