//! `GET`/`POST`/`DELETE /api/notes` (`notes.go`).
//!
//! Cada test que escribe corre en un proceso hijo con su propio
//! `ORUSCRIBE_DATA_DIR`, como los de `settings.rs`: nunca se toca el
//! `notes.json` del perfil real.

mod soporte;

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Datelike, Utc};
use orus_app::App;
use serde_json::{Value, json};
use soporte::{
    AppDePrueba, ConSesion, ConectorPrueba, afirmar_cabeceras, con_runtime, en_datos_aislados,
};

const SIN_SESION: &str = "no has iniciado sesión: inicia sesión para escribir en el bloc";

async fn app_con_sesion() -> App {
    let app = AppDePrueba::nuevo()
        .con_store(Box::new(ConSesion))
        .con_connector(Arc::new(ConectorPrueba))
        .construir();
    app.import_token("syn_inmune")
        .await
        .expect("importar el token de prueba");
    app
}

async fn pedir(base: &str, metodo: &str, ruta: &str, cuerpo: Option<String>) -> reqwest::Response {
    let cliente = reqwest::Client::new();
    let url = format!("{base}{ruta}");
    let peticion = match metodo {
        "GET" => cliente.get(&url),
        "POST" => cliente.post(&url),
        "DELETE" => cliente.delete(&url),
        "PUT" => cliente.put(&url),
        otro => panic!("método no esperado: {otro}"),
    };
    match cuerpo {
        Some(cuerpo) => peticion
            .header("content-type", "application/json")
            .body(cuerpo)
            .send()
            .await
            .expect("petición HTTP"),
        None => peticion.send().await.expect("petición HTTP"),
    }
}

async fn cuerpo_json(response: reqwest::Response) -> Value {
    let texto = response.text().await.expect("leer el cuerpo");
    assert!(
        texto.ends_with('\n'),
        "el cuerpo no termina en \\n: {texto:?}"
    );
    serde_json::from_str(&texto).expect("JSON válido")
}

async fn error_de(response: reqwest::Response) -> (u16, String) {
    let status = response.status().as_u16();
    let body = cuerpo_json(response).await;
    (
        status,
        body["error"].as_str().unwrap_or_default().to_owned(),
    )
}

fn leer_notas_del_disco() -> Value {
    serde_json::from_slice(
        &std::fs::read(orus_config::data_dir().join("notes.json")).expect("leer notes.json"),
    )
    .expect("notes.json válido")
}

#[test]
fn dos_procesos_no_sobrescriben_notas_al_guardar() {
    en_datos_aislados("dos_procesos_no_sobrescriben_notas_al_guardar", || {
        con_runtime(async {
            let app = app_con_sesion().await;
            let (base, server) = soporte::levantar(app).await;
            let ruta = orus_config::data_dir().join("notes.json.lock");
            let archivo = std::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(false)
                .open(&ruta)
                .expect("archivo de candado");
            archivo.lock().expect("tomar el candado");

            let cliente = reqwest::Client::new();
            let cuerpo = json!({"id": "n1", "title": "t", "body": "b"});
            let bloqueada = tokio::time::timeout(
                Duration::from_millis(400),
                cliente
                    .post(format!("{base}/api/notes"))
                    .json(&cuerpo)
                    .send(),
            )
            .await;
            assert!(bloqueada.is_err(), "la escritura entró sin el candado");

            drop(archivo);
            let respuesta = cliente
                .post(format!("{base}/api/notes"))
                .json(&cuerpo)
                .send()
                .await
                .expect("POST tras soltar el candado");
            assert_eq!(respuesta.status(), 200);
            let disco = leer_notas_del_disco();
            assert_eq!(disco["u1@syncra"][0]["id"], "n1", "{disco}");

            server.abort();
            let _ = server.await;
        });
    });
}

#[test]
fn get_notas_sin_sesion_da_200_con_lista_vacia() {
    en_datos_aislados("get_notas_sin_sesion_da_200_con_lista_vacia", || {
        con_runtime(async {
            let app = AppDePrueba::nuevo().construir();
            let (base, server) = soporte::levantar(app).await;

            let response = pedir(&base, "GET", "/api/notes", None).await;
            assert_eq!(response.status(), 200);
            afirmar_cabeceras(&response);
            let texto = response.text().await.expect("leer el cuerpo");
            assert!(
                texto.contains("\"notes\":[]"),
                "sin sesión la lista tiene que ser [] y nunca null: {texto}"
            );
            let body: Value = serde_json::from_str(&texto).expect("JSON válido");
            assert!(body["notes"].as_array().is_some_and(Vec::is_empty));

            server.abort();
            let _ = server.await;
        });
    });
}

#[test]
fn post_sin_id_da_500_aunque_no_haya_sesion() {
    en_datos_aislados("post_sin_id_da_500_aunque_no_haya_sesion", || {
        con_runtime(async {
            let app = AppDePrueba::nuevo().construir();
            let (base, server) = soporte::levantar(app).await;

            let response = pedir(&base, "POST", "/api/notes", Some("{}".to_owned())).await;
            let (status, error) = error_de(response).await;
            assert_eq!(status, 500, "el id se valida antes que la sesión");
            assert_eq!(error, "la nota no tiene identificador");

            server.abort();
            let _ = server.await;
        });
    });
}

#[test]
fn post_sin_sesion_con_id_da_401_con_el_texto_completo() {
    en_datos_aislados(
        "post_sin_sesion_con_id_da_401_con_el_texto_completo",
        || {
            con_runtime(async {
                let app = AppDePrueba::nuevo().construir();
                let (base, server) = soporte::levantar(app).await;

                let response = pedir(
                    &base,
                    "POST",
                    "/api/notes",
                    Some(json!({"id": "n1", "title": "t", "body": "b"}).to_string()),
                )
                .await;
                let (status, error) = error_de(response).await;
                assert_eq!(status, 401);
                assert_eq!(error, SIN_SESION);

                server.abort();
                let _ = server.await;
            });
        },
    );
}

#[test]
fn delete_sin_id_da_500_aunque_no_haya_sesion() {
    en_datos_aislados("delete_sin_id_da_500_aunque_no_haya_sesion", || {
        con_runtime(async {
            let app = AppDePrueba::nuevo().construir();
            let (base, server) = soporte::levantar(app).await;

            let response = pedir(&base, "DELETE", "/api/notes", None).await;
            let (status, error) = error_de(response).await;
            assert_eq!(status, 500, "el id se valida antes que la sesión");
            assert_eq!(error, "falta el identificador");

            server.abort();
            let _ = server.await;
        });
    });
}

#[test]
fn delete_sin_sesion_con_id_da_401_con_el_texto_completo() {
    en_datos_aislados(
        "delete_sin_sesion_con_id_da_401_con_el_texto_completo",
        || {
            con_runtime(async {
                let app = AppDePrueba::nuevo().construir();
                let (base, server) = soporte::levantar(app).await;

                let response = pedir(&base, "DELETE", "/api/notes?id=n1", None).await;
                let (status, error) = error_de(response).await;
                assert_eq!(status, 401);
                assert_eq!(error, SIN_SESION);

                server.abort();
                let _ = server.await;
            });
        },
    );
}

#[test]
fn post_guarda_lee_y_borra() {
    en_datos_aislados("post_guarda_lee_y_borra", || {
        con_runtime(async {
            let app = app_con_sesion().await;
            let (base, server) = soporte::levantar(app).await;

            let response = pedir(
                &base,
                "POST",
                "/api/notes",
                Some(
                    json!({"id": "n-inmune", "title": "Nota inmune", "body": "cuerpo"}).to_string(),
                ),
            )
            .await;
            assert_eq!(response.status(), 200);
            afirmar_cabeceras(&response);
            let guardada = cuerpo_json(response).await;
            assert_eq!(guardada["id"], "n-inmune");
            assert_eq!(guardada["title"], "Nota inmune");
            assert_eq!(guardada["body"], "cuerpo");
            let fecha: DateTime<Utc> = guardada["updated_at"]
                .as_str()
                .expect("updated_at como texto RFC3339")
                .parse()
                .expect("updated_at RFC3339");
            assert!(
                (Utc::now() - fecha).num_seconds().abs() < 60,
                "updated_at tiene que ser la hora del servidor: {}",
                guardada["updated_at"]
            );

            let disco = leer_notas_del_disco();
            assert_eq!(
                disco["u1@syncra"][0]["id"], "n-inmune",
                "el archivo es un mapa por dueño: {disco}"
            );

            let lista = cuerpo_json(pedir(&base, "GET", "/api/notes", None).await).await;
            assert_eq!(lista["notes"][0]["title"], "Nota inmune");

            let baja = pedir(&base, "DELETE", "/api/notes?id=n-inmune", None).await;
            assert_eq!(baja.status(), 200);
            let ok = cuerpo_json(baja).await;
            assert_eq!(ok["ok"], true);

            let vacia = cuerpo_json(pedir(&base, "GET", "/api/notes", None).await).await;
            assert!(
                vacia["notes"].as_array().is_some_and(Vec::is_empty),
                "la nota no se borró: {vacia}"
            );

            let otra_vez = pedir(&base, "DELETE", "/api/notes?id=n-inmune", None).await;
            assert_eq!(
                otra_vez.status(),
                200,
                "borrar algo inexistente también da 200"
            );

            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;

                let archivo = std::fs::metadata(orus_config::data_dir().join("notes.json"))
                    .expect("metadata de notes.json");
                assert_eq!(
                    archivo.permissions().mode() & 0o777,
                    0o600,
                    "notes.json tiene que ser 600"
                );
                let carpeta = std::fs::metadata(orus_config::data_dir())
                    .expect("metadata de la carpeta de datos");
                assert_eq!(
                    carpeta.permissions().mode() & 0o777,
                    0o700,
                    "la carpeta de datos tiene que ser 700"
                );
            }

            server.abort();
            let _ = server.await;
        });
    });
}

#[test]
fn post_ordena_por_updated_at_descendente() {
    en_datos_aislados("post_ordena_por_updated_at_descendente", || {
        con_runtime(async {
            let app = app_con_sesion().await;
            let (base, server) = soporte::levantar(app).await;

            for (id, titulo) in [("n1", "Primera"), ("n2", "Segunda")] {
                let response = pedir(
                    &base,
                    "POST",
                    "/api/notes",
                    Some(json!({"id": id, "title": titulo, "body": ""}).to_string()),
                )
                .await;
                assert_eq!(response.status(), 200);
                tokio::time::sleep(Duration::from_millis(5)).await;
            }

            let lista = cuerpo_json(pedir(&base, "GET", "/api/notes", None).await).await;
            assert_eq!(lista["notes"][0]["id"], "n2");
            assert_eq!(lista["notes"][1]["id"], "n1");

            server.abort();
            let _ = server.await;
        });
    });
}

#[test]
fn post_reemplaza_por_id_y_renueva_la_hora() {
    en_datos_aislados("post_reemplaza_por_id_y_renueva_la_hora", || {
        con_runtime(async {
            let app = app_con_sesion().await;
            let (base, server) = soporte::levantar(app).await;

            let primera = cuerpo_json(
                pedir(
                    &base,
                    "POST",
                    "/api/notes",
                    Some(json!({"id": "n1", "title": "Vieja", "body": "b"}).to_string()),
                )
                .await,
            )
            .await;
            tokio::time::sleep(Duration::from_millis(5)).await;
            let segunda = cuerpo_json(
                pedir(
                    &base,
                    "POST",
                    "/api/notes",
                    Some(json!({"id": "n1", "title": "Nueva", "body": "b2"}).to_string()),
                )
                .await,
            )
            .await;

            assert_eq!(segunda["title"], "Nueva");
            assert!(
                segunda["updated_at"].as_str() > primera["updated_at"].as_str(),
                "el upsert renueva updated_at"
            );
            let lista = cuerpo_json(pedir(&base, "GET", "/api/notes", None).await).await;
            assert_eq!(lista["notes"].as_array().expect("arreglo").len(), 1);

            server.abort();
            let _ = server.await;
        });
    });
}

#[test]
fn post_ignora_el_updated_at_del_cliente() {
    en_datos_aislados("post_ignora_el_updated_at_del_cliente", || {
        con_runtime(async {
            let app = app_con_sesion().await;
            let (base, server) = soporte::levantar(app).await;

            let guardada = cuerpo_json(
                pedir(
                    &base,
                    "POST",
                    "/api/notes",
                    Some(
                        json!({
                            "id": "n1",
                            "title": "t",
                            "body": "b",
                            "updated_at": "2000-01-01T00:00:00Z"
                        })
                        .to_string(),
                    ),
                )
                .await,
            )
            .await;

            let fecha: DateTime<Utc> = guardada["updated_at"]
                .as_str()
                .expect("updated_at")
                .parse()
                .expect("RFC3339");
            assert!(
                fecha.year() >= 2025,
                "el servidor fija la hora e ignora la del cliente: {fecha}"
            );

            server.abort();
            let _ = server.await;
        });
    });
}

#[test]
fn post_con_campos_null_los_trata_como_vacios() {
    en_datos_aislados("post_con_campos_null_los_trata_como_vacios", || {
        con_runtime(async {
            let app = app_con_sesion().await;
            let (base, server) = soporte::levantar(app).await;

            let response = pedir(
                &base,
                "POST",
                "/api/notes",
                Some(json!({"id": "n-null", "title": null, "body": null}).to_string()),
            )
            .await;
            assert_eq!(response.status(), 200);
            let guardada = cuerpo_json(response).await;
            assert_eq!(guardada["title"], "");
            assert_eq!(guardada["body"], "");

            let sin_id = pedir(
                &base,
                "POST",
                "/api/notes",
                Some(json!({"id": null, "title": "t"}).to_string()),
            )
            .await;
            let (status, error) = error_de(sin_id).await;
            assert_eq!(status, 500, "null en id deja el id vacío, como Go");
            assert_eq!(error, "la nota no tiene identificador");

            server.abort();
            let _ = server.await;
        });
    });
}

#[test]
fn json_invalido_da_500() {
    en_datos_aislados("json_invalido_da_500", || {
        con_runtime(async {
            let app = app_con_sesion().await;
            let (base, server) = soporte::levantar(app).await;

            let response = pedir(&base, "POST", "/api/notes", Some("{".to_owned())).await;
            let (status, error) = error_de(response).await;
            assert_eq!(status, 500);
            assert!(!error.is_empty(), "el error de serde viaja en el cuerpo");

            server.abort();
            let _ = server.await;
        });
    });
}

#[test]
fn cuerpo_pasado_de_tope_da_413_cuerpo_grande() {
    en_datos_aislados("cuerpo_pasado_de_tope_da_413_cuerpo_grande", || {
        con_runtime(async {
            let app = app_con_sesion().await;
            let (base, server) = soporte::levantar(app).await;

            let grande = format!(
                "{{\"id\":\"n1\",\"title\":\"t\",\"body\":\"{}\"}}",
                "a".repeat(1 << 20)
            );
            let response = pedir(&base, "POST", "/api/notes", Some(grande)).await;
            assert_eq!(response.status(), 413);
            let body = cuerpo_json(response).await;
            assert_eq!(body["code"], "cuerpo_grande");
            assert_eq!(
                body["error"],
                "el cuerpo de la petición pasa del máximo de la ruta"
            );

            server.abort();
            let _ = server.await;
        });
    });
}

#[test]
fn otro_metodo_da_405() {
    en_datos_aislados("otro_metodo_da_405", || {
        con_runtime(async {
            let app = app_con_sesion().await;
            let (base, server) = soporte::levantar(app).await;

            let response = pedir(&base, "PUT", "/api/notes", Some("{}".to_owned())).await;
            assert_eq!(response.status(), 405);

            server.abort();
            let _ = server.await;
        });
    });
}

#[test]
fn la_lista_vieja_se_migra_al_dueno_de_la_sesion() {
    en_datos_aislados("la_lista_vieja_se_migra_al_dueno_de_la_sesion", || {
        con_runtime(async {
            let carpeta = orus_config::data_dir();
            std::fs::create_dir_all(&carpeta).expect("carpeta de datos");
            std::fs::write(
                carpeta.join("notes.json"),
                json!([{
                    "id": "vieja",
                    "title": "Nota vieja",
                    "body": "cuerpo",
                    "updated_at": "2026-01-01T00:00:00Z"
                }])
                .to_string(),
            )
            .expect("sembrar notes.json");

            let app = app_con_sesion().await;
            let (base, server) = soporte::levantar(app).await;

            let lista = cuerpo_json(pedir(&base, "GET", "/api/notes", None).await).await;
            assert_eq!(lista["notes"][0]["id"], "vieja");

            let disco = leer_notas_del_disco();
            assert_eq!(
                disco["u1@syncra"][0]["id"], "vieja",
                "la lista vieja se reescribe como mapa por dueño: {disco}"
            );

            server.abort();
            let _ = server.await;
        });
    });
}

#[test]
fn una_sesion_no_ve_las_notas_de_otro_dueno() {
    en_datos_aislados("una_sesion_no_ve_las_notas_de_otro_dueno", || {
        con_runtime(async {
            let carpeta = orus_config::data_dir();
            std::fs::create_dir_all(&carpeta).expect("carpeta de datos");
            std::fs::write(
                carpeta.join("notes.json"),
                json!({
                    "u1@syncra": [{
                        "id": "propia",
                        "title": "Propia",
                        "body": "",
                        "updated_at": "2026-01-01T00:00:00Z"
                    }],
                    "u2@otra": [{
                        "id": "ajena",
                        "title": "Ajena",
                        "body": "",
                        "updated_at": "2026-01-02T00:00:00Z"
                    }]
                })
                .to_string(),
            )
            .expect("sembrar notes.json");

            let app = app_con_sesion().await;
            let (base, server) = soporte::levantar(app).await;

            let lista = cuerpo_json(pedir(&base, "GET", "/api/notes", None).await).await;
            let ids: Vec<&str> = lista["notes"]
                .as_array()
                .expect("arreglo")
                .iter()
                .map(|nota| nota["id"].as_str().unwrap_or_default())
                .collect();
            assert_eq!(ids, vec!["propia"]);

            server.abort();
            let _ = server.await;
        });
    });
}
