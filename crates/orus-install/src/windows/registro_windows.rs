//! Backend de [`Registro`] sobre `HKCU`.
//!
//! Vive aparte de `registro.rs` para que la lógica pura no cargue con `unsafe`
//! y para que ningún archivo de producción pase de 400 líneas (R9).

use std::ptr;

use windows_sys::Win32::Foundation::{
    ERROR_FILE_NOT_FOUND, ERROR_MORE_DATA, ERROR_NO_MORE_ITEMS, ERROR_PATH_NOT_FOUND, ERROR_SUCCESS,
};
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_READ, KEY_WRITE, REG_DWORD, REG_EXPAND_SZ, REG_SZ, RegCloseKey,
    RegCreateKeyExW, RegDeleteKeyW, RegDeleteTreeW, RegDeleteValueW, RegEnumKeyExW, RegEnumValueW,
    RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
};

use crate::windows::Error;
use crate::windows::registro::{Registro, Valor};

pub struct RegistroWindows;

enum Contenido {
    Texto(Vec<u16>),
    Dword([u8; 4]),
}

impl Registro for RegistroWindows {
    fn crear(&self, clave: &str) -> Result<(), Error> {
        let hkey = abrir_clave(clave)?;
        // SAFETY: hkey salió de abrir_clave y no se cerró antes.
        unsafe { RegCloseKey(hkey) };
        Ok(())
    }

    fn escribir(&self, clave: &str, nombre: &str, valor: Valor) -> Result<(), Error> {
        let hkey = abrir_clave(clave)?;
        let nombre = wide(nombre);
        let contenido = match &valor {
            Valor::Texto(valor) => {
                Contenido::Texto(valor.encode_utf16().chain(std::iter::once(0)).collect())
            }
            Valor::Dword(valor) => Contenido::Dword(valor.to_le_bytes()),
        };
        let (tipo, datos, bytes) = match &contenido {
            Contenido::Texto(texto) => (
                REG_SZ,
                texto.as_ptr().cast::<u8>(),
                (texto.len() * 2) as u32,
            ),
            Contenido::Dword(dword) => (REG_DWORD, dword.as_ptr(), 4),
        };
        // SAFETY: nombre y datos viven hasta que RegSetValueExW retorna; bytes
        // es el tamaño real del buffer y su tipo corresponde.
        let estado = unsafe { RegSetValueExW(hkey, nombre.as_ptr(), 0, tipo, datos, bytes) };
        // SAFETY: hkey salió de abrir_clave y no se cerró antes.
        unsafe { RegCloseKey(hkey) };
        if estado != ERROR_SUCCESS {
            return Err(Error::Windows(estado));
        }
        Ok(())
    }

    fn leer_texto(&self, clave: &str, nombre: &str) -> Result<Option<String>, Error> {
        let clave = wide(clave);
        let mut hkey: HKEY = ptr::null_mut();
        // SAFETY: la clave termina en NUL y hkey es una salida válida.
        let estado =
            unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, clave.as_ptr(), 0, KEY_READ, &mut hkey) };
        if es_ausente(estado) {
            return Ok(None);
        }
        if estado != ERROR_SUCCESS {
            return Err(Error::Windows(estado));
        }

        let nombre = wide(nombre);
        let mut tipo = 0u32;
        let mut tam = 0u32;
        // SAFETY: nombre termina en NUL; la consulta sin datos devuelve el
        // tamaño necesario en tam.
        let estado = unsafe {
            RegQueryValueExW(
                hkey,
                nombre.as_ptr(),
                ptr::null(),
                &mut tipo,
                ptr::null_mut(),
                &mut tam,
            )
        };
        if es_ausente(estado) {
            // SAFETY: hkey sigue abierta.
            unsafe { RegCloseKey(hkey) };
            return Ok(None);
        }
        if estado != ERROR_SUCCESS {
            // SAFETY: hkey sigue abierta.
            unsafe { RegCloseKey(hkey) };
            return Err(Error::Windows(estado));
        }

        let mut datos = vec![0u8; tam as usize];
        // SAFETY: datos tiene tam bytes y nombre termina en NUL.
        let estado = unsafe {
            RegQueryValueExW(
                hkey,
                nombre.as_ptr(),
                ptr::null(),
                &mut tipo,
                datos.as_mut_ptr(),
                &mut tam,
            )
        };
        // SAFETY: hkey salió de RegOpenKeyExW y no se cerró antes.
        unsafe { RegCloseKey(hkey) };
        if estado != ERROR_SUCCESS {
            return Err(Error::Windows(estado));
        }
        // `User Shell Folders` guarda REG_EXPAND_SZ: se devuelve el texto
        // crudo y quien llama expande (`desde_registro` hace %USERPROFILE%).
        if !es_texto(tipo) {
            return Ok(None);
        }

        datos.truncate(tam as usize);
        Ok(Some(texto_registro(&datos)))
    }

    fn borrar_valor(&self, clave: &str, nombre: &str) -> Result<(), Error> {
        let clave = wide(clave);
        let mut hkey: HKEY = ptr::null_mut();
        // SAFETY: la clave termina en NUL y hkey es una salida válida.
        let estado =
            unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, clave.as_ptr(), 0, KEY_WRITE, &mut hkey) };
        if es_ausente(estado) {
            return Ok(());
        }
        if estado != ERROR_SUCCESS {
            return Err(Error::Windows(estado));
        }

        let nombre = wide(nombre);
        // SAFETY: nombre termina en NUL y hkey sigue abierta.
        let estado = unsafe { RegDeleteValueW(hkey, nombre.as_ptr()) };
        // SAFETY: hkey salió de RegOpenKeyExW y no se cerró antes.
        unsafe { RegCloseKey(hkey) };
        if estado != ERROR_SUCCESS && !es_ausente(estado) {
            return Err(Error::Windows(estado));
        }
        Ok(())
    }

    fn borrar(&self, clave: &str) -> Result<(), Error> {
        let clave = wide(clave);
        // SAFETY: la clave termina en NUL y vive durante la llamada.
        let estado = unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, clave.as_ptr()) };
        if estado != ERROR_SUCCESS && !es_ausente(estado) {
            return Err(Error::Windows(estado));
        }
        // RegDeleteTreeW borra los valores y las subclaves, no la clave.
        // SAFETY: la clave termina en NUL y vive durante la llamada.
        let estado = unsafe { RegDeleteKeyW(HKEY_CURRENT_USER, clave.as_ptr()) };
        if estado != ERROR_SUCCESS && !es_ausente(estado) {
            return Err(Error::Windows(estado));
        }
        Ok(())
    }

    fn subclaves(&self, clave: &str) -> Result<Vec<String>, Error> {
        let clave = wide(clave);
        let mut hkey: HKEY = ptr::null_mut();
        // SAFETY: la clave termina en NUL y hkey es una salida válida.
        let estado =
            unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, clave.as_ptr(), 0, KEY_READ, &mut hkey) };
        if es_ausente(estado) {
            return Ok(Vec::new());
        }
        if estado != ERROR_SUCCESS {
            return Err(Error::Windows(estado));
        }

        let mut nombres = Vec::new();
        let mut indice = 0u32;
        loop {
            let mut buffer = [0u16; 256];
            let mut largo = buffer.len() as u32;
            // SAFETY: buffer tiene capacidad para largo caracteres; los
            // punteros opcionales van en NULL y hkey sigue abierta.
            let estado = unsafe {
                RegEnumKeyExW(
                    hkey,
                    indice,
                    buffer.as_mut_ptr(),
                    &mut largo,
                    ptr::null(),
                    ptr::null_mut(),
                    ptr::null_mut(),
                    ptr::null_mut(),
                )
            };
            if estado == ERROR_NO_MORE_ITEMS {
                break;
            }
            if estado != ERROR_SUCCESS {
                // SAFETY: hkey sigue abierta.
                unsafe { RegCloseKey(hkey) };
                return Err(Error::Windows(estado));
            }
            nombres.push(String::from_utf16_lossy(&buffer[..largo as usize]));
            indice += 1;
        }
        // SAFETY: hkey salió de RegOpenKeyExW y no se cerró antes.
        unsafe { RegCloseKey(hkey) };
        Ok(nombres)
    }

    fn valores(&self, clave: &str) -> Result<Vec<(String, String)>, Error> {
        let clave = wide(clave);
        let mut hkey: HKEY = ptr::null_mut();
        // SAFETY: la clave termina en NUL y hkey es una salida válida.
        let estado =
            unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, clave.as_ptr(), 0, KEY_READ, &mut hkey) };
        if es_ausente(estado) {
            return Ok(Vec::new());
        }
        if estado != ERROR_SUCCESS {
            return Err(Error::Windows(estado));
        }

        let mut valores = Vec::new();
        let mut nombre = vec![0u16; 256];
        let mut indice = 0u32;
        loop {
            let mut largo_nombre = nombre.len() as u32;
            let mut tipo = 0u32;
            let mut tam = 0u32;
            // SAFETY: nombre tiene capacidad para largo_nombre caracteres; la
            // consulta sin datos devuelve el tamaño real en tam.
            let estado = unsafe {
                RegEnumValueW(
                    hkey,
                    indice,
                    nombre.as_mut_ptr(),
                    &mut largo_nombre,
                    ptr::null(),
                    &mut tipo,
                    ptr::null_mut(),
                    &mut tam,
                )
            };
            if estado == ERROR_MORE_DATA {
                // Otro programa puso un nombre de valor más largo que 255:
                // crecer y reintentar en vez de dar el autostart por apagado.
                let capacidad = capacidad_nombre(nombre.len(), largo_nombre);
                nombre.resize(capacidad, 0);
                continue;
            }
            if estado == ERROR_NO_MORE_ITEMS {
                break;
            }
            if estado != ERROR_SUCCESS {
                // SAFETY: hkey sigue abierta.
                unsafe { RegCloseKey(hkey) };
                return Err(Error::Windows(estado));
            }
            if es_texto(tipo) {
                let mut datos = vec![0u8; tam as usize];
                let mut leidos = tam;
                largo_nombre = nombre.len() as u32;
                // SAFETY: nombre y datos viven durante la llamada; leidos es
                // el tamaño real del buffer y hkey sigue abierta.
                let estado = unsafe {
                    RegEnumValueW(
                        hkey,
                        indice,
                        nombre.as_mut_ptr(),
                        &mut largo_nombre,
                        ptr::null(),
                        &mut tipo,
                        datos.as_mut_ptr(),
                        &mut leidos,
                    )
                };
                if estado != ERROR_SUCCESS {
                    // SAFETY: hkey sigue abierta.
                    unsafe { RegCloseKey(hkey) };
                    return Err(Error::Windows(estado));
                }
                if let Some((nombre, texto)) = entrada_texto(
                    &nombre,
                    largo_nombre as usize,
                    tipo,
                    &datos[..leidos as usize],
                ) {
                    valores.push((nombre, texto));
                }
            }
            indice += 1;
        }
        // SAFETY: hkey salió de RegOpenKeyExW y no se cerró antes.
        unsafe { RegCloseKey(hkey) };
        Ok(valores)
    }
}

/// Abre (o crea) una clave de `HKCU` con permiso de escritura.
fn abrir_clave(clave: &str) -> Result<HKEY, Error> {
    let clave = wide(clave);
    let mut hkey: HKEY = ptr::null_mut();
    // SAFETY: la clave termina en NUL y hkey es una salida válida; los
    // atributos opcionales van en NULL.
    let estado = unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            clave.as_ptr(),
            0,
            ptr::null(),
            0,
            KEY_WRITE,
            ptr::null(),
            &mut hkey,
            ptr::null_mut(),
        )
    };
    if estado != ERROR_SUCCESS {
        return Err(Error::Windows(estado));
    }
    Ok(hkey)
}

/// `REG_SZ` y `REG_EXPAND_SZ` son texto; el resto de tipos se ignoran.
fn es_texto(tipo: u32) -> bool {
    tipo == REG_SZ || tipo == REG_EXPAND_SZ
}

/// Convierte una entrada enumerada en el par (nombre, texto) que espera
/// `autostart_activo`. Los tipos que no son texto no cuentan como entrada.
fn entrada_texto(
    nombre: &[u16],
    largo: usize,
    tipo: u32,
    datos: &[u8],
) -> Option<(String, String)> {
    if !es_texto(tipo) {
        return None;
    }
    Some((
        String::from_utf16_lossy(&nombre[..largo]),
        texto_registro(datos),
    ))
}

/// Capacidad del buffer de nombre cuando Windows devuelve ERROR_MORE_DATA.
fn capacidad_nombre(actual: usize, requerido: u32) -> usize {
    (requerido as usize)
        .saturating_add(1)
        .max(actual.saturating_mul(2))
}

/// Texto de un valor `REG_SZ`; corta en el NUL final.
fn texto_registro(datos: &[u8]) -> String {
    let unidades: Vec<u16> = datos
        .as_chunks::<2>()
        .0
        .iter()
        .map(|par| u16::from_le_bytes(*par))
        .take_while(|unidad| *unidad != 0)
        .collect();
    String::from_utf16_lossy(&unidades)
}

fn es_ausente(estado: u32) -> bool {
    estado == ERROR_FILE_NOT_FOUND || estado == ERROR_PATH_NOT_FOUND
}

fn wide(valor: &str) -> Vec<u16> {
    valor.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(all(test, windows))]
mod tests {
    use windows_sys::Win32::System::Registry::{REG_DWORD, REG_EXPAND_SZ, REG_SZ};

    use super::{capacidad_nombre, entrada_texto};

    #[test]
    fn un_valor_expand_sz_cuenta_como_texto() {
        let nombre: Vec<u16> = "oruscribe-agente".encode_utf16().collect();
        let datos: Vec<u8> = "C:\\Programs\\Oruscribe.exe\0"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        let esperado = Some((
            "oruscribe-agente".to_owned(),
            "C:\\Programs\\Oruscribe.exe".to_owned(),
        ));

        assert_eq!(
            entrada_texto(&nombre, nombre.len(), REG_SZ, &datos),
            esperado
        );
        assert_eq!(
            entrada_texto(&nombre, nombre.len(), REG_EXPAND_SZ, &datos),
            esperado
        );
        assert_eq!(
            entrada_texto(&nombre, nombre.len(), REG_DWORD, &datos),
            None
        );
    }

    #[test]
    fn un_nombre_mas_largo_que_el_buffer_hace_crecer_la_capacidad() {
        assert_eq!(capacidad_nombre(256, 600), 601);
        assert!(capacidad_nombre(256, 10) > 256);
    }
}
