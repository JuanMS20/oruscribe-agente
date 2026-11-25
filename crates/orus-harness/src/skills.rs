//! Skills del arnés: carga `SKILL.md` y expone la herramienta `skill`.
//!
//! Port de `internal/harness/skills.go:17-346`.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use orus_agent::{Risk, Tool, ToolError, ToolSpec};
use serde_json::Value;

pub const MAX_SKILL: usize = 64 << 10;
pub const MAX_SKILL_FILES: usize = 20;
const MAX_SKILL_DIRS: usize = 64;
const MAX_SKILL_ENTRIES: usize = 256;
const MAX_SKILL_DEPTH: usize = 16;
const MAX_SKILLS: usize = 256;
const MAX_DESCRIPCION: usize = 512;
const MAX_LISTADO: usize = 16 << 10;
const DESCRIPCION: &str = "Carga una skill (un SKILL.md) y devuelve sus instrucciones. Úsala cuando la tarea coincida con una de las disponibles.";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub path: PathBuf,
    pub dir: PathBuf,
    pub project: bool,
    pub content: String,
    pub files: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct Skills {
    list: Vec<Skill>,
    by_name: HashMap<String, Skill>,
}

impl Skills {
    pub fn list(&self) -> &[Skill] {
        &self.list
    }

    pub fn get(&self, name: &str) -> Option<&Skill> {
        self.by_name.get(name)
    }

    pub fn tool(&self) -> SkillTool {
        SkillTool {
            skills: self.clone(),
        }
    }
}

/// Carga skills globales y del proyecto; el proyecto pisa por nombre.
pub fn load(cwd: &Path, global: bool) -> Skills {
    load_with(
        cwd,
        global,
        &orus_config::data_dir(),
        std::env::home_dir().as_deref(),
    )
}

/// Igual que `load`, con las raíces explícitas para los tests.
pub(crate) fn load_with(cwd: &Path, global: bool, data_dir: &Path, home: Option<&Path>) -> Skills {
    let mut raices: Vec<(PathBuf, bool)> = Vec::new();
    if global {
        raices.extend(home.map(|h| (h.join(".agents").join("skills"), false)));
        raices.push((data_dir.join("skills"), false));
    }
    raices.push((cwd.join(".agents").join("skills"), true));
    raices.push((cwd.join("skills"), true));
    let mut out = Skills::default();
    for (raiz, project) in raices {
        cargar_raiz(&mut out, &raiz, project);
    }
    out.list = out.by_name.values().cloned().collect();
    out.list
        .sort_by(|a, b| b.project.cmp(&a.project).then_with(|| a.name.cmp(&b.name)));
    out
}

fn cargar_raiz(out: &mut Skills, raiz: &Path, project: bool) {
    let Ok(entradas) = fs::read_dir(raiz) else {
        return;
    };
    for entrada in entradas.flatten() {
        let Ok(tipo) = entrada.file_type() else {
            continue;
        };
        let Some(nombre) = entrada.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if !tipo.is_dir() || !nombre_valido(&nombre) {
            continue;
        }
        let Some(mut skill) = leer_skill(&raiz.join(&nombre)) else {
            continue;
        };
        if !out.by_name.contains_key(&skill.name) && out.by_name.len() >= MAX_SKILLS {
            return;
        }
        skill.project = project;
        out.by_name.insert(skill.name.clone(), skill);
    }
}

fn nombre_valido(nombre: &str) -> bool {
    !nombre.is_empty()
        && nombre != "."
        && nombre != ".."
        && nombre.len() <= 80
        && nombre
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

fn leer_skill(dir: &Path) -> Option<Skill> {
    let path = dir.join("SKILL.md");
    let datos = crate::instructions::leer_archivo_seguro(&path, MAX_SKILL)?;
    let texto = String::from_utf8(datos).ok()?;
    let (name, desc, cuerpo) = frontmatter(&texto);
    let name = match name.is_empty() {
        true => dir.file_name()?.to_str()?.to_owned(),
        false => name,
    };
    if !nombre_valido(&name) {
        return None;
    }
    Some(Skill {
        name,
        description: recortar_descripcion(if desc.is_empty() {
            primera_linea(&cuerpo)
        } else {
            desc
        }),
        path,
        dir: dir.to_path_buf(),
        project: false,
        content: cuerpo.trim().to_owned(),
        files: listar_archivos(dir),
    })
}

/// Separa el bloque YAML del cuerpo; solo entiende `name` y `description`.
fn frontmatter(texto: &str) -> (String, String, String) {
    let texto = texto.strip_prefix('\u{feff}').unwrap_or(texto);
    let lineas: Vec<&str> = texto.split('\n').collect();
    if !texto.starts_with("---") || lineas[0].trim() != "---" {
        return (String::new(), String::new(), texto.to_owned());
    }
    let Some(fin) = lineas.iter().skip(1).position(|l| l.trim() == "---") else {
        return (String::new(), String::new(), texto.to_owned());
    };
    let fin = fin + 1;
    let (mut name, mut desc) = (String::new(), String::new());
    for linea in &lineas[1..fin] {
        let Some((clave, valor)) = linea.split_once(':') else {
            continue;
        };
        let valor = valor.trim().trim_matches(|c| c == '"' || c == '\'');
        match clave.trim().to_lowercase().as_str() {
            "name" => name = valor.to_owned(),
            "description" => desc = valor.to_owned(),
            _ => {}
        }
    }
    (name, desc, lineas[fin + 1..].join("\n"))
}

fn recortar_descripcion(texto: String) -> String {
    if texto.chars().count() <= MAX_DESCRIPCION {
        return texto;
    }
    let mut corto: String = texto.chars().take(MAX_DESCRIPCION).collect();
    corto.push('…');
    corto
}

fn primera_linea(texto: &str) -> String {
    for linea in texto.split('\n') {
        let linea = linea.strip_prefix('#').unwrap_or(linea).trim();
        if linea.is_empty() {
            continue;
        }
        return if linea.chars().count() > 200 {
            format!("{}…", linea.chars().take(200).collect::<String>())
        } else {
            linea.to_owned()
        };
    }
    String::new()
}

fn listar_archivos(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut visitadas = 0usize;
    let mut pendientes = vec![(dir.to_path_buf(), 0usize)];
    while let Some((actual, profundidad)) = pendientes.pop() {
        if out.len() >= MAX_SKILL_FILES || visitadas >= MAX_SKILL_DIRS {
            continue;
        }
        visitadas += 1;
        let Ok(entradas) = fs::read_dir(&actual) else {
            continue;
        };
        let mut entradas: Vec<_> = entradas.take(MAX_SKILL_ENTRIES).flatten().collect();
        entradas.sort_by_key(|e| e.file_name());
        let mut subcarpetas = Vec::new();
        for entrada in entradas {
            if out.len() >= MAX_SKILL_FILES {
                break;
            }
            let Ok(tipo) = entrada.file_type() else {
                continue;
            };
            if tipo.is_symlink() {
                continue;
            }
            let ruta = entrada.path();
            if tipo.is_dir() {
                if profundidad < MAX_SKILL_DEPTH {
                    subcarpetas.push((ruta, profundidad + 1));
                }
            } else if entrada.file_name() != OsStr::new("SKILL.md")
                && let Ok(rel) = ruta.strip_prefix(dir)
            {
                out.push(rel.to_string_lossy().replace('\\', "/"));
            }
        }
        for subcarpeta in subcarpetas.into_iter().rev() {
            pendientes.push(subcarpeta);
        }
    }
    out.sort();
    out
}

#[derive(Debug, Clone)]
pub struct SkillTool {
    skills: Skills,
}

fn descripcion(skills: &Skills) -> String {
    if skills.list.is_empty() {
        return DESCRIPCION.to_owned();
    }
    let mut texto = format!("{DESCRIPCION} Hay {} disponibles: ", skills.list.len());
    let mut truncado = false;
    for (i, skill) in skills.list.iter().enumerate() {
        if i > 0 {
            texto.push_str(", ");
        }
        texto.push_str(&skill.name);
        if texto.len() > 1800 {
            texto.push_str("… (llama a skill sin nombre para ver la lista completa)");
            truncado = true;
            break;
        }
    }
    if !truncado {
        texto.push_str(". Llama a skill sin nombre para ver las descripciones.");
    }
    texto
}

fn bloque(skill: &Skill) -> String {
    let mut texto = format!(
        "<skill_content name=\"{}\">\n# Skill: {}\n\n{}\n\nBase directory for this skill: {}\nRelative paths in this skill are relative to this base directory.\n",
        skill.name,
        skill.name,
        skill.content,
        skill.dir.display()
    );
    if !skill.files.is_empty() {
        texto.push_str("\n<skill_files>\n");
        for archivo in &skill.files {
            texto.push_str(&format!("<file>{archivo}</file>\n"));
        }
        texto.push_str("</skill_files>\n");
    }
    texto.push_str("</skill_content>");
    texto
}

#[derive(serde::Deserialize, Default)]
struct Entrada {
    // `encoding/json` deja "" cuando el campo falta o llega `null`; serde no.
    #[serde(default)]
    name: Option<String>,
}

#[async_trait]
impl Tool for SkillTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "skill".to_owned(),
            description: descripcion(&self.skills),
            schema: serde_json::json!({"type":"object","properties":{"name":{"type":"string","description":"Nombre de la skill"}}}),
        }
    }

    fn risk(&self) -> Option<Risk> {
        Some(Risk::Read)
    }

    async fn run(&self, args: Value) -> Result<String, ToolError> {
        let entrada: Entrada = if args.is_null() {
            Entrada::default()
        } else {
            serde_json::from_value(args)
                .map_err(|e| ToolError::Message(format!("argumentos inválidos: {e}")))?
        };
        let nombre = entrada.name.unwrap_or_default();
        if nombre.trim().is_empty() {
            return Ok(self.listar());
        }
        let Some(skill) = self.skills.get(&nombre) else {
            return Err(ToolError::Message(format!(
                "no existe la skill {:?}; disponibles: {}",
                nombre,
                self.nombres()
            )));
        };
        Ok(bloque(skill))
    }
}

impl SkillTool {
    fn listar(&self) -> String {
        if self.skills.list.is_empty() {
            return "No hay skills instaladas.".to_owned();
        }
        let reserva = 64;
        let mut salida = String::from("Skills disponibles:");
        let mut incluidas = 0usize;
        for skill in &self.skills.list {
            let linea = match skill.description.is_empty() {
                true => format!("\n- {}", skill.name),
                false => format!("\n- {}: {}", skill.name, skill.description),
            };
            if salida.len() + linea.len() + reserva > MAX_LISTADO {
                break;
            }
            salida.push_str(&linea);
            incluidas += 1;
        }
        let omitidas = self.skills.list.len() - incluidas;
        if omitidas > 0 {
            salida.push_str(&format!("\n[se omitieron {omitidas} skills por tamaño]"));
        }
        salida
    }

    fn nombres(&self) -> String {
        match self.skills.list.is_empty() {
            true => "ninguna".to_owned(),
            false => self
                .skills
                .list
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>()
                .join(", "),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn con(name: &str, desc: &str, cuerpo: &str) -> String {
        format!("---\nname: {name}\ndescription: {desc}\n---\n{cuerpo}")
    }

    fn skill(base: &Path, dir: &str, texto: &str) -> PathBuf {
        let ruta = base.join(dir);
        fs::create_dir_all(&ruta).expect("dir");
        fs::write(ruta.join("SKILL.md"), texto).expect("skill");
        ruta
    }

    #[test]
    fn load_project_overrides_global() {
        let tmp = tempfile::tempdir().expect("tmp");
        let (home, data, cwd) = (
            tmp.path().join("home"),
            tmp.path().join("data"),
            tmp.path().join("proj"),
        );
        skill(
            &home.join(".agents").join("skills"),
            "comun",
            &con("comun", "global", "cuerpo global"),
        );
        skill(
            &data.join("skills"),
            "otra",
            &con("otra", "global otra", "x"),
        );
        skill(
            &cwd.join(".agents").join("skills"),
            "comun",
            &con("comun", "proyecto", "cuerpo proyecto"),
        );
        skill(&cwd.join("skills"), "local", "# Local\n\nhace cosas");
        let skills = load_with(&cwd, true, &data, Some(&home));
        let nombres: Vec<&str> = skills.list().iter().map(|s| s.name.as_str()).collect();
        assert_eq!(nombres, ["comun", "local", "otra"]);
        let comun = skills.get("comun").expect("comun");
        assert_eq!(
            (comun.description.as_str(), comun.content.as_str()),
            ("proyecto", "cuerpo proyecto")
        );
        assert!(comun.project && !skills.get("otra").expect("otra").project);
    }

    #[test]
    fn frontmatter_reads_name_and_desc() {
        let (n, d, c) =
            frontmatter("\u{feff}---\nName: \"Mi Skill\"\ndescription: 'hace algo'\n---\ncuerpo\n");
        assert_eq!(
            (n.as_str(), d.as_str(), c.as_str()),
            ("Mi Skill", "hace algo", "cuerpo\n")
        );
        let (n, d, texto) = frontmatter("---\nname: sin cierre\ncuerpo");
        assert!(n.is_empty() && d.is_empty());
        assert_eq!(texto, "---\nname: sin cierre\ncuerpo");
    }

    #[test]
    fn primera_linea_truncates_at_200() {
        let larga = "á".repeat(201);
        assert_eq!(
            primera_linea(&format!("# {larga}")),
            format!("{}…", "á".repeat(200))
        );
        assert_eq!(primera_linea("\n# título\nresto"), "título");
    }

    #[test]
    fn files_skip_symlinks_and_skill_md() {
        let tmp = tempfile::tempdir().expect("tmp");
        let dir = skill(&tmp.path().join("skills"), "s", "---\nname: s\n---\nx");
        fs::write(dir.join("b.txt"), "b").expect("b");
        fs::write(dir.join("SKILL.md.bak"), "x").expect("bak");
        fs::create_dir(dir.join("sub")).expect("sub");
        fs::write(dir.join("sub/a.txt"), "a").expect("a");
        let fuera = tmp.path().join("fuera.txt");
        fs::write(&fuera, "fuera").expect("fuera");
        let enlace = dir.join("enlace.txt");
        #[cfg(windows)]
        let creado = std::os::windows::fs::symlink_file(&fuera, &enlace).is_ok();
        #[cfg(not(windows))]
        let creado = std::os::unix::fs::symlink(&fuera, &enlace).is_ok();

        // La junction no pide privilegio: en Windows sin Developer Mode es
        // la que ejercita el skip de reparse points (issue #98). std la
        // reporta como symlink en `file_type`, igual que un enlace.
        #[cfg(windows)]
        let junta = {
            let fuera_dir = tmp.path().join("fuera-dir");
            fs::create_dir(&fuera_dir).expect("fuera-dir");
            fs::write(fuera_dir.join("secreto.txt"), "fuera").expect("secreto");
            let junta = dir.join("junta");
            let creada = std::process::Command::new("cmd")
                .args(["/c", "mklink", "/J"])
                .arg(&junta)
                .arg(&fuera_dir)
                .output()
                .is_ok_and(|salida| salida.status.success());
            creada.then_some(junta)
        };
        #[cfg(not(windows))]
        let junta: Option<PathBuf> = None;

        if !creado && junta.is_none() {
            // Skip explícito y ruidoso: sin enlace ni junction la rama del
            // skip no se ejercitó y el verde no significa cobertura.
            eprintln!(
                "SKIP #98: no se pudo crear symlink ni junction; \
                 la rama de enlaces de listar_archivos no se ejercitó"
            );
            return;
        }
        if !creado {
            eprintln!(
                "aviso #98: sin privilegio para el symlink de archivo; \
                 la rama se ejercita con la junction"
            );
        }
        let mut profunda = dir.join("profunda");
        for _ in 0..=MAX_SKILL_DEPTH {
            profunda = profunda.join("vacia");
            fs::create_dir_all(&profunda).expect("profunda");
        }
        fs::write(profunda.join("lejano.txt"), "lejano").expect("lejano");
        let skills = load_with(tmp.path(), false, &tmp.path().join("data"), None);
        assert_eq!(
            skills.get("s").expect("s").files,
            ["SKILL.md.bak", "b.txt", "sub/a.txt"]
        );
    }

    #[tokio::test]
    async fn tool_lists_and_loads() {
        let tmp = tempfile::tempdir().expect("tmp");
        let dir = skill(
            &tmp.path().join("skills"),
            "uno",
            &con("uno", "La primera", "Haz cosas"),
        );
        fs::write(dir.join("extra.md"), "x").expect("extra");
        let tool = load_with(tmp.path(), false, &tmp.path().join("data"), None).tool();
        let spec = tool.spec();
        assert_eq!(
            (spec.name.as_str(), tool.risk()),
            ("skill", Some(Risk::Read))
        );
        assert!(spec.description.contains("Hay 1 disponibles: uno"));
        assert_eq!(
            tool.run(json!({})).await.expect("lista"),
            "Skills disponibles:\n- uno: La primera"
        );
        let cargada = tool.run(json!({"name": "uno"})).await.expect("carga");
        assert!(cargada.starts_with(
            "<skill_content name=\"uno\">\n# Skill: uno\n\nHaz cosas\n\nBase directory for this skill: "
        ));
        assert!(cargada.contains("<skill_files>\n<file>extra.md</file>\n</skill_files>"));
        assert!(cargada.ends_with("</skill_content>"));
    }

    #[tokio::test]
    async fn tool_lista_topada_antes_de_crecer_sin_limite() {
        let tmp = tempfile::tempdir().expect("tmp");
        let desc = "d".repeat(MAX_DESCRIPCION);
        for n in 0..40 {
            let nombre = format!("s{n:02}");
            skill(
                &tmp.path().join("skills"),
                &nombre,
                &con(&nombre, &desc, "x"),
            );
        }
        let salida = load_with(tmp.path(), false, &tmp.path().join("data"), None)
            .tool()
            .run(json!({}))
            .await
            .expect("lista");
        assert!(salida.contains("[se omitieron"), "{salida:?}");
        assert!(
            salida.len() <= MAX_LISTADO,
            "la lista ocupó {} bytes",
            salida.len()
        );
    }

    #[tokio::test]
    async fn tool_unknown_name_lists_available() {
        let vacias = Skills::default();
        assert_eq!(
            vacias
                .tool()
                .run(json!({"name": null}))
                .await
                .expect("null"),
            "No hay skills instaladas."
        );
        assert_eq!(
            vacias
                .tool()
                .run(json!({"name": "x"}))
                .await
                .expect_err("error"),
            ToolError::Message("no existe la skill \"x\"; disponibles: ninguna".to_owned())
        );
        assert_eq!(
            vacias.tool().run(json!({})).await.expect("lista"),
            "No hay skills instaladas."
        );
        let tmp = tempfile::tempdir().expect("tmp");
        skill(&tmp.path().join("skills"), "uno", "---\nname: uno\n---\nx");
        let tool = load_with(tmp.path(), false, &tmp.path().join("data"), None).tool();
        let err = tool.run(json!({"name": "nope"})).await.expect_err("error");
        assert_eq!(
            err,
            ToolError::Message("no existe la skill \"nope\"; disponibles: uno".to_owned())
        );
        let err = tool.run(json!(42)).await.expect_err("error");
        assert!(err.to_string().starts_with("argumentos inválidos: "));
    }
}
