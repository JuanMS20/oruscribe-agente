//! #117: servidor MCP falso con dos organizaciones. Cambiar a B abre un
//! cliente nuevo con `x-org-slug` de B y no deja rastro de A en la sesión.
//! Nunca se llama a `switch_org`: eso cambiaría todas las conexiones de la
//! cuenta, incluidos los asistentes de la persona.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use orus_app::{
    App, Backend, ClientProvider, Connector, Conversations, Refresh, SessionError, Status,
    WatchSpawner,
};
use orus_auth::{Error as AuthError, Store, Token};
use orus_config::Settings;
use orus_org::{
    Alert, JsonObject, Me, MessagesPage, Org, OtherOrg, Room, Task, ToolSpec,
    doorbell::Config as DoorbellConfig,
};
use serde_json::json;

#[derive(Clone, Default)]
struct StoreMemoria(Arc<Mutex<Option<Token>>>);

impl Store for StoreMemoria {
    fn save(&self, token: Token) -> Result<(), AuthError> {
        *self.0.lock().expect("token de prueba") = Some(token);
        Ok(())
    }

    fn load(&self) -> Result<Token, AuthError> {
        self.0
            .lock()
            .expect("token de prueba")
            .clone()
            .ok_or(AuthError::NoSession)
    }

    fn clear(&self) -> Result<(), AuthError> {
        *self.0.lock().expect("token de prueba") = None;
        Ok(())
    }
}

/// Un almacén fijo para llegar a `connect` sin pasar por `import_token`.
struct StoreFijo(Token);

impl Store for StoreFijo {
    fn save(&self, _: Token) -> Result<(), AuthError> {
        Ok(())
    }

    fn load(&self) -> Result<Token, AuthError> {
        Ok(self.0.clone())
    }

    fn clear(&self) -> Result<(), AuthError> {
        Ok(())
    }
}

fn token_sin_vencer(access: &str) -> Token {
    Token {
        access_token: access.to_owned(),
        refresh_token: String::new(),
        token_type: "Bearer".to_owned(),
        scope: String::new(),
        expiry: serde_json::from_value(json!("0001-01-01T00:00:00Z")).expect("cero de Go"),
    }
}

fn token_vencido(access: &str) -> Token {
    Token {
        refresh_token: "r1".to_owned(),
        expiry: serde_json::from_value(json!("2020-01-01T00:00:00Z")).expect("vencimiento"),
        ..token_sin_vencer(access)
    }
}

struct SinRefresco;

#[async_trait]
impl Refresh for SinRefresco {
    async fn refresh(&self, _: &Token) -> Result<Token, SessionError> {
        Err(SessionError::RefreshRejected)
    }
}

/// El backend responde según el slug con el que se abrió: A y B tienen salas,
/// tareas y avisos distintos. `get_me` ofrece la otra organización.
struct BackendOrg {
    slug: String,
    /// El servidor puede ignorar `x-org-slug` y responder otra org (#125).
    me_slug: Option<String>,
}

impl BackendOrg {
    fn otra(&self) -> OtherOrg {
        if self.slug == "org-b" {
            OtherOrg {
                slug: "org-a".to_owned(),
                name: "TalenAI".to_owned(),
                role: "worker".to_owned(),
            }
        } else {
            OtherOrg {
                slug: "org-b".to_owned(),
                name: "Oruscribe B".to_owned(),
                role: "worker".to_owned(),
            }
        }
    }

    fn nombre(&self) -> &str {
        if self.slug == "org-b" {
            "Oruscribe B"
        } else {
            "TalenAI"
        }
    }
}

#[async_trait]
impl Backend for BackendOrg {
    async fn me(&self) -> Result<Me, SessionError> {
        let slug = self.me_slug.as_deref().unwrap_or(&self.slug);
        Ok(Me {
            user_id: "u1".to_owned(),
            name: "Ana Núñez".to_owned(),
            organization: Org {
                id: slug.to_owned(),
                slug: slug.to_owned(),
                name: self.nombre().to_owned(),
            },
            other_organizations: Some(vec![self.otra()]),
            ..Me::default()
        })
    }

    async fn tasks(&self) -> Result<Vec<Task>, SessionError> {
        Ok(vec![Task {
            id: "t1".to_owned(),
            title: format!("Tarea de {}", self.nombre()),
            ..Task::default()
        }])
    }

    async fn rooms(&self) -> Result<Vec<Room>, SessionError> {
        Ok(vec![Room {
            room_id: "r1".to_owned(),
            name: format!("Sala de {} ({})", self.nombre(), self.slug),
            kind: "team".to_owned(),
            ..Room::default()
        }])
    }

    async fn alerts(&self) -> Result<Vec<Alert>, SessionError> {
        Ok(vec![Alert {
            id: "a1".to_owned(),
            message: format!("Aviso de {}", self.nombre()),
            kind: "info".to_owned(),
            ..Alert::default()
        }])
    }

    async fn messages(&self, _: &str, _: i64, _: i64) -> Result<MessagesPage, SessionError> {
        Ok(MessagesPage {
            messages: Vec::new(),
            has_more: false,
            next_offset: 0,
        })
    }

    async fn send(&self, _: &str, _: &str, _: &str) -> Result<(), SessionError> {
        Ok(())
    }

    async fn tool_specs(&self) -> Result<Vec<ToolSpec>, SessionError> {
        Ok(Vec::new())
    }

    async fn call_tool(&self, _: &str, _: JsonObject) -> Result<String, SessionError> {
        Ok(String::new())
    }

    async fn doorbell(&self) -> Result<DoorbellConfig, SessionError> {
        Err(SessionError::NoDoorbell)
    }
}

/// Conector que recuerda los slug pedidos y entrega un backend distinto por
/// conexión, como haría `x-org-slug` en el cliente MCP real.
#[derive(Default)]
struct ConectorOrg {
    slugs: Mutex<Vec<String>>,
    me_fijo: Option<String>,
}

impl ConectorOrg {
    fn con_me_fijo(slug: &str) -> Self {
        Self {
            me_fijo: Some(slug.to_owned()),
            ..Self::default()
        }
    }

    fn slugs(&self) -> Vec<String> {
        self.slugs.lock().expect("slugs").clone()
    }
}

#[async_trait]
impl Connector for ConectorOrg {
    async fn open(
        &self,
        _: &str,
        _: &str,
        org_slug: &str,
    ) -> Result<Arc<dyn Backend>, SessionError> {
        self.slugs.lock().expect("slugs").push(org_slug.to_owned());
        Ok(Arc::new(BackendOrg {
            slug: org_slug.to_owned(),
            me_slug: self.me_fijo.clone(),
        }))
    }
}

#[derive(Default)]
struct TimbreFalso {
    starts: AtomicUsize,
    stops: AtomicUsize,
}

impl WatchSpawner for TimbreFalso {
    fn start(&self, _: u64, _: Arc<dyn ClientProvider>) {
        self.starts.fetch_add(1, Ordering::SeqCst);
    }

    fn stop(&self) {
        self.stops.fetch_add(1, Ordering::SeqCst);
    }
}

#[tokio::test]
async fn cambiar_a_la_org_b_reconecta_y_no_deja_rastro_de_la_a() {
    let dir = tempfile::tempdir().expect("carpeta temporal");
    let datos = dir.path().join("datos");
    std::fs::create_dir_all(&datos).expect("carpeta de datos");
    temp_env::async_with_vars([("ORUSCRIBE_DATA_DIR", Some(datos.as_os_str()))], async {
        let carpeta_convs = dir.path().join("conversaciones");
        std::fs::create_dir_all(&carpeta_convs).expect("carpeta de conversaciones");
        let vieja = carpeta_convs.join("c-111.json");
        std::fs::write(&vieja, b"{}").expect("conversación de A");
        let mut conversaciones = Conversations::new();
        conversaciones.store.dir = carpeta_convs;

        let watch = Arc::new(TimbreFalso::default());
        let conector = Arc::new(ConectorOrg::default());
        let app = App::new(
            Box::new(StoreMemoria::default()),
            Settings {
                org_slug: "org-a".to_owned(),
                ..Settings::default()
            },
            Arc::clone(&conector) as Arc<dyn Connector>,
            Arc::new(SinRefresco),
        )
        .with_watch_spawner(Arc::clone(&watch) as Arc<dyn WatchSpawner>)
        .with_conversations(Arc::new(conversaciones));

        let importado = Token {
            access_token: "token-de-prueba".to_owned(),
            refresh_token: String::new(),
            token_type: "Bearer".to_owned(),
            scope: String::new(),
            expiry: serde_json::from_value(json!("0001-01-01T00:00:00Z"))
                .expect("vencimiento del cero de Go"),
        };
        app.import_token(&importado.access_token)
            .await
            .expect("importar token");
        assert_eq!(app.status(), Status::Ready);
        assert_eq!(app.settings_snapshot().org_slug, "org-a");
        assert!(!app.settings_snapshot().org_fijada);
        let starts_antes = watch.starts.load(Ordering::SeqCst);

        let me = app.seleccionar_org("org-b").await.expect("cambiar a B");
        assert_eq!(me.organization.slug, "org-b");
        assert_eq!(
            conector.slugs(),
            vec!["org-a".to_owned(), "org-b".to_owned()],
            "la nueva conexión debe abrirse con el slug de B"
        );
        assert_eq!(app.settings_snapshot().org_slug, "org-b");
        assert!(app.settings_snapshot().org_fijada);

        // Sin rastro de A como organización activa ni en los datos que sirve
        // el cliente B. `other_organizations` sí lista a A como opción: eso es
        // lo que el selector debe ofrecer, no un dato de la sesión activa.
        let estado = serde_json::to_value(app.state()).expect("snapshot");
        assert_eq!(
            estado["me"]["organization"]["slug"], "org-b",
            "la sesión sigue mostrando la organización A: {estado}"
        );
        assert_eq!(estado["me"]["organization"]["name"], "Oruscribe B");
        let cliente = app.client().await.expect("cliente de B");
        let salas = cliente.rooms().await.expect("salas");
        assert_eq!(salas.len(), 1);
        assert!(!salas[0].name.contains("TalenAI"));
        let tareas = cliente.tasks().await.expect("tareas");
        assert!(!tareas[0].title.contains("TalenAI"));
        let avisos = cliente.alerts().await.expect("avisos");
        assert!(!avisos[0].message.contains("TalenAI"));

        // Timbre reiniciado y conversación nueva sin borrar la anterior.
        assert_eq!(
            watch.starts.load(Ordering::SeqCst),
            starts_antes + 1,
            "el timbre tiene que arrancar de nuevo para B"
        );
        assert!(
            watch.stops.load(Ordering::SeqCst) >= 1,
            "el timbre de A tiene que detenerse"
        );
        assert!(
            vieja.exists(),
            "las conversaciones de A se conservan; el agente abre una nueva"
        );

        // No hay entrada manual: un slug que no vino en get_me se rechaza y
        // no llega a abrir otra conexión.
        assert!(matches!(
            app.seleccionar_org("org-inventada").await,
            Err(SessionError::Invalido(_))
        ));
        assert_eq!(conector.slugs().len(), 2);
    })
    .await;
}

#[tokio::test]
async fn refresh_rechazado_al_cambiar_de_org_saca_la_sesion_de_ready() {
    let guardado = StoreMemoria::default();
    let app = App::new(
        Box::new(guardado.clone()),
        Settings {
            org_slug: "org-a".to_owned(),
            ..Settings::default()
        },
        Arc::new(ConectorOrg::default()) as Arc<dyn Connector>,
        Arc::new(SinRefresco),
    );
    app.import_token("inicial").await.expect("importar token");
    assert_eq!(app.status(), Status::Ready);
    guardado
        .save(token_vencido("inicial"))
        .expect("token vencido");

    assert!(matches!(
        app.seleccionar_org("org-b").await,
        Err(SessionError::RefreshRejected)
    ));
    assert_eq!(
        app.status(),
        Status::LoggedOut,
        "un refresco rechazado no puede dejar la ventana diciendo «conectada»"
    );
    assert!(
        guardado.load().is_err(),
        "el token rechazado debe salir del disco"
    );
}

#[tokio::test]
async fn connect_reabre_el_cliente_con_el_slug_que_adopto_get_me() {
    let dir = tempfile::tempdir().expect("carpeta temporal");
    let datos = dir.path().join("datos");
    std::fs::create_dir_all(&datos).expect("carpeta de datos");
    temp_env::async_with_vars([("ORUSCRIBE_DATA_DIR", Some(datos.as_os_str()))], async {
        let conector = Arc::new(ConectorOrg::con_me_fijo("org-b"));
        let app = App::new(
            Box::new(StoreFijo(token_sin_vencer("token-de-prueba"))),
            Settings {
                org_slug: "org-viejo".to_owned(),
                ..Settings::default()
            },
            Arc::clone(&conector) as Arc<dyn Connector>,
            Arc::new(SinRefresco),
        );

        let cliente = app.client().await.expect("cliente de la org adoptada");
        assert_eq!(app.settings_snapshot().org_slug, "org-b");
        let salas = cliente.rooms().await.expect("salas");
        assert_eq!(salas.len(), 1);
        assert!(
            salas[0].name.contains("org-b"),
            "el cliente vivo sigue mandando el slug viejo: {}",
            salas[0].name
        );
    })
    .await;
}

#[tokio::test]
async fn tabla_cambio_de_org_rechaza_un_get_me_ajeno() {
    let raiz = tempfile::tempdir().expect("carpeta temporal");
    let datos = raiz.path().join("datos");
    std::fs::create_dir_all(&datos).expect("carpeta de datos");
    temp_env::async_with_vars([("ORUSCRIBE_DATA_DIR", Some(datos.as_os_str()))], async {
        let casos = [
            ("otra organización", "org-a", "org-a"),
            ("sin organización", "", ""),
            ("coincide", "org-b", "org-b"),
        ];
        for (nombre, respondido, me_esperado) in casos {
            let conector = Arc::new(ConectorOrg::con_me_fijo(respondido));
            let app = App::new(
                Box::new(StoreFijo(token_sin_vencer("token-inicial"))),
                Settings {
                    org_slug: "org-a".to_owned(),
                    ..Settings::default()
                },
                Arc::clone(&conector) as Arc<dyn Connector>,
                Arc::new(SinRefresco),
            );
            app.import_token("token-inicial")
                .await
                .expect("importar token");
            let slugs_antes = conector.slugs().len();
            let resultado = app.seleccionar_org("org-b").await;
            let acepta = respondido == "org-b";
            assert_eq!(resultado.is_ok(), acepta, "{nombre}");
            if !acepta {
                assert!(
                    matches!(resultado, Err(SessionError::Invalido(_))),
                    "{nombre}: {resultado:?}"
                );
                assert_eq!(app.settings_snapshot().org_slug, "org-a", "{nombre}");
                assert!(!app.settings_snapshot().org_fijada, "{nombre}");
                assert_eq!(
                    app.state()
                        .me
                        .as_ref()
                        .map(|me| me.organization.slug.as_str()),
                    Some(me_esperado),
                    "{nombre}"
                );
                assert_eq!(conector.slugs().len(), slugs_antes + 1, "{nombre}");
                assert_eq!(app.status(), Status::Ready, "{nombre}");
            }
        }
    })
    .await;
}

#[tokio::test]
async fn tabla_connect_con_org_fijada_rechaza_get_me_ajeno() {
    let casos = [("reinicio", true), ("reintento", false)];
    for (nombre, por_boot) in casos {
        let conector = Arc::new(ConectorOrg::con_me_fijo("org-a"));
        let app = App::new(
            Box::new(StoreFijo(token_sin_vencer("token-inicial"))),
            Settings {
                org_slug: "org-b".to_owned(),
                org_fijada: true,
                ..Settings::default()
            },
            Arc::clone(&conector) as Arc<dyn Connector>,
            Arc::new(SinRefresco),
        );
        if por_boot {
            app.boot();
            let inicio = Instant::now();
            while app.status() != Status::Failed && inicio.elapsed() < Duration::from_secs(2) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        } else {
            for intento in 0..2 {
                assert!(
                    matches!(app.client().await, Err(SessionError::Invalido(_))),
                    "{nombre}: el reintento {intento} aceptó otra organización"
                );
            }
        }
        assert_eq!(app.status(), Status::Failed, "{nombre}");
        assert_eq!(app.settings_snapshot().org_slug, "org-b", "{nombre}");
        assert!(app.settings_snapshot().org_fijada, "{nombre}");
        assert!(app.state().me.is_none(), "{nombre}");
        assert!(app.last_error_sesion().contains("org-a"), "{nombre}");
        assert_eq!(
            conector.slugs(),
            vec!["org-b".to_owned(); if por_boot { 1 } else { 2 }],
            "{nombre}: se abrió un cliente con el slug ajeno"
        );
    }
}

#[tokio::test]
async fn tabla_import_token_con_org_fijada_rechaza_get_me_ajeno() {
    let casos = [("ajena", "org-a", false), ("propia", "org-b", true)];
    for (nombre, respondido, acepta) in casos {
        let guardado = StoreMemoria::default();
        let conector = Arc::new(ConectorOrg::con_me_fijo(respondido));
        let app = App::new(
            Box::new(guardado.clone()),
            Settings {
                org_slug: "org-b".to_owned(),
                org_fijada: true,
                ..Settings::default()
            },
            Arc::clone(&conector) as Arc<dyn Connector>,
            Arc::new(SinRefresco),
        );
        let resultado = app.import_token("token-inicial").await;
        assert_eq!(resultado.is_ok(), acepta, "{nombre}");
        assert_eq!(app.settings_snapshot().org_slug, "org-b", "{nombre}");
        assert!(app.settings_snapshot().org_fijada, "{nombre}");
        assert_eq!(conector.slugs(), vec!["org-b".to_owned()], "{nombre}");
        if acepta {
            assert_eq!(app.status(), Status::Ready, "{nombre}");
            assert_eq!(
                app.state()
                    .me
                    .as_ref()
                    .map(|me| me.organization.slug.as_str()),
                Some("org-b"),
                "{nombre}"
            );
            assert!(guardado.load().is_ok(), "{nombre}");
        } else {
            assert!(
                matches!(resultado, Err(SessionError::Invalido(_))),
                "{nombre}: {resultado:?}"
            );
            assert_eq!(app.status(), Status::Failed, "{nombre}");
            assert!(app.state().me.is_none(), "{nombre}");
            assert!(
                guardado.load().is_err(),
                "{nombre}: guardó un token rechazado"
            );
            assert!(app.last_error_sesion().contains("org-a"), "{nombre}");
        }
    }
}
