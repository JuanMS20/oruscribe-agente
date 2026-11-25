use std::fs;
use std::path::Path;

use tempfile::TempDir;

use super::*;

fn abrir() -> (TempDir, Journal) {
    let base = tempfile::tempdir().expect("crear carpeta temporal");
    let journal = Journal::open(base.path().join("journal")).expect("abrir el diario");
    (base, journal)
}

fn permitir(_: &Path) -> Result<(), String> {
    Ok(())
}

fn negar(_: &Path) -> Result<(), String> {
    Err("fuera de las carpetas".to_owned())
}

#[test]
fn record_y_listar() {
    let (_base, j) = abrir();
    let archivos = tempfile::tempdir().expect("carpeta de archivos");
    let ruta = archivos.path().join("nota.txt");
    fs::write(&ruta, b"antes").expect("escribir antes");

    j.record(&ruta, b"antes", true, b"despues").expect("grabar");

    let entradas = j.list().expect("listar");
    assert_eq!(entradas.len(), 1, "quería 1 entrada");
    let e = &entradas[0];
    assert_eq!(e.path, ruta);
    assert!(e.existed, "la entrada debe recordar que existía");
    assert_eq!(e.size, b"despues".len() as i64);
    assert_ne!(
        e.sha_after, e.sha_before,
        "los hashes antes y después no deberían ser iguales"
    );
}

#[test]
fn record_archivo_nuevo() {
    let (_base, j) = abrir();
    let archivos = tempfile::tempdir().expect("carpeta de archivos");
    let ruta = archivos.path().join("nuevo.txt");

    j.record(&ruta, &[], false, b"hola").expect("grabar");

    let entradas = j.list().expect("listar");
    assert!(!entradas[0].existed, "un archivo nuevo tiene existed=false");
}

#[test]
fn undo_restaura_contenido() {
    let (_base, j) = abrir();
    let archivos = tempfile::tempdir().expect("carpeta de archivos");
    let ruta = archivos.path().join("nota.txt");
    fs::write(&ruta, b"antes").expect("escribir antes");
    j.record(&ruta, b"antes", true, b"despues").expect("grabar");
    fs::write(&ruta, b"despues").expect("escribir despues");

    let (n, err) = j.undo(1, Some(&permitir), false);
    assert_eq!(n, 1, "quería deshacer 1");
    assert!(err.is_none(), "deshacer: {err:?}");
    assert_eq!(fs::read(&ruta).expect("leer restaurado"), b"antes");

    // La entrada consumida desaparece: un segundo deshacer no debe repetir.
    let entradas = j.list().expect("listar");
    assert!(
        entradas.is_empty(),
        "el diario debería quedar vacío, tiene {}",
        entradas.len()
    );
}

#[test]
fn undo_borra_archivo_creado() {
    let (_base, j) = abrir();
    let archivos = tempfile::tempdir().expect("carpeta de archivos");
    let ruta = archivos.path().join("creado.txt");
    j.record(&ruta, &[], false, b"contenido").expect("grabar");
    fs::write(&ruta, b"contenido").expect("escribir contenido");

    let (n, err) = j.undo(1, Some(&permitir), false);
    assert_eq!(n, 1);
    assert!(err.is_none(), "deshacer: {err:?}");
    assert!(
        !ruta.exists(),
        "el archivo creado por el agente debería borrarse al deshacer"
    );
}

#[test]
fn undo_se_niega_si_edito_despues() {
    let (_base, j) = abrir();
    let archivos = tempfile::tempdir().expect("carpeta de archivos");
    let ruta = archivos.path().join("nota.txt");
    fs::write(&ruta, b"antes").expect("escribir antes");
    j.record(&ruta, b"antes", true, b"del agente")
        .expect("grabar");
    // La persona edita el archivo después del agente.
    fs::write(&ruta, b"de la persona").expect("editar después");

    let (n, err) = j.undo(1, Some(&permitir), false);
    assert!(
        err.is_some(),
        "deshacer debería negarse cuando el archivo cambió"
    );
    assert_eq!(n, 0);
    assert_eq!(
        fs::read(&ruta).expect("leer sin tocar"),
        b"de la persona",
        "no debe tocar el archivo cuando se niega"
    );

    // Con fuerza sí restaura.
    let (n, err) = j.undo(1, Some(&permitir), true);
    assert_eq!(n, 1);
    assert!(err.is_none(), "deshacer con fuerza: {err:?}");
    assert_eq!(fs::read(&ruta).expect("leer restaurado"), b"antes");
}

#[test]
fn undo_rechaza_ruta_fuera() {
    let (_base, j) = abrir();
    let archivos = tempfile::tempdir().expect("carpeta de archivos");
    let ruta = archivos.path().join("nota.txt");
    fs::write(&ruta, b"antes").expect("escribir antes");
    j.record(&ruta, b"antes", true, b"despues").expect("grabar");
    fs::write(&ruta, b"despues").expect("escribir despues");

    let (n, err) = j.undo(1, Some(&negar), false);
    assert!(err.is_some(), "la guarda debe poder negarse");
    assert_eq!(n, 0);
    assert_eq!(
        fs::read(&ruta).expect("leer sin tocar"),
        b"despues",
        "no debe tocar una ruta que la guarda rechazó"
    );
}

#[test]
fn undo_varias_en_orden_inverso() {
    let (_base, j) = abrir();
    let archivos = tempfile::tempdir().expect("carpeta de archivos");
    let ruta_a = archivos.path().join("a.txt");
    let ruta_b = archivos.path().join("b.txt");
    fs::write(&ruta_a, b"a1").expect("escribir a1");
    fs::write(&ruta_b, b"b1").expect("escribir b1");
    j.record(&ruta_a, b"a1", true, b"a2").expect("grabar a");
    fs::write(&ruta_a, b"a2").expect("escribir a2");
    j.record(&ruta_b, b"b1", true, b"b2").expect("grabar b");
    fs::write(&ruta_b, b"b2").expect("escribir b2");

    let (n, err) = j.undo(2, Some(&permitir), false);
    assert_eq!(n, 2);
    assert!(err.is_none(), "deshacer: {err:?}");
    assert_eq!(fs::read(&ruta_a).expect("leer a"), b"a1");
    assert_eq!(fs::read(&ruta_b).expect("leer b"), b"b1");
}

#[test]
fn recorte_mantiene_el_tope() {
    let (_base, j) = abrir();
    let archivos = tempfile::tempdir().expect("carpeta de archivos");
    for i in 0..MAX_ENTRADAS + 10 {
        let ruta = archivos.path().join(format!("f{i:03}.txt"));
        j.record(&ruta, &[], false, &[i as u8]).expect("grabar");
    }

    let entradas = j.list().expect("listar");
    assert_eq!(
        entradas.len(),
        MAX_ENTRADAS,
        "el diario debe conservar {MAX_ENTRADAS} entradas"
    );
    let ultimo = &entradas[entradas.len() - 1].path;
    assert!(
        ultimo.ends_with("f010.txt"),
        "la entrada más vieja conservada debería ser f010.txt, es {}",
        ultimo.display()
    );
}

#[test]
fn bytes_respaldados_coinciden() {
    let (_base, j) = abrir();
    let contenido = b"linea\n".repeat(100);
    let archivos = tempfile::tempdir().expect("carpeta de archivos");
    let ruta = archivos.path().join("grande.txt");
    fs::write(&ruta, &contenido).expect("escribir grande");
    j.record(&ruta, &contenido, true, b"nuevo").expect("grabar");
    fs::write(&ruta, b"nuevo").expect("escribir nuevo");

    let (_, err) = j.undo(1, Some(&permitir), false);
    assert!(err.is_none(), "deshacer: {err:?}");
    assert_eq!(
        fs::read(&ruta).expect("leer restaurado"),
        contenido,
        "el contenido restaurado no coincide byte a byte"
    );
}

#[test]
fn modo_conservado_al_restaurar() {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let (_base, j) = abrir();
        let archivos = tempfile::tempdir().expect("carpeta de archivos");
        let ruta = archivos.path().join("con-modo.txt");
        fs::write(&ruta, b"antes").expect("escribir antes");
        fs::set_permissions(&ruta, fs::Permissions::from_mode(0o640)).expect("chmod del archivo");
        let modo = fs::metadata(&ruta)
            .expect("modo del archivo")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(modo, 0o640);

        j.record(&ruta, b"antes", true, b"despues").expect("grabar");
        // La escritura posterior pisa el archivo con otro modo.
        fs::write(&ruta, b"despues").expect("escribir despues");
        fs::set_permissions(&ruta, fs::Permissions::from_mode(0o600)).expect("chmod posterior");
        let (_, err) = j.undo(1, Some(&permitir), false);
        assert!(err.is_none(), "deshacer: {err:?}");

        let modo = fs::metadata(&ruta)
            .expect("modo restaurado")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(modo, 0o640, "modo restaurado distinto del previo");
    }
}

// Si la entrada no llega al disco, grabar tiene que fallar. Antes se tragaba
// el error de Write/Sync, el agente creía poder deshacer y no podía.
#[test]
fn anexar_entrada_devuelve_error_de_escritura() {
    let dir = tempfile::tempdir().expect("carpeta temporal");
    let ruta = dir.path().join("solo-lectura");
    fs::write(&ruta, b"previo").expect("escribir previo");
    let mut archivo = fs::File::open(&ruta).expect("abrir solo lectura");
    assert!(
        anexar_entrada(&mut archivo, b"entrada\n").is_err(),
        "anexar en un archivo de solo lectura debe devolver error"
    );
}

// Un temporal que no se pudo escribir no puede darse por bueno: antes el error
// de Write se perdía y el archivo bueno quedaba reemplazado por basura.
#[test]
fn volcar_temporal_devuelve_error_de_escritura() {
    let dir = tempfile::tempdir().expect("carpeta temporal");
    let ruta = dir.path().join("solo-lectura");
    fs::write(&ruta, b"previo").expect("escribir previo");
    let mut archivo = fs::File::open(&ruta).expect("abrir solo lectura");
    assert!(
        volcar_temporal(&mut archivo, b"datos", 0o600).is_err(),
        "volcar en un temporal de solo lectura debe devolver error"
    );
}

#[test]
fn entrada_danada_se_salta_al_listar() {
    let (_base, j) = abrir();
    let archivos = tempfile::tempdir().expect("carpeta de archivos");
    let ruta = archivos.path().join("nota.txt");
    j.record(&ruta, &[], false, b"x").expect("grabar");

    // Corrompe una línea del diario a mano.
    let diario = j.dir.join("journal.jsonl");
    let datos = fs::read(&diario).expect("leer diario");
    let mut corrompido = b"esto no es JSON\n".to_vec();
    corrompido.extend(datos);
    fs::write(&diario, corrompido).expect("corromper diario");

    let entradas = j.list().expect("listar");
    assert_eq!(
        entradas.len(),
        1,
        "la línea dañada debe saltarse; entradas: {}",
        entradas.len()
    );
}

#[test]
fn undo_no_restaura_un_respaldo_corrupto() {
    let (_base, j) = abrir();
    let archivos = tempfile::tempdir().expect("carpeta de archivos");
    let ruta = archivos.path().join("nota.txt");
    fs::write(&ruta, b"antes").expect("escribir antes");
    j.record(&ruta, b"antes", true, b"despues").expect("grabar");
    fs::write(&ruta, b"despues").expect("escribir despues");

    let entradas = j.list().expect("listar");
    assert_eq!(entradas.len(), 1);
    let respaldo = j.dir.join(&entradas[0].backup);
    fs::write(&respaldo, b"basura").expect("corromper respaldo");

    let (n, err) = j.undo(1, Some(&permitir), false);
    assert!(err.is_some(), "Undo aceptó un respaldo corrupto");
    assert_eq!(n, 0, "deshizo {n} operaciones con el respaldo corrupto");
    assert_eq!(
        fs::read(&ruta).expect("leer sin tocar"),
        b"despues",
        "el archivo se pisó con basura"
    );
    assert_eq!(
        j.list().expect("listar").len(),
        1,
        "la entrada se consumió pese al error"
    );
}

#[test]
fn open_rechaza_carpeta_vacia() {
    let error = Journal::open("").expect_err("una carpeta vacía no vale");
    assert_eq!(error.to_string(), "el diario necesita una carpeta");
    let error = Journal::open("   ").expect_err("solo espacios no vale");
    assert_eq!(error.to_string(), "el diario necesita una carpeta");
}

/// Issue #83: si reescribir el diario falla, los respaldos de las entradas
/// descartadas tienen que seguir vivos. Antes se borraban primero y el
/// deshacer quedaba trabado para siempre con BackupMissing.
#[test]
fn deshacer_no_borra_respaldos_antes_de_reescribir() {
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    let (_base, j) = abrir();
    let archivos = tempfile::tempdir().expect("carpeta de archivos");
    let ruta = archivos.path().join("nota.txt");
    fs::write(&ruta, b"antes").expect("escribir antes");
    j.record(&ruta, b"antes", true, b"despues").expect("grabar");
    fs::write(&ruta, b"despues").expect("escribir despues");
    let entradas = j.list().expect("listar");
    let respaldo = j.dir.join(&entradas[0].backup);
    assert!(respaldo.exists(), "el respaldo debe existir tras record");

    // Bloquea la reescritura del diario sin bloquear su lectura: en Windows
    // se abre compartiendo solo lectura (el rename encima falla); en Unix la
    // carpeta de solo lectura no deja crear el temporal (como root no
    // bloquea: se salta).
    #[cfg(windows)]
    let _candado = {
        use std::os::windows::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(j.dir.join("journal.jsonl"))
            .expect("candado del diario")
    };
    #[cfg(unix)]
    {
        if unsafe { libc::geteuid() } == 0 {
            eprintln!("corriendo como root: el caso se salta");
            return;
        }
        fs::set_permissions(&j.dir, fs::Permissions::from_mode(0o555))
            .expect("carpeta de solo lectura");
    }

    let (hechas, error) = j.undo(1, Some(&permitir), false);
    assert_eq!(hechas, 1, "la restauración del archivo sí se aplica");
    assert!(error.is_some(), "consumir el diario bloqueado debe fallar");
    assert!(
        respaldo.exists(),
        "el respaldo se borró pese a que el diario no se pudo reescribir: deshacer queda trabado"
    );

    #[cfg(unix)]
    fs::set_permissions(&j.dir, fs::Permissions::from_mode(0o700)).expect("restaurar carpeta");
    #[cfg(windows)]
    drop(_candado);

    // Con el diario desbloqueado, la entrada sigue ahí y el respaldo vivo:
    // deshacer de nuevo (force: el archivo ya quedó restaurado) completa.
    let (hechas, error) = j.undo(1, Some(&permitir), true);
    assert_eq!(hechas, 1);
    assert!(error.is_none(), "deshacer tras desbloquear: {error:?}");
    assert_eq!(fs::read(&ruta).expect("leer"), b"antes");
    assert!(
        j.list().expect("listar").is_empty(),
        "el diario quedó trabado"
    );
    assert!(
        !respaldo.exists(),
        "el respaldo se borra recién tras reescribir bien"
    );
}

/// Issue #128: si anexar falla, el respaldo tiene que seguir en disco. Cuando
/// `write_all` entró y solo falló `sync_all`, la línea queda en el diario:
/// borrar el `.bak` deja el siguiente `undo` en `BackupMissing` (#83).
#[test]
fn fallo_al_anexar_conserva_el_respaldo_para_deshacer() {
    let (_base, j) = abrir();
    let archivos = tempfile::tempdir().expect("carpeta de archivos");
    let ruta = archivos.path().join("nota.txt");
    fs::write(&ruta, b"antes").expect("escribir antes");

    // `journal.jsonl` como carpeta: abrir para anexar falla y `record` vuelve
    // con error. Es el mismo estado que deja un `sync_all` fallido: intento
    // fallido de anexar con un respaldo ya creado.
    let diario = j.dir.join("journal.jsonl");
    fs::create_dir(&diario).expect("diario como carpeta");
    assert!(
        j.record(&ruta, b"antes", true, b"despues").is_err(),
        "anexar en una carpeta debe fallar"
    );

    let respaldos: Vec<_> = fs::read_dir(&j.dir)
        .expect("leer carpeta del diario")
        .filter_map(Result::ok)
        .map(|entrada| entrada.file_name())
        .filter(|nombre| nombre.to_string_lossy().ends_with(".bak"))
        .collect();
    assert_eq!(
        respaldos.len(),
        1,
        "el respaldo no puede borrarse cuando falló el anexado"
    );
    let backup = respaldos[0].to_string_lossy().into_owned();

    // Simula la línea que `write_all` dejó en el page cache antes del fallo
    // de `sync_all`: el diario la lista y apunta al respaldo conservado.
    fs::remove_dir(&diario).expect("quitar carpeta del diario");
    let entrada = Entry {
        id: "sync-fallido".to_owned(),
        path: ruta.clone(),
        existed: true,
        backup,
        sha_before: hash(b"antes"),
        sha_after: hash(b"despues"),
        size: b"despues".len() as i64,
        mode: 0,
        when: Utc::now(),
        pending: false,
    };
    let mut linea = serde_json::to_vec(&entrada).expect("serializar entrada");
    linea.push(b'\n');
    fs::write(&diario, linea).expect("escribir linea del diario");
    fs::write(&ruta, b"despues").expect("escribir despues");

    let (hechas, error) = j.undo(1, Some(&permitir), false);
    assert_eq!(hechas, 1, "deshacer no debía quedar trabado: {error:?}");
    assert!(error.is_none(), "deshacer: {error:?}");
    assert_eq!(
        fs::read(&ruta).expect("leer restaurado"),
        b"antes",
        "el respaldo conservado debe restaurar el contenido previo"
    );
}
