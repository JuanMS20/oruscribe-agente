//! Adaptador de las herramientas locales al trait `Tool` del agente. Es el
//! `localTool` de `internal/localtools/localtools.go:112-127`.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use async_trait::async_trait;
use orus_agent::{CancelToken, Risk, Tool, ToolError, ToolSpec};
use serde_json::Value;

use super::{Error, Set};

const MARGEN_PLAZO: u64 = 5;

/// Una función del `Set` con la firma que usa el adaptador.
/// `func(context.Context, json.RawMessage)`; acá el token se agrega aparte
/// porque el trait `Tool` no recibe contexto (D41).
pub(crate) type Correr = fn(&Set, Value, &CancelToken) -> Result<String, Error>;

/// Una herramienta con nombre, riesgo, descripción y schema fijos.
pub(crate) struct HerramientaLocal {
    set: Arc<Set>,
    nombre: &'static str,
    riesgo: Risk,
    descripcion: &'static str,
    schema: Value,
    correr: Correr,
}

impl HerramientaLocal {
    pub(crate) fn nueva(
        set: Arc<Set>,
        nombre: &'static str,
        riesgo: Risk,
        descripcion: &'static str,
        schema: Value,
        correr: Correr,
    ) -> Self {
        Self {
            set,
            nombre,
            riesgo,
            descripcion,
            schema,
            correr,
        }
    }
}

#[async_trait]
impl Tool for HerramientaLocal {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.nombre.to_owned(),
            description: self.descripcion.to_owned(),
            schema: self.schema.clone(),
        }
    }

    fn risk(&self) -> Option<Risk> {
        Some(self.riesgo)
    }

    fn plazo(&self, args: &Value) -> Option<Duration> {
        if self.nombre != "bash" {
            return None;
        }
        let margen = Duration::from_secs(MARGEN_PLAZO);
        match args.get("timeout").filter(|valor| !valor.is_null()) {
            None => Some(self.set.timeout + margen),
            Some(valor) => valor
                .as_u64()
                .filter(|segundos| (1..=600).contains(segundos))
                .map(|segundos| Duration::from_secs(segundos) + margen),
        }
    }

    async fn run(&self, args: Value) -> Result<String, ToolError> {
        // La herramienta corre en un hilo aparte: un árbol enorme o un
        // comando colgado no bloquean al worker. El plazo y la cancelación
        // del runner sueltan este future (D41); al soltarlo se señaliza el
        // token, la búsqueda se corta y `run_command` mata el árbol (#94).
        let cancel = CancelToken::new();
        let set = Arc::clone(&self.set);
        let correr = self.correr;
        let cancel_hilo = cancel.clone();
        let estado = Arc::new(Mutex::new(Estado {
            resultado: None,
            waker: None,
        }));
        let estado_hilo = Arc::clone(&estado);
        std::thread::Builder::new()
            .name(format!("herramienta-{}", self.nombre))
            .spawn(move || {
                let resultado = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    correr(&set, args, &cancel_hilo)
                }))
                .unwrap_or_else(|_| {
                    Err(Error::Io(std::io::Error::other(
                        "la herramienta local entró en pánico",
                    )))
                });
                let mut estado = estado_hilo
                    .lock()
                    .unwrap_or_else(|envenenado| envenenado.into_inner());
                estado.resultado = Some(resultado);
                if let Some(waker) = estado.waker.take() {
                    waker.wake();
                }
            })
            .map_err(|error| {
                ToolError::Message(format!("no se pudo arrancar la herramienta: {error}"))
            })?;
        EnHilo { estado, cancel }.await
    }
}

/// Lo que el hilo de la herramienta comparte con el future: el resultado
/// cuando llega y el waker que despierta al runtime.
struct Estado {
    resultado: Option<Result<String, Error>>,
    waker: Option<Waker>,
}

/// Espera el resultado de la herramienta sin bloquear al worker.
/// `orus-tools` no depende de tokio en código normal, así que el equivalente
/// a `spawn_blocking` se arma con la biblioteca estándar; y a diferencia de
/// `spawn_blocking`, soltar este future señaliza la cancelación: el corte
/// del runner llega de verdad al proceso.
struct EnHilo {
    estado: Arc<Mutex<Estado>>,
    cancel: CancelToken,
}

impl Future for EnHilo {
    type Output = Result<String, ToolError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut estado = self
            .estado
            .lock()
            .unwrap_or_else(|envenenado| envenenado.into_inner());
        if let Some(resultado) = estado.resultado.take() {
            return Poll::Ready(resultado.map_err(|error| ToolError::Message(error.to_string())));
        }
        estado.waker = Some(cx.waker().clone());
        Poll::Pending
    }
}

impl Drop for EnHilo {
    fn drop(&mut self) {
        // El runner cortó por plazo o cancelación (o la herramienta ya
        // terminó y el token no hace nada): avisar es idempotente.
        self.cancel.cancelar();
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::path::Path;
    use std::time::Duration;

    use serde_json::json;

    use super::super::{Config, Set};
    use super::*;

    fn conjunto(raiz: &Path, timeout: Duration) -> Arc<Set> {
        Arc::new(
            Set::new(Config {
                folders: vec![raiz.to_string_lossy().into_owned()],
                programs: vec![
                    std::env::current_exe()
                        .expect("exe de pruebas")
                        .to_string_lossy()
                        .into_owned(),
                ],
                timeout,
                ..Config::default()
            })
            .expect("Set con una carpeta temporal"),
        )
    }

    /// Issue #94: el runner corta por `select!`/plazo soltando el future de
    /// `run`. Una herramienta local colgada tiene que morir con ese corte:
    /// antes el primer poll corría la herramienta entera sobre el worker y
    /// ni el plazo ni la cancelación llegaban al proceso.
    #[tokio::test]
    async fn el_corte_del_runner_mata_la_herramienta_colgada() {
        let raiz = tempfile::tempdir().expect("raiz");
        let marca = raiz.path().join("marca");
        let set = conjunto(raiz.path(), Duration::from_secs(10));
        let herramienta = set
            .tools()
            .into_iter()
            .find(|herramienta| herramienta.spec().name == "local_run_command")
            .expect("local_run_command registrada");
        let exe = std::env::current_exe().expect("exe de pruebas");
        let mut futuro = herramienta.run(json!({
            "program": exe.to_string_lossy(),
            "args": [
                "--exact",
                super::super::comando::NOMBRE_AYUDANTE,
                "--nocapture",
                "--",
                "mode=marker",
                format!("marker={}", marca.to_string_lossy()),
            ],
            "cwd": raiz.path().to_string_lossy(),
        }));
        // El plazo del runner vence mientras la herramienta sigue colgada:
        // el primer poll no puede bloquear al worker (el modo marcador
        // escribe a los 3 s; el plazo interno es de 10 s).
        let cortado = tokio::time::timeout(Duration::from_millis(500), &mut futuro).await;
        assert!(
            cortado.is_err(),
            "el primer poll bloqueó al worker con la herramienta colgada: {cortado:?}"
        );
        // El runner suelta el future: la cancelación tiene que llegar al
        // proceso y matarlo antes de que escriba la marca.
        drop(futuro);
        tokio::time::sleep(Duration::from_secs(4)).await;
        assert!(
            !marca.exists(),
            "la herramienta colgada sobrevivió al corte del runner"
        );
    }

    #[test]
    fn bash_declara_el_plazo_que_pide_el_modelo() {
        let raiz = tempfile::tempdir().expect("raiz");
        let set = conjunto(raiz.path(), Duration::from_secs(60));
        let bash = set
            .tools_pi()
            .into_iter()
            .find(|herramienta| herramienta.spec().name == "bash")
            .expect("bash registrada");
        assert_eq!(
            bash.plazo(&json!({"command": "x", "timeout": 120})),
            Some(Duration::from_secs(125))
        );
        assert_eq!(bash.plazo(&json!({"timeout": 601})), None);
        assert_eq!(
            bash.plazo(&json!({})),
            Some(Duration::from_secs(65)),
            "sin timeout el runner espera el plazo interno más el margen para que el modelo reciba la salida parcial"
        );
    }

    /// El camino feliz sigue respondiendo igual con la herramienta fuera del
    /// worker.
    #[tokio::test]
    async fn una_herramienta_normal_sigue_respondiendo() {
        let raiz = tempfile::tempdir().expect("raiz");
        std::fs::write(raiz.path().join("nota.txt"), "hola").expect("escribir");
        let set = conjunto(raiz.path(), Duration::from_secs(10));
        let herramienta = set
            .tools()
            .into_iter()
            .find(|herramienta| herramienta.spec().name == "local_read_file")
            .expect("local_read_file registrada");
        let salida = herramienta
            .run(json!({"path": raiz.path().join("nota.txt").to_string_lossy()}))
            .await
            .expect("leer");
        assert_eq!(salida, "hola");
    }
}
