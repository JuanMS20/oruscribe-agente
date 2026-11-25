use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use chrono::{DateTime, FixedOffset, Utc};
use orus_auth::Token;
use orus_org::Me;
use sha2::{Digest as ShaDigest, Sha256};

use super::backend::{Backend, SessionError};
use super::{App, Status, endpoint, sin_vencimiento};

/// Los tres intentos de una conexión nueva (`app.go:779`).
const CONNECT_ATTEMPTS: u32 = 3;
/// Una restauración no espera más que esto (`app.go:253`).
pub(super) const RESTORE_TIMEOUT: Duration = Duration::from_secs(45);
/// El backoff duplica mientras sea menor a un minuto (`app.go:697-702`).
const RESTORE_BACKOFF_CAP: Duration = Duration::from_secs(60);

impl App {
    /// `fijada` distingue la elección de la persona de la adopción automática
    /// de `get_me`: solo la primera gana sobre el slug que traiga el servidor
    /// (#117).
    fn guardar_org_slug(&self, slug: &str, fijada: bool) {
        {
            let mut settings = crate::sync::lock(&self.settings);
            settings.org_slug = slug.to_owned();
            settings.org_fijada = fijada;
        }
        if let Err(error) = orus_config::editar(|settings| {
            settings.org_slug = slug.to_owned();
            settings.org_fijada = fijada;
        }) {
            // La organización sigue en memoria para esta sesión, pero sin el
            // archivo el próximo arranque vuelve a la organización por defecto.
            self.estado().last_error = format!("no se pudo guardar la organización: {error}");
        }
    }

    /// Cambia la organización de ESTA conexión. Abre un cliente nuevo con la
    /// cabecera `x-org-slug` del slug elegido: nunca usa la herramienta global
    /// de cambio de organización, que afectaría a todas las conexiones de la
    /// cuenta (`docs/prompt-paridad-android.md:127`).
    pub async fn seleccionar_org(&self, slug: &str) -> Result<Me, SessionError> {
        let slug = slug.trim();
        if slug.is_empty() {
            return Err(SessionError::Invalido(
                "falta el slug de la organización".to_owned(),
            ));
        }
        // Sin entrada manual: solo se puede elegir lo que `get_me` ofreció.
        let actual = self.me().ok_or(SessionError::LoggedOut)?;
        let ofrecida = actual.organization.slug == slug
            || actual
                .other_organizations
                .as_ref()
                .is_some_and(|orgs| orgs.iter().any(|org| org.slug == slug));
        if !ofrecida {
            return Err(SessionError::Invalido(
                "esa organización no viene en get_me".to_owned(),
            ));
        }

        let _guard = self.refresh_mu.lock().await;
        let token = self.fresh_token().await.inspect_err(|error| {
            // #77: el mismo trato que `client`: un rechazo al renovar no
            // puede dejar la ventana diciendo «conectada» cuando el cambio
            // de organización no llegó a hacerse.
            self.restore_failed(error);
        })?;
        let settings = self.settings_snapshot();
        let endpoint = endpoint(&settings);
        let client = self
            .connector
            .open(&endpoint, &token.access_token, slug)
            .await?;
        let me = match client.me().await {
            Ok(me) => me,
            Err(error) => {
                drop(client);
                return Err(error);
            }
        };
        if me.organization.slug != slug {
            drop(client);
            return Err(SessionError::Invalido(format!(
                "el servidor respondió la organización {} en lugar de {}",
                me.organization.slug, slug
            )));
        }
        if self.status() == Status::LoggedOut || !self.sesion_sigue(&token) {
            drop(client);
            return Err(SessionError::LoggedOut);
        }

        // El slug elegido gana sobre el que traiga `get_me` en la reconexión.
        self.estado().last_error.clear();
        self.guardar_org_slug(slug, true);
        // El timbre de A no puede seguir alimentando la sesión de B; la clave
        // es el token, que no cambia al cambiar de organización, así que
        // `detener_timbre` la suelta antes de `kick_watch`.
        self.detener_timbre();
        // Cortar el turno y abrir conversación nueva, sin borrar las anteriores.
        self.nueva_conversacion_org();
        let viejo = {
            let mut estado = self.estado();
            let viejo = estado.client.replace(client);
            estado.me = Some(me.clone());
            estado.status = Status::Ready;
            viejo
        };
        drop(viejo);
        self.kick_watch(&token.access_token);
        Ok(me)
    }

    /// Devuelve un cliente vivo, reconectando si hace falta (`app.go:487-535`).
    ///
    /// Nada de red se queda con `refresh_mu` más de `restore_timeout`: el
    /// candado se suelta al salir, también cuando el plazo vence (#150).
    pub async fn client(&self) -> Result<Arc<dyn Backend>, SessionError> {
        if self.status() == Status::LoggedOut {
            return Err(SessionError::LoggedOut);
        }
        let token = self.load_token()?;
        if let Some(client) = self.cached(&token) {
            return Ok(client);
        }
        let _guard = self.refresh_mu.lock().await;
        match tokio::time::timeout(self.restore_timeout, self.conectar_bajo_candado()).await {
            Ok(resultado) => resultado,
            // Sin plazo, un `open` que no responde retenía el candado y la
            // ventana entera esperaba detrás: el vencimiento es terminal.
            Err(_) => Err(self.vencida("la conexión")),
        }
    }

    /// El tramo de `client` que sí toca la red: renovar y conectar. Corre bajo
    /// `refresh_mu` y siempre dentro de un plazo.
    async fn conectar_bajo_candado(&self) -> Result<Arc<dyn Backend>, SessionError> {
        // Quien esperó aquí puede encontrar el trabajo ya hecho.
        let token = self.load_token()?;
        if let Some(client) = self.cached(&token) {
            return Ok(client);
        }
        let token = self.fresh_token().await.inspect_err(|error| {
            // Un fallo de renovación no puede dejar la ventana diciendo
            // «conectada» (#77): mismo trato que el camino de boot.
            self.restore_failed(error);
        })?;
        self.connect(&token).await?;
        self.estado().client.clone().ok_or(SessionError::LoggedOut)
    }

    /// Restaura la sesión guardada sin bloquear la ventana (`app.go:242-259`).
    pub fn boot(&self) {
        if self.store.load().is_err() {
            return;
        }
        self.estado().status = Status::Connecting;
        let app = self.clone();
        tokio::spawn(async move {
            if let Err(error) = app.restore_with_timeout().await
                && app.restore_failed(&error)
            {
                app.retry_with_stored_session();
            }
        });
    }

    /// Importa el token que el panel ya entrega, sin pasar por OAuth
    /// (`app.go:901-972`).
    pub async fn import_token(&self, token: &str) -> Result<(), SessionError> {
        let token = token.trim();
        if token.is_empty() {
            return Err(SessionError::Connect("el token está vacío".to_owned()));
        }
        // La misma serialización que `client` y `restore_session`: una
        // restauración en vuelo no debe reponer el cliente que acabamos de
        // importar.
        let guard = self.refresh_mu.lock().await;
        let generacion = self.sesion_gen.load(Ordering::SeqCst);
        let settings = self.settings_snapshot();
        let endpoint = endpoint(&settings);
        let guardado = settings.org_slug;
        let fijada = settings.org_fijada;
        let mut client = self.connector.open(&endpoint, token, &guardado).await?;
        let me = match client.me().await {
            Ok(me) => me,
            Err(error) => {
                drop(client);
                return Err(error);
            }
        };
        if fijada && !me.organization.slug.is_empty() && me.organization.slug != guardado {
            drop(client);
            return Err(self.org_rechazada(&me.organization.slug, &guardado));
        }
        // El token puede ser de otra organización. El cliente manda el slug en
        // cada petición: si había uno guardado y no coincide, se vuelve a abrir
        // con el que respondió el servidor (`app.go:920-930`).
        if !fijada
            && !guardado.is_empty()
            && !me.organization.slug.is_empty()
            && me.organization.slug != guardado
        {
            drop(client);
            client = self
                .connector
                .open(&endpoint, token, &me.organization.slug)
                .await?;
        }
        let nuevo_slug = if !fijada && !me.organization.slug.is_empty() {
            me.organization.slug.clone()
        } else {
            guardado.clone()
        };
        if !self.sesion_actual(generacion) {
            drop(client);
            return Err(SessionError::LoggedOut);
        }
        let importado = Token {
            access_token: token.to_owned(),
            refresh_token: String::new(),
            token_type: "Bearer".to_owned(),
            scope: String::new(),
            expiry: exp_del_jwt(token).unwrap_or_else(sin_vencimiento),
        };
        if importado.expired(ahora()) {
            drop(client);
            return Err(SessionError::Auth);
        }
        if let Err(error) = self.store.save(importado.clone()) {
            drop(client);
            return Err(SessionError::GuardarToken(error.to_string()));
        }
        if !self.sesion_actual(generacion) {
            self.deshacer_token(&importado);
            drop(client);
            return Err(SessionError::LoggedOut);
        }
        self.limpiar_token_efimero();
        let cambio_slug = nuevo_slug != guardado;
        let viejo = {
            let mut estado = self.estado();
            if !self.sesion_actual(generacion) {
                return Err(SessionError::LoggedOut);
            }
            let viejo = estado.client.replace(client);
            estado.me = Some(me);
            estado.status = Status::Ready;
            estado.last_error.clear();
            viejo
        };
        self.reemplazar_flujo();
        drop(viejo);
        // El fallo de guardado no tumba la sesión: la memoria ya quedó lista,
        // como el `logf` de Go (`app.go:963-969`).
        if cambio_slug {
            self.limpiar_cambio_org();
            self.guardar_org_slug(&nuevo_slug, false);
        }
        // `kick_watch` llama a la costura, que puede volver a `client`; el
        // candado ya no debe estar tomado.
        drop(guard);
        self.kick_watch(token);
        Ok(())
    }

    /// Cierra la sesión y borra el token (`app.go:977-1009`).
    pub fn logout(&self) -> Result<(), SessionError> {
        self.detener_timbre();
        if let Some(agent) = &self.agent {
            agent.clear();
        }
        self.logout_en_memoria();
        self.login.clear_flow();
        // El token se borra antes que las conversaciones: si Windows bloquea un
        // archivo de conversación, la persona ya está desconectada.
        let token_borrado = self.store.clear();
        let conversaciones_borradas = self
            .conversations
            .as_ref()
            .map(|conversations| conversations.clear());
        if let Err(error) = token_borrado {
            return Err(SessionError::Cerrar(error.to_string()));
        }
        if let Some(Err(error)) = conversaciones_borradas {
            return Err(SessionError::Conversaciones(error.to_string()));
        }
        Ok(())
    }

    fn detener_timbre(&self) {
        {
            let _serial = crate::sync::lock(&self.watch_mu);
            if let Some(spawner) = &self.watch_spawner {
                spawner.stop();
            }
            crate::sync::lock(&self.watch_key).clear();
        }
        self.clear_bell_config();
    }

    fn cached(&self, token: &Token) -> Option<Arc<dyn Backend>> {
        let estado = self.estado();
        let ready = estado.status == Status::Ready && !token.expired(ahora());
        ready.then(|| estado.client.clone()).flatten()
    }

    pub(crate) fn load_token(&self) -> Result<Token, SessionError> {
        if let Some(token) = self.estado().token_efimero.clone() {
            return Ok(token);
        }
        match self.store.load() {
            Ok(token) => Ok(token),
            Err(orus_auth::Error::NoSession) => Err(SessionError::LoggedOut),
            Err(error) => Err(SessionError::StoreLeer(error.to_string())),
        }
    }

    pub(crate) fn usar_token_efimero(&self, token: Token) {
        self.estado().token_efimero = Some(token);
    }

    pub(crate) fn limpiar_token_efimero(&self) {
        self.estado().token_efimero = None;
    }

    pub(crate) fn limpiar_token_efimero_si_coincide(&self, token: &Token) {
        let mut estado = self.estado();
        if estado
            .token_efimero
            .as_ref()
            .is_some_and(|actual| actual.access_token == token.access_token)
        {
            estado.token_efimero = None;
        }
    }

    /// `logout` es síncrono y no puede esperar `refresh_mu`: para no reponer un
    /// cliente revocado, `connect` comprueba que la sesión siga siendo la suya.
    fn sesion_sigue(&self, token: &Token) -> bool {
        match self.load_token() {
            Ok(actual) => actual.access_token == token.access_token,
            Err(SessionError::LoggedOut) => false,
            Err(_) => true,
        }
    }

    fn sesion_actual(&self, generacion: u64) -> bool {
        self.sesion_gen.load(Ordering::SeqCst) == generacion
    }

    fn deshacer_token(&self, token: &Token) {
        if self
            .store
            .load()
            .is_ok_and(|actual| actual.access_token == token.access_token)
        {
            let _ = self.store.clear();
        }
        self.limpiar_token_efimero_si_coincide(token);
    }

    async fn fresh_token(&self) -> Result<Token, SessionError> {
        let generacion = self.sesion_gen.load(Ordering::SeqCst);
        let token = self.load_token()?;
        if !token.expired(ahora()) || token.refresh_token.is_empty() {
            return Ok(token);
        }
        let refreshed = self.refresher.refresh(&token).await?;
        if !self.sesion_sigue(&token) || !self.sesion_actual(generacion) {
            return Err(SessionError::LoggedOut);
        }
        if let Err(error) = self.store.save(refreshed.clone()) {
            self.usar_token_efimero(refreshed.clone());
            self.aviso_persistencia(format!(
                "la sesión sigue activa, pero no se pudo guardar la renovación: {error}"
            ));
        } else {
            self.limpiar_token_efimero();
        }
        if !self.sesion_actual(generacion) {
            self.deshacer_token(&refreshed);
            return Err(SessionError::LoggedOut);
        }
        Ok(refreshed)
    }

    /// Renueva el token si venció y reconecta, sin dos restauraciones a la vez
    /// (`app.go:719-727`).
    async fn restore_session(&self) -> Result<(), SessionError> {
        let _guard = self.refresh_mu.lock().await;
        let token = self.fresh_token().await?;
        self.connect(&token).await
    }

    /// `restoreSession` con el tope de `restore_timeout` (`app.go:253`).
    async fn restore_with_timeout(&self) -> Result<(), SessionError> {
        match tokio::time::timeout(self.restore_timeout, self.restore_session()).await {
            Ok(result) => result,
            Err(_) => Err(SessionError::Connect(format!(
                "la restauración tardó más de {}",
                plazo_legible(self.restore_timeout)
            ))),
        }
    }

    /// El error de un plazo vencido, ya anotado en el estado: esperar de más
    /// no puede dejar la ventana en «conectando».
    fn vencida(&self, que: &str) -> SessionError {
        let error = SessionError::Connect(format!(
            "{que} tardó más de {}",
            plazo_legible(self.restore_timeout)
        ));
        self.set_failed(error.to_string());
        error
    }

    /// Reintenta mientras el fallo sea de red; un flag evita dos bucles
    /// (`app.go:687-714`).
    fn retry_with_stored_session(&self) {
        {
            let mut estado = self.estado();
            if estado.retrying {
                return;
            }
            estado.retrying = true;
        }
        let app = self.clone();
        tokio::spawn(async move {
            let mut backoff = app.restore_backoff;
            loop {
                tokio::time::sleep(backoff).await;
                if backoff < RESTORE_BACKOFF_CAP {
                    backoff = backoff.saturating_mul(2);
                }
                let done = match app.restore_with_timeout().await {
                    Ok(()) => true,
                    Err(error) => !app.restore_failed(&error),
                };
                if done {
                    break;
                }
            }
            app.estado().retrying = false;
        });
    }

    /// Anota el fallo y dice si conviene reintentar (`app.go:751-767`).
    fn restore_failed(&self, error: &SessionError) -> bool {
        match error {
            SessionError::LoggedOut => false,
            SessionError::RefreshRejected => {
                // El servidor ya no acepta esta sesión: el token sale del disco
                // para que el arranque siguiente no lo restaure.
                let _ = self.store.clear();
                self.detener_timbre();
                if let Some(agent) = &self.agent {
                    agent.clear();
                }
                self.logout_en_memoria();
                false
            }
            SessionError::Auth => {
                // Un 401/403 no se reintenta (`backend.rs:25`): cada vuelta del
                // bucle consume cuota cada 60 s. El token sigue en el disco;
                // borrarlo obligaría a un login que nadie pidió.
                self.set_failed(error.to_string());
                false
            }
            other => {
                self.set_failed(other.to_string());
                reintentar_tras(other)
            }
        }
    }

    fn set_failed(&self, message: String) {
        self.telemetry.report(
            "session.connect",
            "error",
            &message,
            serde_json::json!({ "kind": "connect" }),
        );
        let mut estado = self.estado();
        estado.status = Status::Failed;
        estado.last_error = message;
    }

    fn org_rechazada(&self, respondida: &str, pedida: &str) -> SessionError {
        let error = SessionError::Invalido(format!(
            "el servidor respondió la organización {respondida} en lugar de {pedida}"
        ));
        let mut estado = self.estado();
        estado.client = None;
        estado.me = None;
        estado.status = Status::Failed;
        estado.last_error = error.to_string();
        error
    }

    /// Cierra en memoria una conexión que se quedó sin sesión: el estado sale
    /// de `Connecting` y el motivo queda visible (#150).
    ///
    /// No pisa un `LoggedOut` deliberado: `logout` ya lo escribió y limpió el
    /// error, y anotar ahí un fallo mostraría algo que nadie pidió.
    fn sesion_cerrada(&self) {
        let mut estado = self.estado();
        if estado.status != Status::LoggedOut {
            estado.status = Status::LoggedOut;
            estado.last_error = "la sesión cambió mientras se conectaba".to_owned();
            estado.token_efimero = None;
        }
    }

    pub(crate) async fn connect(&self, token: &Token) -> Result<(), SessionError> {
        self.estado().status = Status::Connecting;
        let settings = self.settings_snapshot();
        let endpoint = endpoint(&settings);
        let org_slug = settings.org_slug;
        let org_fijada = settings.org_fijada;
        let mut last_error = None;
        for intento in 0..CONNECT_ATTEMPTS {
            if intento > 0 {
                tokio::time::sleep(self.connect_backoff * intento).await;
            }
            let opened = self
                .connector
                .open(&endpoint, &token.access_token, &org_slug)
                .await;
            let client = match opened {
                Ok(client) => client,
                Err(SessionError::Auth) => {
                    self.set_failed(SessionError::Auth.to_string());
                    return Err(SessionError::Auth);
                }
                Err(error) => {
                    last_error = Some(error);
                    continue;
                }
            };
            match client.me().await {
                Ok(me) => {
                    if self.status() == Status::LoggedOut || !self.sesion_sigue(token) {
                        drop(client);
                        // Este cliente ya no es el de la sesión en disco: otro
                        // proceso cerró o cambió la sesión. El estado tiene que
                        // salir de `Connecting`; `restore_failed` no escribe
                        // nada para `LoggedOut` y `boot` se quedaba girando
                        // para siempre (#150).
                        self.sesion_cerrada();
                        return Err(SessionError::LoggedOut);
                    }
                    let nuevo_slug = me.organization.slug.clone();
                    if org_fijada && !nuevo_slug.is_empty() && nuevo_slug != org_slug {
                        drop(client);
                        return Err(self.org_rechazada(&nuevo_slug, &org_slug));
                    }
                    // El servidor mandaba la organización y eso bastaba
                    // antes. Con una elección hecha en el selector, esa
                    // adopción automática pisaba la decisión de la persona
                    // (#117).
                    let adoptar = !org_fijada && !nuevo_slug.is_empty() && nuevo_slug != org_slug;
                    // El cliente manda el slug en cada petición. Guardar el
                    // slug nuevo sin reabrir deja todas las herramientas
                    // apuntando a la organización anterior (#125).
                    let client = if adoptar {
                        drop(client);
                        let reabierto = self
                            .connector
                            .open(&endpoint, &token.access_token, &nuevo_slug)
                            .await?;
                        if self.status() == Status::LoggedOut || !self.sesion_sigue(token) {
                            drop(reabierto);
                            return Err(SessionError::LoggedOut);
                        }
                        reabierto
                    } else {
                        client
                    };
                    let viejo = {
                        let mut estado = self.estado();
                        let viejo = estado.client.replace(client);
                        estado.me = Some(me);
                        estado.status = Status::Ready;
                        estado.last_error.clear();
                        viejo
                    };
                    drop(viejo);
                    if adoptar {
                        self.limpiar_cambio_org();
                        self.guardar_org_slug(&nuevo_slug, false);
                    }
                    self.kick_watch(&token.access_token);
                    return Ok(());
                }
                Err(SessionError::Auth) => {
                    self.set_failed(SessionError::Auth.to_string());
                    return Err(SessionError::Auth);
                }
                Err(error) => last_error = Some(error),
            }
        }
        let error =
            last_error.unwrap_or_else(|| SessionError::Connect("no se pudo conectar".to_owned()));
        self.set_failed(error.to_string());
        Err(error)
    }

    /// Arranca el timbre de la sesión que acaba de subir y no hace nada si esa
    /// misma sesión ya tiene uno (`app.go:853-876`).
    fn kick_watch(&self, token: &str) {
        // Serializa dos conexiones que terminan a la vez, como `a.mu` en Go;
        // no se retiene el candado de `watch_key` durante `start`/`stop`.
        let _serial = crate::sync::lock(&self.watch_mu);
        let key = session_key(token);
        let habia = {
            let mut actual = crate::sync::lock(&self.watch_key);
            if !actual.is_empty() && *actual == key {
                return;
            }
            let habia = !actual.is_empty();
            actual.clear();
            habia
        };
        if habia && let Some(spawner) = &self.watch_spawner {
            spawner.stop();
        }
        if self.status() != Status::Ready {
            return;
        }
        let generation = self.watch_gen.fetch_add(1, Ordering::SeqCst) + 1;
        *crate::sync::lock(&self.watch_key) = key;
        // Nunca se llama a la costura con el candado de `watch_key` tomado: el
        // implementador puede volver a `App`.
        if let Some(spawner) = &self.watch_spawner {
            spawner.start(generation, Arc::new(self.clone()));
        }
    }

    fn logout_en_memoria(&self) {
        let mut estado = self.estado();
        self.sesion_gen.fetch_add(1, Ordering::SeqCst);
        estado.client = None;
        estado.me = None;
        estado.status = Status::LoggedOut;
        estado.last_error.clear();
        estado.token_efimero = None;
    }

    /// Cambio de organización elegido por la persona: cancela el turno en
    /// curso y sube la generación del agente, así que el próximo mensaje abre
    /// una conversación nueva. Los archivos de las conversaciones anteriores
    /// NO se borran: se conservan bajo la organización en que se crearon.
    /// Decisión y límite documentados en `docs/decision-org-conversaciones.md`.
    fn nueva_conversacion_org(&self) {
        if let Some(agent) = &self.agent {
            agent.clear();
        }
    }

    /// Adopción automática de `get_me` en login/reconexión: la conversación
    /// anterior se descarta porque pertenece a la organización anterior.
    fn limpiar_cambio_org(&self) {
        self.detener_timbre();
        self.nueva_conversacion_org();
        if let Some(conversations) = &self.conversations
            && let Err(error) = conversations.clear()
        {
            // El borrado fallido no tumba la sesión nueva, pero no puede
            // pasar en silencio: el motivo queda en el estado.
            self.estado().last_error =
                format!("no se pudo limpiar la conversación anterior: {error}");
        }
    }
}

/// El plazo en su unidad, para el texto del vencimiento («45 s», «100 ms»).
fn plazo_legible(plazo: Duration) -> String {
    if plazo.subsec_millis() == 0 {
        format!("{} s", plazo.as_secs())
    } else {
        format!("{} ms", plazo.as_millis())
    }
}

fn reintentar_tras(error: &SessionError) -> bool {
    !matches!(
        error,
        SessionError::LoggedOut
            | SessionError::RefreshRejected
            | SessionError::Auth
            | SessionError::Invalido(_)
            | SessionError::StoreLeer(_)
            | SessionError::Guardar(_)
            | SessionError::Tool(_)
    )
}

/// Nombra la sesión del timbre sin guardar otra copia del token
/// (`app.go:878-883`).
fn session_key(token: &str) -> String {
    let digest = Sha256::digest(token.as_bytes());
    hex::encode(&digest[..8])
}

fn exp_del_jwt(token: &str) -> Option<DateTime<FixedOffset>> {
    let payload = token.split('.').nth(1)?;
    use base64::Engine;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .or_else(|_| base64::engine::general_purpose::URL_SAFE.decode(payload))
        .ok()?;
    let claims: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let exp = claims.get("exp")?.as_i64()?;
    chrono::DateTime::<Utc>::from_timestamp(exp, 0).map(|fecha| fecha.fixed_offset())
}

fn ahora() -> DateTime<FixedOffset> {
    Utc::now().fixed_offset()
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::Mutex;

    use async_trait::async_trait;
    use orus_auth::{Store, Token};
    use orus_config::Settings;
    use orus_org::{JsonObject, MessagesPage, Org, ToolSpec};

    use crate::{Connector, Refresh};

    #[test]
    fn auth_no_se_reintenta() {
        assert!(!reintentar_tras(&SessionError::Auth));
    }

    #[test]
    fn reintentar_tras_clasifica() {
        assert!(!reintentar_tras(&SessionError::LoggedOut));
        assert!(!reintentar_tras(&SessionError::RefreshRejected));
        assert!(!reintentar_tras(&SessionError::StoreLeer("x".to_owned())));
        assert!(!reintentar_tras(&SessionError::Guardar("x".to_owned())));
        assert!(!reintentar_tras(&SessionError::Tool("x".to_owned())));
        assert!(reintentar_tras(&SessionError::Connect("x".to_owned())));
    }

    struct StoreFijo(Token);

    impl Store for StoreFijo {
        fn save(&self, _: Token) -> Result<(), orus_auth::Error> {
            Ok(())
        }

        fn load(&self) -> Result<Token, orus_auth::Error> {
            Ok(self.0.clone())
        }

        fn clear(&self) -> Result<(), orus_auth::Error> {
            Ok(())
        }
    }

    /// Un servidor que ignora `x-org-slug` y responde con otra organización no
    /// puede pisar la elección hecha en el selector (#117).
    struct BackendOtraOrg;

    #[async_trait]
    impl Backend for BackendOtraOrg {
        async fn me(&self) -> Result<Me, SessionError> {
            Ok(Me {
                organization: Org {
                    slug: "org-a".to_owned(),
                    ..Org::default()
                },
                ..Me::default()
            })
        }

        async fn tasks(&self) -> Result<Vec<orus_org::Task>, SessionError> {
            Ok(Vec::new())
        }

        async fn rooms(&self) -> Result<Vec<orus_org::Room>, SessionError> {
            Ok(Vec::new())
        }

        async fn messages(&self, _: &str, _: i64, _: i64) -> Result<MessagesPage, SessionError> {
            Ok(MessagesPage::default())
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

        async fn doorbell(&self) -> Result<orus_org::doorbell::Config, SessionError> {
            Err(SessionError::NoDoorbell)
        }
    }

    struct ConectorOtraOrg;

    #[async_trait]
    impl Connector for ConectorOtraOrg {
        async fn open(&self, _: &str, _: &str, _: &str) -> Result<Arc<dyn Backend>, SessionError> {
            Ok(Arc::new(BackendOtraOrg))
        }
    }

    struct SinRefresco;

    #[async_trait]
    impl Refresh for SinRefresco {
        async fn refresh(&self, _: &Token) -> Result<Token, SessionError> {
            Err(SessionError::RefreshRejected)
        }
    }

    fn token_de_prueba() -> Token {
        Token {
            access_token: "token-de-prueba".to_owned(),
            refresh_token: String::new(),
            token_type: "Bearer".to_owned(),
            scope: String::new(),
            expiry: sin_vencimiento(),
        }
    }

    /// Un conector que nunca responde: la red no puede retener el candado de
    /// refresco más allá del plazo de restauración (#150).
    struct ConectorMudo;

    #[async_trait]
    impl Connector for ConectorMudo {
        async fn open(&self, _: &str, _: &str, _: &str) -> Result<Arc<dyn Backend>, SessionError> {
            std::future::pending::<()>().await;
            Err(SessionError::Connect("no debería responder".to_owned()))
        }
    }

    /// Un `Store` que otro proceso puede cerrar a mitad de la conexión.
    #[derive(Clone)]
    struct StoreQueSeCierra(Arc<Mutex<Option<Token>>>);

    impl Store for StoreQueSeCierra {
        fn save(&self, token: Token) -> Result<(), orus_auth::Error> {
            *crate::sync::lock(&self.0) = Some(token);
            Ok(())
        }

        fn load(&self) -> Result<Token, orus_auth::Error> {
            crate::sync::lock(&self.0)
                .clone()
                .ok_or(orus_auth::Error::NoSession)
        }

        fn clear(&self) -> Result<(), orus_auth::Error> {
            *crate::sync::lock(&self.0) = None;
            Ok(())
        }
    }

    /// `get_me` responde, pero para entonces el token de disco ya no está: es
    /// el logout de otro proceso visto desde esta conexión (#150).
    struct BackendQueCierra(StoreQueSeCierra);

    #[async_trait]
    impl Backend for BackendQueCierra {
        async fn me(&self) -> Result<Me, SessionError> {
            self.0
                .clear()
                .map_err(|error| SessionError::StoreLeer(error.to_string()))?;
            Ok(Me::default())
        }

        async fn tasks(&self) -> Result<Vec<orus_org::Task>, SessionError> {
            Ok(Vec::new())
        }

        async fn rooms(&self) -> Result<Vec<orus_org::Room>, SessionError> {
            Ok(Vec::new())
        }

        async fn messages(&self, _: &str, _: i64, _: i64) -> Result<MessagesPage, SessionError> {
            Ok(MessagesPage::default())
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

        async fn doorbell(&self) -> Result<orus_org::doorbell::Config, SessionError> {
            Err(SessionError::NoDoorbell)
        }
    }

    struct ConectorQueCierra(StoreQueSeCierra);

    #[async_trait]
    impl Connector for ConectorQueCierra {
        async fn open(&self, _: &str, _: &str, _: &str) -> Result<Arc<dyn Backend>, SessionError> {
            Ok(Arc::new(BackendQueCierra(self.0.clone())))
        }
    }

    /// El desenlace de un `client()` envuelto en un plazo externo, para el
    /// mensaje de fallo: `Backend` no implementa `Debug`.
    fn desenlace(
        resultado: &Result<Result<Arc<dyn Backend>, SessionError>, tokio::time::error::Elapsed>,
    ) -> String {
        match resultado {
            Ok(Err(error)) => error.to_string(),
            Ok(Ok(_)) => "entregó un cliente".to_owned(),
            Err(_) => "no venció solo: el plazo externo lo cortó".to_owned(),
        }
    }

    /// #150: una llamada a `client` no puede quedarse colgada con el candado
    /// de refresco tomado; al vencer el plazo, el candado queda libre.
    #[tokio::test]
    async fn client_vencido_por_plazo_suelta_el_candado() {
        let mut app = App::new(
            Box::new(StoreFijo(token_de_prueba())),
            Settings::default(),
            Arc::new(ConectorMudo),
            Arc::new(SinRefresco),
        );
        app.set_restore_timeout(Duration::from_millis(100));

        let comienzo = std::time::Instant::now();
        let resultado = tokio::time::timeout(Duration::from_secs(2), app.client()).await;
        let tardanza = comienzo.elapsed();

        assert!(
            matches!(&resultado, Ok(Err(SessionError::Connect(_)))),
            "un conector mudo tiene que vencer por plazo, no colgarse: {}",
            desenlace(&resultado)
        );
        assert!(
            tardanza < Duration::from_secs(2),
            "el plazo de 100 ms tardó {tardanza:?}"
        );

        // El candado se soltó con el vencimiento: esta llamada vuelve a vencer
        // en vez de esperar al que ya no existe.
        let otra = tokio::time::timeout(Duration::from_secs(2), app.client()).await;
        assert!(
            matches!(&otra, Ok(Err(SessionError::Connect(_)))),
            "el candado de refresco quedó tomado: {}",
            desenlace(&otra)
        );
    }

    /// #150: si el token cambia bajo los pies de `get_me`, el intento no puede
    /// dejar el estado en `Connecting`: `restore_failed` no escribe nada para
    /// `LoggedOut` y el arranque se quedaba girando para siempre.
    #[tokio::test]
    async fn un_token_que_cambia_a_mitad_deja_estado_terminal() {
        let store = StoreQueSeCierra(Arc::new(Mutex::new(Some(token_de_prueba()))));
        let app = App::new(
            Box::new(store.clone()),
            Settings::default(),
            Arc::new(ConectorQueCierra(store.clone())),
            Arc::new(SinRefresco),
        );

        let resultado = app.client().await;

        let fallo = match &resultado {
            Err(error) => error.to_string(),
            Ok(_) => "entregó un cliente".to_owned(),
        };
        assert!(
            matches!(&resultado, Err(SessionError::LoggedOut)),
            "una sesión cerrada por otro proceso no entrega cliente: {fallo}"
        );
        assert_ne!(
            app.status(),
            Status::Connecting,
            "el estado no puede quedarse en «conectando» para siempre"
        );
        assert!(matches!(app.status(), Status::LoggedOut | Status::Failed));
        assert!(
            !app.last_error_sesion().is_empty(),
            "el cierre tiene que quedar visible en el estado"
        );
    }

    #[tokio::test]
    async fn org_fijada_no_es_pisada_por_la_adopcion_de_get_me() {
        let token = Token {
            access_token: "token-de-prueba".to_owned(),
            refresh_token: String::new(),
            token_type: "Bearer".to_owned(),
            scope: String::new(),
            expiry: sin_vencimiento(),
        };
        let app = App::new(
            Box::new(StoreFijo(token.clone())),
            Settings {
                org_slug: "org-b".to_owned(),
                org_fijada: true,
                ..Settings::default()
            },
            Arc::new(ConectorOtraOrg),
            Arc::new(SinRefresco),
        );

        let conectado = app.connect(&token).await;
        assert!(
            matches!(conectado, Err(SessionError::Invalido(_))),
            "un get_me ajeno a la organización fijada se rechaza: {conectado:?}"
        );

        assert_eq!(
            app.settings_snapshot().org_slug,
            "org-b",
            "get_me no puede reemplazar la organización elegida"
        );
        assert!(app.settings_snapshot().org_fijada);
    }
}
