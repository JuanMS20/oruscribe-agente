//! Términos de uso que la pantalla de instalación muestra antes de tocar nada.
//!
//! `TERMS_VERSION` cambia cuando
//! cambia el texto: la instalación guarda la versión aceptada, así un texto
//! nuevo se vuelve a mostrar en vez de darse por aceptado.

/// Versión de los términos que acepta la instalación vigente.
pub const TERMS_VERSION: &str = "2026-11-2";

/// Título que aparece encima del texto.
pub const TERMS_TITLE: &str = "Términos de uso";

/// Texto de los términos. Se arma con `concat!` para que los saltos de línea
/// sean `\n` también en un checkout con CRLF: el texto del contrato no lleva `\r`.
pub const TERMS_BODY: &str = concat!(
    "Oruscribe Agente · versión de prueba para la organización\n",
    "\n",
    "Qué hace este programa\n",
    "Se conecta a tu cuenta de Oruscribe para mostrarte tus tareas, el chat de tu equipo, tus avisos y tus notas. Cuando algo pasa, te avisa.\n",
    "\n",
    "Qué guarda en este equipo\n",
    "Tu sesión, para no pedirte la contraseña cada vez. Tus notas. La configuración de idioma y arranque. Nada de eso sale de tu computador.\n",
    "\n",
    "Qué no hace\n",
    "No lee tus archivos personales. No ve tu contraseña: el inicio de sesión ocurre en tu navegador. No envía mensajes sin que tú los escribas.\n",
    "\n",
    "Sobre los errores técnicos\n",
    "El reporte de errores está apagado por defecto. Si lo activas en Ajustes, se envían solo fallos técnicos con rutas y secretos tachados; nunca tus mensajes ni el contenido de tus archivos. Puedes apagarlo cuando quieras.\n",
    "\n",
    "Dónde queda\n",
    "El programa y sus accesos directos quedan en tu carpeta de usuario, sin permisos de administrador. Puedes desinstalarlo desde Configuración o desde Agregar o quitar programas.\n",
    "\n",
    "Sobre esta versión\n",
    "Es una versión de prueba. Puede fallar y puede cambiar. Cuéntanos qué se rompe en la sala del equipo.",
);
