//! Regresión del arranque real: la sesión guardada tiene que salir de
//! `connecting`.
//!
//! `App::new` deja `connecting` cuando hay token en disco, y el `Boot` del contrato
//! era quien arrancaba la restauración. Los tests de `orus-app` llamaban
//! `app.boot()` a mano; el binario no, así que la ventana se quedaba en el
//! splash. Este test usa `arranque::iniciar`, el mismo ensamblaje del binario.

use std::io::{Read as _, Write as _};
use std::net::TcpStream;
use std::sync::Arc;
use std::time::Duration;

use orus_auth::Store as _;
use orus_shell::arranque::{Acciones, Salir};
use serial_test::serial;

/// Pide una ruta por HTTP/1.1 y devuelve la respuesta completa, con la línea
/// de estado y las cabeceras. `cookie` lleva el par `nombre=valor`, sin
/// la cabecera `Cookie:`; `extra` son cabeceras tal cual se mandan
/// (`Origin`, `Sec-Fetch-Site`). Los métodos con cuerpo van con largo cero.
fn pedir_con(
    addr: &str,
    metodo: &str,
    ruta: &str,
    cookie: Option<&str>,
    extra: &[(&str, &str)],
) -> String {
    let mut conexion = TcpStream::connect(addr).expect("conectar al servidor local");
    let mut peticion = format!("{metodo} {ruta} HTTP/1.1\r\nHost: {addr}\r\n");
    if let Some(valor) = cookie {
        peticion.push_str(&format!("Cookie: {valor}\r\n"));
    }
    for (nombre, valor) in extra {
        peticion.push_str(&format!("{nombre}: {valor}\r\n"));
    }
    if metodo != "GET" && metodo != "HEAD" {
        peticion.push_str("Content-Length: 0\r\n");
    }
    peticion.push_str("Connection: close\r\n\r\n");
    conexion
        .write_all(peticion.as_bytes())
        .expect("enviar la petición");
    let mut respuesta = String::new();
    conexion
        .read_to_string(&mut respuesta)
        .expect("leer la respuesta");
    respuesta
}

/// Un GET sin cabeceras extra: el caso de las pruebas que ya existían.
fn pedir_crudo(addr: &str, cookie: Option<&str>, ruta: &str) -> String {
    pedir_con(addr, "GET", ruta, cookie, &[])
}

#[test]
#[serial]
fn la_sesion_guardada_sale_de_connecting() {
    let datos = tempfile::tempdir().expect("carpeta temporal");
    let ruta_datos = datos.path().to_path_buf();

    temp_env::with_vars(
        [
            ("ORUSCRIBE_DATA_DIR", Some(ruta_datos.as_os_str())),
            (
                "ORUSCRIBE_ALLOW_FILE_SECRETS",
                Some(std::ffi::OsStr::new("1")),
            ),
        ],
        || {
            // Un puerto cerrado hace fallar la restauración de inmediato, sin
            // depender de la red ni del backend real.
            orus_config::editar(|settings| {
                settings.endpoint = "http://127.0.0.1:1".to_owned();
            })
            .expect("guardar el endpoint de prueba");

            let vencido: orus_auth::Token = serde_json::from_value(serde_json::json!({
                "access_token": "token-de-prueba",
                "expiry": "2020-01-01T00:00:00Z"
            }))
            .expect("token vencido");
            orus_auth::FileStore::from_data_dir(&ruta_datos)
                .save(vencido)
                .expect("guardar la sesión");

            let salir: Salir = Arc::new(|| {});
            let servidor = orus_shell::arranque::iniciar(Arc::new(Acciones::nuevas(salir)))
                .expect("arrancar el servidor del shell");

            let resto = servidor
                .url
                .strip_prefix("http://")
                .expect("URL local con http");
            let (addr, token) = resto.split_once("/?token=").expect("URL con token");

            let mut respuesta = String::new();
            for _ in 0..300 {
                respuesta = pedir_crudo(addr, Some(&format!("orus_token={token}")), "/api/state");
                if !respuesta.contains("\"connecting\"") {
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }

            // Un 401 con cookie válida también sale del bucle: sin exigir
            // 200, una puerta del token que rechaza todo quedaría verde
            // (issue #98).
            assert!(
                respuesta.starts_with("HTTP/1.1 200"),
                "la puerta del token tenía que aceptar la cookie válida: {respuesta}"
            );
            assert!(
                !respuesta.contains("\"connecting\""),
                "la sesión guardada se quedó en connecting: {respuesta}"
            );
        },
    );
}

/// Arranca el shell con una sesión vencida guardada y llama a `cuerpo` con la
/// dirección local y el token de la URL. La restauración falla de inmediato
/// contra un puerto cerrado, sin red ni backend real; lo que importa aquí es
/// la puerta del token.
fn con_servidor_con_sesion_vencida(cuerpo: impl FnOnce(&str, &str)) {
    let datos = tempfile::tempdir().expect("carpeta temporal");
    let ruta_datos = datos.path().to_path_buf();

    temp_env::with_vars(
        [
            ("ORUSCRIBE_DATA_DIR", Some(ruta_datos.as_os_str())),
            (
                "ORUSCRIBE_ALLOW_FILE_SECRETS",
                Some(std::ffi::OsStr::new("1")),
            ),
        ],
        || {
            orus_config::editar(|settings| {
                settings.endpoint = "http://127.0.0.1:1".to_owned();
            })
            .expect("guardar el endpoint de prueba");

            let vencido: orus_auth::Token = serde_json::from_value(serde_json::json!({
                "access_token": "token-de-prueba",
                "expiry": "2020-01-01T00:00:00Z"
            }))
            .expect("token vencido");
            orus_auth::FileStore::from_data_dir(&ruta_datos)
                .save(vencido)
                .expect("guardar la sesión");

            let salir: Salir = Arc::new(|| {});
            let servidor = orus_shell::arranque::iniciar(Arc::new(Acciones::nuevas(salir)))
                .expect("arrancar el servidor del shell");

            let resto = servidor
                .url
                .strip_prefix("http://")
                .expect("URL local con http");
            let (addr, token) = resto.split_once("/?token=").expect("URL con token");
            cuerpo(addr, token);
        },
    );
}

#[test]
#[serial]
fn la_api_sin_cookie_exige_401() {
    con_servidor_con_sesion_vencida(|addr, _token| {
        let respuesta = pedir_crudo(addr, None, "/api/state");
        assert!(
            respuesta.starts_with("HTTP/1.1 401"),
            "una petición sin la cookie del token tiene que exigir 401: {respuesta}"
        );
        assert!(respuesta.contains("no autorizado"), "{respuesta}");
    });
}

#[test]
#[serial]
fn la_api_con_cookie_falsa_exige_401() {
    con_servidor_con_sesion_vencida(|addr, _token| {
        let respuesta = pedir_crudo(
            addr,
            Some("orus_token=0000000000000000000000000000000000000000000000000000000000000000"),
            "/api/state",
        );
        assert!(
            respuesta.starts_with("HTTP/1.1 401"),
            "una cookie con un token falso tiene que exigir 401: {respuesta}"
        );
        assert!(respuesta.contains("no autorizado"), "{respuesta}");
    });
}

/// La otra mitad de la puerta: con la cookie válida del arranque la API
/// responde 200. Sin este caso, una puerta que rechaza todo queda verde
/// (issue #98). Cubre el caso 6 de #73: un GET sin `Origin` sigue pasando.
#[test]
#[serial]
fn la_api_con_cookie_valida_responde_200() {
    con_servidor_con_sesion_vencida(|addr, token| {
        let respuesta = pedir_crudo(addr, Some(&format!("orus_token={token}")), "/api/state");
        assert!(
            respuesta.starts_with("HTTP/1.1 200"),
            "la puerta del token tiene que aceptar la cookie válida: {respuesta}"
        );
    });
}

/// #73, caso 1: la cookie es `SameSite=Strict`, pero SameSite no separa
/// puertos. Una página en otro puerto de 127.0.0.1 es mismo sitio, recibe la
/// cookie válida del navegador y podía reiniciar al agente, apagar la app o
/// desinstalarla. El origen propio se exige aparte.
#[test]
#[serial]
fn el_post_de_otro_origen_de_loopback_se_niega() {
    con_servidor_con_sesion_vencida(|addr, token| {
        let cookie = format!("orus_token={token}");
        let respuesta = pedir_con(
            addr,
            "POST",
            "/api/agent/reset",
            Some(&cookie),
            &[("Origin", "http://127.0.0.1:9999")],
        );
        assert!(
            respuesta.starts_with("HTTP/1.1 403"),
            "un POST desde otro puerto de 127.0.0.1 tiene que exigir 403: {respuesta}"
        );
        assert!(respuesta.contains("origen no permitido"), "{respuesta}");
    });
}

/// #73, casos 2 a 4: la página propia sigue funcionando. Con `Origin` del
/// loopback de este arranque, con el `localhost` del mismo puerto, y con
/// `Sec-Fetch-Site: same-origin` cuando el navegador no manda `Origin`.
#[test]
#[serial]
fn el_post_del_origen_propio_pasa() {
    con_servidor_con_sesion_vencida(|addr, token| {
        let cookie = format!("orus_token={token}");
        let puerto = addr.rsplit_once(':').expect("dirección con puerto").1;
        let casos = [
            ("origin 127.0.0.1", format!("http://{addr}")),
            ("origin localhost", format!("http://localhost:{puerto}")),
        ];
        for (caso, origen) in &casos {
            let respuesta = pedir_con(
                addr,
                "POST",
                "/api/agent/reset",
                Some(&cookie),
                &[("Origin", origen.as_str())],
            );
            assert!(
                respuesta.starts_with("HTTP/1.1 200"),
                "el POST del origen propio ({caso}) tiene que pasar: {respuesta}"
            );
        }

        let respuesta = pedir_con(
            addr,
            "POST",
            "/api/agent/reset",
            Some(&cookie),
            &[("Sec-Fetch-Site", "same-origin")],
        );
        assert!(
            respuesta.starts_with("HTTP/1.1 200"),
            "sin Origin, Sec-Fetch-Site same-origin tiene que pasar: {respuesta}"
        );
    });
}

/// #73, caso 5 y sus bordes: sin `Origin` no se puede distinguir el origen, y
/// `Origin: null` (iframe en arena) u otro esquema tampoco son la página
/// propia. Se niega antes de llegar al handler.
#[test]
#[serial]
fn el_post_sin_origen_o_con_origen_ajeno_se_niega() {
    con_servidor_con_sesion_vencida(|addr, token| {
        let cookie = format!("orus_token={token}");
        let casos: [(&str, Option<(&str, String)>); 3] = [
            ("sin Origin ni Sec-Fetch-Site", None),
            ("Origin: null", Some(("Origin", "null".to_owned()))),
            ("otro esquema", Some(("Origin", format!("https://{addr}")))),
        ];
        for (caso, extra) in &casos {
            let cabeceras: Vec<(&str, &str)> = extra
                .iter()
                .map(|(nombre, valor)| (*nombre, valor.as_str()))
                .collect();
            let respuesta = pedir_con(addr, "POST", "/api/agent/reset", Some(&cookie), &cabeceras);
            assert!(
                respuesta.starts_with("HTTP/1.1 403"),
                "el POST sin origen propio ({caso}) tiene que exigir 403: {respuesta}"
            );
            assert!(respuesta.contains("origen no permitido"), "{respuesta}");
        }
    });
}

/// #73, borde del método: `DELETE` también cambia estado y también exige
/// origen propio. Sin este caso, una lista de métodos incompleta en la puerta
/// pasaría verde.
#[test]
#[serial]
fn el_delete_de_otro_origen_se_niega() {
    con_servidor_con_sesion_vencida(|addr, token| {
        let cookie = format!("orus_token={token}");
        let respuesta = pedir_con(
            addr,
            "DELETE",
            "/api/notes",
            Some(&cookie),
            &[("Origin", "http://127.0.0.1:9999")],
        );
        assert!(
            respuesta.starts_with("HTTP/1.1 403"),
            "un DELETE desde otro puerto de 127.0.0.1 tiene que exigir 403: {respuesta}"
        );
        assert!(respuesta.contains("origen no permitido"), "{respuesta}");
    });
}

/// #73, caso 7: la navegación inicial no cambia estado. `GET /?token=` sigue
/// cambiando el token por la cookie como hoy.
#[test]
#[serial]
fn la_navegacion_inicial_sigue_emitiendo_la_cookie() {
    con_servidor_con_sesion_vencida(|addr, token| {
        let respuesta = pedir_crudo(addr, None, &format!("/?token={token}"));
        assert!(
            respuesta.starts_with("HTTP/1.1 30"),
            "la URL inicial tiene que redirigir a la app: {respuesta}"
        );
        let minusculas = respuesta.to_ascii_lowercase();
        assert!(
            minusculas.contains(&format!("set-cookie: orus_token={token}")),
            "la navegación inicial dejó de emitir la cookie: {respuesta}"
        );
        assert!(
            minusculas.contains("httponly") && minusculas.contains("samesite=strict"),
            "la cookie perdió HttpOnly o SameSite=Strict: {respuesta}"
        );
    });
}
