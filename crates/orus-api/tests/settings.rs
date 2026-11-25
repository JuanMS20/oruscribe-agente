//! `GET`/`POST /api/settings`: leer y cambiar los ajustes del panel.
//!
//! Los tests que guardan corren en un proceso hijo con `ORUSCRIBE_DATA_DIR`
//! temporal: `std::env::set_var` es `unsafe` en edition 2024 y este crate
//! prohíbe `unsafe`. Nunca se toca el `settings.json` real.

mod soporte;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use orus_api::{Api, Autostart, BellStatus};
use orus_app::App;
use orus_config::{Locale, Settings};
use serde_json::{Value, json};
use soporte::{AppDePrueba, Timbre, con_runtime, cuerpo_json, en_datos_aislados, levantar_api};

fn app_de_prueba(settings: Settings) -> App {
    AppDePrueba::nuevo().con_settings(settings).construir()
}

fn api(app: App, version: &str, timbre: Option<Arc<dyn BellStatus>>) -> Api {
    let api = Api::new(Arc::new(app)).with_version(version.to_owned());
    match timbre {
        Some(timbre) => api.with_bell_status(timbre),
        None => api,
    }
}

async fn error_de(response: reqwest::Response) -> (u16, String) {
    let status = response.status().as_u16();
    let body = cuerpo_json(response).await;
    (
        status,
        body["error"].as_str().unwrap_or_default().to_owned(),
    )
}

async fn leer(base: &str) -> reqwest::Response {
    reqwest::Client::new()
        .get(format!("{base}/api/settings"))
        .send()
        .await
        .expect("pedir ajustes")
}

async fn cambiar(base: &str, cuerpo: Value) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("{base}/api/settings"))
        .json(&cuerpo)
        .send()
        .await
        .expect("cambiar ajustes")
}

fn guardar_settings(valores: Value) {
    let datos = orus_config::data_dir();
    std::fs::create_dir_all(&datos).expect("carpeta de datos");
    std::fs::write(
        datos.join("settings.json"),
        serde_json::to_vec(&valores).expect("serializar ajustes"),
    )
    .expect("escribir ajustes");
}

fn leer_settings_del_disco() -> Value {
    serde_json::from_slice(
        &std::fs::read(orus_config::data_dir().join("settings.json")).expect("leer settings.json"),
    )
    .expect("settings.json válido")
}

fn ajustes(locale: Locale, autostart_asked: bool, auto_update: Option<bool>) -> Settings {
    Settings {
        locale,
        autostart_asked,
        auto_update,
        ..Settings::default()
    }
}

struct AutostartFalso(AtomicBool);

impl Autostart for AutostartFalso {
    fn activo(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }

    fn cambiar(&self, encendido: bool) -> Result<(), String> {
        self.0.store(encendido, Ordering::SeqCst);
        Ok(())
    }
}

#[tokio::test]
async fn get_settings_devuelve_todas_las_claves() {
    let app = app_de_prueba(ajustes(Locale::Spanish, true, Some(false)));
    let timbre = Arc::new(Timbre {
        encendido: true,
        error: "timbre roto".to_owned(),
    });
    let (base, server) = levantar_api(api(app, "0.0.0-test", Some(timbre))).await;

    let response = leer(&base).await;
    assert_eq!(response.status(), 200);
    assert_eq!(
        response.headers()["cache-control"],
        "no-store",
        "la respuesta se puede cachear"
    );
    let info = cuerpo_json(response).await;

    assert_eq!(info["version"], "0.0.0-test");
    assert_eq!(
        info["dataDir"],
        orus_config::data_dir().display().to_string()
    );
    assert_eq!(info["endpoint"], orus_org::DEFAULT_ENDPOINT);
    assert_eq!(info["locale"], "es");
    assert_eq!(info["autostart"], false);
    assert_eq!(info["autostartAsked"], true);
    assert_eq!(info["autoUpdate"], false);
    assert_eq!(info["doorbell"], true);
    assert_eq!(info["bellError"], "timbre roto");
    assert_eq!(info["platform"], std::env::consts::OS);

    let mut claves: Vec<String> = info
        .as_object()
        .expect("objeto de ajustes")
        .keys()
        .cloned()
        .collect();
    claves.sort();
    assert_eq!(
        claves,
        vec![
            "autoUpdate",
            "autostart",
            "autostartAsked",
            "bellError",
            "dataDir",
            "doorbell",
            "endpoint",
            "locale",
            "platform",
            "telemetry",
            "version"
        ]
    );

    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn get_settings_sin_sesion_da_200_y_omite_bell_error() {
    let app = app_de_prueba(Settings::default());
    let (base, server) = levantar_api(api(app, "", None)).await;

    let response = leer(&base).await;
    assert_eq!(response.status(), 200);
    let info = cuerpo_json(response).await;
    assert_eq!(info["version"], "");
    assert_eq!(info["doorbell"], false);
    assert!(
        info.get("bellError").is_none(),
        "bellError vacío se omite: {info}"
    );

    server.abort();
    let _ = server.await;
}

#[test]
fn post_locale_en_guarda_y_devuelve() {
    en_datos_aislados("post_locale_en_guarda_y_devuelve", || {
        con_runtime(async {
            guardar_settings(json!({"locale": "es"}));
            let app = app_de_prueba(ajustes(Locale::Spanish, false, None));
            let (base, server) = levantar_api(api(app, "0.0.0-test", None)).await;

            let response = cambiar(&base, json!({"locale": "en"})).await;
            assert_eq!(response.status(), 200);
            let info = cuerpo_json(response).await;
            assert_eq!(info["locale"], "en");
            assert_eq!(info["version"], "0.0.0-test");

            let despues = cuerpo_json(leer(&base).await).await;
            assert_eq!(despues["locale"], "en", "un GET posterior lo confirma");
            assert_eq!(leer_settings_del_disco()["locale"], "en");

            server.abort();
            let _ = server.await;
        });
    });
}

#[tokio::test]
async fn post_locale_desconocido_da_500_sin_cambiar_la_memoria() {
    let app = app_de_prueba(ajustes(Locale::Spanish, false, None));
    let (base, server) = levantar_api(api(app, "", None)).await;

    let response = cambiar(&base, json!({"locale": "fr"})).await;
    let (status, error) = error_de(response).await;
    assert_eq!(status, 500);
    assert_eq!(error, "idioma desconocido: \"fr\"");

    let despues = cuerpo_json(leer(&base).await).await;
    assert_eq!(despues["locale"], "es", "el rechazo no cambia la memoria");

    server.abort();
    let _ = server.await;
}

#[test]
fn post_autostart_da_500_aunque_sea_false() {
    en_datos_aislados("post_autostart_da_500_aunque_sea_false", || {
        con_runtime(async {
            guardar_settings(json!({"locale": "es"}));
            let app = app_de_prueba(ajustes(Locale::Spanish, false, None));
            let (base, server) = levantar_api(api(app, "", None)).await;

            for valor in [true, false] {
                let response = cambiar(&base, json!({"autostart": valor})).await;
                let (status, error) = error_de(response).await;
                assert_eq!(status, 500, "autostart={valor}");
                assert_eq!(error, "este equipo no puede arrancar la app solo");
            }

            let despues = cuerpo_json(leer(&base).await).await;
            assert_eq!(despues["locale"], "es");
            assert_eq!(despues["autostart"], false);

            server.abort();
            let _ = server.await;
        });
    });
}

struct AutostartQueFalla;

impl Autostart for AutostartQueFalla {
    fn activo(&self) -> bool {
        false
    }

    fn cambiar(&self, _encendido: bool) -> Result<(), String> {
        Err("el registro está ocupado".to_owned())
    }
}

/// Issue #129: si el arranque falla, los ajustes ya tienen que estar guardados;
/// si no, el panel y el sistema quedan desincronizados después de reiniciar.
#[test]
fn fallo_del_arranque_no_descarta_los_ajustes_guardados() {
    en_datos_aislados(
        "fallo_del_arranque_no_descarta_los_ajustes_guardados",
        || {
            con_runtime(async {
                guardar_settings(json!({"locale": "es"}));
                let app = app_de_prueba(ajustes(Locale::Spanish, false, None));
                let api = Api::new(Arc::new(app))
                    .with_version("0.0.0-test".to_owned())
                    .with_autostart(Arc::new(AutostartQueFalla));
                let (base, server) = levantar_api(api).await;

                let response = cambiar(&base, json!({"locale": "en", "autostart": true})).await;
                let (status, error) = error_de(response).await;
                assert_eq!(status, 500, "el fallo del arranque sigue siendo 500");
                assert_eq!(error, "el registro está ocupado");

                assert_eq!(
                    leer_settings_del_disco()["locale"],
                    "en",
                    "los ajustes se guardan antes de tocar el arranque"
                );

                server.abort();
                let _ = server.await;
            });
        },
    );
}

#[test]
fn post_autostart_lo_cambia_cuando_el_borde_lo_soporta() {
    en_datos_aislados(
        "post_autostart_lo_cambia_cuando_el_borde_lo_soporta",
        || {
            con_runtime(async {
                guardar_settings(json!({"locale": "es"}));
                let app = app_de_prueba(ajustes(Locale::Spanish, false, None));
                let falso = Arc::new(AutostartFalso(AtomicBool::new(false)));
                let api = Api::new(Arc::new(app))
                    .with_version("0.0.0-test".to_owned())
                    .with_autostart(falso.clone());
                let (base, server) = levantar_api(api).await;

                let response =
                    cambiar(&base, json!({"autostart": true, "autostartAsked": true})).await;
                assert_eq!(response.status(), 200);
                let info = cuerpo_json(response).await;
                assert_eq!(info["autostart"], true, "el panel lee el estado real");
                assert_eq!(info["autostartAsked"], true);
                assert!(falso.activo(), "el borde recibió el cambio");
                assert_eq!(
                    leer_settings_del_disco()["autostart_asked"],
                    true,
                    "el aviso de ya preguntado se guarda"
                );

                let response = cambiar(&base, json!({"autostart": false})).await;
                assert_eq!(response.status(), 200);
                assert_eq!(cuerpo_json(response).await["autostart"], false);
                assert!(!falso.activo());

                server.abort();
                let _ = server.await;
            });
        },
    );
}

#[test]
fn post_vacio_y_locale_vacio_no_cambian_nada() {
    en_datos_aislados("post_vacio_y_locale_vacio_no_cambian_nada", || {
        con_runtime(async {
            guardar_settings(json!({
                "locale": "en",
                "autostart_asked": true,
                "auto_update": false
            }));
            let app = app_de_prueba(ajustes(Locale::English, true, Some(false)));
            let (base, server) = levantar_api(api(app, "", None)).await;

            for cuerpo in [json!({}), json!({"locale": ""}), json!({"locale": null})] {
                let response = cambiar(&base, cuerpo.clone()).await;
                assert_eq!(response.status(), 200, "cuerpo {cuerpo}");
                let info = cuerpo_json(response).await;
                assert_eq!(info["locale"], "en");
                assert_eq!(info["autostartAsked"], true);
                assert_eq!(info["autoUpdate"], false);
            }

            let disco = leer_settings_del_disco();
            assert_eq!(disco["locale"], "en");
            assert_eq!(disco["autostart_asked"], true);
            assert_eq!(disco["auto_update"], false);

            server.abort();
            let _ = server.await;
        });
    });
}

#[test]
fn fallo_de_guardado_no_cambia_la_memoria() {
    en_datos_aislados("fallo_de_guardado_no_cambia_la_memoria", || {
        con_runtime(async {
            // Un settings.json que es carpeta hace fallar el rename final.
            let datos = orus_config::data_dir();
            std::fs::create_dir_all(&datos).expect("carpeta de datos");
            std::fs::create_dir(datos.join("settings.json"))
                .expect("bloquear el settings.json con una carpeta");

            let app = app_de_prueba(ajustes(Locale::Spanish, false, None));
            let (base, server) = levantar_api(api(app, "", None)).await;

            let response = cambiar(&base, json!({"locale": "en", "autostartAsked": true})).await;
            assert_eq!(response.status(), 500);

            let despues = cuerpo_json(leer(&base).await).await;
            assert_eq!(despues["locale"], "es", "la memoria no cambia");
            assert_eq!(despues["autostartAsked"], false);

            server.abort();
            let _ = server.await;
        });
    });
}
