//! Errores del proveedor y costo estimado. Port de `internal/agent/errors.go:12-63`.

use std::error::Error as StdError;
use std::fmt;
use std::io;

use orus_core::Message;

use crate::types::ChatReply;

/// Texto del guard de respuesta vacía del fix #93, extendido a cloud, a los
/// streams y al runner en el issue #126.
pub(crate) const SIN_RESPUESTA: &str = "el proveedor no devolvió ninguna respuesta";

/// Texto del corte de stream compartido por los dos adaptadores.
pub(crate) const CORTE_STREAM: &str = "la respuesta del proveedor se cortó a mitad";

/// Un turno sin texto (ni siquiera espacios) y sin llamadas no es respuesta.
pub(crate) fn respuesta_vacia(mensaje: &Message) -> bool {
    mensaje.content.trim().is_empty() && mensaje.tool_calls.as_ref().is_none_or(Vec::is_empty)
}

/// Lo que devolvió el proveedor cuando el turno no se completó: un código HTTP
/// con su cuerpo, un error local anterior a la red, o un fallo de transporte.
/// El bucle lo usa para decidir si tiene sentido reintentar.
///
/// Copia de `internal/agent/errors.go:15-19`. Go devuelve la respuesta parcial
/// y el error a la vez; acá la parcial viaja en [`ProviderError::partial`].
#[derive(Debug)]
pub struct ProviderError {
    /// Código HTTP; 0 cuando la petición falló antes de llegar o el error es
    /// local (dirección inválida, cuerpo ilegible).
    pub status: u16,
    /// Cuerpo del error, ya truncado por quien lo crea. Un error local guarda
    /// acá su mensaje completo.
    pub body: String,
    /// Fallo de transporte, si lo hubo.
    pub source: Option<Box<dyn StdError + Send + Sync>>,
    /// Respuesta parcial cuando el proveedor cortó el turno por el límite de
    /// tokens: el texto llegó, pero no se puede presentar como completo. Va en
    /// caja porque es un caso raro y no debe engordar todos los errores.
    pub partial: Option<Box<ChatReply>>,
}

impl ProviderError {
    /// Error local anterior a la red: validación, cuerpo ilegible o límite de
    /// tamaño. No se reintenta.
    pub fn local(message: impl Into<String>) -> Self {
        Self {
            status: 0,
            body: message.into(),
            source: None,
            partial: None,
        }
    }

    /// Respuesta HTTP con un código que no es 2xx.
    pub fn http(status: u16, body: String) -> Self {
        Self {
            status,
            body,
            source: None,
            partial: None,
        }
    }

    /// Fallo de transporte: la petición no llegó a tener respuesta.
    pub fn transporte(source: reqwest::Error) -> Self {
        Self {
            status: 0,
            body: String::new(),
            source: Some(Box::new(source)),
            partial: None,
        }
    }

    /// Adjunta la respuesta parcial al error que avisa del corte.
    pub fn con_parcial(mut self, reply: ChatReply) -> Self {
        self.partial = Some(Box::new(reply));
        self
    }

    /// El proveedor contestó sin texto ni llamadas ([`SIN_RESPUESTA`]).
    pub fn es_sin_respuesta(&self) -> bool {
        self.status == 0 && self.source.is_none() && self.body == SIN_RESPUESTA
    }

    /// Dice si tiene sentido reintentar el turno: 429, 5xx o red. Un 4xx
    /// distinto de 429 no se reintenta; un contexto cancelado o vencido
    /// tampoco.
    ///
    /// Port de `EsErrorTransitorio` (`internal/agent/errors.go:34-53`). En Go
    /// un `net.Error` es transitorio; acá se reconocen los `ErrorKind` de red
    /// de `std::io`. No hay `context.Canceled` en Rust: la cancelación llega
    /// como `Interrupted` y el vencimiento como `TimedOut`, y ninguno se
    /// reintenta.
    pub fn is_transient(&self) -> bool {
        match &self.source {
            Some(source) => fuente_es_transitoria(source.as_ref()),
            None => self.status == 429 || self.status >= 500,
        }
    }
}

impl fmt::Display for ProviderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (&self.source, self.status) {
            (Some(source), _) => write!(f, "no se pudo contactar al proveedor: {source}"),
            (None, 0) => write!(f, "{}", self.body),
            (None, status) => write!(f, "el proveedor respondió {status}: {}", self.body),
        }
    }
}

impl StdError for ProviderError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match &self.source {
            Some(source) => Some(source.as_ref()),
            None => None,
        }
    }
}

/// Recorre la cadena de causas como hacía `EsErrorTransitorio` al recursar en
/// `pe.Err`: un error de transporte puede venir envuelto por otro.
fn fuente_es_transitoria(fuente: &(dyn StdError + 'static)) -> bool {
    let mut actual = fuente;
    loop {
        if let Some(proveedor) = actual.downcast_ref::<ProviderError>() {
            return proveedor.is_transient();
        }
        if let Some(error) = actual.downcast_ref::<reqwest::Error>() {
            return reqwest_es_transitoria(error);
        }
        if let Some(io_error) = actual.downcast_ref::<io::Error>() {
            return es_kind_de_red(io_error.kind());
        }
        match actual.source() {
            Some(siguiente) => actual = siguiente,
            None => return false,
        }
    }
}

/// Un `reqwest::Error` sin status HTTP es un fallo de transporte: corte antes
/// de la cabecera, DNS, conexión o lectura del cuerpo. Un vencimiento también
/// cumple `is_request` en reqwest y no se reintenta. Un error con status no se
/// clasifica acá.
fn reqwest_es_transitoria(error: &reqwest::Error) -> bool {
    if error.status().is_some() || error.is_timeout() {
        return false;
    }
    error.is_connect() || error.is_request() || error.is_body() || error.is_decode()
}

fn es_kind_de_red(kind: io::ErrorKind) -> bool {
    matches!(
        kind,
        io::ErrorKind::ConnectionRefused
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::NotConnected
            | io::ErrorKind::NetworkUnreachable
            | io::ErrorKind::HostUnreachable
            | io::ErrorKind::BrokenPipe
            | io::ErrorKind::WouldBlock
            | io::ErrorKind::UnexpectedEof
    )
}

/// Convierte tokens a dólares con las tarifas que declaró la persona (dólares
/// por millón de tokens, entrada y salida por separado). Sin tarifas
/// declaradas devuelve 0: el código no adivina precios que se pudren.
///
/// Port de `CostoEstimado` (`internal/agent/errors.go:58-63`).
pub fn costo_estimado(
    prompt: i64,
    completion: i64,
    input_por_mtok: f64,
    output_por_mtok: f64,
) -> f64 {
    if input_por_mtok <= 0.0 && output_por_mtok <= 0.0 {
        return 0.0;
    }
    prompt as f64 * input_por_mtok / 1e6 + completion as f64 * output_por_mtok / 1e6
}

#[cfg(test)]
mod tests {
    use std::io;

    use super::*;

    fn proveedor(status: u16) -> ProviderError {
        ProviderError {
            status,
            body: "cuerpo".to_owned(),
            source: None,
            partial: None,
        }
    }

    fn con_fuente(kind: io::ErrorKind, detalle: &str) -> ProviderError {
        ProviderError {
            status: 0,
            body: String::new(),
            source: Some(Box::new(io::Error::new(kind, detalle.to_owned()))),
            partial: None,
        }
    }

    /// Clasificación de `internal/agent/errors.go:34-53`.
    #[test]
    fn error_transitorio() {
        for status in [429u16, 500, 503] {
            assert!(
                proveedor(status).is_transient(),
                "{status} debe reintentarse"
            );
        }
        assert!(!proveedor(400).is_transient(), "400 no se reintenta");
        assert!(!proveedor(401).is_transient(), "401 no se reintenta");
        assert!(
            !con_fuente(io::ErrorKind::Interrupted, "cancelado").is_transient(),
            "cancelado no se reintenta"
        );
        assert!(
            !con_fuente(io::ErrorKind::TimedOut, "vencido").is_transient(),
            "deadline no se reintenta"
        );
        assert!(
            con_fuente(io::ErrorKind::UnexpectedEof, "cortado").is_transient(),
            "EOF se reintenta"
        );
        assert!(
            con_fuente(io::ErrorKind::ConnectionReset, "red").is_transient(),
            "un fallo de red se reintenta"
        );
    }

    /// El texto exacto de `internal/agent/errors.go:21-26`.
    #[test]
    fn mensaje_del_error() {
        assert_eq!(
            proveedor(400).to_string(),
            "el proveedor respondió 400: cuerpo"
        );
        assert_eq!(
            con_fuente(io::ErrorKind::ConnectionReset, "se cayó").to_string(),
            "no se pudo contactar al proveedor: se cayó"
        );
    }

    /// Un error local se muestra tal cual y nunca se reintenta: no hay red que
    /// esperar.
    #[test]
    fn error_local_se_muestra_tal_cual() {
        let error = ProviderError::local("la dirección del proveedor no es válida");
        assert_eq!(error.to_string(), "la dirección del proveedor no es válida");
        assert!(!error.is_transient());
    }

    /// Fórmula de `internal/agent/errors.go:58-63`.
    #[test]
    fn costo_estimado() {
        // El nombre del test tapa el de la función; se llama por ruta completa.
        assert_eq!(super::costo_estimado(1_000_000, 500_000, 0.0, 0.0), 0.0);
        assert_eq!(super::costo_estimado(1_000_000, 500_000, 3.0, 6.0), 6.0);
        assert_eq!(super::costo_estimado(2_000_000, 0, 1.5, 0.0), 3.0);
    }
}
