//! Selector de carpetas nativo de Windows para la costura `PickFolder`.
//!
//! `IFileDialog` exige un apartamento COM en el hilo que muestra el diálogo; el
//! borde lo llama desde un handler async, así que se muestra en
//! `block_in_place` (runtime multi-hilo) o en un hilo propio para no frenar el
//! resto de las tareas. `Show` es modal y no se ejercita en un test; lo que sí
//! se prueba es crear el diálogo por COM y tocar sus opciones (`crear_dialogo`),
//! que es donde un IID mal puesto o un slot fuera de orden de la vtable revienta.

use std::ffi::c_void;
use std::ptr;

use windows_sys::Win32::Foundation::{HWND, RPC_E_CHANGED_MODE, S_FALSE, S_OK};
use windows_sys::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
    CoTaskMemFree, CoUninitialize,
};
use windows_sys::Win32::UI::Shell::{
    FILEOPENDIALOGOPTIONS, FOS_FORCEFILESYSTEM, FOS_PATHMUSTEXIST, FOS_PICKFOLDERS, FileOpenDialog,
    SIGDN, SIGDN_FILESYSPATH,
};
use windows_sys::Win32::UI::WindowsAndMessaging::GetForegroundWindow;
use windows_sys::core::{GUID, HRESULT, IUnknown_Vtbl, PWSTR};

/// `HRESULT_FROM_WIN32(ERROR_CANCELLED)`: lo devuelve `Show` al cancelar.
const CANCELADO: HRESULT = 0x8007_04C7_u32 as _;

const IID_IFILEOPENDIALOG: GUID = GUID::from_u128(0xd57c7288_d4ad_4768_be02_9d969532d960);

/// La costura real del binario publicado: abre el selector nativo de carpetas.
pub struct SelectorCarpeta;

impl orus_api::PickFolder for SelectorCarpeta {
    fn elegir(&self) -> Result<Option<String>, String> {
        let trabajo = abrir_dialogo;
        match tokio::runtime::Handle::try_current() {
            Ok(handle) if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
                tokio::task::block_in_place(trabajo)
            }
            _ => std::thread::spawn(trabajo)
                .join()
                .map_err(|_| "el selector de carpetas se cortó".to_owned())?,
        }
    }
}

/// El prefijo común de toda interfaz COM. Los métodos propios de `IFileDialog`
/// van después, en el mismo orden que `shobjidl_core.h`; los que no se usan
/// quedan como huecos para no copiar firmas que nadie llama.
#[repr(C)]
struct VtblDialogo {
    base: IUnknown_Vtbl,
    show: unsafe extern "system" fn(*mut c_void, HWND) -> HRESULT,
    set_file_types: *const c_void,
    set_file_type_index: *const c_void,
    get_file_type_index: *const c_void,
    advise: *const c_void,
    unadvise: *const c_void,
    set_options: unsafe extern "system" fn(*mut c_void, FILEOPENDIALOGOPTIONS) -> HRESULT,
    get_options: unsafe extern "system" fn(*mut c_void, *mut FILEOPENDIALOGOPTIONS) -> HRESULT,
    set_default_folder: *const c_void,
    set_folder: *const c_void,
    get_folder: *const c_void,
    get_current_selection: *const c_void,
    set_file_name: *const c_void,
    get_file_name: *const c_void,
    set_title: *const c_void,
    set_ok_button_label: *const c_void,
    set_file_name_label: *const c_void,
    get_result: unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> HRESULT,
    add_place: *const c_void,
    set_default_extension: *const c_void,
    close: *const c_void,
    set_client_guid: *const c_void,
    clear_client_data: *const c_void,
    set_filter: *const c_void,
}

/// `IShellItem`: solo se usa `GetDisplayName`, cuarto método propio.
#[repr(C)]
struct VtblItemShell {
    base: IUnknown_Vtbl,
    bind_to_handler: *const c_void,
    get_parent: *const c_void,
    get_display_name: unsafe extern "system" fn(*mut c_void, SIGDN, *mut PWSTR) -> HRESULT,
}

struct Apartamento;

impl Apartamento {
    fn nuevo() -> Result<Self, String> {
        let hr = unsafe { CoInitializeEx(ptr::null(), COINIT_APARTMENTTHREADED as u32) };
        if hr == S_OK || hr == S_FALSE {
            Ok(Self)
        } else if hr == RPC_E_CHANGED_MODE {
            Err("el hilo del selector ya usa COM en otro modo".to_owned())
        } else {
            Err(format!(
                "no se pudo inicializar COM para el selector (0x{:08X})",
                hr as u32
            ))
        }
    }
}

impl Drop for Apartamento {
    fn drop(&mut self) {
        unsafe { CoUninitialize() };
    }
}

struct Dialogo(*mut c_void);

impl Drop for Dialogo {
    fn drop(&mut self) {
        if !self.0.is_null() {
            liberar(self.0);
        }
    }
}

struct ItemShell(*mut c_void);

impl Drop for ItemShell {
    fn drop(&mut self) {
        if !self.0.is_null() {
            liberar(self.0);
        }
    }
}

fn liberar(objeto: *mut c_void) {
    unsafe {
        let vt = &**(objeto.cast::<*const IUnknown_Vtbl>());
        (vt.Release)(objeto);
    }
}

fn vtable_dialogo(dialogo: *mut c_void) -> &'static VtblDialogo {
    unsafe { &**(dialogo.cast::<*const VtblDialogo>()) }
}

fn vtable_item(item: *mut c_void) -> &'static VtblItemShell {
    unsafe { &**(item.cast::<*const VtblItemShell>()) }
}

fn abrir_dialogo() -> Result<Option<String>, String> {
    let _apartamento = Apartamento::nuevo()?;
    let dialogo = crear_dialogo()?;
    mostrar(&dialogo)
}

/// Crea el diálogo por COM y deja fijadas las opciones de carpeta. No llama a
/// `Show`; el apartamento COM lo abre quien llama (`abrir_dialogo` o el test).
fn crear_dialogo() -> Result<Dialogo, String> {
    let mut puntero: *mut c_void = ptr::null_mut();
    let hr = unsafe {
        CoCreateInstance(
            &FileOpenDialog,
            ptr::null_mut(),
            CLSCTX_INPROC_SERVER,
            &IID_IFILEOPENDIALOG,
            &mut puntero,
        )
    };
    if hr < 0 {
        return Err(format!(
            "no se pudo crear el diálogo de carpetas (0x{:08X})",
            hr as u32
        ));
    }
    let dialogo = Dialogo(puntero);
    let vt = vtable_dialogo(dialogo.0);

    let mut opciones: FILEOPENDIALOGOPTIONS = 0;
    let hr = unsafe { (vt.get_options)(dialogo.0, &mut opciones) };
    if hr < 0 {
        return Err(format!(
            "no se pudieron leer las opciones del diálogo (0x{:08X})",
            hr as u32
        ));
    }
    let opciones = opciones | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM | FOS_PATHMUSTEXIST;
    let hr = unsafe { (vt.set_options)(dialogo.0, opciones) };
    if hr < 0 {
        return Err(format!(
            "no se pudieron fijar las opciones del diálogo (0x{:08X})",
            hr as u32
        ));
    }

    Ok(dialogo)
}

/// Muestra el diálogo ya creado y devuelve la carpeta elegida. `Ok(None)` es el
/// usuario que canceló: no es un fallo y el borde responde 200 sin `path`.
fn mostrar(dialogo: &Dialogo) -> Result<Option<String>, String> {
    let vt = vtable_dialogo(dialogo.0);

    let dueno: HWND = unsafe { GetForegroundWindow() };
    let hr = unsafe { (vt.show)(dialogo.0, dueno) };
    if hr == CANCELADO {
        return Ok(None);
    }
    if hr < 0 {
        return Err(format!(
            "el diálogo de carpetas falló (0x{:08X})",
            hr as u32
        ));
    }

    let mut puntero_item: *mut c_void = ptr::null_mut();
    let hr = unsafe { (vt.get_result)(dialogo.0, &mut puntero_item) };
    if hr < 0 || puntero_item.is_null() {
        return Err(format!(
            "el diálogo no devolvió una carpeta (0x{:08X})",
            hr as u32
        ));
    }
    let item = ItemShell(puntero_item);
    let vt_item = vtable_item(item.0);

    let mut nombre: PWSTR = ptr::null_mut();
    let hr = unsafe { (vt_item.get_display_name)(item.0, SIGDN_FILESYSPATH, &mut nombre) };
    if hr < 0 || nombre.is_null() {
        return Err(format!(
            "no se pudo leer la ruta de la carpeta (0x{:08X})",
            hr as u32
        ));
    }
    let largo = unsafe {
        let mut largo = 0usize;
        while *nombre.add(largo) != 0 {
            largo += 1;
        }
        largo
    };
    let ruta = unsafe { String::from_utf16_lossy(std::slice::from_raw_parts(nombre, largo)) };
    unsafe { CoTaskMemFree(nombre.cast()) };
    Ok(Some(ruta))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    /// Ejercita `CoCreateInstance` con el `IID_IFILEOPENDIALOG`, los dos slots
    /// de opciones de la vtable y el `Release` del diálogo. No llama a `Show`,
    /// así que no abre ventana. Si esta máquina no deja crear el objeto COM, el
    /// test falla con el HRESULT: no se salta en silencio.
    #[test]
    fn crear_el_dialogo_y_fijar_las_opciones() {
        let _apartamento = Apartamento::nuevo().expect("apartamento COM del hilo");
        let dialogo = crear_dialogo().expect("CoCreateInstance del selector de carpetas");

        let vt = vtable_dialogo(dialogo.0);
        let mut opciones: FILEOPENDIALOGOPTIONS = 0;
        let hr = unsafe { (vt.get_options)(dialogo.0, &mut opciones) };
        assert!(hr >= 0, "GetOptions devolvió 0x{:08X}", hr as u32);
        let pedidas = FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM | FOS_PATHMUSTEXIST;
        assert_eq!(
            opciones & pedidas,
            pedidas,
            "SetOptions no dejó las opciones pedidas (0x{opciones:08X})"
        );

        drop(dialogo);
    }
}
