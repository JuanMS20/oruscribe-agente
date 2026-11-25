//! Descarga del ejecutable y comprobación de su SHA-256
//! (`internal/update/update.go:158-270`).

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;

use sha2::{Digest, Sha256};

use super::{ALLOWED_HOSTS, Error, LIMITE_DESCARGA};

const PREFIJO_REPO: &str = "/JuanMS20/oruscribe-agente-descargas/";

/// Acepta una descarga solo desde https en un host conocido.
///
/// Loopback se acepta con cualquier esquema para tests y pruebas manuales
/// (`update.go:158-178`). La redirección la sigue el cliente HTTP; esta
/// comprobación mira la URL inicial, igual que Go.
pub(crate) fn check_download_url(raw: &str) -> Result<(), Error> {
    let Ok(url) = reqwest::Url::parse(raw) else {
        return Err(Error::SitioNoPermitido(raw.to_owned()));
    };
    let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
    if matches!(host.as_str(), "localhost" | "127.0.0.1" | "::1" | "[::1]") {
        return Ok(());
    }
    if url.scheme() == "https" && ALLOWED_HOSTS.contains(&host.as_str()) {
        // `path.Clean` evita que un `/../` salga del repositorio. El parser de
        // `url` ya normaliza los segmentos `.` y `..`.
        if host == "github.com" && !url.path().starts_with(PREFIJO_REPO) {
            return Err(Error::RepositorioAjeno(format!("{host}{}", url.path())));
        }
        return Ok(());
    }
    Err(Error::SitioNoPermitido(host))
}

/// Baja `url` a `dest` y exige que el SHA-256 coincida con el manifiesto.
///
/// Es [`download_con`] con un receptor que no mira los bytes: los llamadores
/// que no muestran progreso no cambian.
pub async fn download(
    http: &reqwest::Client,
    url: &str,
    want_sha: &str,
    dest: &Path,
) -> Result<(), Error> {
    download_con(http, url, want_sha, dest, &mut |_, _| {}).await
}

/// Baja `url` a `dest`, avisa del avance y exige que el SHA-256 coincida con
/// el manifiesto.
///
/// En el flujo de la app, `dest` es `<exe>.new` en Windows y `<exe>.new.pkg`
/// en Linux; el swap recibe esa misma ruta.
///
/// `on_bytes(recibidos, total)` sale una vez con `recibidos = 0` (para que la
/// ventana conozca el tamaño antes del primer fragmento) y después tras cada
/// fragmento escrito. `total` viene de `Content-Length` y es `None` cuando el
/// servidor no lo manda. El trait object pide `Send` porque el futuro de
/// `apply_update` cruza este `await` y el borde exige futuros `Send`.
///
/// El archivo se crea exclusivo después de borrar lo que hubiera: un symlink
/// plantado en `dest` no se sigue (`update.go:249-254`).
///
/// shortcut: no se hace `fsync`, igual que el `Download` de Go; mejorarlo
/// requiere decidir si el costo de sincronizar 512 MiB vale para el update.
///
/// El plazo sin datos lo trae el cliente inyectado (`read_timeout` y
/// `connect_timeout` de `cliente_de_descarga` en el shell): un servidor que
/// acepta la conexión y calla corta en vez de dejar esta función —y el
/// candado de actualización— colgados para siempre. No es un corte total:
/// mientras lleguen fragmentos, un paquete grande en una red lenta sigue
/// bajando (`internal/update/update.go:63`).
pub async fn download_con(
    http: &reqwest::Client,
    url: &str,
    want_sha: &str,
    dest: &Path,
    on_bytes: &mut (dyn FnMut(u64, Option<u64>) + Send),
) -> Result<(), Error> {
    if want_sha.is_empty() {
        return Err(Error::HuellaInvalida);
    }
    check_download_url(url)?;

    let mut respuesta = http.get(url).send().await?;
    if respuesta.status() != reqwest::StatusCode::OK {
        return Err(Error::Descarga(respuesta.status().as_u16()));
    }
    let total = respuesta.content_length();

    if let Some(padre) = dest.parent()
        && !padre.as_os_str().is_empty()
    {
        fs::create_dir_all(padre)?;
    }

    let _ = fs::remove_file(dest);
    let mut opciones = OpenOptions::new();
    opciones.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opciones.mode(0o755);
    }
    let mut archivo = opciones.open(dest)?;

    on_bytes(0, total);

    let mut hash = Sha256::new();
    let mut escritos = 0u64;
    loop {
        let fragmento = match respuesta.chunk().await {
            Ok(fragmento) => fragmento,
            Err(error) => {
                drop(archivo);
                let _ = fs::remove_file(dest);
                return Err(Error::Http(error));
            }
        };
        let Some(fragmento) = fragmento else {
            break;
        };
        let espacio = LIMITE_DESCARGA.saturating_sub(escritos as usize);
        if espacio == 0 {
            break;
        }
        let trozo = &fragmento[..fragmento.len().min(espacio)];
        if let Err(error) = archivo.write_all(trozo) {
            drop(archivo);
            let _ = fs::remove_file(dest);
            return Err(Error::Io(error));
        }
        hash.update(trozo);
        escritos += trozo.len() as u64;
        on_bytes(escritos, total);
    }
    drop(archivo);

    let obtenido = hex::encode(hash.finalize());
    if !obtenido.eq_ignore_ascii_case(want_sha) {
        let _ = fs::remove_file(dest);
        return Err(Error::DescargaNoCoincide);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use std::net::TcpListener;
    use std::time::Duration;

    use sha2::Digest as _;

    use super::{Error, download, download_con};

    /// Un servidor que acepta la conexión y calla no puede dejar la descarga
    /// colgada para siempre: el candado de actualización quedaría tomado
    /// hasta reiniciar la app (`arranque.rs`, `apply_update`).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn un_servidor_colgado_no_deja_la_descarga_colgada() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("puerto local");
        let direccion = listener.local_addr().expect("dirección local");
        let _colgado = std::thread::spawn(move || {
            let (_conexion, _) = listener.accept().expect("conexión entrante");
            // Acepta y no responde nunca: el cliente queda esperando.
            std::thread::sleep(Duration::from_secs(60));
        });
        let url = format!("http://{direccion}/paquete.exe");
        let temporal = tempfile::tempdir().expect("carpeta temporal");
        let dest = temporal.path().join("paquete.exe");
        let dest_clon = dest.clone();
        // La receta del cliente de descarga del shell (`cliente_de_descarga`),
        // con un plazo corto para la prueba: plazo sin datos, no corte total.
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_millis(300))
            .read_timeout(Duration::from_millis(300))
            .build()
            .expect("cliente con plazo");
        let sha = "a".repeat(64);

        let tarea = tokio::spawn(async move { download(&http, &url, &sha, &dest).await });
        std::thread::sleep(Duration::from_secs(2));
        assert!(
            tarea.is_finished(),
            "la descarga sigue colgada después de 2 s contra un servidor mudo"
        );
        let error = tarea
            .await
            .expect("la tarea de descarga murió")
            .expect_err("un servidor colgado tiene que dar error");
        assert!(
            matches!(&error, Error::Http(e) if e.is_timeout()),
            "se esperaba un error de plazo: {error}"
        );
        assert!(!dest_clon.exists(), "el archivo parcial quedó en disco");
    }

    /// El callback ve el total antes del primer byte y el acumulado crece
    /// hasta el tamaño completo; `download` sigue funcionando sin mirarlo.
    #[tokio::test]
    async fn download_con_avisa_el_avance() {
        let temporal = tempfile::tempdir().expect("carpeta temporal");
        let cuerpo = vec![b'x'; 64 << 10];
        let sha = hex::encode(sha2::Sha256::digest(&cuerpo));
        let url = servidor_de_un_solo_cuerpo(cuerpo.clone()).await;
        let dest = temporal.path().join("paquete");

        let mut avisos: Vec<(u64, Option<u64>)> = Vec::new();
        download_con(&reqwest::Client::new(), &url, &sha, &dest, &mut |r, t| {
            avisos.push((r, t))
        })
        .await
        .expect("descarga");

        assert_eq!(
            avisos.first().copied(),
            Some((0, Some(cuerpo.len() as u64))),
            "el primer aviso lleva el total y cero recibidos"
        );
        assert_eq!(
            avisos.last().copied(),
            Some((cuerpo.len() as u64, Some(cuerpo.len() as u64))),
            "el último aviso lleva todo lo descargado"
        );
        assert!(
            avisos.windows(2).all(|par| par[0].0 <= par[1].0),
            "los recibidos tienen que crecer: {avisos:?}"
        );
        assert_eq!(std::fs::read(&dest).expect("archivo"), cuerpo);
    }

    /// Servidor HTTP de una sola respuesta: `Content-Length` real y cierre.
    async fn servidor_de_un_solo_cuerpo(cuerpo: Vec<u8>) -> String {
        use std::io::{Read as _, Write as _};

        let listener = TcpListener::bind("127.0.0.1:0").expect("puerto local");
        let direccion = listener.local_addr().expect("dirección local");
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut peticion = [0u8; 1024];
                let _ = stream.read(&mut peticion);
                let cabecera = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    cuerpo.len()
                );
                let _ = stream.write_all(cabecera.as_bytes());
                let _ = stream.write_all(&cuerpo);
                let _ = stream.flush();
            }
        });
        format!("http://{direccion}/paquete")
    }
}
