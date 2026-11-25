//! El tope de la puesta al día en la fuente: git debe recibir `--max-count`
//! con margen sobre los 200 commits del parser, para que la memoria no crezca
//! con el historial entero (issue #97, residual de captura).
//!
//! Usa el git falso de `comun`: cambiar el PATH afecta a todo el proceso de
//! test, así que [`comun::GUARDA_PATH`] serializa los tests de este binario.
mod comun;

use chrono::{Duration, Utc};
use orus_org::captura;

#[test]
fn git_log_pide_max_count_con_margen_sobre_el_tope() {
    let _guarda = comun::GUARDA_PATH.lock().expect("guarda del PATH");
    comun::usar_git_falso();
    let repo = tempfile::tempdir().expect("repo temporal");
    comun::repo_con_salida(repo.path(), &comun::registros(201));

    let fuente = captura::git_log(
        repo.path().to_str().expect("ruta utf-8"),
        Some(Utc::now() - Duration::hours(1)),
    )
    .expect("el git falso responde con commits");

    assert_eq!(
        fuente.items.as_ref().map(Vec::len),
        Some(200),
        "el parser conserva su tope de 200"
    );
    let args = comun::argumentos(repo.path());
    let tope = args
        .lines()
        .find_map(|linea| linea.strip_prefix("--max-count="))
        .and_then(|valor| valor.parse::<usize>().ok())
        .expect("git no recibió --max-count: la salida se lee sin tope en la fuente");
    assert!(
        tope > 200 && tope <= 1000,
        "el tope del comando debe dar margen sobre los 200 del parser: {tope}"
    );
}

#[test]
fn git_log_con_fallo_de_git_conserva_el_stderr() {
    let _guarda = comun::GUARDA_PATH.lock().expect("guarda del PATH");
    comun::usar_git_falso();
    let repo = tempfile::tempdir().expect("repo temporal");
    comun::repo_con_modo(repo.path(), "fallo");

    let error = captura::git_log(
        repo.path().to_str().expect("ruta utf-8"),
        Some(Utc::now() - Duration::hours(1)),
    )
    .unwrap_err();

    assert!(
        error.to_string().contains("fallo guionado"),
        "el stderr de git llega al error: {error}"
    );
}

#[test]
fn git_log_devuelve_el_aviso_de_corte_al_llamador() {
    let _guarda = comun::GUARDA_PATH.lock().expect("guarda del PATH");
    comun::usar_git_falso();
    let repo = tempfile::tempdir().expect("repo temporal");
    comun::repo_con_salida(repo.path(), &comun::registros(201));

    let fuente = captura::git_log(
        repo.path().to_str().expect("ruta utf-8"),
        Some(Utc::now() - Duration::hours(1)),
    )
    .expect("el git falso responde con commits");
    let valor = serde_json::to_value(&fuente).expect("DigestSource serializable");

    assert_eq!(
        valor["truncated"],
        serde_json::json!(true),
        "el corte a 200 commits tiene que llegar al modelo, no solo a stderr: {valor}"
    );
}

#[test]
fn git_log_no_aviso_de_corte_con_pocos_commits() {
    let _guarda = comun::GUARDA_PATH.lock().expect("guarda del PATH");
    comun::usar_git_falso();
    let repo = tempfile::tempdir().expect("repo temporal");
    comun::repo_con_salida(repo.path(), &comun::registros(3));

    let fuente = captura::git_log(
        repo.path().to_str().expect("ruta utf-8"),
        Some(Utc::now() - Duration::hours(1)),
    )
    .expect("el git falso responde con commits");
    let valor = serde_json::to_value(&fuente).expect("DigestSource serializable");

    assert_eq!(valor["truncated"], serde_json::json!(false));
}
