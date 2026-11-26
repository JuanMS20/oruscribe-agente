//! Catálogo público de modelos para el desplegable del agente.
//!
//! La URL es una costura
//! (`Api::with_models_url`) y la caché vive en el borde, una sola por `Api`:
//! guarda el mapa completo del catálogo, no una lista por proveedor.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use axum::Extension;
use axum::extract::Query;
use axum::http::StatusCode;
use axum::response::Response;
use serde::{Deserialize, Deserializer, Serialize};

use crate::{ErrorRuta, write_err, write_json};

pub(crate) const URL_CATALOGO: &str = "https://pi.dev/api/models";
const TOPE_CATALOGO: usize = 8 << 20;
const TTL_CATALOGO: Duration = Duration::from_secs(10 * 60);
const ESPERA_CATALOGO: Duration = Duration::from_secs(12);

/// La URL del catálogo, con tipo propio para no chocar con la versión, que
/// también viaja como `Extension<String>`.
#[derive(Clone)]
pub(crate) struct UrlCatalogo(pub String);

/// Una entrada del selector.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct Modelo {
    id: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    name: String,
}

/// El mapa completo del catálogo y cuándo se leyó.
#[derive(Default)]
pub(crate) struct CacheModelos {
    at: Option<Instant>,
    por_proveedor: HashMap<String, Vec<Modelo>>,
}

#[derive(Deserialize, Default)]
pub(crate) struct Consulta {
    #[serde(default)]
    provider: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct DatosCatalogo {
    #[serde(deserialize_with = "texto_o_vacio")]
    id: String,
    #[serde(deserialize_with = "texto_o_vacio")]
    name: String,
}

fn texto_o_vacio<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(Option::<String>::deserialize(deserializer)?.unwrap_or_default())
}

/// El mapa del catálogo tal como llega: proveedor → clave → datos.
///
/// `null` en cualquier nivel es el mapa/registro vacío de `encoding/json`, no
/// un error de formato, así que cada nivel opcional se conserva.
type Catalogo = HashMap<String, Option<HashMap<String, Option<DatosCatalogo>>>>;

/// `GET /api/agent/models?provider=`.
pub(crate) async fn modelos(
    Extension(UrlCatalogo(url)): Extension<UrlCatalogo>,
    Extension(http): Extension<reqwest::Client>,
    Extension(cache): Extension<Arc<Mutex<CacheModelos>>>,
    Query(consulta): Query<Consulta>,
) -> Response {
    let proveedor = consulta.provider.unwrap_or_default();
    match obtener(&proveedor, &url, &http, &cache).await {
        Ok(lista) => write_json(StatusCode::OK, &serde_json::json!({"models": lista})),
        Err(error) => write_err(error),
    }
}

/// Valida, sirve de la
/// caché si el proveedor está en el mapa vigente y, si no, vuelve a leer el
/// catálogo completo.
pub(crate) async fn obtener(
    proveedor: &str,
    url: &str,
    http: &reqwest::Client,
    cache: &Mutex<CacheModelos>,
) -> Result<Vec<Modelo>, ErrorRuta> {
    let proveedor = proveedor.trim();
    if proveedor.is_empty() {
        return Err(ErrorRuta::Invalido("falta el proveedor".to_owned()));
    }
    if !orus_app::presets_de_proveedores()
        .iter()
        .any(|preset| preset.id == proveedor)
    {
        return Err(ErrorRuta::Invalido("proveedor desconocido".to_owned()));
    }

    {
        let cache = bloqueo(cache);
        if cache.at.is_some_and(|at| at.elapsed() < TTL_CATALOGO)
            && let Some(lista) = cache.por_proveedor.get(proveedor)
        {
            return Ok(lista.clone());
        }
    }

    let catalogo = leer_catalogo(url, http).await?;
    let mut cache = bloqueo(cache);
    cache.at = Some(Instant::now());
    let lista = catalogo.get(proveedor).cloned().unwrap_or_default();
    cache.por_proveedor = catalogo;
    Ok(lista)
}

async fn leer_catalogo(
    url: &str,
    http: &reqwest::Client,
) -> Result<HashMap<String, Vec<Modelo>>, ErrorRuta> {
    let mut respuesta = http
        .get(url)
        .timeout(ESPERA_CATALOGO)
        .send()
        .await
        .map_err(|error| {
            ErrorRuta::Lectura(format!("no se pudo leer el catálogo de modelos: {error}"))
        })?;
    if respuesta.status().as_u16() != 200 {
        return Err(ErrorRuta::Lectura(format!(
            "el catálogo de modelos respondió {}",
            respuesta.status().as_u16()
        )));
    }

    let mut cuerpo = Vec::new();
    while let Some(trozo) = respuesta.chunk().await.map_err(|error| {
        ErrorRuta::Lectura(format!("no se pudo leer el catálogo de modelos: {error}"))
    })? {
        let restante = TOPE_CATALOGO.saturating_sub(cuerpo.len());
        if trozo.len() >= restante {
            cuerpo.extend_from_slice(&trozo[..restante]);
            break;
        }
        cuerpo.extend_from_slice(&trozo);
    }

    // `null` es el mapa vacío para `encoding/json`; un proveedor o una entrada
    // nulos también son mapa/registro vacío, no un error de formato.
    let catalogo: Option<Catalogo> = serde_json::from_slice(&cuerpo).map_err(|_| {
        ErrorRuta::Lectura("el catálogo de modelos no tiene el formato esperado".to_owned())
    })?;
    let catalogo = catalogo.unwrap_or_default();

    let mut por_proveedor = HashMap::with_capacity(catalogo.len());
    for (id_proveedor, modelos) in catalogo {
        let mut lista = Vec::new();
        for (clave, datos) in modelos.unwrap_or_default() {
            let datos = datos.unwrap_or_default();
            let mut id = datos.id;
            // `trim` solo decide si falta: la clave se conserva sin recortar y
            // el id emitido se deja tal cual vino.
            if id.trim().is_empty() {
                id = clave;
            }
            if id.is_empty() {
                continue;
            }
            lista.push(Modelo {
                id,
                name: datos.name.trim().to_owned(),
            });
        }
        lista.sort_by(|a, b| {
            a.name
                .to_lowercase()
                .cmp(&b.name.to_lowercase())
                .then_with(|| a.id.cmp(&b.id))
        });
        por_proveedor.insert(id_proveedor, lista);
    }
    Ok(por_proveedor)
}

fn bloqueo(cache: &Mutex<CacheModelos>) -> MutexGuard<'_, CacheModelos> {
    match cache.lock() {
        Ok(guardia) => guardia,
        Err(envenenado) => envenenado.into_inner(),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use axum::Router;
    use axum::routing::get;

    use super::*;

    async fn catalogo(
        cuerpo: &'static str,
        codigo: u16,
    ) -> (String, Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
        let golpes = Arc::new(AtomicUsize::new(0));
        let contador = Arc::clone(&golpes);
        let app = Router::new().route(
            "/",
            get(move || {
                let cuerpo = cuerpo.to_owned();
                let contador = Arc::clone(&contador);
                async move {
                    contador.fetch_add(1, Ordering::Relaxed);
                    (
                        StatusCode::from_u16(codigo).expect("código de prueba"),
                        cuerpo,
                    )
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listener de prueba");
        let direccion = listener.local_addr().expect("dirección de prueba");
        let servidor = tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("servidor de prueba");
        });
        (format!("http://{direccion}"), golpes, servidor)
    }

    fn cache() -> Mutex<CacheModelos> {
        Mutex::new(CacheModelos::default())
    }

    #[tokio::test]
    async fn agent_models_valida_antes_de_salir_a_la_red() {
        let cache = cache();
        let http = reqwest::Client::new();
        for (proveedor, esperado) in [
            (" ", "falta el proveedor"),
            (" no-existe ", "proveedor desconocido"),
        ] {
            let error = obtener(proveedor, "http://127.0.0.1:1", &http, &cache)
                .await
                .expect_err("el proveedor inválido no puede responder");
            match error {
                ErrorRuta::Invalido(texto) => assert_eq!(texto, esperado),
                otro => panic!("el error no es 400: {otro:?}"),
            }
        }
    }

    #[tokio::test]
    async fn agent_models_trae_solo_el_proveedor_pedido_y_ordena() {
        let (url, golpes, servidor) = catalogo(
            r#"{
                "openrouter": {
                    "z-ultimo": {"id":"z-ultimo","name":"Zeta"},
                    "a-primero": {"id":"a-primero","name":"Alfa"}
                },
                "openai": {"gpt-x": {"id":"gpt-x","name":"GPT X"}}
            }"#,
            200,
        )
        .await;
        let cache = cache();
        let http = reqwest::Client::new();

        let modelos = obtener(" openrouter ", &url, &http, &cache)
            .await
            .expect("el catálogo válido responde");
        assert_eq!(modelos.len(), 2, "solo van los modelos del proveedor");
        assert_eq!(modelos[0].id, "a-primero");
        assert_eq!(modelos[1].id, "z-ultimo");
        assert_eq!(golpes.load(Ordering::Relaxed), 1);

        let otra = obtener("openai", &url, &http, &cache)
            .await
            .expect("la segunda consulta usa la caché");
        assert_eq!(otra[0].id, "gpt-x");
        assert_eq!(
            golpes.load(Ordering::Relaxed),
            1,
            "el mapa completo se cachea una sola vez"
        );
        servidor.abort();
    }

    #[tokio::test]
    async fn agent_models_conserva_el_id_de_la_clave_y_recorta_el_nombre() {
        let (url, _, servidor) = catalogo(
            r#"{"deepseek":{
                "deepseek-chat":{"name":"  DeepSeek Chat  "},
                "con-espacios":{"id":"  id tal cual  ","name":"Nombre"},
                "sin-id":{"id":"   ","name":" "},
                "": {"id":"","name":"vacío"}
            }}"#,
            200,
        )
        .await;
        let cache = cache();
        let http = reqwest::Client::new();

        let modelos = obtener("deepseek", &url, &http, &cache)
            .await
            .expect("catálogo válido");
        assert_eq!(modelos.len(), 3);
        let por_id: HashMap<&str, &Modelo> = modelos
            .iter()
            .map(|modelo| (modelo.id.as_str(), modelo))
            .collect();
        assert_eq!(por_id["deepseek-chat"].name, "DeepSeek Chat");
        assert_eq!(
            por_id["  id tal cual  "].name, "Nombre",
            "el id emitido se deja tal cual vino"
        );
        assert_eq!(
            por_id["sin-id"].name, "",
            "sin id se usa la clave sin recortar; el nombre vacío se omite"
        );
        assert!(!por_id.contains_key(""));
        servidor.abort();
    }

    #[tokio::test]
    async fn agent_models_no_cachea_una_lista_vacia() {
        let (url, golpes, servidor) =
            catalogo(r#"{"openai":{"gpt-x":{"name":"GPT X"}}}"#, 200).await;
        let cache = cache();
        let http = reqwest::Client::new();

        for _ in 0..2 {
            let modelos = obtener("openrouter", &url, &http, &cache)
                .await
                .expect("proveedor válido sin catálogo responde vacío");
            assert!(modelos.is_empty());
        }
        assert_eq!(
            golpes.load(Ordering::Relaxed),
            2,
            "un proveedor ausente del mapa vuelve a pedir el catálogo"
        );
        servidor.abort();
    }

    #[tokio::test]
    async fn agent_models_vuelve_a_pedir_cuando_vence_la_cache() {
        let (url, golpes, servidor) =
            catalogo(r#"{"openai":{"gpt-x":{"name":"GPT X"}}}"#, 200).await;
        let cache = cache();
        let http = reqwest::Client::new();

        obtener("openai", &url, &http, &cache)
            .await
            .expect("primera lectura");
        {
            let mut guardia = bloqueo(&cache);
            guardia.at = Some(
                Instant::now()
                    .checked_sub(TTL_CATALOGO + Duration::from_secs(1))
                    .expect("instante de prueba"),
            );
        }
        obtener("openai", &url, &http, &cache)
            .await
            .expect("lectura vencida");
        assert_eq!(golpes.load(Ordering::Relaxed), 2);
        servidor.abort();
    }

    #[tokio::test]
    async fn agent_models_reporta_codigo_y_formato() {
        let (url, _, servidor) = catalogo("sin json", 200).await;
        let cache = cache();
        let http = reqwest::Client::new();
        let error = obtener("openai", &url, &http, &cache)
            .await
            .expect_err("un catálogo roto no devuelve lista");
        assert!(matches!(
            error,
            ErrorRuta::Lectura(ref texto)
                if texto == "el catálogo de modelos no tiene el formato esperado"
        ));
        servidor.abort();

        let (url, _, servidor) = catalogo("{}", 503).await;
        let error = obtener("openai", &url, &http, &cache)
            .await
            .expect_err("un 503 no devuelve lista");
        assert!(matches!(
            error,
            ErrorRuta::Lectura(ref texto) if texto == "el catálogo de modelos respondió 503"
        ));
        servidor.abort();
    }

    #[tokio::test]
    async fn agent_models_envuelve_el_error_de_red() {
        let cache = cache();
        let http = reqwest::Client::new();
        let error = obtener("openai", "http://127.0.0.1:1", &http, &cache)
            .await
            .expect_err("sin catálogo hay error");
        assert!(matches!(
            error,
            ErrorRuta::Lectura(ref texto)
                if texto.starts_with("no se pudo leer el catálogo de modelos: ")
        ));
    }
}
