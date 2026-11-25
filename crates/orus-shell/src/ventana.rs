//! Ventana principal con el WebView de la UI.
//!
//! Medidas y título son los de Go (`main.go:241-249`): 1180×760, mínimo
//! 960×620. Cerrar la ventana no destruye el WebView: la esconde, y la salida
//! real la decide la bandeja.

use tao::dpi::LogicalSize;
use tao::event_loop::EventLoopWindowTarget;
use tao::window::{Window, WindowBuilder};
#[cfg(any(not(target_os = "linux"), test))]
use wry::{PermissionKind, PermissionResponse};
use wry::{WebView, WebViewBuilder};

use crate::bandeja::Aviso;

/// Título de la ventana (`main.go:242`).
pub const TITULO: &str = "Oruscribe Agente";
/// Ancho inicial (`main.go:243`).
pub const ANCHO: f64 = 1180.0;
/// Alto inicial (`main.go:244`).
pub const ALTO: f64 = 760.0;
/// Ancho mínimo (`main.go:245`).
pub const ANCHO_MINIMO: f64 = 960.0;
/// Alto mínimo (`main.go:246`).
pub const ALTO_MINIMO: f64 = 620.0;

/// La ventana y su contenido; el WebView se conserva para que no se destruya
/// al ocultar la ventana.
pub struct Ventana {
    ventana: Window,
    _webview: WebView,
}

impl Ventana {
    /// Crea la ventana y carga `url`, que ya trae el token del servidor local.
    pub fn crear(
        evento: &EventLoopWindowTarget<Aviso>,
        url: &str,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let ventana = WindowBuilder::new()
            .with_title(TITULO)
            .with_inner_size(LogicalSize::new(ANCHO, ALTO))
            .with_min_inner_size(LogicalSize::new(ANCHO_MINIMO, ALTO_MINIMO))
            .build(evento)?;
        // La app no navega fuera de su propio origen. Un enlace del chat abre
        // el navegador del sistema; así la cookie local no viaja a otra página
        // y la ventana no puede quedar convertida en un navegador cualquiera.
        let origen = url.to_owned();
        let builder = WebViewBuilder::new()
            .with_background_color((9, 9, 9, 255))
            .with_navigation_handler(move |destino| {
                if crate::navegador::navegacion_permitida(&origen, &destino) {
                    return true;
                }
                let _ = crate::navegador::abrir(&destino);
                false
            })
            .with_new_window_req_handler(|destino, _| {
                let _ = crate::navegador::abrir(&destino);
                wry::NewWindowResponse::Deny
            });

        #[cfg(windows)]
        let webview = builder
            .with_permission_handler(permiso_de_media)
            .with_url(url)
            .build(&ventana)?;

        // En Linux WebKitGTK se monta dentro del contenedor GTK de tao; `build`
        // solo toma X11 y dejaría la ventana en blanco bajo Wayland. La ventana
        // ya tiene un `GtkBox` como único hijo: montar sobre la ventana hace
        // que GTK rechace el WebView y quede el fondo del tema.
        //
        // Además, activamos media-stream/webaudio y conectamos `permission-request`
        // (permitiendo micrófono y `DeviceInfoPermissionRequest`, que `wry` mapea
        // a `Other`) ANTES de cargar la URL para evitar carreras en WebKitGTK.
        #[cfg(not(windows))]
        let webview = {
            use tao::platform::unix::WindowExtUnix;
            use wry::WebViewBuilderExtUnix;

            let vbox = ventana.default_vbox().ok_or_else(|| {
                std::io::Error::other(
                    "la ventana de tao no trajo su GtkBox: no hay dónde montar el WebView",
                )
            })?;
            #[cfg(target_os = "linux")]
            let webview = {
                use webkit2gtk::glib::{Cast as _, ObjectExt as _};
                use webkit2gtk::{
                    DeviceInfoPermissionRequest, PermissionRequestExt as _, SettingsExt as _,
                    UserMediaPermissionRequest, UserMediaPermissionRequestExt as _,
                    WebViewExt as _,
                };
                use wry::WebViewExtUnix as _;

                let webview = builder.build_gtk(vbox)?;
                let wk = webview.webview();
                if let Some(ajustes) = wk.settings() {
                    ajustes.set_enable_media_stream(true);
                    ajustes.set_enable_webaudio(true);
                    ajustes.set_media_playback_requires_user_gesture(false);
                }
                wk.connect_permission_request(|_, req| {
                    if let Some(media) = req.downcast_ref::<UserMediaPermissionRequest>() {
                        if media.is_for_audio_device() && !media.is_for_video_device() {
                            req.allow();
                        } else {
                            req.deny();
                        }
                        return true;
                    }
                    if req.is::<DeviceInfoPermissionRequest>() {
                        req.allow();
                        return true;
                    }
                    req.deny();
                    true
                });
                webview.load_url(url)?;
                webview
            };
            #[cfg(not(target_os = "linux"))]
            let webview = builder
                .with_permission_handler(permiso_de_media)
                .with_url(url)
                .build_gtk(vbox)?;
            webview
        };
        Ok(Self {
            ventana,
            _webview: webview,
        })
    }

    /// La muestra, la restaura si estaba minimizada y le da el foco, como el
    /// «Abrir» de la bandeja de Go.
    pub fn mostrar(&self) {
        self.ventana.set_minimized(false);
        self.ventana.set_visible(true);
        self.ventana.set_focus();
    }

    /// Esconde la ventana sin cerrar el proceso.
    pub fn ocultar(&self) {
        self.ventana.set_visible(false);
    }
}

#[cfg(any(not(target_os = "linux"), test))]
fn permiso_de_media(kind: PermissionKind) -> PermissionResponse {
    match kind {
        PermissionKind::Microphone => PermissionResponse::Allow,
        _ => PermissionResponse::Deny,
    }
}

#[cfg(test)]
mod tests {
    use super::permiso_de_media;
    use wry::{PermissionKind, PermissionResponse};

    #[test]
    fn solo_el_microfono_se_concede() {
        assert_eq!(
            permiso_de_media(PermissionKind::Microphone),
            PermissionResponse::Allow
        );
        for negado in [
            PermissionKind::Camera,
            PermissionKind::Geolocation,
            PermissionKind::Other,
        ] {
            assert_eq!(permiso_de_media(negado), PermissionResponse::Deny);
        }
    }
}
