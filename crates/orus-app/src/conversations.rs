//! Conversaciones de la ventana: listar, abrir, borrar y migrar.
//!
//! Port de `internal/app/conversations.go:17-230`. El formato en disco, la
//! escritura atómica y la validación viven en `orus_harness::sessions`; aquí
//! solo se eligen la carpeta y el prefijo histórico `c-` de la ventana.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;
use std::sync::{LazyLock, Mutex};
use std::time::SystemTime;

use chrono::{DateTime, Utc};
use orus_harness::sessions::{self, Info, Session, SessionsError, Store, VERSION};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::sync::lock;

/// Lo que la lista de la ventana necesita, sin los mensajes
/// (`conversations.go:18-23`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConversationInfo {
    #[serde(rename = "id")]
    pub id: String,
    #[serde(rename = "title")]
    pub title: String,
    #[serde(rename = "at")]
    pub at: DateTime<Utc>,
    #[serde(rename = "messages")]
    pub messages: usize,
}

/// `config.DataDir()/conversations` (`conversations.go:36`).
pub fn conversations_dir() -> PathBuf {
    orus_config::data_dir().join("conversations")
}

/// El mismo almacén del arnés con la carpeta y el prefijo que la ventana ya
/// tenía (`conversations.go:38-47`). El prefijo conserva `c-<id>.json`.
pub fn conversation_store() -> Store {
    Store {
        dir: conversations_dir(),
        prefix: "c-".to_owned(),
    }
}

/// Las conversaciones guardadas por la ventana.
#[derive(Debug, Clone)]
pub struct Conversations {
    pub store: Store,
}

/// Huella de un archivo: nombre, tamaño y fecha de modificación. Comparar
/// huellas solo mira metadata; leer y parsear el contenido es lo caro (#95).
type Huella = (String, u64, Option<SystemTime>);

/// Cache por directorio: la última lista calculada con la huella que la
/// produjo, y los archivos que `migrate` ya revisó. Varias instancias de
/// `Conversations` apuntan al mismo directorio: el cache es del proceso.
#[derive(Default)]
struct CacheConversaciones {
    migrados: HashSet<Huella>,
    lista: Option<(Vec<Huella>, Vec<ConversationInfo>)>,
}

static CACHE: LazyLock<Mutex<HashMap<PathBuf, CacheConversaciones>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Huellas de los archivos de conversación del directorio, ordenadas por
/// nombre. Solo metadata: ningún contenido se lee acá.
fn huellas_dir(dir: &std::path::Path, prefix: &str) -> Vec<Huella> {
    let Ok(entradas) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut huellas = Vec::new();
    for entrada in entradas.flatten() {
        if entrada.file_type().is_ok_and(|tipo| tipo.is_dir()) {
            continue;
        }
        let Some(nombre) = entrada.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let Some(id) = nombre
            .strip_prefix(prefix)
            .and_then(|resto| resto.strip_suffix(".json"))
        else {
            continue;
        };
        if !sessions::valid_id(id) {
            continue;
        }
        if let Some(huella) = huella_de(&entrada, nombre) {
            huellas.push(huella);
        }
    }
    huellas.sort();
    huellas
}

fn huella_de(entrada: &fs::DirEntry, nombre: String) -> Option<Huella> {
    let meta = entrada.metadata().ok()?;
    Some((nombre, meta.len(), meta.modified().ok()))
}

fn lista_cacheada(dir: &std::path::Path, huellas: &[Huella]) -> Option<Vec<ConversationInfo>> {
    let cache = lock(&CACHE);
    let entrada = cache.get(dir)?;
    let (huella_cache, lista) = entrada.lista.as_ref()?;
    (huella_cache.as_slice() == huellas).then(|| lista.clone())
}

fn guardar_lista_cache(dir: &std::path::Path, huellas: Vec<Huella>, lista: &[ConversationInfo]) {
    let mut cache = lock(&CACHE);
    let entrada = cache.entry(dir.to_path_buf()).or_default();
    // Las huellas de archivos que ya no existen no se acumulan.
    let nombres: HashSet<&str> = huellas
        .iter()
        .map(|(nombre, _, _)| nombre.as_str())
        .collect();
    entrada
        .migrados
        .retain(|(nombre, _, _)| nombres.contains(nombre.as_str()));
    entrada.lista = Some((huellas, lista.to_vec()));
}

impl Conversations {
    pub fn new() -> Self {
        Self {
            store: conversation_store(),
        }
    }

    /// Lista de la más reciente a la más vieja (`conversations.go:49-61`).
    /// Con el directorio sin cambios la lista sale del cache: no se relee ni
    /// se reparsea el contenido de cada archivo en cada llamada (#95).
    pub fn list(&self) -> Result<Vec<ConversationInfo>, SessionsError> {
        let huellas = huellas_dir(&self.store.dir, &self.store.prefix);
        if let Some(lista) = lista_cacheada(&self.store.dir, &huellas) {
            return Ok(lista);
        }
        self.migrate();
        // La migración pudo cambiar archivos: la huella que se cachea es la
        // del directorio después de migrar.
        let huellas = huellas_dir(&self.store.dir, &self.store.prefix);
        let mut salida: Vec<ConversationInfo> =
            self.store.list()?.into_iter().map(conversacion).collect();
        // Un archivo con el prefijo que el store no puede leer no desaparece:
        // se lista como ilegible para que la persona sepa que sigue ahí.
        let mut vistos: HashSet<String> = salida.iter().map(|c| c.id.clone()).collect();
        if let Ok(entradas) = fs::read_dir(&self.store.dir) {
            for entrada in entradas.flatten() {
                let Some(nombre) = entrada.file_name().to_str().map(str::to_owned) else {
                    continue;
                };
                let Some(id) = nombre
                    .strip_prefix(&self.store.prefix)
                    .and_then(|resto| resto.strip_suffix(".json"))
                else {
                    continue;
                };
                if !sessions::valid_id(id) || !vistos.insert(id.to_owned()) {
                    continue;
                }
                let datos = fs::read(entrada.path()).unwrap_or_default();
                let objeto = serde_json::from_slice::<Value>(&datos).ok();
                let futuro = objeto
                    .as_ref()
                    .and_then(|valor| valor.get("version"))
                    .and_then(Value::as_i64)
                    .is_some_and(|version| version > VERSION);
                let vacio = objeto
                    .as_ref()
                    .and_then(Value::as_object)
                    .is_some_and(|objeto| !objeto.contains_key("version") && legacy_vacia(objeto));
                if futuro || vacio {
                    continue;
                }
                let at = entrada
                    .metadata()
                    .and_then(|meta| meta.modified())
                    .map(DateTime::<Utc>::from)
                    .unwrap_or_else(|_| Utc::now());
                salida.push(ConversationInfo {
                    id: id.to_owned(),
                    title: "Conversación ilegible".to_owned(),
                    at,
                    messages: 0,
                });
            }
        }
        guardar_lista_cache(&self.store.dir, huellas, &salida);
        Ok(salida)
    }

    /// Carga una conversación para que el borde la aplique
    /// (`conversations.go:63-85`, sin el estado de pantalla).
    pub fn load(&self, id: &str) -> Result<Session, SessionsError> {
        self.migrate();
        self.store.load(id)
    }

    /// Borra una; con id vacío borra todas (`conversations.go:87-118`). El
    /// reinicio de la conversación abierta lo decide O5-06.
    pub fn delete(&self, id: &str) -> Result<(), SessionsError> {
        if id.is_empty() {
            self.clear()
        } else {
            self.store.delete(id)
        }
    }

    /// Borra solo los archivos de conversación de la ventana
    /// (`conversations.go:120-123`).
    pub fn clear(&self) -> Result<(), SessionsError> {
        self.store.clear()
    }

    /// Guarda la foto completa (`conversations.go:223-230`).
    pub fn save(&self, session: &mut Session) -> Result<(), SessionsError> {
        self.store.save(Some(session))
    }

    /// Id del reloj en nanosegundos; si el reloj no los da, micros
    /// (`conversations.go:214-218`).
    pub fn new_id(&self) -> String {
        let ahora = Utc::now();
        match ahora.timestamp_nanos_opt() {
            Some(nanos) => nanos.to_string(),
            None => ahora.timestamp_micros().to_string(),
        }
    }

    /// Convierte los archivos del formato viejo al compartido
    /// (`conversations.go:125-174`). Es idempotente. Solo lee el contenido de
    /// los archivos cuya huella no fue revisada todavía: un archivo ya
    /// migrado (o ya en el formato nuevo) no se reparsea en cada llamada
    /// (#95).
    pub fn migrate(&self) {
        let Ok(entradas) = fs::read_dir(&self.store.dir) else {
            return;
        };
        let mut cache = lock(&CACHE);
        let revisados = &mut cache.entry(self.store.dir.clone()).or_default().migrados;
        for entrada in entradas.flatten() {
            if entrada.file_type().is_ok_and(|tipo| tipo.is_dir()) {
                continue;
            }
            let Some(nombre) = entrada.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let Some(id) = nombre
                .strip_prefix(&self.store.prefix)
                .and_then(|resto| resto.strip_suffix(".json"))
            else {
                continue;
            };
            if !sessions::valid_id(id) {
                continue;
            }
            let id = id.to_owned();
            let Some(huella) = huella_de(&entrada, nombre) else {
                continue;
            };
            if revisados.contains(&huella) {
                continue;
            }
            let Ok(datos) = fs::read(entrada.path()) else {
                revisados.insert(huella);
                continue;
            };
            let Ok(Value::Object(mut objeto)) = serde_json::from_slice::<Value>(&datos) else {
                revisados.insert(huella);
                continue;
            };
            // Un archivo con `version` ya es del formato compartido: aunque
            // esta versión no lo entienda, no se reescribe como si fuera viejo.
            if objeto.contains_key("version") || !objeto.contains_key("at") {
                revisados.insert(huella);
                continue;
            }
            if legacy_vacia(&objeto) {
                revisados.insert(huella);
                continue;
            }
            let Some(at) = objeto.get("at").cloned() else {
                revisados.insert(huella);
                continue;
            };
            objeto.insert("version".to_owned(), Value::from(VERSION));
            objeto.remove("at");
            objeto.insert("created".to_owned(), at.clone());
            objeto.insert("updated".to_owned(), at);
            let Ok(mut sesion) = serde_json::from_value::<Session>(Value::Object(objeto)) else {
                revisados.insert(huella);
                continue;
            };
            sesion.id = id;
            // Igual que el Go: un archivo que no se puede guardar no corta la
            // migración de los demás. Si el guardado falla, la huella no se
            // marca: el próximo intento vuelve a leerlo.
            if self.store.save(Some(&mut sesion)).is_ok() {
                revisados.insert(huella);
                // El archivo migrado tiene otra huella: se marca también para
                // que la próxima lista no lo vuelva a leer.
                if let Ok(meta) = fs::metadata(entrada.path()) {
                    revisados.insert((
                        entrada.file_name().to_string_lossy().into_owned(),
                        meta.len(),
                        meta.modified().ok(),
                    ));
                }
            }
        }
    }
}

impl Default for Conversations {
    fn default() -> Self {
        Self::new()
    }
}

fn conversacion(info: Info) -> ConversationInfo {
    ConversationInfo {
        id: info.id,
        title: info.title,
        at: info.updated,
        messages: info.messages,
    }
}

/// Los mensajes vacíos y el título vacío no son una conversación
/// (`conversations.go:185-187`).
fn legacy_vacia(objeto: &serde_json::Map<String, Value>) -> bool {
    let sin_mensajes = objeto
        .get("messages")
        .and_then(Value::as_array)
        .is_none_or(Vec::is_empty);
    let sin_titulo = objeto
        .get("title")
        .and_then(Value::as_str)
        .is_none_or(str::is_empty);
    sin_mensajes && sin_titulo
}
