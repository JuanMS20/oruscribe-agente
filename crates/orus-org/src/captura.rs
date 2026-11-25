//! Puesta al día: metas, borradores de trabajo y lectura de `git log`.
//!
//! Port de `internal/oruscribe/capture.go:1-227`. `GitLog` corre en la máquina,
//! nunca contra el servidor; el resto de los métodos hablan MCP por
//! [`ToolCaller`].

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Instant;

use crate::rfc3339;
use chrono::{DateTime, Datelike, Duration, Utc};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::client::{Client, JsonObject};
use crate::error::{OrgError, Result};
use crate::mensajes::ToolCaller;
use crate::types::{Digest, DigestItem, DigestSource, Goal, GoalInput};

/// Tope de commits que conserva la puesta al día (defensa del parser).
const TOPE_COMMITS: usize = 200;

/// Margen sobre el tope del parser: git trae unos pocos más para poder saber
/// si la lista quedó cortada.
const MARGEN_COMMITS: usize = 50;

/// Bytes de stdout de git que se conservan en memoria; el resto se drena.
const TOPE_STDOUT: usize = 8 * 1024 * 1024;

/// Bytes de stderr de git que se conservan: solo son contexto del error.
const TOPE_STDERR: usize = 64 * 1024;

/// Plazo por defecto de `git log`: al vencer se mata el proceso git.
const PLAZO_GIT_LOG: std::time::Duration = std::time::Duration::from_secs(60);

#[derive(Debug, Deserialize)]
struct CreateGoalResponse {
    #[serde(default)]
    goal: Option<Goal>,
    #[serde(default)]
    id: String,
    #[serde(default)]
    goal_id: String,
}

/// Crea una meta y devuelve lo que el servidor guardó.
pub async fn create_goal<C: ToolCaller + ?Sized>(caller: &C, input: GoalInput) -> Result<Goal> {
    let title = input.title.trim();
    if title.is_empty() {
        return Err(OrgError::InvalidInput(
            "la meta necesita un título".to_owned(),
        ));
    }
    let mut args = JsonObject::new();
    args.insert("title".to_owned(), Value::from(title));
    let description = input.description.trim();
    if !description.is_empty() {
        args.insert("description".to_owned(), Value::from(description));
    }
    if !input.due_date.is_empty() {
        args.insert("due_date".to_owned(), Value::from(input.due_date.as_str()));
    }
    if !input.team_id.is_empty() {
        args.insert("team_id".to_owned(), Value::from(input.team_id.as_str()));
    }
    if !input.period_id.is_empty() {
        args.insert(
            "period_id".to_owned(),
            Value::from(input.period_id.as_str()),
        );
    }
    if input.weight > 0.0 {
        args.insert("weight".to_owned(), Value::from(input.weight));
    }
    if input.is_primary {
        args.insert("is_primary".to_owned(), Value::from(true));
    }
    let milestones = input.milestones.as_ref().filter(|items| !items.is_empty());
    let workers = input.workers.as_ref().filter(|items| !items.is_empty());
    if let (Some(milestones), Some(workers)) = (milestones, workers) {
        args.insert("milestones".to_owned(), json!(milestones));
        args.insert("workers".to_owned(), json!(workers));
    }
    let response: CreateGoalResponse = caller.call_json("create_goal", args).await?;
    if let Some(goal) = response.goal.filter(|goal| !goal.id.is_empty()) {
        return Ok(goal);
    }
    let id = if response.goal_id.is_empty() {
        response.id
    } else {
        response.goal_id
    };
    if id.is_empty() {
        return Err(OrgError::MissingId {
            tool: "create_goal".to_owned(),
        });
    }
    Ok(Goal {
        id,
        title: title.to_owned(),
        description: input.description.clone(),
        due_date: string_ptr(&input.due_date),
        ..Goal::default()
    })
}

/// Manda el trabajo visto en carpetas locales y recibe el borrador. El
/// borrador vive en el servidor veinticuatro horas.
pub async fn propose_digest<C: ToolCaller + ?Sized>(
    caller: &C,
    since: &str,
    sources: &[DigestSource],
) -> Result<Digest> {
    if since.trim().is_empty() {
        return Err(OrgError::InvalidInput(
            "falta la fecha desde la que buscar".to_owned(),
        ));
    }
    if sources.is_empty() {
        return Err(OrgError::InvalidInput(
            "no hay trabajo que proponer".to_owned(),
        ));
    }
    let mut args = JsonObject::new();
    args.insert("since".to_owned(), Value::from(since));
    args.insert("sources".to_owned(), json!(sources));
    caller.call_json("propose_work_digest", args).await
}

/// Escribe los candidatos aceptados. Solo viajan los ids que una persona
/// aceptó; el resto queda afuera.
pub async fn apply_digest<C: ToolCaller + ?Sized>(
    caller: &C,
    digest_id: &str,
    accept: &[String],
) -> Result<String> {
    if digest_id.is_empty() || accept.is_empty() {
        return Err(OrgError::InvalidInput(
            "no hay nada seleccionado".to_owned(),
        ));
    }
    let mut args = JsonObject::new();
    args.insert("digest_id".to_owned(), Value::from(digest_id));
    args.insert("accept".to_owned(), json!(accept));
    caller.call("apply_work_digest", args).await
}

/// Lee los commits de un repositorio local desde un momento. Es la materia
/// prima de la puesta al día: se lee de la máquina y nunca del servidor.
///
/// `since = None` representa el cero de Go: se buscan los últimos siete días.
pub fn git_log(dir: &str, since: Option<DateTime<Utc>>) -> Result<DigestSource> {
    git_log_con_plazo(dir, since, PLAZO_GIT_LOG)
}

/// Como [`git_log`], con plazo explícito para el proceso git. Al vencer el
/// plazo el proceso se mata: no queda un git colgado consumiendo recursos
/// hasta que termine solo (residual E de #97). La cancelación externa de un
/// hilo bloqueante no se observa desde esta costura síncrona; el plazo es el
/// corte que sí mata al proceso.
pub fn git_log_con_plazo(
    dir: &str,
    since: Option<DateTime<Utc>>,
    plazo: std::time::Duration,
) -> Result<DigestSource> {
    leer_git_log(dir, since, plazo, "git")
}

fn leer_git_log(
    dir: &str,
    since: Option<DateTime<Utc>>,
    plazo: std::time::Duration,
    programa: &str,
) -> Result<DigestSource> {
    if !Path::new(dir).is_dir() {
        return Err(OrgError::MissingFolder(dir.to_owned()));
    }
    let since = since.unwrap_or_else(|| Utc::now() - Duration::days(7));
    // \x1f separa campos y \x1e separa commits. El formato reproduce el string
    // de Go, donde los escapes ya son bytes de control al llegar a git.
    let mut command = Command::new(programa);
    command
        .arg("-C")
        .arg(dir)
        .arg("log")
        .arg(format!("--since={}", rfc3339(since)))
        // Tope en la fuente: la memoria no crece con el historial entero.
        .arg(format!("--max-count={}", TOPE_COMMITS + MARGEN_COMMITS))
        .arg("--date=iso-strict")
        .arg("--pretty=format:%h\u{1f}%aI\u{1f}%s\u{1f}%b\u{1e}")
        // Lectura pura: git no pide credenciales por terminal ni bloquea el
        // repositorio.
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    let mut child = command.spawn().map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            OrgError::GitMissing
        } else {
            OrgError::GitFailed(error.to_string())
        }
    })?;
    // Lectores con tope: drenan hasta EOF para que git no se bloquee con el
    // pipe lleno, pero solo conservan TOPE bytes en memoria.
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| OrgError::GitFailed("git sin pipe de stdout".to_owned()))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| OrgError::GitFailed("git sin pipe de stderr".to_owned()))?;
    let lector_stdout = std::thread::spawn(move || leer_con_tope(stdout, TOPE_STDOUT));
    let lector_stderr = std::thread::spawn(move || leer_con_tope(stderr, TOPE_STDERR));
    let inicio = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if inicio.elapsed() >= plazo {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = lector_stdout.join();
                    let _ = lector_stderr.join();
                    return Err(OrgError::GitFailed(format!(
                        "git no respondió en {} ms; se mató el proceso",
                        plazo.as_millis()
                    )));
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = lector_stdout.join();
                let _ = lector_stderr.join();
                return Err(OrgError::GitFailed(error.to_string()));
            }
        }
    };
    let (stdout, stdout_cortado) = lector_stdout
        .join()
        .map_err(|_| OrgError::GitFailed("el lector de stdout de git falló".to_owned()))?
        .map_err(|error| OrgError::GitFailed(format!("no se pudo leer stdout de git: {error}")))?;
    let (stderr, _) = lector_stderr
        .join()
        .map_err(|_| OrgError::GitFailed("el lector de stderr de git falló".to_owned()))?
        .map_err(|error| OrgError::GitFailed(format!("no se pudo leer stderr de git: {error}")))?;
    if !status.success() {
        let stderr = String::from_utf8_lossy(&stderr);
        let stderr = stderr.trim();
        let detail = if stderr.is_empty() {
            status.to_string()
        } else {
            stderr.to_owned()
        };
        return Err(OrgError::GitFailed(detail));
    }
    let (items, parser_cortado) = parsear_git_log(&String::from_utf8_lossy(&stdout));
    if items.is_empty() {
        return Err(OrgError::NoCommits(fecha_larga(since)));
    }
    let trimmed = dir.trim();
    let name = Path::new(trimmed)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| trimmed.to_owned());
    Ok(DigestSource {
        name,
        items: Some(items),
        truncated: parser_cortado || stdout_cortado,
    })
}

/// Convierte la salida de `git log` en commits.
///
/// Go corta el título a 300 bytes y el resumen a 2000, aunque parta un rune;
/// acá se corta en la frontera de rune anterior para no producir texto
/// inválido. Con texto ASCII el resultado es el mismo.
pub fn parse_git_log(raw: &str) -> Vec<DigestItem> {
    parsear_git_log(raw).0
}

/// Como [`parse_git_log`], pero también dice si había más commits válidos
/// después del tope: la puesta al día no puede presentar una lista corta
/// como si fuera completa.
fn parsear_git_log(raw: &str) -> (Vec<DigestItem>, bool) {
    let mut items = Vec::new();
    for chunk in raw.split('\x1e') {
        let chunk = chunk.trim_matches(|character| character == '\r' || character == '\n');
        if chunk.is_empty() {
            continue;
        }
        let parts: Vec<&str> = chunk.splitn(4, '\x1f').collect();
        if parts.len() < 3 {
            continue;
        }
        let title = parts[2].trim();
        if title.is_empty() {
            continue;
        }
        if items.len() == TOPE_COMMITS {
            return (items, true);
        }
        let summary = parts.get(3).map_or("", |part| part.trim());
        items.push(DigestItem {
            kind: "commit".to_owned(),
            title: recortar(title, 300).to_owned(),
            summary: recortar(summary, 2000).to_owned(),
            r#ref: parts[0].trim().to_owned(),
            at: parts[1].trim().to_owned(),
            state: "done".to_owned(),
        });
    }
    (items, false)
}

/// Lee hasta EOF conservando como mucho `tope` bytes. Lo que pasa del tope se
/// drena sin guardar: git no debe bloquearse por un pipe lleno y la memoria
/// queda acotada por construcción. Devuelve lo conservado y si se cortó, o el
/// error de lectura.
fn leer_con_tope(
    mut fuente: impl Read,
    tope: usize,
) -> std::result::Result<(Vec<u8>, bool), std::io::Error> {
    let mut datos = Vec::new();
    let mut buffer = [0_u8; 8192];
    let mut cortado = false;
    loop {
        match fuente.read(&mut buffer) {
            Ok(0) => break,
            Ok(leidos) => {
                let falta = tope.saturating_sub(datos.len());
                if leidos > falta {
                    cortado = true;
                }
                datos.extend_from_slice(&buffer[..leidos.min(falta)]);
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
    Ok((datos, cortado))
}

/// Corta en la frontera de rune más cercana sin pasar de `max` bytes.
fn recortar(value: &str, max: usize) -> &str {
    if value.len() <= max {
        return value;
    }
    let mut end = max;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

/// Igual que el `stringPtr` de Go: una cadena vacía se vuelve `None`.
fn string_ptr(value: &str) -> Option<String> {
    if value.is_empty() {
        None
    } else {
        Some(value.to_owned())
    }
}

/// El formato `2 Jan 2006` de Go: día sin ceros, mes en inglés.
fn fecha_larga(value: DateTime<Utc>) -> String {
    format!("{} {} {}", value.day(), value.format("%b"), value.year())
}

impl Client {
    /// Crea una meta.
    pub async fn create_goal(&self, input: GoalInput) -> Result<Goal> {
        create_goal(self, input).await
    }

    /// Pide el borrador de la puesta al día.
    pub async fn propose_digest(&self, since: &str, sources: &[DigestSource]) -> Result<Digest> {
        propose_digest(self, since, sources).await
    }

    /// Aplica los candidatos aceptados.
    pub async fn apply_digest(&self, digest_id: &str, accept: &[String]) -> Result<String> {
        apply_digest(self, digest_id, accept).await
    }
}

#[cfg(test)]
mod tests {
    use std::io::Read;

    use super::{TOPE_COMMITS, leer_con_tope, parsear_git_log};

    fn registros(n: usize) -> String {
        let mut raw = String::new();
        for indice in 0..n {
            raw.push_str(&format!("r{indice}\x1fat\x1ft{indice}\x1f\x1e"));
        }
        raw
    }

    #[test]
    fn el_parser_avisa_cuando_hay_mas_commits_que_el_tope() {
        let (items, cortado) = parsear_git_log(&registros(TOPE_COMMITS + 1));
        assert_eq!(items.len(), TOPE_COMMITS);
        assert!(cortado, "el commit siguiente al tope debe marcar el corte");
    }

    #[test]
    fn el_parser_no_avisa_corte_con_exactamente_el_tope() {
        let (items, cortado) = parsear_git_log(&registros(TOPE_COMMITS));
        assert_eq!(items.len(), TOPE_COMMITS);
        assert!(!cortado, "con exactamente 200 no se cortó nada");
    }

    #[test]
    #[allow(clippy::unwrap_used)]
    fn el_lector_conserva_el_tope_y_drena_el_resto() {
        let datos = vec![7_u8; 4096];
        let (leidos, cortado) = leer_con_tope(std::io::Cursor::new(&datos), 1024).unwrap();
        assert_eq!(leidos.len(), 1024);
        assert!(cortado, "lo que pasa del tope se marca como corte");
        let (completo, cortado) = leer_con_tope(std::io::Cursor::new(&datos), 8192).unwrap();
        assert_eq!(completo.len(), 4096);
        assert!(!cortado);
    }

    struct FallaTrasDatos {
        datos: std::io::Cursor<Vec<u8>>,
    }

    impl Read for FallaTrasDatos {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            let leidos = self.datos.read(buffer)?;
            if leidos == 0 {
                Err(std::io::Error::other("fallo de E/S"))
            } else {
                Ok(leidos)
            }
        }
    }

    #[test]
    #[allow(clippy::unwrap_used, clippy::expect_used)]
    fn el_lector_propaga_el_fallo_de_lectura() {
        let fuente = FallaTrasDatos {
            datos: std::io::Cursor::new(vec![7_u8; 16]),
        };
        let error = leer_con_tope(fuente, 1024).expect_err("quería el fallo de E/S");
        assert_eq!(error.to_string(), "fallo de E/S");
    }
}
