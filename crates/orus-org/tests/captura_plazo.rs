//! El plazo de git_log: un git que no responde debe cortarse y el proceso
//! debe morir, no quedarse corriendo hasta terminar solo (issue #97,
//! residual E: el hilo bloqueante seguía hasta que git terminaba).
//!
//! Usa el git falso de `comun`: cambiar el PATH afecta a todo el proceso de
//! test, así que [`comun::GUARDA_PATH`] serializa los tests de este binario.
mod comun;

use std::time::{Duration, Instant};

use chrono::Utc;
use orus_org::captura;

#[test]
fn git_log_con_git_colgado_se_corta_y_el_proceso_muere() {
    let _guarda = comun::GUARDA_PATH.lock().expect("guarda del PATH");
    comun::usar_git_falso();
    let repo = tempfile::tempdir().expect("repo temporal");
    comun::repo_con_modo(repo.path(), "colgado");

    let inicio = Instant::now();
    let resultado = captura::git_log_con_plazo(
        repo.path().to_str().expect("ruta utf-8"),
        Some(Utc::now() - chrono::Duration::hours(1)),
        Duration::from_millis(500),
    );
    assert!(
        inicio.elapsed() < Duration::from_secs(10),
        "git_log esperó a git en vez de cortar: {:?}",
        inicio.elapsed()
    );
    let error = resultado.expect_err("un git colgado debe dar error, no esperar a que termine");
    assert!(
        error.to_string().contains("500 ms"),
        "el error dice el plazo que se venció: {error}"
    );

    // El latido del git falso prueba que el proceso murió: si siguiera vivo,
    // seguiría escribiendo una línea por segundo.
    let vivos = comun::latidos(repo.path());
    assert!(vivos > 0, "el git falso nunca latió");
    std::thread::sleep(Duration::from_millis(2500));
    assert_eq!(
        comun::latidos(repo.path()),
        vivos,
        "el proceso git sigue vivo después del corte"
    );
}
