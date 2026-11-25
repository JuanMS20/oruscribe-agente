use axum::Router;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Redirect, Response};

#[derive(Clone)]
pub struct Guardia {
    token: String,
    host: String,
}

impl Guardia {
    pub fn nuevo(host: String) -> Self {
        use rand::RngCore;

        let mut bytes = [0u8; 32];
        let mut azar = rand::rngs::OsRng;
        azar.fill_bytes(&mut bytes);
        Self {
            token: hex::encode(bytes),
            host,
        }
    }

    pub fn token(&self) -> &str {
        &self.token
    }
}

pub fn proteger(router: Router, guardia: Guardia) -> Router {
    router.layer(axum::middleware::from_fn_with_state(guardia, exigir_token))
}

async fn exigir_token(State(guardia): State<Guardia>, request: Request, next: Next) -> Response {
    if !host_permitido(request.headers(), &guardia.host) {
        return (StatusCode::MISDIRECTED_REQUEST, "host no permitido").into_response();
    }
    let uri = request.uri();
    let es_api = uri.path() == "/api" || uri.path().starts_with("/api/");
    if uri.path() == "/" && parametro(uri.query(), "token") == Some(guardia.token.as_str()) {
        return endurecer(con_cookie(&guardia.token), false);
    }
    if es_api && !cookie_valido(request.headers(), &guardia.token) {
        return endurecer(
            (StatusCode::UNAUTHORIZED, "no autorizado").into_response(),
            true,
        );
    }
    if es_api
        && cambia_estado(request.method())
        && !origen_permitido(request.headers(), &guardia.host)
    {
        return endurecer(
            (StatusCode::FORBIDDEN, "origen no permitido").into_response(),
            true,
        );
    }
    endurecer(next.run(request).await, es_api)
}

fn cambia_estado(metodo: &Method) -> bool {
    !matches!(metodo, &Method::GET | &Method::HEAD | &Method::OPTIONS)
}

fn origen_permitido(headers: &HeaderMap, esperado: &str) -> bool {
    let Some(origen) = headers.get(header::ORIGIN) else {
        return headers
            .get("sec-fetch-site")
            .and_then(|valor| valor.to_str().ok())
            .is_some_and(|valor| valor.eq_ignore_ascii_case("same-origin"));
    };
    let Ok(origen) = origen.to_str() else {
        return false;
    };
    if origen.eq_ignore_ascii_case(&format!("http://{esperado}")) {
        return true;
    }
    match esperado.rsplit_once(':') {
        Some((_, puerto)) => origen.eq_ignore_ascii_case(&format!("http://localhost:{puerto}")),
        None => false,
    }
}

fn host_permitido(headers: &HeaderMap, esperado: &str) -> bool {
    let Some(host) = headers
        .get(header::HOST)
        .and_then(|valor| valor.to_str().ok())
    else {
        return false;
    };
    if host.eq_ignore_ascii_case(esperado) {
        return true;
    }
    match esperado.rsplit_once(':') {
        Some((_, puerto)) => host.eq_ignore_ascii_case(&format!("localhost:{puerto}")),
        None => false,
    }
}

fn endurecer(mut respuesta: Response, api: bool) -> Response {
    let cabeceras = respuesta.headers_mut();
    cabeceras.insert(
        HeaderName::from_static("x-content-type-options"),
        HeaderValue::from_static("nosniff"),
    );
    cabeceras.insert(
        HeaderName::from_static("referrer-policy"),
        HeaderValue::from_static("no-referrer"),
    );
    cabeceras.insert(
        HeaderName::from_static("x-frame-options"),
        HeaderValue::from_static("DENY"),
    );
    if api {
        cabeceras.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    }
    respuesta
}

fn con_cookie(token: &str) -> Response {
    let mut respuesta = Redirect::to("/").into_response();
    if let Ok(valor) = format!("orus_token={token}; Path=/; HttpOnly; SameSite=Strict")
        .parse::<axum::http::HeaderValue>()
    {
        let _ = respuesta.headers_mut().insert(header::SET_COOKIE, valor);
    }
    respuesta
}

fn cookie_valido(headers: &HeaderMap, token: &str) -> bool {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|valor| valor.to_str().ok())
        .flat_map(|valor| valor.split(';'))
        .any(|par| {
            matches!(
                par.trim().split_once('='),
                Some(("orus_token", valor)) if valor == token
            )
        })
}

fn parametro<'a>(consulta: Option<&'a str>, nombre: &str) -> Option<&'a str> {
    consulta?.split('&').find_map(|par| {
        let (clave, valor) = par.split_once('=')?;
        (clave == nombre).then_some(valor)
    })
}
