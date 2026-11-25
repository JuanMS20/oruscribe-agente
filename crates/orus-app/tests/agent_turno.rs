//! La costura real del turno: `Agent::send` con `HarnessRunner`,
//! `correr_turno_real` y el proveedor falso en `127.0.0.1` (O5-07e).

use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use orus_app::{Agent, ApprovalQueue, Conversations, HarnessRunner, Turno};
use orus_harness::Toolset;
use orus_harness::sessions::Store;
use serde_json::json;

/// Mata el proveedor falso aunque el test falle en un `assert`.
struct ProveedorFalso {
    hijo: Child,
}

impl Drop for ProveedorFalso {
    fn drop(&mut self) {
        let _ = self.hijo.kill();
        let _ = self.hijo.wait();
    }
}

/// `ORUSCRIBE_FAKE_PROVIDER` o el binario junto a los tests, en `target/debug`.
fn ruta_del_binario() -> PathBuf {
    if let Some(ruta) = std::env::var_os("ORUSCRIBE_FAKE_PROVIDER") {
        return PathBuf::from(ruta);
    }
    let actual = std::env::current_exe().expect("la ruta del test");
    let debug = actual
        .parent()
        .and_then(|deps| deps.parent())
        .expect("la carpeta target/debug");
    let nombre = if cfg!(windows) {
        "fake-provider.exe"
    } else {
        "fake-provider"
    };
    debug.join(nombre)
}

fn puerto_libre() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("un puerto libre");
    let puerto = listener.local_addr().expect("la dirección local").port();
    drop(listener);
    puerto
}

/// Espera a que el proveedor acepte conexiones; 5 s como máximo.
async fn esperar_puerto(puerto: u16) {
    let limite = Instant::now() + Duration::from_secs(5);
    while Instant::now() < limite {
        if TcpStream::connect(("127.0.0.1", puerto)).is_ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("el proveedor falso no escuchó en 127.0.0.1:{puerto} en 5 s");
}

fn escribir_guion(ruta: &Path, leeme: &Path) {
    let guion = json!({
        "respuestas": [
            {
                "texto": ["Miro el archivo. "],
                "herramienta": {
                    "id": "call_1",
                    "nombre": "local_read_file",
                    "argumentos": { "path": leeme.to_str().expect("ruta utf-8") }
                },
                "uso": { "prompt_tokens": 100, "completion_tokens": 12 }
            },
            {
                "texto": ["Listo."],
                "uso": { "prompt_tokens": 130, "completion_tokens": 9 }
            }
        ]
    });
    std::fs::write(
        ruta,
        serde_json::to_string_pretty(&guion).expect("el guion es JSON"),
    )
    .expect("el guion temporal");
}

fn arrancar(binario: &Path, guion: &Path, puerto: u16, registro: &Path) -> ProveedorFalso {
    let hijo = Command::new(binario.as_os_str())
        .arg("--guion")
        .arg(guion.as_os_str())
        .arg("--puerto")
        .arg(puerto.to_string())
        .arg("--registro")
        .arg(registro.as_os_str())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap_or_else(|error| {
            panic!(
                "no pude arrancar {}: {error}; compílalo con: cargo build -p fake-provider",
                binario.display()
            )
        });
    ProveedorFalso { hijo }
}

fn conversaciones(dir: &Path) -> Conversations {
    Conversations {
        store: Store {
            dir: dir.to_path_buf(),
            prefix: "c-".to_owned(),
        },
    }
}

fn turno(cwd: &Path, puerto: u16) -> Turno {
    Turno {
        base_url: format!("http://127.0.0.1:{puerto}/v1"),
        modelo: "fake".to_owned(),
        modo: "ask".to_owned(),
        cwd: cwd.to_string_lossy().into_owned(),
        folders: vec![cwd.to_string_lossy().into_owned()],
        cerebro: false,
        mensaje: String::new(),
        imagenes: Vec::new(),
        clave: "fake-key".to_owned(),
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
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requiere el binario fake-provider: cargo build -p fake-provider"]
async fn agent_send_uses_fake_provider_and_saves() {
    let binario = ruta_del_binario();
    assert!(
        binario.is_file(),
        "no encontré {}; compílalo con: cargo build -p fake-provider",
        binario.display()
    );

    // El diario del arnés vive en `data_dir()`: los tests no escriben en el
    // perfil real (R12).
    let datos = tempfile::tempdir().expect("la carpeta de datos");
    let datos_path = datos.path().to_str().expect("ruta utf-8").to_owned();
    // El cwd del turno: `local_read_file` solo acepta rutas dentro de él.
    let cwd = tempfile::tempdir().expect("la carpeta de trabajo");
    let leeme = cwd.path().join("leeme.txt");
    std::fs::write(&leeme, "contenido conocido\n").expect("leeme.txt");
    let guion = cwd.path().join("guion.json");
    escribir_guion(&guion, &leeme);
    let registro = cwd.path().join("registro.jsonl");

    let puerto = puerto_libre();
    let _proveedor = arrancar(&binario, &guion, puerto, &registro);
    esperar_puerto(puerto).await;

    temp_env::async_with_vars([("ORUSCRIBE_DATA_DIR", Some(datos_path.as_str()))], async {
        let approvals = Arc::new(ApprovalQueue::new());
        let store = conversaciones(cwd.path());
        let agent = Agent::new(
            store.clone(),
            Arc::clone(&approvals),
            Arc::new(HarnessRunner::new(Arc::clone(&approvals))),
        );

        let resultado = tokio::time::timeout(
            Duration::from_secs(30),
            agent.send("lee el archivo", turno(cwd.path(), puerto)),
        )
        .await;
        let resultado = resultado.expect("Agent::send no terminó en 30 s");
        assert!(
            resultado.is_ok(),
            "el turno real falló: {:?}",
            resultado.err()
        );

        let historial = agent.history();
        assert!(
            historial
                .iter()
                .any(|mensaje| mensaje.role == "user" && mensaje.content == "lee el archivo"),
            "falta el mensaje de la persona: {historial:?}"
        );
        assert!(
            historial
                .iter()
                .any(|mensaje| mensaje.role == "assistant" && mensaje.content.contains("Listo.")),
            "falta la respuesta con Listo.: {historial:?}"
        );

        let id = agent.conversation_id().expect("id de la conversación");
        assert!(!id.is_empty(), "el id de la conversación está vacío");
        let archivo = cwd.path().join(format!("c-{id}.json"));
        assert!(archivo.is_file(), "no existe {}", archivo.display());

        let guardada = store.load(&id).expect("la conversación guardada");
        assert_eq!(guardada.messages, historial);

        let (entrada, salida) = agent.tokens();
        assert!(
            entrada + salida > 0,
            "los tokens no suman: {entrada}/{salida}"
        );
        assert!(!agent.busy(), "el agente quedó ocupado");
        assert_eq!(agent.error(), "");

        let registro_texto = std::fs::read_to_string(&registro).unwrap_or_default();
        assert!(
            registro_texto.contains("local_read_file"),
            "el registro del proveedor no menciona local_read_file"
        );
    })
    .await;
}
