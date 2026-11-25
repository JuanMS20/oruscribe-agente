//! Contrato de `internal/app/conversations.go:17-230`.
//!
//! Cada prueba usa una carpeta temporal y archivos escritos a mano: nunca el
//! directorio real de la persona.

use std::fs;
use std::path::Path;

use chrono::{DateTime, Utc};
use orus_app::Conversations;
use orus_harness::sessions::{Session, Store, VERSION};
use serde_json::Value;

fn conversations(dir: &Path) -> Conversations {
    Conversations {
        store: Store {
            dir: dir.to_path_buf(),
            prefix: "c-".to_owned(),
        },
    }
}

fn fecha(texto: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(texto)
        .unwrap()
        .with_timezone(&Utc)
}

fn mensaje(titulo: &str) -> String {
    format!(
        r#"{{"Role":"user","Content":"{titulo}","Images":null,"ToolCalls":null,"ToolCallID":""}}"#
    )
}

/// Un archivo del formato compartido, con fecha y título controlados.
fn escribir_sesion(dir: &Path, id: &str, title: &str, updated: &str) {
    let json = format!(
        r#"{{"version":{VERSION},"id":"{id}","title":"{title}","created":"{updated}","updated":"{updated}","messages":[{}]}}"#,
        mensaje(title)
    );
    fs::write(dir.join(format!("c-{id}.json")), json).unwrap();
}

fn leer_sesion(dir: &Path, id: &str) -> Session {
    let datos = fs::read(dir.join(format!("c-{id}.json"))).unwrap();
    serde_json::from_slice(&datos).unwrap()
}

#[test]
fn list_is_newest_first() {
    let dir = tempfile::tempdir().unwrap();
    escribir_sesion(dir.path(), "100", "vieja", "2026-11-11T10:00:00Z");
    escribir_sesion(dir.path(), "200", "nueva", "2026-11-11T11:00:00Z");

    let lista = conversations(dir.path()).list().unwrap();
    let ids: Vec<&str> = lista.iter().map(|info| info.id.as_str()).collect();
    assert_eq!(ids, ["200", "100"]);
    assert_eq!(lista[0].title, "nueva");
    assert_eq!(lista[0].at, fecha("2026-11-11T11:00:00Z"));
    assert_eq!(lista[0].messages, 1);

    let valor = serde_json::to_value(&lista[0]).unwrap();
    assert_eq!(valor["id"], "200");
    assert_eq!(valor["at"], "2026-11-11T11:00:00Z");
    assert_eq!(valor["messages"], 1);
}

#[test]
fn delete_one_keeps_others() {
    let dir = tempfile::tempdir().unwrap();
    escribir_sesion(dir.path(), "1", "uno", "2026-11-11T10:00:00Z");
    escribir_sesion(dir.path(), "2", "dos", "2026-11-11T11:00:00Z");
    escribir_sesion(dir.path(), "3", "tres", "2026-11-11T12:00:00Z");
    let conv = conversations(dir.path());

    conv.delete("2").unwrap();

    let ids: Vec<String> = conv
        .list()
        .unwrap()
        .into_iter()
        .map(|info| info.id)
        .collect();
    assert_eq!(ids, ["3", "1"]);
    assert!(conv.load("2").is_err(), "la conversación borrada se carga");
    assert!(conv.load("1").is_ok(), "se borró una conversación de más");
}

#[test]
fn delete_all_clears() {
    let dir = tempfile::tempdir().unwrap();
    escribir_sesion(dir.path(), "1", "uno", "2026-11-11T10:00:00Z");
    escribir_sesion(dir.path(), "2", "dos", "2026-11-11T11:00:00Z");
    let ajeno = dir.path().join("nota.txt");
    fs::write(&ajeno, b"no tocar").unwrap();
    let conv = conversations(dir.path());

    conv.delete("").unwrap();

    assert!(conv.list().unwrap().is_empty());
    assert!(!dir.path().join("c-1.json").exists());
    assert!(!dir.path().join("c-2.json").exists());
    assert!(ajeno.exists(), "delete(\"\") borró un archivo ajeno");
}

#[test]
fn migrates_legacy_file() {
    let dir = tempfile::tempdir().unwrap();
    let ruta = dir.path().join("c-123.json");
    let legacy = format!(
        r#"{{"id":"123","title":"hola","at":"2026-11-11T10:00:00Z","messages":[{}],"tokens_in":3,"tokens_out":4}}"#,
        mensaje("hola")
    );
    fs::write(&ruta, legacy).unwrap();
    let conv = conversations(dir.path());

    let lista = conv.list().unwrap();
    assert_eq!(lista.len(), 1);
    assert_eq!(lista[0].id, "123");
    assert_eq!(lista[0].title, "hola");
    assert_eq!(lista[0].messages, 1);

    let cargada = conv.load("123").unwrap();
    assert_eq!(cargada.version, VERSION);
    assert_eq!(cargada.created, fecha("2026-11-11T10:00:00Z"));
    assert_eq!((cargada.tokens_in, cargada.tokens_out), (3, 4));
    assert_eq!(cargada.messages.len(), 1);

    let migrado: Value = serde_json::from_slice(&fs::read(&ruta).unwrap()).unwrap();
    assert_eq!(migrado["version"], VERSION);
    assert!(
        migrado.get("at").is_none(),
        "el archivo conserva el campo viejo"
    );

    // Idempotente: la segunda pasada no reescribe nada.
    let antes = fs::read(&ruta).unwrap();
    conv.migrate();
    assert_eq!(fs::read(&ruta).unwrap(), antes);
}

#[test]
fn leaves_versioned_file_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let ruta = dir.path().join("c-77.json");
    let original = br#"{"version":99,"id":"77","title":"futuro","created":"2026-11-11T10:00:00Z","updated":"2026-11-11T10:00:00Z","messages":[],"campo_nuevo":"no perder"}"#;
    fs::write(&ruta, original).unwrap();
    let conv = conversations(dir.path());

    assert!(
        conv.list().unwrap().is_empty(),
        "una versión futura no se lista"
    );

    assert_eq!(
        fs::read(&ruta).unwrap(),
        original,
        "migrate reescribió un archivo versionado"
    );
}

#[test]
fn skips_empty_legacy_file() {
    let dir = tempfile::tempdir().unwrap();
    let ruta = dir.path().join("c-9.json");
    let original = br#"{"id":"9","title":"","at":"2026-11-11T10:00:00Z","messages":[]}"#;
    fs::write(&ruta, original).unwrap();
    let conv = conversations(dir.path());

    conv.migrate();

    assert_eq!(
        fs::read(&ruta).unwrap(),
        original,
        "migrate tocó un archivo sin conversación"
    );
    assert!(conv.list().unwrap().is_empty());
    assert_eq!(leer_sesion(dir.path(), "9").version, 0);
}

#[test]
fn new_id_is_unique() {
    let dir = tempfile::tempdir().unwrap();
    let conv = conversations(dir.path());

    let uno = conv.new_id();
    let otro = conv.new_id();

    assert_ne!(uno, otro, "dos conversaciones seguidas comparten id");
    for id in [&uno, &otro] {
        assert!(!id.is_empty());
        assert!(id.bytes().all(|byte| byte.is_ascii_digit()));
        assert!(id.parse::<i64>().is_ok());
    }
}

/// Una conversación vieja que no se puede migrar (un mensaje con un campo de
/// tipo raro, un disco lleno) sigue en la lista como ilegible: desaparecer sin
/// decir nada es peor que mostrarla rota.
#[test]
fn una_conversacion_vieja_que_no_migra_sigue_en_la_lista() {
    let dir = tempfile::tempdir().expect("carpeta temporal");
    let rota = dir.path().join("c-77.json");
    fs::write(
        &rota,
        br#"{"id":"77","title":"vieja","at":"2026-01-01T00:00:00Z","messages":[{"Role":"user","Content":"hola","Images":null,"ToolCalls":null,"ToolCallID":123}]}"#,
    )
    .expect("archivo viejo");
    escribir_sesion(dir.path(), "78", "Buena", "2026-01-02T00:00:00Z");

    let lista = conversations(dir.path()).list().expect("listar");
    let ids: Vec<&str> = lista.iter().map(|c| c.id.as_str()).collect();

    assert!(ids.contains(&"78"), "falta la buena: {ids:?}");
    assert!(ids.contains(&"77"), "la vieja desapareció: {ids:?}");
    let vieja = lista.iter().find(|c| c.id == "77").unwrap();
    assert_eq!(vieja.title, "Conversación ilegible");
}

#[test]
fn una_conversacion_actual_ilegible_sigue_en_la_lista() {
    let dir = tempfile::tempdir().expect("carpeta temporal");
    fs::write(
        dir.path().join("c-77.json"),
        br#"{"version":1,"id":"77","title":"rota","created":"2026-01-01T00:00:00Z","updated":"2026-01-01T00:00:00Z","messages":[{"Role":"user""#,
    )
    .expect("archivo truncado");
    fs::write(
        dir.path().join("c-78.json"),
        br#"{"version":1,"id":"78","title":"rota","created":"2026-01-01T00:00:00Z","updated":"2026-01-01T00:00:00Z","messages":"no es una lista"}"#,
    )
    .expect("archivo con campos rotos");
    escribir_sesion(dir.path(), "79", "Buena", "2026-01-02T00:00:00Z");

    let lista = conversations(dir.path()).list().expect("listar");
    let ids: Vec<&str> = lista.iter().map(|c| c.id.as_str()).collect();
    assert!(ids.contains(&"79"), "falta la buena: {ids:?}");
    for id in ["77", "78"] {
        let rota = lista
            .iter()
            .find(|c| c.id == id)
            .expect("sigue en la lista");
        assert_eq!(rota.title, "Conversación ilegible");
    }
}

/// #95: `list()` no puede releer y reparsear todos los archivos en cada
/// llamada. Con el directorio sin cambios (mismo nombre, tamaño y fecha de
/// modificación), la segunda lista sale del cache y ni siquiera remigra.
#[test]
fn list_no_reparsea_un_directorio_sin_cambios() {
    let dir = tempfile::tempdir().expect("carpeta temporal");
    let ruta = dir.path().join("c-1.json");
    let legacy = r#"{"title":"vieja","at":"2026-11-11T10:00:00Z","messages":[{"Role":"user","Content":"hola","Images":null,"ToolCalls":null,"ToolCallID":""}]}"#;
    fs::write(&ruta, legacy).expect("archivo viejo");
    let conv = conversations(dir.path());

    let lista = conv.list().expect("la primera lista migra");
    assert_eq!(lista.len(), 1, "la legacy se migra y se lista");

    // El mismo contenido legacy, al mismo tamaño y con la misma fecha que
    // dejó la migración: para el directorio, nada cambió.
    let meta = fs::metadata(&ruta).expect("metadata tras migrar");
    let tamano = usize::try_from(meta.len()).expect("tamaño");
    let modificado = meta.modified().expect("fecha");
    let mut mismo = legacy.as_bytes().to_vec();
    assert!(tamano >= mismo.len(), "la migración agregó campos");
    mismo.resize(tamano, b' ');
    fs::write(&ruta, &mismo).expect("reescribir legacy");
    let archivo = fs::File::options()
        .write(true)
        .open(&ruta)
        .expect("abrir para fijar la fecha");
    archivo
        .set_times(fs::FileTimes::new().set_modified(modificado))
        .expect("fijar la fecha");

    let lista2 = conv.list().expect("la segunda lista");
    assert_eq!(lista, lista2, "la lista cambió sin cambios en el disco");
    let despues = fs::read_to_string(&ruta).expect("releer");
    assert!(
        !despues.contains("\"version\""),
        "list() reparseó y remigró un directorio que no cambió"
    );
}

/// El cache no puede ocultar cambios reales: una conversación nueva aparece
/// y una borrada desaparece (#95).
#[test]
fn list_refleja_los_cambios_del_directorio() {
    let dir = tempfile::tempdir().expect("carpeta temporal");
    escribir_sesion(dir.path(), "1", "uno", "2026-11-11T10:00:00Z");
    let conv = conversations(dir.path());
    assert_eq!(conv.list().expect("listar").len(), 1);

    escribir_sesion(dir.path(), "2", "dos", "2026-11-11T11:00:00Z");
    let lista = conv.list().expect("listar tras escribir");
    let ids: Vec<&str> = lista.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(ids, ["2", "1"], "la conversación nueva no apareció");

    conv.delete("1").expect("borrar");
    let lista = conv.list().expect("listar tras borrar");
    let ids: Vec<&str> = lista.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(ids, ["2"], "la conversación borrada sigue apareciendo");
}
