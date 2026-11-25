//! Contrato real del frontend servido por `orus-server`:
//! - Todos los activos y módulos ES embebidos cargan con 200 y su Content-Type correcto.
//! - La CSP publicada en `index.html` protege contra scripts/objetos externos.
//! - Toda ruta `/api/*` referenciada en cualquier módulo `/js/*.js` existe en el router real (sin 404).

mod soporte;

use std::collections::BTreeSet;
use std::fs;
use std::future::Future;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use serial_test::serial;
use soporte::levantar;

fn cabeceras(token: &str) -> reqwest::header::HeaderMap {
    let mut cabeceras = reqwest::header::HeaderMap::new();
    cabeceras.insert(
        reqwest::header::COOKIE,
        format!("orus_token={token}")
            .parse()
            .expect("cookie del test"),
    );
    cabeceras
}

const INDEX: &str = include_str!("../../../frontend/dist/index.html");
const APP_CSS: &str = include_str!("../../../frontend/dist/app.css");
const LOGO_SVG: &str = include_str!("../../../frontend/dist/logo.svg");
const LOGO_PNG: &[u8] = include_bytes!("../../../frontend/dist/logo-512.png");
const FONT: &[u8] = include_bytes!("../../../frontend/dist/fonts/inter.woff2");

/// Rutas de long-poll / streaming que no se prueban con un GET síncrono simple.
const FUERA: &[&str] = &["/api/events"];

/// Rutas mínimas que el escáner siempre debe encontrar en los módulos JS.
const OBLIGATORIAS: &[&str] = &[
    "/api/state",
    "/api/agent/send",
    "/api/agent/rules",
    "/api/agent/rules/revoke",
    "/api/agent/transcribe",
    "/api/agent/speak",
    "/api/tasks/files",
    "/api/chat/files",
    "/api/quit",
];

fn rutas_de_api(js: &str) -> Vec<String> {
    let mut rutas: Vec<String> = Vec::new();
    let mut resto = js;
    while let Some(inicio) = resto.find("\"/api/") {
        let desde = &resto[inicio + 1..];
        let Some(fin) = desde.find('"') else {
            break;
        };
        let literal = &desde[..fin];
        let ruta = literal.find('?').map_or(literal, |corte| &literal[..corte]);
        if !rutas.iter().any(|vista| vista.as_str() == ruta) {
            rutas.push(ruta.to_owned());
        }
        resto = &resto[inicio + 1 + fin..];
    }
    rutas
}

async fn con_datos<T>(datos: &std::path::Path, futuro: impl Future<Output = T>) -> T {
    temp_env::async_with_vars([("ORUSCRIBE_DATA_DIR", Some(datos.as_os_str()))], futuro).await
}

#[test]
fn frontend_el_escaner_ignora_la_base_externa_de_openrouter() {
    let js = r#"const base = "https://openrouter.ai/api/v1"; await api("/api/state"); fetch("/api/chat/files");"#;
    assert_eq!(
        rutas_de_api(js),
        vec![String::from("/api/state"), String::from("/api/chat/files")]
    );
}

#[test]
fn frontend_csp_protege_scripts_y_objetos_y_permite_audio_blob() {
    assert!(
        INDEX.contains("media-src 'self' blob:"),
        "la CSP publicada debe permitir audio blob: para reproducción TTS"
    );
    assert!(
        INDEX.contains("object-src 'none'"),
        "la CSP perdió object-src 'none'"
    );
    assert!(
        INDEX.contains("script-src 'self'"),
        "la CSP perdió script-src 'self'"
    );
}

#[tokio::test]
#[serial]
async fn frontend_carga_los_estaticos_y_el_estado() {
    let datos = tempfile::tempdir().expect("carpeta temporal");
    con_datos(datos.path(), async {
        let (addr, token, servidor) = levantar(datos.path()).await;
        let cliente = reqwest::Client::builder()
            .default_headers(cabeceras(&token))
            .build()
            .expect("cliente con token");

        let index = cliente
            .get(format!("http://{addr}/"))
            .send()
            .await
            .expect("GET /");
        assert_eq!(index.status(), 200);
        assert_eq!(index.headers()["content-type"], "text/html; charset=utf-8");
        assert_eq!(index.text().await.expect("cuerpo de /"), INDEX);

        for (ruta, esperado) in orus_server::JS_MODULOS {
            let js = cliente
                .get(format!("http://{addr}{ruta}"))
                .send()
                .await
                .unwrap_or_else(|e| panic!("GET {ruta}: {e}"));
            assert_eq!(js.status(), 200, "GET {ruta}");
            assert_eq!(
                js.headers()["content-type"],
                "text/javascript; charset=utf-8"
            );
            assert_eq!(js.text().await.expect("cuerpo JS"), *esperado);
        }

        let css = cliente
            .get(format!("http://{addr}/app.css"))
            .send()
            .await
            .expect("GET /app.css");
        assert_eq!(css.status(), 200);
        assert_eq!(css.headers()["content-type"], "text/css; charset=utf-8");
        assert_eq!(css.text().await.expect("cuerpo de /app.css"), APP_CSS);

        let svg = cliente
            .get(format!("http://{addr}/logo.svg"))
            .send()
            .await
            .expect("GET /logo.svg");
        assert_eq!(svg.status(), 200);
        assert_eq!(svg.headers()["content-type"], "image/svg+xml");
        assert_eq!(svg.text().await.expect("cuerpo de /logo.svg"), LOGO_SVG);

        let png = cliente
            .get(format!("http://{addr}/logo-512.png"))
            .send()
            .await
            .expect("GET /logo-512.png");
        assert_eq!(png.status(), 200);
        assert_eq!(png.headers()["content-type"], "image/png");
        assert_eq!(
            png.bytes().await.expect("cuerpo de /logo-512.png").as_ref(),
            LOGO_PNG
        );

        let fuente = cliente
            .get(format!("http://{addr}/fonts/inter.woff2"))
            .send()
            .await
            .expect("GET /fonts/inter.woff2");
        assert_eq!(fuente.status(), 200);
        assert_eq!(fuente.headers()["content-type"], "font/woff2");
        assert_eq!(
            fuente
                .bytes()
                .await
                .expect("cuerpo de /fonts/inter.woff2")
                .as_ref(),
            FONT
        );

        let estado = cliente
            .get(format!("http://{addr}/api/state"))
            .send()
            .await
            .expect("GET /api/state");
        assert_eq!(estado.status(), 200);
        assert_eq!(
            estado.headers()["content-type"],
            "application/json; charset=utf-8"
        );
        let cuerpo: serde_json::Value = estado.json().await.expect("JSON de /api/state");
        assert!(cuerpo.is_object(), "el estado no es un objeto: {cuerpo}");

        let sin_redirects = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("cliente sin redirects");
        let index_html = sin_redirects
            .get(format!("http://{addr}/index.html"))
            .send()
            .await
            .expect("GET /index.html");
        assert_eq!(index_html.status(), 301);
        assert_eq!(index_html.headers()["location"], "./");

        servidor.abort();
    })
    .await;
}

#[tokio::test]
#[serial]
async fn frontend_no_llama_rutas_inexistentes() {
    let datos = tempfile::tempdir().expect("carpeta temporal");
    con_datos(datos.path(), async {
        let (addr, token, servidor) = levantar(datos.path()).await;
        let cliente = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .default_headers(cabeceras(&token))
            .build()
            .expect("cliente con tope");

        let mut todo_js = String::new();
        for (ruta, _) in orus_server::JS_MODULOS {
            let js = cliente
                .get(format!("http://{addr}{ruta}"))
                .send()
                .await
                .unwrap_or_else(|e| panic!("GET {ruta}: {e}"))
                .text()
                .await
                .expect("cuerpo JS");
            todo_js.push_str(&js);
            todo_js.push('\n');
        }
        let rutas = rutas_de_api(&todo_js);

        assert!(
            rutas.len() >= 50,
            "el escáner solo encontró {} rutas en los módulos JS",
            rutas.len()
        );
        for esperada in OBLIGATORIAS {
            assert!(
                rutas.iter().any(|ruta| ruta.as_str() == *esperada),
                "el escáner no vio {esperada}"
            );
        }
        for fuera in FUERA {
            assert!(
                rutas.iter().any(|ruta| ruta.as_str() == *fuera),
                "la excepción {fuera} ya no aparece en los módulos JS"
            );
        }

        let mut comprobadas = 0;
        for ruta in &rutas {
            if FUERA.contains(&ruta.as_str()) {
                continue;
            }
            let respuesta = cliente
                .get(format!("http://{addr}{ruta}"))
                .send()
                .await
                .unwrap_or_else(|error| panic!("GET {ruta}: {error}"));
            assert_ne!(
                respuesta.status(),
                reqwest::StatusCode::NOT_FOUND,
                "GET {ruta} da 404: la UI la llama y el router no la registra"
            );
            comprobadas += 1;
        }
        assert_eq!(
            comprobadas,
            rutas.len() - FUERA.len(),
            "alguna ruta de los módulos JS quedó sin comprobar"
        );

        servidor.abort();
    })
    .await;
}

fn raiz_js() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../frontend/dist/js")
}

fn archivos_js(dir: &Path, salida: &mut Vec<PathBuf>) {
    for entrada in fs::read_dir(dir).expect("leer dist/js") {
        let ruta = entrada.expect("entrada de dist/js").path();
        if ruta.is_dir() {
            archivos_js(&ruta, salida);
        } else if matches!(
            ruta.extension().and_then(|e| e.to_str()),
            Some("js" | "mjs")
        ) {
            salida.push(ruta);
        }
    }
}

fn normalizar(ruta: &Path) -> PathBuf {
    let mut salida = PathBuf::new();
    for parte in ruta.components() {
        match parte {
            Component::ParentDir => {
                salida.pop();
            }
            Component::CurDir | Component::RootDir | Component::Prefix(_) => {}
            Component::Normal(nombre) => salida.push(nombre),
        }
    }
    salida
}

fn especificadores(codigo: &str) -> Vec<&str> {
    let mut salida = Vec::new();
    for marca in ["from \"", "import \""] {
        let mut resto = codigo;
        while let Some(inicio) = resto.find(marca) {
            let desde = &resto[inicio + marca.len()..];
            let Some(fin) = desde.find('"') else {
                break;
            };
            salida.push(&desde[..fin]);
            resto = &desde[fin + 1..];
        }
    }
    salida
}

fn modulos_referenciados() -> BTreeSet<String> {
    let raiz = raiz_js();
    let mut archivos = Vec::new();
    archivos_js(&raiz, &mut archivos);
    let mut rutas = BTreeSet::new();
    for archivo in archivos {
        let relativo = archivo
            .strip_prefix(&raiz)
            .expect("módulo dentro de dist/js");
        let carpeta = relativo.parent().unwrap_or_else(|| Path::new(""));
        let codigo = fs::read_to_string(&archivo).expect("leer módulo JS");
        for modulo in especificadores(&codigo) {
            if !modulo.starts_with('.') {
                continue;
            }
            let destino = normalizar(&carpeta.join(modulo));
            rutas.insert(format!(
                "/js/{}",
                destino.to_string_lossy().replace('\\', "/")
            ));
        }
    }
    let html = fs::read_to_string(raiz.join("../index.html")).expect("leer index.html");
    let mut resto = html.as_str();
    while let Some(inicio) = resto.find("src=\"/js/") {
        let desde = &resto[inicio + "src=\"".len()..];
        let Some(fin) = desde.find('"') else {
            break;
        };
        rutas.insert(desde[..fin].to_owned());
        resto = &desde[fin + 1..];
    }
    rutas
}

#[test]
fn frontend_la_tabla_cubre_todos_los_modulos_referenciados() {
    let referidos = modulos_referenciados();
    let servidos: BTreeSet<String> = orus_server::JS_MODULOS
        .iter()
        .map(|(ruta, _)| (*ruta).to_owned())
        .collect();
    let faltan: Vec<&String> = referidos.difference(&servidos).collect();
    assert!(
        faltan.is_empty(),
        "módulos que la UI importa y el servidor no sirve: {faltan:?}"
    );
    let sueltos: Vec<&String> = servidos.difference(&referidos).collect();
    assert!(
        sueltos.is_empty(),
        "módulos en la tabla que nadie referencia: {sueltos:?}"
    );
}
