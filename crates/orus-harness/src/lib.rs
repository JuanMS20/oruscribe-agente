#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// Los tests sí pueden usar `unwrap` y `expect`: son la red que prueba el
// contrato, no código que atiende a una persona.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod compactar;
pub mod instructions;
pub mod mcptools;
pub mod sessions;
pub mod skills;

mod aprobaciones;
mod prompt;
mod turno;

pub use turno::{Toolset, Turno};

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

#[cfg(test)]
use aprobaciones::{aprobador, aprobador_plan, interpretar_respuesta, texto_prompt_llamada};
use aprobaciones::{aprobador_con_externo, aprobador_plan_con_externo};
use orus_agent::{
    AllowRuleFn, CancelToken, EmitFn, HERRAMIENTA_PROPOSE_PLAN, Mode, OnTokenFn, OpenAICompat,
    Provider, Risk, Runner, Step as PasoAgente, StepKind, ToolError, validar_base,
};
#[cfg(test)]
use orus_agent::{ApproveFn, Tool};
use orus_core::{Event, Message, ToolCall};
use orus_tools::{journal, permissions};
#[cfg(test)]
use prompt::prompt_sistema;
use prompt::{agregar_skills, agregar_subagente, config_local, sistema_del_turno};
#[cfg(test)]
use serde_json::Value;
use sessions::{Session, Store};

/// El contrato omite los contadores en cero.
fn distinto_de_cero(n: u64) -> Option<u64> {
    if n == 0 { None } else { Some(n) }
}

/// El aviso del `start` cuando no hay sesión de Oruscribe. Es el texto que se muestra
/// cuando falta la sesión.
pub const ORG_NOTE_SIN_SESION: &str = "sin sesión de Oruscribe; uso solo las herramientas locales. Abre la app e inicia sesión para usar tareas, chat y metas.";

/// Aprobación externa de una herramienta con su riesgo. La ventana la usa como
/// único `Approve`; `None` es una herramienta sin
/// riesgo declarado. La costura es síncrona porque `ApproveFn`
/// lo es (`orus-agent/src/runner.rs:85`).
pub type ApproveRiskFn =
    Arc<dyn Fn(&ToolCall, Option<Risk>) -> std::result::Result<bool, ToolError> + Send + Sync>;

/// El turno real: los datos ya resueltos por el borde más las dos costuras que
/// solo este camino necesita. `Deref` deja los accesos a los campos como antes
/// (`turno.modelo`, `turno.folders`, ...).
pub struct TurnoReal<'a> {
    pub datos: &'a Turno,
    /// Aprobación externa con el riesgo de la llamada (O5-07b). Con costura,
    /// contesta ella aunque no haya terminal; sin costura, la CLI se comporta
    /// como siempre.
    pub approve: Option<ApproveRiskFn>,
    /// Token con el que el borde corta el turno en curso. Sin token, el turno corre como antes.
    pub cancel: Option<CancelToken>,
}

impl std::ops::Deref for TurnoReal<'_> {
    type Target = Turno;

    fn deref(&self) -> &Self::Target {
        self.datos
    }
}

/// Error del turno real en la frontera del crate (R8).
#[derive(Debug, thiserror::Error)]
pub enum ErrorTurno {
    #[error("modo desconocido: {0}. Usa autonomous, ask o readonly.")]
    ModoDesconocido(String),
    #[error(transparent)]
    Proveedor(#[from] orus_agent::ProviderError),
    #[error(transparent)]
    Herramientas(#[from] orus_tools::localtools::Error),
    #[error(transparent)]
    Turno(#[from] orus_agent::RunnerError),
    #[error(transparent)]
    Instrucciones(#[from] instructions::InstructionsError),
    /// `journal.Open` falla antes de armar las herramientas.
    #[error("no se pudo abrir el diario de deshacer: {0}")]
    Diario(#[from] journal::Error),
    /// La respuesta llegó pero la sesión no se pudo escribir.
    #[error("la respuesta llegó pero no se pudo guardar la sesión: {0}")]
    Sesiones(#[from] sessions::SessionsError),

    /// Fallaron el turno y el guardado. Se
    /// conservan las dos fuentes para no perder el aviso de historial perdido.
    #[error("el turno falló ({turno}) y no se pudo guardar la sesión ({sesiones})")]
    TurnoYSesiones {
        turno: orus_agent::RunnerError,
        sesiones: sessions::SessionsError,
    },
}

/// El `start` del turno real. `org` viaja solo si hay herramientas de la
/// organización y `org_note` solo si hay algo que avisar.
pub fn inicio(turno: &TurnoReal<'_>, sesion: &str) -> Event {
    Event::Start {
        session: sesion.to_owned(),
        model: turno.modelo.to_owned(),
        cwd: turno.cwd.to_owned(),
        mode: turno.modo.to_owned(),
        org: (!turno.org_tools.is_empty()).then_some(true),
        org_note: (!turno.org_note.is_empty()).then(|| turno.org_note.clone()),
    }
}

/// Corre el turno con el proveedor real: un turno,
/// un proveedor, las herramientas locales y los eventos del contrato.
///
/// `sesion` llega cargada o nueva desde la CLI; acá se le agrega el mensaje,
/// se reanuda su historial y se guarda aunque el turno falle
/// `guardar` es `None` cuando el borde no persiste.
///
/// `emitir` recibe cada evento en orden; los deltas llegan mientras el
/// proveedor transmite.
pub async fn correr_turno_real<F>(
    turno: &TurnoReal<'_>,
    sesion: &mut Session,
    guardar: Option<&Store>,
    emitir: F,
) -> std::result::Result<(), ErrorTurno>
where
    F: Fn(Event) + Send + Sync + 'static,
{
    Mode::parse(&turno.modo).ok_or_else(|| ErrorTurno::ModoDesconocido(turno.modo.to_owned()))?;
    let proveedor: Arc<dyn Provider> = match &turno.proveedor {
        Some(proveedor) => Arc::clone(proveedor),
        None => Arc::new(OpenAICompat::new(
            validar_base(&turno.base_url)?,
            turno.clave.clone(),
        )?),
    };
    correr_turno_con(
        turno,
        sesion,
        guardar,
        emitir,
        proveedor,
        journal::default_dir(),
    )
    .await
}

/// Como [`correr_turno_real`], con proveedor y diario inyectables para los tests.
async fn correr_turno_con<F>(
    turno: &TurnoReal<'_>,
    sesion: &mut Session,
    guardar: Option<&Store>,
    emitir: F,
    proveedor: Arc<dyn Provider>,
    diario_dir: impl AsRef<Path>,
) -> std::result::Result<(), ErrorTurno>
where
    F: Fn(Event) + Send + Sync + 'static,
{
    let modo = Mode::parse(&turno.modo)
        .ok_or_else(|| ErrorTurno::ModoDesconocido(turno.modo.to_owned()))?;
    // El cerebro de la organización trae su prompt y sus herramientas del
    // servidor: el contrato no arma locales, instrucciones ni skills.
    // El contrato descubre instrucciones antes de abrir el diario.
    let sistema = if turno.cerebro {
        String::new()
    } else {
        sistema_del_turno(turno)?
    };
    // Las skills se cargan antes de armar las herramientas locales: sus
    // carpetas entran como raíces de solo lectura, para que el modelo pueda
    // abrir los `references/*.md` que el bloque de la skill le promete.
    let skills = if turno.cerebro || turno.no_skills {
        None
    } else {
        Some(skills::load(Path::new(&turno.cwd), true))
    };
    // Sin carpetas permitidas no hay herramientas locales, y el diario no se
    // abre. Con carpetas permitidas, tanto `Mi clave`
    // como `IA de Oruscribe` pueden usar las herramientas Pi (`read`, `write`,
    // `edit`, `bash`) confinadas a esas carpetas y sujetas a permisos/aprobación.
    let locales = if turno.folders.is_empty() {
        None
    } else {
        let diario = journal::Journal::open(diario_dir)?;
        Some(Arc::new(orus_tools::localtools::Set::new(config_local(
            turno,
            diario,
            skills.as_ref(),
        ))?))
    };
    let mut herramientas = turno.org_tools.clone();
    if let Some(locales) = &locales {
        herramientas.extend(if turno.cerebro {
            locales.tools_pi()
        } else {
            match turno.toolset {
                Toolset::Full => locales.tools(),
                Toolset::Pi => locales.tools_pi(),
            }
        });
    }
    let mut riesgos: HashMap<String, Option<Risk>> = HashMap::new();
    for herramienta in &herramientas {
        riesgos
            .entry(herramienta.spec().name)
            .or_insert_with(|| herramienta.risk());
    }
    // El plan se aprueba con la misma puerta que una escritura.
    riesgos.insert(HERRAMIENTA_PROPOSE_PLAN.to_owned(), Some(Risk::Write));

    if let Some(skills) = &skills {
        agregar_skills(&mut herramientas, &mut riesgos, skills);
    }
    // El subagente se arma con la lista ya filtrada: `task` no está en ella, así
    // que no se delega a sí mismo. El cerebro no lo lleva.
    if !turno.cerebro {
        let _ = agregar_subagente(
            &mut herramientas,
            &mut riesgos,
            Arc::clone(&proveedor),
            &turno.modelo,
            &sistema,
        );
    }
    // La memoria va después del subagente para que `task` no la vea
    // Si el índice no abrió, la CLI no pasa nada.
    if let Some(memoria) = &turno.memoria {
        riesgos.insert(memoria.spec().name.clone(), memoria.risk());
        herramientas.push(Arc::clone(memoria));
    }
    let riesgos = Arc::new(riesgos);
    let emitir = Arc::new(emitir);
    let al_token: OnTokenFn = {
        let emitir = Arc::clone(&emitir);
        Arc::new(move |texto: &str| {
            if !texto.is_empty() {
                emitir(Event::Delta {
                    text: texto.to_owned(),
                });
            }
        })
    };
    let al_paso: EmitFn = {
        let emitir = Arc::clone(&emitir);
        Arc::new(move |paso: PasoAgente| {
            let evento = match paso.kind {
                StepKind::Answer => Event::Answer { text: paso.text },
                StepKind::ToolCall => Event::ToolCall {
                    tool: paso.tool,
                    args: paso.text,
                },
                StepKind::ToolResult => Event::ToolResult {
                    tool: paso.tool,
                    text: paso.text,
                },
                StepKind::Denied => Event::Denied { tool: paso.tool },
                // El contrato no pinta el paso `plan`: no está en
                // el contrato de eventos.
                StepKind::Plan => return,
            };
            emitir(evento);
        })
    };
    let aprobar = aprobador_con_externo(
        turno.approve.clone(),
        Arc::clone(&riesgos),
        turno.yes,
        turno.allow_exec,
        turno.es_tty,
    );
    let aprobar_plan = aprobador_plan_con_externo(turno.approve.clone(), turno.yes, turno.es_tty);
    // Las reglas recordadas se aplican antes de preguntar:
    // el runner no llama a `approve` cuando `allow_rule` responde que sí.
    let regla_permitida: AllowRuleFn = Arc::new(|llamada: &ToolCall, riesgo: Risk| {
        permissions::check(&llamada.name, &llamada.args, riesgo)
    });

    let mut runner = Runner::new(proveedor, turno.modelo.clone());
    runner.system = sistema;
    runner.tools = herramientas;
    runner.mode = Some(modo);
    // El valor por defecto del contrato.
    runner.chat_retries = 1;
    runner.max_steps = turno.max_steps;
    // La ventana del modelo llega al runner para que la compactación y la
    // guardia del 95 % funcionen (#80, issue #93). El cerebro no reporta uso
    // de tokens: su ventana no se puede medir y el presupuesto de caracteres
    // sigue siendo su única red. `max_chars` no tiene fuente declarada en el
    // turno: queda el presupuesto por defecto del runner.
    runner.window_tokens = ventana_del_turno(turno);
    runner.approve = Some(aprobar);
    runner.plan_approve = Some(aprobar_plan);
    runner.allow_rule = Some(regla_permitida);
    runner.on_token = Some(al_token);
    runner.emit = Some(al_paso);

    emitir(inicio(turno, &sesion.id));

    sesion.messages.push(Message {
        role: "user".to_owned(),
        content: turno.mensaje.to_owned(),
        // `turno` es `TurnoReal`; `imagenes` llega por `Deref` desde `Turno`.
        images: (!turno.imagenes.is_empty()).then(|| turno.imagenes.clone()),
        ..Message::default()
    });
    let cancel = turno.cancel.clone().unwrap_or_default();
    let (mensajes, resultado) = runner.run(&sesion.messages, &cancel).await;
    let uso = runner.usage;

    // El id se clona antes de reemplazar el historial y guardar: los eventos
    // no pueden compartir el préstamo mutable con `save`.
    let id = sesion.id.clone();
    sesion.messages = mensajes;
    sesion.tokens_in += uso.prompt;
    sesion.tokens_out += uso.completion;
    sesion.model = turno.modelo.to_owned();

    // El contrato guarda si hay historial aunque el turno falle.
    let guardado = match guardar {
        Some(store) if !sesion.messages.is_empty() => {
            store.save(Some(sesion)).map_err(ErrorTurno::Sesiones)
        }
        _ => Ok(()),
    };

    emitir(Event::Usage {
        session: id.clone(),
        prompt_tokens: tokens(uso.prompt),
        completion_tokens: tokens(uso.completion),
        cost_usd: None,
    });

    match (resultado, guardado) {
        (Err(turno), Err(ErrorTurno::Sesiones(sesiones))) => {
            // Ninguno se traga al otro.
            Err(ErrorTurno::TurnoYSesiones { turno, sesiones })
        }
        (Err(turno), _) => Err(turno.into()),
        (Ok(()), guardado) => {
            guardado?;
            emitir(Event::Done { session: id });
            Ok(())
        }
    }
}

/// Los contadores son `i64` en el runner y `u64` en el evento; el contrato omite el
/// cero.
fn tokens(n: i64) -> Option<u64> {
    u64::try_from(n).ok().and_then(distinto_de_cero)
}

fn ventana_del_turno(turno: &Turno) -> i64 {
    if turno.window_tokens > 0 {
        turno.window_tokens
    } else if turno.cerebro {
        0
    } else {
        ventana_del_modelo(&turno.modelo)
    }
}

/// Ventana de contexto de las familias de modelos con ventana estable y
/// pública. El catálogo (`orus-agent/src/catalog.rs`) todavía no declara
/// ventanas por modelo; mientras lo hace, el arnés declara estas y el resto
/// queda en 0: el comportamiento de siempre (#80, issue #93).
fn ventana_del_modelo(modelo: &str) -> i64 {
    let modelo = modelo.trim().to_ascii_lowercase();
    let modelo = modelo.rsplit('/').next().unwrap_or(&modelo);
    if modelo.starts_with("claude") {
        200_000
    } else if modelo.starts_with("gpt-4o") {
        128_000
    } else if modelo.starts_with("gpt-4.1") {
        1_000_000
    } else if modelo.starts_with("gpt-5") {
        400_000
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mcptools::Backend as _;

    fn turno_real() -> Turno {
        Turno {
            base_url: "http://127.0.0.1:8787/v1".to_owned(),
            modelo: "fake".to_owned(),
            modo: "ask".to_owned(),
            cwd: "C:\\repo".to_owned(),
            folders: Vec::new(),
            cerebro: false,
            mensaje: "resume el README".to_owned(),
            imagenes: Vec::new(),
            clave: "clave-falsa".to_owned(),
            proveedor: None,
            programas: Vec::new(),
            toolset: Toolset::Full,
            max_steps: 0,
            no_agents_md: false,
            no_skills: false,
            yes: false,
            allow_exec: false,
            es_tty: false,
            memoria: None,
            org_tools: Vec::new(),
            org_note: ORG_NOTE_SIN_SESION.to_owned(),
            window_tokens: 0,
            cost_input_per_mtok: 0.0,
            cost_output_per_mtok: 0.0,
        }
    }

    use async_trait::async_trait;
    use orus_agent::{ChatReply, ChatRequest, ToolError, ToolSpec};

    struct ProveedorNulo;

    #[async_trait]
    impl Provider for ProveedorNulo {
        async fn chat(
            &self,
            _req: ChatRequest,
        ) -> std::result::Result<ChatReply, orus_agent::ProviderError> {
            Err(orus_agent::ProviderError::local("no se usa en este test"))
        }
    }

    struct Herramienta {
        nombre: &'static str,
        riesgo: Option<Risk>,
    }

    #[async_trait]
    impl Tool for Herramienta {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: self.nombre.to_owned(),
                description: String::new(),
                schema: serde_json::json!({}),
            }
        }

        fn risk(&self) -> Option<Risk> {
            self.riesgo
        }

        async fn run(&self, _args: serde_json::Value) -> std::result::Result<String, ToolError> {
            Ok(String::new())
        }
    }

    fn herramienta(nombre: &'static str, riesgo: Risk) -> Arc<dyn Tool> {
        Arc::new(Herramienta {
            nombre,
            riesgo: Some(riesgo),
        })
    }

    #[test]
    fn prompt_incluye_instrucciones_y_programas() {
        let tmp = tempfile::tempdir().expect("tmp");
        std::fs::write(tmp.path().join("AGENTS.md"), "regla del proyecto").expect("agents");
        let cwd = tmp.path().to_str().expect("cwd utf-8");
        let instrucciones = instructions::discover(tmp.path(), false).expect("instrucciones");
        let programas = vec!["git".to_owned(), "cargo".to_owned()];
        let prompt = prompt_sistema(cwd, &programas, &instrucciones, false, Toolset::Full);

        assert!(
            prompt.contains("Programas permitidos para local_run_command: git, cargo.\n"),
            "{prompt}"
        );
        assert!(prompt.contains("regla del proyecto"), "{prompt}");
        assert!(
            prompt.contains(
                "En esta ejecución no tienes herramientas de Oruscribe: no inventes tareas, mensajes, metas ni avisos de la organización.\n"
            ),
            "{prompt}"
        );
        let pos_programas = prompt.find("Programas permitidos").expect("programas");
        let pos_instrucciones = prompt
            .find("<instrucciones_del_proyecto>")
            .expect("instrucciones");
        assert!(
            pos_programas < pos_instrucciones,
            "los programas van antes de las instrucciones: {prompt}"
        );
        let con_org = prompt_sistema(cwd, &programas, &instrucciones, true, Toolset::Full);
        assert!(
            con_org.contains("También tienes las herramientas de Oruscribe: tareas, chat, metas, avisos y actividad. Úsalas para leer o cambiar datos reales de la organización; si un dato no está, dilo en vez de inventarlo.\n"),
            "{con_org}"
        );
        assert!(
            con_org.contains("nunca sondees las herramientas de lectura en bucles"),
            "el prompt fija la regla del timbre: {con_org}"
        );
    }

    #[test]
    fn el_prompt_de_pi_nombra_las_cuatro_herramientas() {
        let tmp = tempfile::tempdir().expect("tmp");
        let cwd = tmp.path().to_str().expect("cwd utf-8");
        let instrucciones = instructions::discover(tmp.path(), false).expect("instrucciones");
        let programas = vec!["git".to_owned()];

        let prompt = prompt_sistema(cwd, &programas, &instrucciones, false, Toolset::Pi);

        assert!(
            prompt.contains(
                "Tienes read, write, edit y bash. bash ejecuta en la carpeta de trabajo; no es una jaula del sistema.\n"
            ),
            "{prompt}"
        );
        assert!(!prompt.contains("local_run_command"), "{prompt}");
        assert!(
            prompt.contains("Programas permitidos para bash: git.\n"),
            "{prompt}"
        );
    }

    async fn nombres_de_herramientas(toolset: Toolset) -> Vec<String> {
        let tmp = tempfile::tempdir().expect("tmp");
        let cwd = tmp.path().to_str().expect("cwd utf-8");
        let captura = Arc::new(ProveedorCaptura(std::sync::Mutex::new(Vec::new())));
        let proveedor: Arc<dyn Provider> = captura.clone();
        let folders = vec![cwd.to_owned()];
        let mut datos = turno_sin_org(cwd, &folders, String::new());
        datos.toolset = toolset;
        let turno = TurnoReal {
            datos: &datos,
            approve: None,
            cancel: None,
        };
        let mut sesion = sessions::new(cwd, "fake");

        let resultado = correr_turno_con(
            &turno,
            &mut sesion,
            None,
            |_| {},
            proveedor,
            tmp.path().join("journal"),
        )
        .await;
        assert!(resultado.is_ok(), "{resultado:?}");

        let pedidos = captura.0.lock().expect("captura del proveedor");
        assert_eq!(pedidos.len(), 1, "el turno habla una vez con el proveedor");
        pedidos[0].clone()
    }

    #[tokio::test]
    async fn pi_arma_solo_las_cuatro_herramientas_locales() {
        let nombres = nombres_de_herramientas(Toolset::Pi).await;

        assert_eq!(
            nombres,
            ["read", "write", "edit", "bash", "task"],
            "las cuatro de Pi y el subagente"
        );
        assert!(
            nombres.iter().all(|nombre| !nombre.starts_with("local_")),
            "ninguna local del CLI: {nombres:?}"
        );
    }

    #[tokio::test]
    async fn full_arma_las_seis_locales_de_siempre() {
        let nombres = nombres_de_herramientas(Toolset::Full).await;

        assert_eq!(
            nombres,
            [
                "local_list_dir",
                "local_read_file",
                "local_search_text",
                "local_write_file",
                "local_edit_file",
                "local_run_command",
                "task"
            ],
            "las seis locales y el subagente"
        );
    }

    #[tokio::test]
    async fn el_tope_de_pasos_del_turno_llega_al_runner() {
        let tmp = tempfile::tempdir().expect("tmp");
        let cwd = tmp.path().to_str().expect("cwd utf-8");
        let proveedor = Arc::new(ProveedorGuion(std::sync::Mutex::new(
            std::collections::VecDeque::from([
                respuesta_herramienta("eco"),
                respuesta_herramienta("eco"),
            ]),
        )));
        let como_proveedor: Arc<dyn Provider> = proveedor.clone();
        let folders = vec![cwd.to_owned()];
        let mut datos = turno_sin_org(cwd, &folders, String::new());
        datos.max_steps = 2;
        datos.org_tools = vec![herramienta("eco", Risk::Read)];
        let turno = TurnoReal {
            datos: &datos,
            approve: None,
            cancel: None,
        };
        let mut sesion = sessions::new(cwd, "fake");

        let resultado = correr_turno_con(
            &turno,
            &mut sesion,
            None,
            |_| {},
            como_proveedor,
            tmp.path().join("journal"),
        )
        .await;

        match resultado {
            Err(ErrorTurno::Turno(orus_agent::RunnerError::TopeDePasos)) => {}
            otro => panic!("el tope de dos pasos debía cortar el turno, llegó {otro:?}"),
        }
        assert_eq!(
            proveedor.0.lock().expect("guion del proveedor").len(),
            0,
            "el turno pidió exactamente dos veces y paró"
        );
    }

    #[tokio::test]
    async fn el_tope_en_cero_usa_el_del_runner() {
        let tmp = tempfile::tempdir().expect("tmp");
        let cwd = tmp.path().to_str().expect("cwd utf-8");
        let proveedor = Arc::new(ProveedorGuion(std::sync::Mutex::new(
            std::collections::VecDeque::from([
                respuesta_herramienta("eco"),
                respuesta_herramienta("eco"),
                respuesta_herramienta("eco"),
                respuesta(Message {
                    role: "assistant".to_owned(),
                    content: "listo".to_owned(),
                    ..Message::default()
                }),
            ]),
        )));
        let como_proveedor: Arc<dyn Provider> = proveedor.clone();
        let folders = vec![cwd.to_owned()];
        let mut datos = turno_sin_org(cwd, &folders, String::new());
        datos.max_steps = 0;
        datos.org_tools = vec![herramienta("eco", Risk::Read)];
        let turno = TurnoReal {
            datos: &datos,
            approve: None,
            cancel: None,
        };
        let mut sesion = sessions::new(cwd, "fake");

        let resultado = correr_turno_con(
            &turno,
            &mut sesion,
            None,
            |_| {},
            como_proveedor,
            tmp.path().join("journal"),
        )
        .await;

        assert!(resultado.is_ok(), "{resultado:?}");
        assert_eq!(
            proveedor.0.lock().expect("guion del proveedor").len(),
            0,
            "tres pasos caben en los doce por defecto"
        );
    }

    #[test]
    fn task_no_se_delega_a_si_mismo() {
        let proveedor: Arc<dyn Provider> = Arc::new(ProveedorNulo);
        let mut herramientas: Vec<Arc<dyn Tool>> = vec![
            herramienta("leer", Risk::Read),
            herramienta("escribir", Risk::Write),
        ];
        let mut riesgos = HashMap::new();

        let task = agregar_subagente(
            &mut herramientas,
            &mut riesgos,
            proveedor,
            "fake",
            "sistema del padre",
        )
        .expect("hay una herramienta de lectura");

        assert_eq!(task.spec().name, "task");
        assert_eq!(task.system, "sistema del padre");
        let vistas: Vec<String> = task.tools.iter().map(|tool| tool.spec().name).collect();
        assert_eq!(vistas, ["leer"], "solo las herramientas de lectura");
        assert!(
            vistas.iter().all(|nombre| *nombre != "task"),
            "el subagente no se delega a sí mismo: {vistas:?}"
        );
        assert_eq!(
            herramientas
                .iter()
                .filter(|tool| tool.spec().name == "task")
                .count(),
            1,
            "task se agrega una sola vez"
        );
        assert_eq!(riesgos.get("task"), Some(&Some(Risk::Read)));
    }

    #[test]
    fn program_se_pasa_a_localtools() {
        let tmp = tempfile::tempdir().expect("tmp");
        let cwd = tmp.path().to_str().expect("cwd utf-8");
        let programas = vec!["git".to_owned(), "cargo".to_owned()];
        let folders = vec![cwd.to_owned(), "C:\\otra".to_owned()];
        let datos = Turno {
            base_url: "http://127.0.0.1:8787/v1".to_owned(),
            modelo: "fake".to_owned(),
            modo: "ask".to_owned(),
            cwd: cwd.to_owned(),
            folders: folders.clone(),
            cerebro: false,
            mensaje: "hola".to_owned(),
            imagenes: Vec::new(),
            clave: "clave-falsa".to_owned(),
            proveedor: None,
            programas: programas.clone(),
            toolset: Toolset::Full,
            max_steps: 0,
            no_agents_md: true,
            no_skills: true,
            yes: false,
            allow_exec: false,
            es_tty: false,
            memoria: None,
            org_tools: Vec::new(),
            org_note: String::new(),
            window_tokens: 0,
            cost_input_per_mtok: 0.0,
            cost_output_per_mtok: 0.0,
        };
        let turno = TurnoReal {
            datos: &datos,
            approve: None,
            cancel: None,
        };
        let diario = journal::Journal::open(tmp.path().join("journal")).expect("diario");

        let config = config_local(&turno, diario, None);

        assert_eq!(
            config.folders, folders,
            "todas las carpetas permitidas llegan al set local"
        );
        assert_eq!(
            config.programs, programas,
            "los --program repetidos llegan al set local"
        );
        assert!(config.journal.is_some(), "el diario viaja al set local");
    }

    /// El bloque de la skill promete `references/*.md`; las herramientas de
    /// lectura los abren aunque la skill viva fuera de las carpetas del
    /// usuario, y la escritura en esa carpeta sigue devolviendo `Fuera`
    /// (issue #61).
    #[tokio::test]
    async fn las_carpetas_de_las_skills_son_solo_lectura() {
        let tmp = tempfile::tempdir().expect("tmp");
        let home = tmp.path().join("home");
        let data = tmp.path().join("data");
        let cwd = tmp.path().join("proy");
        std::fs::create_dir_all(&cwd).expect("cwd");
        let dir = home.join(".agents").join("skills").join("global");
        std::fs::create_dir_all(dir.join("references")).expect("references");
        std::fs::write(
            dir.join("SKILL.md"),
            "---\nname: global\ndescription: de afuera\n---\nLee references/x.md\n",
        )
        .expect("SKILL.md");
        let referencia = dir.join("references").join("x.md");
        std::fs::write(&referencia, "contenido de la skill\n").expect("x.md");

        let skills = skills::load_with(&cwd, true, &data, Some(&home));
        assert_eq!(
            skills.list().len(),
            1,
            "la skill global se cargó: {:?}",
            skills.list()
        );
        let bloque = skills
            .tool()
            .run(serde_json::json!({"name": "global"}))
            .await
            .expect("bloque de la skill");
        assert!(
            bloque.contains("<file>references/x.md</file>"),
            "el bloque promete el archivo: {bloque}"
        );

        let cwd = cwd.to_string_lossy().into_owned();
        let folders = vec![cwd.clone()];
        let datos = turno_sin_org(&cwd, &folders, String::new());
        let turno = TurnoReal {
            datos: &datos,
            approve: None,
            cancel: None,
        };
        let diario = journal::Journal::open(tmp.path().join("journal")).expect("diario");
        let locales = Arc::new(
            orus_tools::localtools::Set::new(config_local(&turno, diario, Some(&skills)))
                .expect("set local"),
        );
        let herramientas = locales.tools();
        let buscar = |nombre: &str| {
            herramientas
                .iter()
                .find(|herramienta| herramienta.spec().name == nombre)
                .expect("herramienta registrada")
                .clone()
        };

        let leida = buscar("local_read_file")
            .run(serde_json::json!({"path": referencia.to_string_lossy()}))
            .await
            .expect("el archivo de la skill se lee");
        assert_eq!(leida, "contenido de la skill\n");

        let listado = buscar("local_list_dir")
            .run(serde_json::json!({"path": dir.join("references").to_string_lossy()}))
            .await
            .expect("la carpeta de la skill se lista");
        assert!(listado.contains("x.md"), "{listado}");

        let nueva = dir.join("references").join("otra.md");
        let error = buscar("local_write_file")
            .run(serde_json::json!({
                "path": nueva.to_string_lossy(),
                "content": "x"
            }))
            .await
            .expect_err("la carpeta de la skill no se escribe");
        assert_eq!(
            error.to_string(),
            "la ruta queda fuera de las carpetas permitidas"
        );
        assert!(!nueva.exists(), "la escritura no debía llegar al disco");
    }

    fn mapa_de_riesgos(riesgos: &[(&str, Option<Risk>)]) -> Arc<HashMap<String, Option<Risk>>> {
        let mapa = riesgos.iter().map(|(n, r)| ((*n).to_owned(), *r)).collect();
        Arc::new(mapa)
    }

    fn riesgos_write_exec() -> Arc<HashMap<String, Option<Risk>>> {
        mapa_de_riesgos(&[
            ("escribir", Some(Risk::Write)),
            ("ejecutar", Some(Risk::Exec)),
        ])
    }

    fn llamada(nombre: &str) -> ToolCall {
        ToolCall {
            name: nombre.to_owned(),
            ..ToolCall::default()
        }
    }

    #[test]
    fn yes_approves_write_but_not_exec() {
        let aprobar = aprobador(riesgos_write_exec(), true, false, false);
        assert!(aprobar(&llamada("escribir")).unwrap());
        assert!(!aprobar(&llamada("ejecutar")).unwrap());
    }

    #[test]
    fn allow_exec_approves_exec() {
        let aprobar = aprobador(riesgos_write_exec(), false, true, false);
        assert!(aprobar(&llamada("ejecutar")).unwrap());
        assert!(!aprobar(&llamada("escribir")).unwrap());
    }

    #[test]
    fn no_tty_rejects_ask() {
        let riesgos = mapa_de_riesgos(&[("escribir", Some(Risk::Write)), ("sin_riesgo", None)]);
        let aprobar = aprobador(riesgos, false, false, false);
        assert!(!aprobar(&llamada("escribir")).unwrap());
        let banderas = aprobador(mapa_de_riesgos(&[("sin_riesgo", None)]), true, true, false);
        assert!(!banderas(&llamada("sin_riesgo")).unwrap());
        assert!(interpretar_respuesta(" Sí ") && !interpretar_respuesta("n"));
        let esperado = "\nEl agente quiere escribir un archivo:\n  e null\n¿Permitir? [s/N] ";
        let obtenido = texto_prompt_llamada("e", &Value::Null, Some(Risk::Write));
        assert_eq!(obtenido, esperado);
        let plan = Value::String("p".to_owned());
        let obtenido = texto_prompt_llamada(HERRAMIENTA_PROPOSE_PLAN, &plan, Some(Risk::Write));
        assert_eq!(
            obtenido,
            "\nEl agente quiere ejecutar este plan:\np\n¿Permitir? [s/N] "
        );
    }

    #[test]
    fn plan_requires_yes_without_tty() {
        assert!(!aprobador_plan(false, false)("paso 1").unwrap());
        assert!(aprobador_plan(true, false)("paso 1").unwrap());
    }

    #[test]
    fn external_approve_receives_risk() {
        use std::sync::Mutex;

        let vistos = Arc::new(Mutex::new(Vec::new()));
        let capturados = Arc::clone(&vistos);
        let externo: ApproveRiskFn = Arc::new(move |llamada: &ToolCall, riesgo: Option<Risk>| {
            capturados
                .lock()
                .expect("captura de la costura")
                .push((llamada.name.clone(), riesgo));
            Ok(false)
        });
        let aprobar =
            aprobador_con_externo(Some(externo), riesgos_write_exec(), false, false, false);

        assert!(
            !aprobar(&llamada("escribir")).expect("la costura contesta"),
            "la respuesta de la costura manda"
        );
        let vistos = vistos.lock().expect("captura de la costura");
        assert_eq!(vistos.len(), 1, "la costura recibe la llamada");
        assert_eq!(vistos[0].0, "escribir");
        assert_eq!(
            vistos[0].1,
            Some(Risk::Write),
            "la costura recibe el riesgo real de la herramienta"
        );
    }

    #[test]
    fn external_approve_bypasses_tty_gate() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let llamadas = Arc::new(AtomicUsize::new(0));
        let contador = Arc::clone(&llamadas);
        let externo: ApproveRiskFn = Arc::new(move |_: &ToolCall, _: Option<Risk>| {
            contador.fetch_add(1, Ordering::SeqCst);
            Ok(true)
        });
        let aprobar =
            aprobador_con_externo(Some(externo), riesgos_write_exec(), false, false, false);

        assert!(
            aprobar(&llamada("escribir")).expect("la costura contesta"),
            "la costura decide aunque no haya terminal"
        );
        assert_eq!(
            llamadas.load(Ordering::SeqCst),
            1,
            "es_tty=false no salta la costura"
        );
    }

    #[test]
    fn yes_skips_external_approve() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let llamadas = Arc::new(AtomicUsize::new(0));
        let contador = Arc::clone(&llamadas);
        let externo: ApproveRiskFn = Arc::new(move |_: &ToolCall, _: Option<Risk>| {
            contador.fetch_add(1, Ordering::SeqCst);
            Ok(false)
        });
        let aprobar =
            aprobador_con_externo(Some(externo), riesgos_write_exec(), true, false, false);

        assert!(
            aprobar(&llamada("escribir")).expect("--yes aprueba"),
            "--yes aprueba la escritura"
        );
        assert_eq!(
            llamadas.load(Ordering::SeqCst),
            0,
            "la costura no se consulta con --yes"
        );
    }

    #[test]
    fn external_plan_approve_uses_propose_plan() {
        use std::sync::Mutex;

        let vistos = Arc::new(Mutex::new(Vec::new()));
        let capturados = Arc::clone(&vistos);
        let externo: ApproveRiskFn = Arc::new(move |llamada: &ToolCall, riesgo: Option<Risk>| {
            capturados.lock().expect("captura de la costura").push((
                llamada.name.clone(),
                llamada.args.clone(),
                riesgo,
            ));
            Ok(true)
        });
        let aprobar = aprobador_plan_con_externo(Some(externo), false, false);

        assert!(
            aprobar("paso 1").expect("la costura contesta"),
            "la costura aprueba el plan"
        );
        let vistos = vistos.lock().expect("captura de la costura");
        assert_eq!(vistos.len(), 1, "la costura recibe el plan una vez");
        assert_eq!(vistos[0].0, HERRAMIENTA_PROPOSE_PLAN);
        assert_eq!(vistos[0].1, Value::String("paso 1".to_owned()));
        assert_eq!(vistos[0].2, Some(Risk::Write));
    }

    #[test]
    fn without_external_non_tty_still_denies() {
        let aprobar = aprobador_con_externo(None, riesgos_write_exec(), false, false, false);
        assert!(
            !aprobar(&llamada("escribir")).expect("sin terminal se rechaza"),
            "una herramienta AskUser sin costura y sin terminal se rechaza"
        );

        let aprobar_plan = aprobador_plan_con_externo(None, false, false);
        assert!(
            !aprobar_plan("paso 1").expect("sin terminal el plan se rechaza"),
            "un plan sin costura y sin terminal se rechaza"
        );
    }

    struct ProveedorGuion(std::sync::Mutex<std::collections::VecDeque<ChatReply>>);

    #[async_trait]
    impl Provider for ProveedorGuion {
        async fn chat(
            &self,
            _req: ChatRequest,
        ) -> std::result::Result<ChatReply, orus_agent::ProviderError> {
            self.0
                .lock()
                .expect("el guion del proveedor")
                .pop_front()
                .ok_or_else(|| {
                    orus_agent::ProviderError::local("el proveedor se quedó sin respuestas")
                })
        }
    }

    fn respuesta(mensaje: Message) -> ChatReply {
        ChatReply {
            message: mensaje,
            prompt_tokens: 0,
            completion_tokens: 0,
        }
    }

    fn respuesta_herramienta(nombre: &str) -> ChatReply {
        respuesta(Message {
            role: "assistant".to_owned(),
            tool_calls: Some(vec![llamada(nombre)]),
            ..Message::default()
        })
    }

    #[tokio::test]
    async fn remembered_rule_skips_prompt() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let aprobaciones = Arc::new(AtomicUsize::new(0));
        let contador = Arc::clone(&aprobaciones);
        let approve: ApproveFn = Arc::new(move |_: &ToolCall| {
            contador.fetch_add(1, Ordering::SeqCst);
            Ok(false)
        });
        let consultas = Arc::new(AtomicUsize::new(0));
        let contador_reglas = Arc::clone(&consultas);
        let allow_rule: AllowRuleFn = Arc::new(move |_: &ToolCall, _: Risk| {
            contador_reglas.fetch_add(1, Ordering::SeqCst);
            true
        });
        let proveedor: Arc<dyn Provider> = Arc::new(ProveedorGuion(std::sync::Mutex::new(
            std::collections::VecDeque::from([
                respuesta_herramienta("escribir"),
                respuesta(Message {
                    role: "assistant".to_owned(),
                    content: "listo".to_owned(),
                    ..Message::default()
                }),
            ]),
        )));
        let mut runner = Runner::new(proveedor, "fake");
        runner.mode = Some(Mode::Ask);
        runner.tools = vec![herramienta("escribir", Risk::Write)];
        runner.approve = Some(approve);
        runner.allow_rule = Some(allow_rule);

        let mensaje = Message {
            role: "user".to_owned(),
            content: "hola".to_owned(),
            ..Message::default()
        };
        let (mensajes, resultado) = runner.run(&[mensaje], &CancelToken::new()).await;

        assert!(resultado.is_ok(), "{resultado:?}");
        assert_eq!(aprobaciones.load(Ordering::SeqCst), 0);
        assert_eq!(consultas.load(Ordering::SeqCst), 1);
        assert!(mensajes.iter().any(|m| m.content.is_empty()));
    }

    /// El turno `go-lectura-simple` cuenta las claves del `start`: `session`,
    /// `model`, `cwd`, `mode` y `org_note`, sin `org` (`parity/turnos`).
    #[test]
    fn el_start_sin_sesion_de_oruscribe_lleva_org_note_y_no_org() {
        let datos = turno_real();
        let turno = TurnoReal {
            datos: &datos,
            approve: None,
            cancel: None,
        };
        let evento = inicio(&turno, "123");
        let valor = serde_json::to_value(&evento).expect("start serializa");
        assert_eq!(valor["org_note"], ORG_NOTE_SIN_SESION);
        assert!(
            valor.get("org").is_none(),
            "sin herramientas de Oruscribe, org no viaja: {valor}"
        );
        assert_eq!(valor["session"], "123");
        assert_eq!(
            valor.as_object().map(serde_json::Map::len),
            Some(6),
            "las 6 claves del turno congelado: {valor}"
        );
        let mut datos = turno_real();
        datos.org_tools = vec![herramienta("get_my_tasks", Risk::Read)];
        datos.org_note = String::new();
        let con_org = TurnoReal {
            datos: &datos,
            approve: None,
            cancel: None,
        };
        let valor = serde_json::to_value(inicio(&con_org, "123")).expect("start serializa");
        assert_eq!(valor["org"], true);
    }

    struct BackendOrg {
        specs: Vec<orus_org::ToolSpec>,
    }

    #[async_trait]
    impl mcptools::Backend for BackendOrg {
        async fn tool_specs(&self) -> orus_org::Result<Vec<orus_org::ToolSpec>> {
            Ok(self.specs.clone())
        }

        async fn call(
            &self,
            _nombre: &str,
            _args: orus_org::JsonObject,
        ) -> orus_org::Result<String> {
            Ok(String::new())
        }
    }

    fn spec_org(nombre: &str) -> orus_org::ToolSpec {
        orus_org::ToolSpec {
            name: nombre.to_owned(),
            description: String::new(),
            schema: serde_json::json!({"type": "object"}),
        }
    }

    struct ProveedorCaptura(std::sync::Mutex<Vec<Vec<String>>>);

    #[async_trait]
    impl Provider for ProveedorCaptura {
        async fn chat(
            &self,
            req: ChatRequest,
        ) -> std::result::Result<ChatReply, orus_agent::ProviderError> {
            self.0.lock().expect("captura del proveedor").push(
                req.tools
                    .iter()
                    .map(|herramienta| herramienta.name.clone())
                    .collect(),
            );
            Ok(respuesta(Message {
                role: "assistant".to_owned(),
                content: "listo".to_owned(),
                ..Message::default()
            }))
        }
    }

    struct ProveedorSistema(std::sync::Mutex<Vec<(Vec<String>, Option<String>)>>);

    #[async_trait]
    impl Provider for ProveedorSistema {
        async fn chat(
            &self,
            req: ChatRequest,
        ) -> std::result::Result<ChatReply, orus_agent::ProviderError> {
            let sistema = req
                .messages
                .iter()
                .find(|mensaje| mensaje.role == "system")
                .map(|mensaje| mensaje.content.clone());
            self.0.lock().expect("captura del proveedor").push((
                req.tools
                    .iter()
                    .map(|herramienta| herramienta.name.clone())
                    .collect(),
                sistema,
            ));
            Ok(respuesta(Message {
                role: "assistant".to_owned(),
                content: "listo".to_owned(),
                ..Message::default()
            }))
        }
    }

    fn turno_sin_org(cwd: &str, folders: &[String], org_note: String) -> Turno {
        Turno {
            base_url: "http://127.0.0.1:8787/v1".to_owned(),
            modelo: "fake".to_owned(),
            modo: "readonly".to_owned(),
            cwd: cwd.to_owned(),
            folders: folders.to_vec(),
            cerebro: false,
            mensaje: "hola".to_owned(),
            imagenes: Vec::new(),
            clave: "clave-falsa".to_owned(),
            proveedor: None,
            programas: Vec::new(),
            toolset: Toolset::Full,
            max_steps: 0,
            no_agents_md: true,
            no_skills: true,
            yes: false,
            allow_exec: false,
            es_tty: false,
            memoria: None,
            org_tools: Vec::new(),
            org_note,
            window_tokens: 0,
            cost_input_per_mtok: 0.0,
            cost_output_per_mtok: 0.0,
        }
    }

    #[tokio::test]
    async fn org_tools_are_filtered() {
        let tmp = tempfile::tempdir().expect("tmp");
        let cwd = tmp.path().to_str().expect("cwd utf-8");

        let backend = Arc::new(BackendOrg {
            specs: vec![
                spec_org("get_my_tasks"),
                spec_org("create_agent"),
                spec_org("send_chat_message"),
            ],
        });
        let specs = backend.tool_specs().await.expect("specs del backend");
        let org_tools = mcptools::build(
            Some(backend as Arc<dyn mcptools::Backend>),
            specs,
            &mcptools::RegistroLecturas::new(),
        );
        let captura = Arc::new(ProveedorCaptura(std::sync::Mutex::new(Vec::new())));
        let proveedor: Arc<dyn Provider> = captura.clone();
        let mut sesion = sessions::new(cwd, "fake");
        let folders = vec![cwd.to_owned()];
        let mut datos = turno_sin_org(cwd, &folders, String::new());
        datos.org_tools = org_tools;
        let turno = TurnoReal {
            datos: &datos,
            approve: None,
            cancel: None,
        };

        let resultado = correr_turno_con(
            &turno,
            &mut sesion,
            None,
            |_| {},
            proveedor,
            tmp.path().join("journal"),
        )
        .await;
        assert!(resultado.is_ok(), "{resultado:?}");

        let pedidos = captura.0.lock().expect("captura del proveedor");
        assert_eq!(pedidos.len(), 1, "el turno habla una vez con el proveedor");
        let herramientas = &pedidos[0];
        let pos_org = herramientas
            .iter()
            .position(|nombre| nombre == "get_my_tasks")
            .expect("la herramienta de la organización llega al proveedor");
        let pos_local = herramientas
            .iter()
            .position(|nombre| nombre.starts_with("local_"))
            .expect("las herramientas locales llegan al proveedor");
        assert!(
            pos_org < pos_local,
            "las org van antes que las locales: {herramientas:?}"
        );
        assert!(
            herramientas.iter().all(|nombre| nombre != "create_agent"),
            "la prohibida no llega al proveedor: {herramientas:?}"
        );
    }

    #[tokio::test]
    async fn cerebro_no_arma_locales_ni_sistema() {
        let tmp = tempfile::tempdir().expect("tmp");
        let cwd = tmp.path().to_str().expect("cwd utf-8");
        std::fs::write(tmp.path().join("AGENTS.md"), "instrucciones locales\n").expect("AGENTS.md");
        let folders = vec![cwd.to_owned()];
        let captura = Arc::new(ProveedorSistema(std::sync::Mutex::new(Vec::new())));
        let proveedor: Arc<dyn Provider> = captura.clone();
        let mut datos = turno_sin_org(cwd, &folders, String::new());
        datos.cerebro = true;
        datos.no_agents_md = false;
        datos.no_skills = false;
        let turno = TurnoReal {
            datos: &datos,
            approve: None,
            cancel: None,
        };
        let mut sesion = sessions::new(cwd, "fake");

        let resultado = correr_turno_con(
            &turno,
            &mut sesion,
            None,
            |_| {},
            proveedor,
            tmp.path().join("journal"),
        )
        .await;
        assert!(resultado.is_ok(), "{resultado:?}");

        {
            let pedidos = captura.0.lock().expect("captura del proveedor");
            assert_eq!(pedidos.len(), 1, "el turno habla una vez con el proveedor");
            let (herramientas, sistema) = &pedidos[0];
            assert_eq!(
                herramientas,
                &vec![
                    "read".to_owned(),
                    "write".to_owned(),
                    "edit".to_owned(),
                    "bash".to_owned(),
                ],
                "con carpetas permitidas, el cerebro expone exactamente las 4 herramientas Pi: {herramientas:?}"
            );
            assert!(
                herramientas
                    .iter()
                    .all(|nombre| !nombre.starts_with("local_")
                        && nombre != "skill"
                        && nombre != "task"),
                "el cerebro no lleva herramientas legacy local_*, skills ni subagente: {herramientas:?}"
            );
            assert_eq!(
                *sistema, None,
                "el cerebro no manda instrucciones locales al servidor"
            );
        }

        // Sin carpetas permitidas, el cerebro no arma herramientas locales.
        let sin_carpetas = Vec::new();
        let captura_vacia = Arc::new(ProveedorSistema(std::sync::Mutex::new(Vec::new())));
        let mut datos_vacios = turno_sin_org(cwd, &sin_carpetas, String::new());
        datos_vacios.cerebro = true;
        let turno_vacio = TurnoReal {
            datos: &datos_vacios,
            approve: None,
            cancel: None,
        };
        let mut sesion_vacia = sessions::new(cwd, "fake");
        correr_turno_con(
            &turno_vacio,
            &mut sesion_vacia,
            None,
            |_| {},
            captura_vacia.clone(),
            tmp.path().join("journal-vacio"),
        )
        .await
        .expect("cerebro sin carpetas");
        let pedidos_vacios = captura_vacia.0.lock().expect("captura vacía");
        assert!(
            pedidos_vacios[0].0.is_empty(),
            "sin carpetas el cerebro no adjunta herramientas: {:?}",
            pedidos_vacios[0].0
        );
    }

    struct ProveedorMensajes(std::sync::Mutex<Vec<Vec<Message>>>);

    #[async_trait]
    impl Provider for ProveedorMensajes {
        async fn chat(
            &self,
            req: ChatRequest,
        ) -> std::result::Result<ChatReply, orus_agent::ProviderError> {
            self.0
                .lock()
                .expect("captura del proveedor")
                .push(req.messages);
            Ok(respuesta(Message {
                role: "assistant".to_owned(),
                content: "listo".to_owned(),
                ..Message::default()
            }))
        }
    }

    /// El mensaje que arma `correr_turno_real` lleva la imagen completa a la
    /// sesión y al proveedor.
    #[tokio::test]
    async fn la_imagen_del_turno_llega_al_proveedor_y_a_la_sesion() {
        let tmp = tempfile::tempdir().expect("tmp");
        let cwd = tmp.path().to_str().expect("cwd utf-8");
        let captura = Arc::new(ProveedorMensajes(std::sync::Mutex::new(Vec::new())));
        let proveedor: Arc<dyn Provider> = captura.clone();
        let folders = vec![cwd.to_owned()];
        let imagen = orus_core::Image {
            mime: "image/png".to_owned(),
            data: "iVBORw0KGgo=".to_owned(),
            preview: "data:image/png;base64,iVBORw0KGgo=".to_owned(),
        };
        let mut datos = turno_sin_org(cwd, &folders, String::new());
        datos.mensaje = "mira".to_owned();
        datos.imagenes = vec![imagen.clone()];
        let turno = TurnoReal {
            datos: &datos,
            approve: None,
            cancel: None,
        };
        let mut sesion = sessions::new(cwd, "fake");

        let resultado = correr_turno_con(
            &turno,
            &mut sesion,
            None,
            |_| {},
            proveedor,
            tmp.path().join("journal"),
        )
        .await;
        assert!(resultado.is_ok(), "{resultado:?}");

        let pedidos = captura.0.lock().expect("captura del proveedor");
        let usuario = pedidos[0]
            .iter()
            .find(|mensaje| mensaje.role == "user")
            .expect("el proveedor recibe el mensaje de la persona");
        assert_eq!(usuario.content, "mira");
        let esperada = vec![imagen.clone()];
        assert_eq!(
            usuario.images.as_ref(),
            Some(&esperada),
            "la imagen completa viaja al proveedor"
        );
        drop(pedidos);
        assert!(
            sesion.messages.iter().any(|mensaje| mensaje.role == "user"
                && mensaje.images.as_ref() == Some(&esperada)),
            "la imagen queda en el historial de la sesión: {:?}",
            sesion.messages
        );
    }

    #[tokio::test]
    async fn sin_imagenes_el_mensaje_no_lleva_la_lista() {
        let tmp = tempfile::tempdir().expect("tmp");
        let cwd = tmp.path().to_str().expect("cwd utf-8");
        let captura = Arc::new(ProveedorMensajes(std::sync::Mutex::new(Vec::new())));
        let proveedor: Arc<dyn Provider> = captura.clone();
        let folders = vec![cwd.to_owned()];
        let datos = turno_sin_org(cwd, &folders, String::new());
        let turno = TurnoReal {
            datos: &datos,
            approve: None,
            cancel: None,
        };
        let mut sesion = sessions::new(cwd, "fake");

        let resultado = correr_turno_con(
            &turno,
            &mut sesion,
            None,
            |_| {},
            proveedor,
            tmp.path().join("journal"),
        )
        .await;
        assert!(resultado.is_ok(), "{resultado:?}");

        let pedidos = captura.0.lock().expect("captura del proveedor");
        let usuario = pedidos[0]
            .iter()
            .find(|mensaje| mensaje.role == "user")
            .expect("el proveedor recibe el mensaje de la persona");
        assert_eq!(
            usuario.images, None,
            "un turno de solo texto no manda `Images`"
        );
    }

    #[tokio::test]
    async fn org_failure_does_not_kill_turn() {
        let tmp = tempfile::tempdir().expect("tmp");
        let cwd = tmp.path().to_str().expect("cwd utf-8");
        let nota = "no se pudo conectar con Oruscribe: sin red. Sigo con las herramientas locales.";
        let proveedor = Arc::new(ProveedorGuion(std::sync::Mutex::new(
            std::collections::VecDeque::from([respuesta(Message {
                role: "assistant".to_owned(),
                content: "sigo sin organización".to_owned(),
                ..Message::default()
            })]),
        )));
        let como_proveedor: Arc<dyn Provider> = proveedor.clone();
        let eventos = Arc::new(std::sync::Mutex::new(Vec::new()));
        let emitir = {
            let eventos = Arc::clone(&eventos);
            move |evento| eventos.lock().expect("eventos").push(evento)
        };
        let mut sesion = sessions::new(cwd, "fake");
        let folders = vec![cwd.to_owned()];
        let datos = turno_sin_org(cwd, &folders, nota.to_owned());
        let turno = TurnoReal {
            datos: &datos,
            approve: None,
            cancel: None,
        };

        let resultado = correr_turno_con(
            &turno,
            &mut sesion,
            None,
            emitir,
            como_proveedor,
            tmp.path().join("journal"),
        )
        .await;
        assert!(resultado.is_ok(), "{resultado:?}");
        assert!(
            proveedor.0.lock().expect("guion del proveedor").is_empty(),
            "el proveedor no respondió: la organización tumbó el turno"
        );

        let eventos = eventos.lock().expect("eventos");
        match eventos.first() {
            Some(Event::Start { org, org_note, .. }) => {
                assert!(org.is_none(), "sin herramientas, org no viaja");
                assert_eq!(org_note.as_deref(), Some(nota));
            }
            otro => panic!("el turno abre con start, llegó {otro:?}"),
        }
    }

    struct HerramientaCanceladora {
        token: CancelToken,
    }

    #[async_trait]
    impl Tool for HerramientaCanceladora {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: "cancelar".to_owned(),
                description: String::new(),
                schema: serde_json::json!({}),
            }
        }

        fn risk(&self) -> Option<Risk> {
            Some(Risk::Read)
        }

        async fn run(&self, _args: serde_json::Value) -> std::result::Result<String, ToolError> {
            self.token.cancelar();
            Ok(String::new())
        }
    }

    struct HerramientaContada {
        nombre: &'static str,
        ejecuciones: Arc<std::sync::atomic::AtomicUsize>,
    }

    #[async_trait]
    impl Tool for HerramientaContada {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: self.nombre.to_owned(),
                description: String::new(),
                schema: serde_json::json!({}),
            }
        }

        fn risk(&self) -> Option<Risk> {
            Some(Risk::Read)
        }

        async fn run(&self, _args: serde_json::Value) -> std::result::Result<String, ToolError> {
            self.ejecuciones
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(String::new())
        }
    }

    /// La cancelación del borde llega al runner: el token de `TurnoReal` corta
    /// el turno y la herramienta siguiente no se ejecuta. Sin esa costura, el
    /// runner usaría un token nuevo y el turno terminaría bien.
    #[tokio::test]
    async fn cancel_stops_turn() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let tmp = tempfile::tempdir().expect("tmp");
        let cwd = tmp.path().to_str().expect("cwd utf-8");
        let cancel = CancelToken::new();
        let siguientes = Arc::new(AtomicUsize::new(0));
        let proveedor = Arc::new(ProveedorGuion(std::sync::Mutex::new(
            std::collections::VecDeque::from([
                respuesta_herramienta("cancelar"),
                respuesta_herramienta("siguiente"),
                respuesta(Message {
                    role: "assistant".to_owned(),
                    content: "no debía llegar".to_owned(),
                    ..Message::default()
                }),
            ]),
        )));
        let como_proveedor: Arc<dyn Provider> = proveedor.clone();
        let folders = vec![cwd.to_owned()];
        let mut datos = turno_sin_org(cwd, &folders, String::new());
        datos.org_tools = vec![
            Arc::new(HerramientaCanceladora {
                token: cancel.clone(),
            }),
            Arc::new(HerramientaContada {
                nombre: "siguiente",
                ejecuciones: Arc::clone(&siguientes),
            }),
        ];
        let turno = TurnoReal {
            datos: &datos,
            approve: None,
            cancel: Some(cancel),
        };
        let mut sesion = sessions::new(cwd, "fake");

        let resultado = correr_turno_con(
            &turno,
            &mut sesion,
            None,
            |_| {},
            como_proveedor,
            tmp.path().join("journal"),
        )
        .await;

        match resultado {
            Err(ErrorTurno::Turno(orus_agent::RunnerError::Cancelado)) => {}
            otro => panic!("se esperaba la cancelación del turno, llegó {otro:?}"),
        }
        assert_eq!(
            siguientes.load(Ordering::SeqCst),
            0,
            "la herramienta siguiente no se ejecuta tras cancelar"
        );
        assert_eq!(
            proveedor.0.lock().expect("guion del proveedor").len(),
            2,
            "el turno no vuelve a pedir al proveedor después de cancelar"
        );
    }

    /// #131: si fallan el turno y el guardado, el error del guardado no puede
    /// descartarse en silencio: la persona perdió también el historial.
    #[tokio::test]
    async fn fallo_de_turno_y_de_guardado_se_reportan_juntos() {
        let tmp = tempfile::tempdir().expect("tmp");
        let cwd = tmp.path().to_str().expect("cwd utf-8");
        let cancel = CancelToken::new();
        let proveedor = Arc::new(ProveedorGuion(std::sync::Mutex::new(
            std::collections::VecDeque::from([respuesta_herramienta("cancelar")]),
        )));
        let como_proveedor: Arc<dyn Provider> = proveedor.clone();
        let folders = vec![cwd.to_owned()];
        let mut datos = turno_sin_org(cwd, &folders, String::new());
        datos.org_tools = vec![Arc::new(HerramientaCanceladora {
            token: cancel.clone(),
        })];
        let turno = TurnoReal {
            datos: &datos,
            approve: None,
            cancel: Some(cancel),
        };
        let mut sesion = sessions::new(cwd, "fake");
        // Un id inválido hace fallar el guardado con `InvalidId`.
        sesion.id = "id-invalido".to_owned();
        let store = sessions::Store {
            dir: tmp.path().join("sesiones"),
            prefix: String::new(),
        };

        let error = correr_turno_con(
            &turno,
            &mut sesion,
            Some(&store),
            |_| {},
            como_proveedor,
            tmp.path().join("journal"),
        )
        .await
        .expect_err("el turno se cancela");

        let mensaje = error.to_string();
        assert!(
            mensaje.contains("guardar"),
            "se descartó el error de guardado: {mensaje}"
        );
    }

    /// Dos carpetas permitidas: una escritura en la segunda y una lectura de la
    /// primera; el `cwd` sigue siendo la primera.
    #[tokio::test]
    async fn folders_escriben_en_la_segunda_y_leen_la_primera() {
        let tmp = tempfile::tempdir().expect("tmp");
        let primera = tmp.path().join("primera");
        let segunda = tmp.path().join("segunda");
        std::fs::create_dir_all(&primera).expect("primera");
        std::fs::create_dir_all(&segunda).expect("segunda");
        let leeme = primera.join("leeme.txt");
        std::fs::write(&leeme, "contenido de la primera\n").expect("leeme");
        let destino = segunda.join("salida.txt");
        let cwd = primera.to_str().expect("cwd utf-8");
        let folders = vec![
            primera.to_string_lossy().into_owned(),
            segunda.to_string_lossy().into_owned(),
        ];
        let proveedor: Arc<dyn Provider> = Arc::new(ProveedorGuion(std::sync::Mutex::new(
            std::collections::VecDeque::from([
                respuesta(Message {
                    role: "assistant".to_owned(),
                    tool_calls: Some(vec![ToolCall {
                        id: "1".to_owned(),
                        name: "local_write_file".to_owned(),
                        args: serde_json::json!({
                            "path": destino.to_string_lossy(),
                            "content": "escrito en la segunda\n"
                        }),
                        ..ToolCall::default()
                    }]),
                    ..Message::default()
                }),
                respuesta(Message {
                    role: "assistant".to_owned(),
                    tool_calls: Some(vec![ToolCall {
                        id: "2".to_owned(),
                        name: "local_read_file".to_owned(),
                        args: serde_json::json!({"path": leeme.to_string_lossy()}),
                        ..ToolCall::default()
                    }]),
                    ..Message::default()
                }),
                respuesta(Message {
                    role: "assistant".to_owned(),
                    content: "listo".to_owned(),
                    ..Message::default()
                }),
            ]),
        )));
        let eventos = Arc::new(std::sync::Mutex::new(Vec::new()));
        let emitir = {
            let eventos = Arc::clone(&eventos);
            move |evento| eventos.lock().expect("eventos").push(evento)
        };
        let datos = Turno {
            base_url: "http://127.0.0.1:8787/v1".to_owned(),
            modelo: "fake".to_owned(),
            modo: "ask".to_owned(),
            cwd: cwd.to_owned(),
            folders: folders.clone(),
            cerebro: false,
            mensaje: "hola".to_owned(),
            imagenes: Vec::new(),
            clave: "clave-falsa".to_owned(),
            proveedor: None,
            programas: Vec::new(),
            toolset: Toolset::Full,
            max_steps: 0,
            no_agents_md: true,
            no_skills: true,
            yes: true,
            allow_exec: false,
            es_tty: false,
            memoria: None,
            org_tools: Vec::new(),
            org_note: String::new(),
            window_tokens: 0,
            cost_input_per_mtok: 0.0,
            cost_output_per_mtok: 0.0,
        };
        let turno = TurnoReal {
            datos: &datos,
            approve: None,
            cancel: None,
        };
        let mut sesion = sessions::new(cwd, "fake");

        let resultado = correr_turno_con(
            &turno,
            &mut sesion,
            None,
            emitir,
            proveedor,
            tmp.path().join("journal"),
        )
        .await;

        assert!(resultado.is_ok(), "{resultado:?}");
        assert_eq!(
            std::fs::read_to_string(&destino).expect("la escritura quedó en la segunda"),
            "escrito en la segunda\n"
        );
        assert!(
            sesion.messages.iter().any(|mensaje| mensaje.role == "tool"
                && mensaje.content.contains("contenido de la primera")),
            "la lectura de la primera llegó al historial: {:?}",
            sesion.messages
        );
        let eventos = eventos.lock().expect("eventos");
        match eventos.first() {
            Some(Event::Start { cwd: visto, .. }) => {
                assert_eq!(visto, cwd, "la primera carpeta sigue siendo el cwd");
            }
            otro => panic!("el turno abre con start, llegó {otro:?}"),
        }
    }

    /// La ventana del modelo llega al runner: con un modelo de ventana
    /// declarada, la guardia del 95 % detiene el turno antes del siguiente
    /// pedido (#80, issue #93). Sin la ventana, el turno seguiría pidiendo.
    #[tokio::test]
    async fn la_ventana_del_modelo_dispara_la_guardia_del_runner() {
        let tmp = tempfile::tempdir().expect("tmp");
        let cwd = tmp.path().to_str().expect("cwd utf-8");
        let mut primera = respuesta_herramienta("eco");
        primera.prompt_tokens = 190_000;
        let proveedor = Arc::new(ProveedorGuion(std::sync::Mutex::new(
            std::collections::VecDeque::from([
                primera,
                respuesta(Message {
                    role: "assistant".to_owned(),
                    content: "no debía llegar".to_owned(),
                    ..Message::default()
                }),
            ]),
        )));
        let como_proveedor: Arc<dyn Provider> = proveedor.clone();
        let folders = vec![cwd.to_owned()];
        let mut datos = turno_sin_org(cwd, &folders, String::new());
        datos.modelo = "claude-sonnet-4-5".to_owned();
        datos.org_tools = vec![herramienta("eco", Risk::Read)];
        let turno = TurnoReal {
            datos: &datos,
            approve: None,
            cancel: None,
        };
        let mut sesion = sessions::new(cwd, "fake");

        let resultado = correr_turno_con(
            &turno,
            &mut sesion,
            None,
            |_| {},
            como_proveedor,
            tmp.path().join("journal"),
        )
        .await;

        match resultado {
            Err(ErrorTurno::Turno(orus_agent::RunnerError::VentanaLlena { usados, ventana })) => {
                assert_eq!(usados, 190_000);
                assert_eq!(ventana, 200_000);
            }
            otro => panic!("la guardia del 95 % debía detener el turno: {otro:?}"),
        }
        assert_eq!(
            proveedor.0.lock().expect("guion del proveedor").len(),
            1,
            "el turno no vuelve a pedir al proveedor con la ventana llena"
        );
    }

    /// La ventana se declara por familia estable; el resto queda en 0 y
    /// conserva el comportamiento de siempre (#80).
    #[test]
    fn la_ventana_se_declara_por_familia_y_el_resto_queda_en_cero() {
        assert_eq!(ventana_del_modelo("claude-sonnet-4-5"), 200_000);
        assert_eq!(ventana_del_modelo("Claude-Opus-4-1"), 200_000);
        assert_eq!(ventana_del_modelo("gpt-4o-mini"), 128_000);
        assert_eq!(ventana_del_modelo("gpt-4.1-2025-04-14"), 1_000_000);
        assert_eq!(ventana_del_modelo("gpt-5"), 400_000);
        assert_eq!(
            ventana_del_modelo("anthropic/claude-sonnet-4-5"),
            200_000,
            "el id de OpenRouter trae el proveedor delante"
        );
        assert_eq!(
            ventana_del_modelo("openai/gpt-4o-mini"),
            128_000,
            "el id de OpenRouter trae el proveedor delante"
        );
        assert_eq!(ventana_del_modelo("fake"), 0);
        assert_eq!(ventana_del_modelo("minimax-m2"), 0);

        let mut turno = turno_real();
        turno.modelo = "gpt-5".to_owned();
        assert_eq!(ventana_del_turno(&turno), 400_000);
        turno.window_tokens = 8192;
        assert_eq!(
            ventana_del_turno(&turno),
            8192,
            "la ventana declarada gana a la de la familia del modelo"
        );
    }

    /// Verifica de punta a punta que el arnés Pi encadena `write` -> `read` ->
    /// `edit` -> `bash` sobre una carpeta real y emite los eventos de llamada y
    /// resultado en orden.
    #[tokio::test]
    async fn turno_pi_encadena_write_read_edit_y_bash() {
        let tmp = tempfile::tempdir().expect("tmp");
        let cwd = tmp.path().to_str().expect("cwd utf-8");
        let archivo = tmp.path().join("nota.txt");
        let ruta = archivo.to_str().expect("ruta utf-8");

        let paso = |id: &str, name: &str, args: serde_json::Value| {
            respuesta(Message {
                role: "assistant".to_owned(),
                tool_calls: Some(vec![ToolCall {
                    id: id.to_owned(),
                    name: name.to_owned(),
                    args,
                    ..ToolCall::default()
                }]),
                ..Message::default()
            })
        };

        let proveedor = Arc::new(ProveedorGuion(std::sync::Mutex::new(
            std::collections::VecDeque::from([
                paso(
                    "call_w",
                    "write",
                    serde_json::json!({"path": ruta, "content": "hola pi\n"}),
                ),
                paso("call_r", "read", serde_json::json!({"path": ruta})),
                paso(
                    "call_e",
                    "edit",
                    serde_json::json!({
                        "path": ruta,
                        "old_string": "hola pi",
                        "new_string": "hola oruscribe"
                    }),
                ),
                paso(
                    "call_b",
                    "bash",
                    serde_json::json!({"command": "echo pi-harness-ok", "cwd": cwd}),
                ),
                respuesta(Message {
                    role: "assistant".to_owned(),
                    content: "todas las herramientas Pi corrieron".to_owned(),
                    ..Message::default()
                }),
            ]),
        )));
        let como_proveedor: Arc<dyn Provider> = proveedor.clone();

        let folders = vec![cwd.to_owned()];
        let mut datos = turno_sin_org(cwd, &folders, String::new());
        datos.modo = "autonomous".to_owned();
        datos.toolset = Toolset::Pi;
        datos.max_steps = 10;
        let turno = TurnoReal {
            datos: &datos,
            approve: None,
            cancel: None,
        };
        let mut sesion = sessions::new(cwd, "fake");
        let eventos = Arc::new(std::sync::Mutex::new(Vec::<Event>::new()));
        let eventos_emitir = Arc::clone(&eventos);

        correr_turno_con(
            &turno,
            &mut sesion,
            None,
            move |ev| eventos_emitir.lock().expect("eventos").push(ev),
            como_proveedor,
            tmp.path().join("journal"),
        )
        .await
        .expect("el turno Pi completo debe terminar sin errores");

        let contenido = std::fs::read_to_string(&archivo).expect("nota.txt en disco");
        assert_eq!(contenido, "hola oruscribe\n");

        let registrados = eventos.lock().expect("eventos");
        let llamadas: Vec<&str> = registrados
            .iter()
            .filter_map(|ev| match ev {
                Event::ToolCall { tool, .. } => Some(tool.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(llamadas, vec!["write", "read", "edit", "bash"]);

        let resultado_bash = registrados
            .iter()
            .find_map(|ev| match ev {
                Event::ToolResult { tool, text } if tool == "bash" => Some(text.as_str()),
                _ => None,
            })
            .expect("debe emitir ToolResult para bash");
        assert!(
            resultado_bash.contains("pi-harness-ok"),
            "la salida de bash debe contener pi-harness-ok: {resultado_bash}"
        );
    }
}
