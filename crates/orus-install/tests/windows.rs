//! Tests de la instalación Windows sin tocar el `HKCU` real ni el perfil.
//!
//! La carpeta y el
//! binario se inyectan, el registro es en memoria y los accesos van a una
//! carpeta temporal.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use orus_install::windows::{
    CLAVE_DESINSTALACION, CLAVE_DESINSTALACION_GO, CLAVE_INICIO, CreadorAccesos, Error, NOMBRE_APP,
    NOMBRE_EXE, Registro, RutasAccesos, VALOR_AUTOSTART, Valor, activar_autostart,
    actualizar_version, argumentos_desinstalacion, autostart_activo, desactivar_autostart,
    entradas_oruscribe, escribir_entrada, guion, instalar, preparar_desinstalacion,
};
use orus_install::{Platform, Rutas};

type Valores = BTreeMap<String, Valor>;
type Claves = BTreeMap<String, Valores>;

// Tipos de valor de Windows (Win32): la fixture los modela porque el backend
// real solo devuelve texto de `REG_SZ` y `REG_EXPAND_SZ`, no de cualquier tipo.
const REG_SZ: u32 = 1;
const REG_EXPAND_SZ: u32 = 2;
const REG_DWORD: u32 = 4;

#[derive(Default)]
struct RegistroMemoria {
    claves: Mutex<Claves>,
    tipos: Mutex<BTreeMap<(String, String), u32>>,
    falla_borrar: Mutex<bool>,
}

impl RegistroMemoria {
    fn fallar_al_borrar(&self) {
        *self.falla_borrar.lock().expect("candado del registro") = true;
    }

    /// Escribe un valor con su tipo de registro, como lo guarda Windows.
    fn escribir_con_tipo(&self, clave: &str, nombre: &str, valor: Valor, tipo: u32) {
        self.escribir(clave, nombre, valor).expect("escribir valor");
        self.tipos
            .lock()
            .expect("candado del registro")
            .insert((clave.to_owned(), nombre.to_owned()), tipo);
    }

    fn valor(&self, clave: &str, nombre: &str) -> Option<Valor> {
        self.claves
            .lock()
            .expect("candado del registro")
            .get(clave)
            .and_then(|valores| valores.get(nombre))
            .cloned()
    }

    fn existe(&self, clave: &str) -> bool {
        self.claves
            .lock()
            .expect("candado del registro")
            .contains_key(clave)
    }
}

impl Registro for RegistroMemoria {
    fn crear(&self, clave: &str) -> Result<(), Error> {
        self.claves
            .lock()
            .expect("candado del registro")
            .entry(clave.to_owned())
            .or_default();
        Ok(())
    }

    fn escribir(&self, clave: &str, nombre: &str, valor: Valor) -> Result<(), Error> {
        self.claves
            .lock()
            .expect("candado del registro")
            .entry(clave.to_owned())
            .or_default()
            .insert(nombre.to_owned(), valor);
        Ok(())
    }

    fn leer_texto(&self, clave: &str, nombre: &str) -> Result<Option<String>, Error> {
        // El backend real (`RegistroWindows`) decide por el tipo del valor;
        // la fixture replica su regla. Sin tipo, `escribir` guarda `REG_SZ`.
        let tipo = self
            .tipos
            .lock()
            .expect("candado del registro")
            .get(&(clave.to_owned(), nombre.to_owned()))
            .copied()
            .unwrap_or(REG_SZ);
        if tipo != REG_SZ && tipo != REG_EXPAND_SZ {
            return Ok(None);
        }
        Ok(match self.valor(clave, nombre) {
            Some(Valor::Texto(texto)) => Some(texto),
            _ => None,
        })
    }

    fn borrar_valor(&self, clave: &str, nombre: &str) -> Result<(), Error> {
        if let Some(valores) = self
            .claves
            .lock()
            .expect("candado del registro")
            .get_mut(clave)
        {
            valores.remove(nombre);
        }
        Ok(())
    }

    fn borrar(&self, clave: &str) -> Result<(), Error> {
        if *self.falla_borrar.lock().expect("candado del registro") {
            return Err(Error::Windows(5));
        }
        let prefijo = format!("{clave}\\");
        self.claves
            .lock()
            .expect("candado del registro")
            .retain(|nombre, _| nombre.as_str() != clave && !nombre.starts_with(prefijo.as_str()));
        Ok(())
    }

    fn subclaves(&self, clave: &str) -> Result<Vec<String>, Error> {
        let prefijo = format!("{clave}\\");
        let claves = self.claves.lock().expect("candado del registro");
        let mut hijos = BTreeSet::new();
        for nombre in claves.keys() {
            let Some(resto) = nombre.strip_prefix(prefijo.as_str()) else {
                continue;
            };
            let Some(hijo) = resto.split('\\').next() else {
                continue;
            };
            hijos.insert(hijo.to_owned());
        }
        Ok(hijos.into_iter().collect())
    }

    fn valores(&self, clave: &str) -> Result<Vec<(String, String)>, Error> {
        let claves = self.claves.lock().expect("candado del registro");
        let mut valores = Vec::new();
        if let Some(entradas) = claves.get(clave) {
            for (nombre, valor) in entradas {
                if let Valor::Texto(texto) = valor {
                    valores.push((nombre.clone(), texto.clone()));
                }
            }
        }
        Ok(valores)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Llamada {
    lnk: PathBuf,
    destino: PathBuf,
    trabajo: PathBuf,
}

#[derive(Default)]
struct CreadorMemoria {
    llamadas: Mutex<Vec<Llamada>>,
}

impl CreadorMemoria {
    fn llamadas(&self) -> Vec<Llamada> {
        self.llamadas.lock().expect("candado del creador").clone()
    }
}

impl CreadorAccesos for CreadorMemoria {
    fn crear(&self, lnk: &Path, destino: &Path, trabajo: &Path) -> Result<(), String> {
        self.llamadas
            .lock()
            .expect("candado del creador")
            .push(Llamada {
                lnk: lnk.to_path_buf(),
                destino: destino.to_path_buf(),
                trabajo: trabajo.to_path_buf(),
            });
        Ok(())
    }
}

/// Registro mínimo que falla en el paso que pida la prueba.
struct RegistroQueFalla {
    falla_crear: bool,
    falla_escribir: bool,
}

impl Registro for RegistroQueFalla {
    fn crear(&self, _clave: &str) -> Result<(), Error> {
        if self.falla_crear {
            Err(Error::Windows(5))
        } else {
            Ok(())
        }
    }

    fn escribir(&self, _clave: &str, _nombre: &str, _valor: Valor) -> Result<(), Error> {
        if self.falla_escribir {
            Err(Error::Windows(5))
        } else {
            Ok(())
        }
    }

    fn leer_texto(&self, _clave: &str, _nombre: &str) -> Result<Option<String>, Error> {
        Ok(None)
    }

    fn borrar_valor(&self, _clave: &str, _nombre: &str) -> Result<(), Error> {
        Ok(())
    }

    fn borrar(&self, _clave: &str) -> Result<(), Error> {
        Ok(())
    }

    fn subclaves(&self, _clave: &str) -> Result<Vec<String>, Error> {
        Ok(Vec::new())
    }

    fn valores(&self, _clave: &str) -> Result<Vec<(String, String)>, Error> {
        Ok(Vec::new())
    }
}

fn con_sufijo(ruta: &Path, sufijo: &str) -> PathBuf {
    let mut texto = ruta.as_os_str().to_owned();
    texto.push(sufijo);
    PathBuf::from(texto)
}

fn de_prueba(base: &Path) -> (Rutas, RutasAccesos) {
    (
        Rutas::new(Platform::Windows, base.join("local")),
        RutasAccesos::new(base.join("inicio"), base.join("escritorio")),
    )
}

// D201 y el contrato de nombres: una sola clave canónica, la de NSIS.
#[test]
fn claves_y_nombres_del_registro() {
    assert_eq!(
        CLAVE_DESINSTALACION,
        r"Software\Microsoft\Windows\CurrentVersion\Uninstall\Oruscribe Agente"
    );
    assert_eq!(
        CLAVE_DESINSTALACION_GO,
        r"Software\Microsoft\Windows\CurrentVersion\Uninstall\OruscribeAgente"
    );
    assert_eq!(
        CLAVE_INICIO,
        r"Software\Microsoft\Windows\CurrentVersion\Run"
    );
    assert_eq!(VALOR_AUTOSTART, "oruscribe-agente");
    assert_eq!(NOMBRE_APP, "Oruscribe Agente");
    assert_eq!(NOMBRE_EXE, "Oruscribe Agente.exe");

    let temp = tempfile::tempdir().expect("carpeta temporal");
    let rutas = Rutas::new(Platform::Windows, temp.path());
    assert_eq!(
        rutas.exe(),
        temp.path()
            .join("Programs")
            .join("Oruscribe")
            .join("Oruscribe Agente.exe")
    );
}

// Copia atómica, registro y dos accesos.
#[test]
fn instalar_copia_registra_y_crea_los_dos_accesos() {
    let temp = tempfile::tempdir().expect("carpeta temporal");
    let (rutas, accesos) = de_prueba(temp.path());
    let registro = RegistroMemoria::default();
    let creador = CreadorMemoria::default();
    let origen = temp.path().join("descarga.exe");
    fs::write(&origen, b"binario nuevo").expect("binario de origen");

    instalar(&rutas, &accesos, &registro, &creador, &origen, "0.6.0").expect("instalar");

    assert_eq!(
        fs::read(rutas.exe()).expect("exe instalado"),
        b"binario nuevo".to_vec()
    );
    assert!(!con_sufijo(&rutas.exe(), ".nuevo").exists());

    let exe = rutas.exe().display().to_string();
    let carpeta = rutas.dir().display().to_string();
    assert_eq!(
        registro.valor(CLAVE_DESINSTALACION, "DisplayName"),
        Some(Valor::Texto("Oruscribe Agente".to_owned()))
    );
    assert_eq!(
        registro.valor(CLAVE_DESINSTALACION, "DisplayVersion"),
        Some(Valor::Texto("0.6.0".to_owned()))
    );
    assert_eq!(
        registro.valor(CLAVE_DESINSTALACION, "Publisher"),
        Some(Valor::Texto("Oruscribe".to_owned()))
    );
    assert_eq!(
        registro.valor(CLAVE_DESINSTALACION, "DisplayIcon"),
        Some(Valor::Texto(exe.clone()))
    );
    assert_eq!(
        registro.valor(CLAVE_DESINSTALACION, "InstallLocation"),
        Some(Valor::Texto(carpeta))
    );
    assert_eq!(
        registro.valor(CLAVE_DESINSTALACION, "UninstallString"),
        Some(Valor::Texto(format!("\"{exe}\" --uninstall")))
    );
    assert_eq!(
        registro.valor(CLAVE_DESINSTALACION, "QuietUninstallString"),
        Some(Valor::Texto(format!("\"{exe}\" --uninstall")))
    );
    assert_eq!(
        registro.valor(CLAVE_DESINSTALACION, "NoModify"),
        Some(Valor::Dword(1))
    );
    assert_eq!(
        registro.valor(CLAVE_DESINSTALACION, "NoRepair"),
        Some(Valor::Dword(1))
    );

    let [menu, escritorio] = accesos.lista();
    let esperadas = [
        Llamada {
            lnk: menu,
            destino: rutas.exe(),
            trabajo: rutas.dir(),
        },
        Llamada {
            lnk: escritorio,
            destino: rutas.exe(),
            trabajo: rutas.dir(),
        },
    ];
    assert_eq!(creador.llamadas(), esperadas.to_vec());
}

// D201: la entrada del contrato se migra y se borra; queda exactamente una.
#[test]
fn instalar_migra_la_entrada_de_go_y_deja_una_sola() {
    let temp = tempfile::tempdir().expect("carpeta temporal");
    let (rutas, accesos) = de_prueba(temp.path());
    let registro = RegistroMemoria::default();
    let creador = CreadorMemoria::default();
    registro
        .escribir(
            CLAVE_DESINSTALACION_GO,
            "DisplayName",
            Valor::Texto("Oruscribe Agente".to_owned()),
        )
        .expect("clave de la instalación anterior");
    registro
        .escribir(
            CLAVE_DESINSTALACION_GO,
            "DisplayVersion",
            Valor::Texto("0.5.4".to_owned()),
        )
        .expect("versión de la instalación anterior");
    registro
        .escribir(
            CLAVE_DESINSTALACION,
            "DisplayVersion",
            Valor::Texto("0.5.3".to_owned()),
        )
        .expect("clave de NSIS");
    let origen = temp.path().join("descarga.exe");
    fs::write(&origen, b"binario").expect("binario de origen");

    instalar(&rutas, &accesos, &registro, &creador, &origen, "0.6.0").expect("instalar");

    assert_eq!(
        entradas_oruscribe(&registro).expect("entradas"),
        vec!["Oruscribe Agente".to_owned()]
    );
    assert!(!registro.existe(CLAVE_DESINSTALACION_GO));
    assert_eq!(
        registro.valor(CLAVE_DESINSTALACION, "DisplayVersion"),
        Some(Valor::Texto("0.6.0".to_owned()))
    );
}

// Hallazgo 7: la migración de la clave anterior es best-effort. Si borrar falla, la
// instalación no se aborta y los accesos se crean igual.
#[test]
fn instalar_sigue_si_falla_la_migracion_de_la_clave_anterior() {
    let temp = tempfile::tempdir().expect("carpeta temporal");
    let (rutas, accesos) = de_prueba(temp.path());
    let registro = RegistroMemoria::default();
    let creador = CreadorMemoria::default();
    registro.fallar_al_borrar();
    registro
        .escribir(
            CLAVE_DESINSTALACION_GO,
            "DisplayName",
            Valor::Texto("Oruscribe Agente".to_owned()),
        )
        .expect("clave de la instalación anterior");
    let origen = temp.path().join("descarga.exe");
    fs::write(&origen, b"binario").expect("binario de origen");

    instalar(&rutas, &accesos, &registro, &creador, &origen, "0.6.0")
        .expect("la migración fallida no aborta la instalación");

    assert!(
        registro.existe(CLAVE_DESINSTALACION_GO),
        "la clave vieja debe quedar para el próximo intento"
    );
    assert_eq!(
        registro.valor(CLAVE_DESINSTALACION, "DisplayVersion"),
        Some(Valor::Texto("0.6.0".to_owned()))
    );
    assert_eq!(creador.llamadas().len(), 2);
}

// Si el proceso ya es la copia instalada, no se copia sobre sí mismo.
#[test]
fn instalar_sobre_el_mismo_binario_no_lo_copia() {
    let temp = tempfile::tempdir().expect("carpeta temporal");
    let (rutas, accesos) = de_prueba(temp.path());
    fs::create_dir_all(rutas.dir()).expect("carpeta de instalación");
    fs::write(rutas.exe(), b"ya instalado").expect("exe instalado");
    let registro = RegistroMemoria::default();
    let creador = CreadorMemoria::default();

    instalar(&rutas, &accesos, &registro, &creador, &rutas.exe(), "0.6.0").expect("instalar");

    assert_eq!(
        fs::read(rutas.exe()).expect("exe instalado"),
        b"ya instalado".to_vec()
    );
    assert!(!con_sufijo(&rutas.exe(), ".nuevo").exists());
    assert_eq!(creador.llamadas().len(), 2);
}

#[test]
fn sin_carpeta_de_usuario_falla_con_texto_claro() {
    let temp = tempfile::tempdir().expect("carpeta temporal");
    let rutas = Rutas::new(Platform::Windows, PathBuf::new());
    let accesos = RutasAccesos::new(temp.path().join("inicio"), temp.path().join("escritorio"));
    let registro = RegistroMemoria::default();
    let creador = CreadorMemoria::default();

    let error = instalar(
        &rutas,
        &accesos,
        &registro,
        &creador,
        &temp.path().join("x.exe"),
        "0.6.0",
    )
    .expect_err("sin LOCALAPPDATA no se instala");

    assert_eq!(
        error.to_string(),
        "este Windows no tiene carpeta de usuario"
    );
}

// Wails escribe el valor `oruscribe-agente` con la ruta citada.
#[test]
fn autostart_usa_el_valor_de_wails() {
    let registro = RegistroMemoria::default();
    let exe = PathBuf::from(r"C:\Users\Ana\AppData\Local\Programs\Oruscribe\Oruscribe Agente.exe");

    activar_autostart(&registro, &exe).expect("activar autostart");

    assert_eq!(
        registro.valor(CLAVE_INICIO, VALOR_AUTOSTART),
        Some(Valor::Texto(format!("\"{}\"", exe.display())))
    );
    assert!(autostart_activo(&registro, &exe).expect("leer autostart"));

    desactivar_autostart(&registro, &exe).expect("desactivar autostart");
    assert!(!autostart_activo(&registro, &exe).expect("leer autostart"));
}

// Hallazgo 4: se compara la ruta, no el nombre fijo. Con dos valores, solo se
// borra el que apunta al exe actual.
#[test]
fn autostart_se_lee_y_borra_por_ruta_no_por_nombre() {
    let registro = RegistroMemoria::default();
    let exe = PathBuf::from(r"C:\Users\Ana\AppData\Local\Programs\Oruscribe\Oruscribe Agente.exe");
    let otro = PathBuf::from(r"C:\Users\Ana\Descargas\Oruscribe Agente.exe");

    // El nombre canónico apunta a otra copia: no cuenta como activo.
    registro
        .escribir(
            CLAVE_INICIO,
            VALOR_AUTOSTART,
            Valor::Texto(format!("\"{}\"", otro.display())),
        )
        .expect("valor de otra copia");
    assert!(!autostart_activo(&registro, &exe).expect("leer autostart"));

    // Dos nombres distintos apuntan al exe actual: los dos cuentan.
    for nombre in ["otra-copia", "duplicado"] {
        registro
            .escribir(
                CLAVE_INICIO,
                nombre,
                Valor::Texto(format!("\"{}\"", exe.display())),
            )
            .expect("valor del exe actual");
    }
    assert!(autostart_activo(&registro, &exe).expect("leer autostart"));

    desactivar_autostart(&registro, &exe).expect("desactivar autostart");

    for nombre in ["otra-copia", "duplicado"] {
        assert!(
            registro.valor(CLAVE_INICIO, nombre).is_none(),
            "debe borrar el valor {nombre} que apunta al exe actual"
        );
    }
    assert_eq!(
        registro.valor(CLAVE_INICIO, VALOR_AUTOSTART),
        Some(Valor::Texto(format!("\"{}\"", otro.display()))),
        "no debe borrar el valor de otra ruta"
    );
    assert!(!autostart_activo(&registro, &exe).expect("leer autostart"));
    assert!(autostart_activo(&registro, &otro).expect("la otra copia sigue activa"));

    for nombre in ["otra-copia", "duplicado"] {
        registro
            .escribir(
                CLAVE_INICIO,
                nombre,
                Valor::Texto(format!("\"{}\"", exe.display())),
            )
            .expect("duplicado antes de activar");
    }
    activar_autostart(&registro, &exe).expect("activar autostart");
    for nombre in ["otra-copia", "duplicado"] {
        assert!(
            registro.valor(CLAVE_INICIO, nombre).is_none(),
            "activar no limpió el duplicado {nombre}"
        );
    }
    assert_eq!(
        registro.valor(CLAVE_INICIO, VALOR_AUTOSTART),
        Some(Valor::Texto(format!("\"{}\"", exe.display())))
    );
}

// dos `rd` por carpeta, `%` escapado y borrado
// del propio script.
#[test]
fn el_guion_escapa_porcentaje_y_borra_dos_veces() {
    let carpetas = [
        PathBuf::from(r"C:\Usuarios\José Núñez 100%"),
        PathBuf::from(r"D:\Datos"),
    ];

    let texto = guion("rem", &carpetas);

    assert!(texto.starts_with("@echo off\r\nchcp 65001 >nul\r\n"));
    assert_eq!(texto.matches("rem").count(), 4);
    assert!(texto.contains(r#"rd /s /q "C:\Usuarios\José Núñez 100%%""#));
    assert_eq!(texto.matches("rd /s /q").count(), 4);
    assert!(texto.ends_with("del /q \"%~f0\""));
    assert!(!texto.replace("\r\n", "").contains('\n'));
}

// Hallazgo 5: el error conserva el texto de Windows; El contrato solo envuelve el
// error de crear la clave de desinstalación.
#[test]
fn el_error_de_windows_conserva_el_texto_y_solo_se_envuelve_la_creacion() {
    let directo = Error::Windows(5).to_string();
    assert!(
        !directo.contains("error de Windows"),
        "sigue mostrando el número: {directo}"
    );
    assert!(
        !directo.contains("(os error"),
        "sobra el sufijo de Rust: {directo}"
    );
    assert!(!directo.is_empty());

    let exe = PathBuf::from(r"C:\Programs\Oruscribe\Oruscribe Agente.exe");
    let creacion = escribir_entrada(
        &RegistroQueFalla {
            falla_crear: true,
            falla_escribir: false,
        },
        &exe,
        "1.0",
    )
    .expect_err("crear falla");
    assert_eq!(
        creacion.to_string(),
        format!("no se pudo registrar la desinstalación: {directo}")
    );

    let escritura = escribir_entrada(
        &RegistroQueFalla {
            falla_crear: false,
            falla_escribir: true,
        },
        &exe,
        "1.0",
    )
    .expect_err("escribir falla");
    assert_eq!(escritura.to_string(), directo);
}

#[test]
fn preparar_desinstalacion_limpia_accesos_y_claves() {
    let temp = tempfile::tempdir().expect("carpeta temporal");
    let (rutas, accesos) = de_prueba(temp.path());
    let registro = RegistroMemoria::default();
    for lnk in accesos.lista() {
        fs::create_dir_all(lnk.parent().expect("carpeta del acceso")).expect("carpeta");
        fs::write(&lnk, b"acceso").expect("acceso");
    }
    registro
        .escribir(
            CLAVE_DESINSTALACION,
            "DisplayName",
            Valor::Texto("Oruscribe Agente".to_owned()),
        )
        .expect("clave canónica");
    registro
        .escribir(
            CLAVE_DESINSTALACION_GO,
            "DisplayName",
            Valor::Texto("Oruscribe Agente".to_owned()),
        )
        .expect("clave de la instalación anterior");
    let datos = temp.path().join("datos");
    fs::create_dir_all(&datos).expect("carpeta de datos");

    let plan = preparar_desinstalacion(&rutas, &accesos, &registro, Some(&datos), temp.path())
        .expect("preparar desinstalación");

    for lnk in accesos.lista() {
        assert!(!lnk.exists(), "el acceso {} sigue en disco", lnk.display());
    }
    assert!(!registro.existe(CLAVE_DESINSTALACION));
    assert!(!registro.existe(CLAVE_DESINSTALACION_GO));
    assert_eq!(plan.carpetas, vec![rutas.dir(), datos]);
    assert_eq!(
        plan.contenido,
        guion("ping -n 4 127.0.0.1 >nul", &plan.carpetas)
    );
    assert!(plan.script.exists());
    assert_eq!(
        fs::read_to_string(&plan.script).expect("script escrito"),
        plan.contenido
    );
}

#[test]
fn preparar_desinstalacion_falla_si_no_puede_limpiar_accesos_o_claves() {
    let temp = tempfile::tempdir().expect("carpeta temporal");
    let (rutas, accesos) = de_prueba(temp.path());
    let registro = RegistroMemoria::default();
    let mut lnks = accesos.lista().into_iter();
    let bloqueado = lnks.next().expect("un acceso");
    fs::create_dir_all(&bloqueado).expect("acceso que no se puede borrar");
    for lnk in lnks {
        fs::create_dir_all(lnk.parent().expect("carpeta del acceso")).expect("carpeta");
        fs::write(lnk, b"acceso").expect("acceso");
    }
    for clave in [CLAVE_DESINSTALACION, CLAVE_DESINSTALACION_GO] {
        registro
            .escribir(clave, "DisplayName", Valor::Texto("Oruscribe".to_owned()))
            .expect("clave instalada");
    }
    registro.fallar_al_borrar();

    let error = preparar_desinstalacion(&rutas, &accesos, &registro, None, temp.path())
        .expect_err("limpiar a medias no es un plan válido");

    assert!(error.to_string().contains("el acceso"), "{error}");
    assert!(error.to_string().contains("la clave"), "{error}");
    assert!(bloqueado.exists(), "el acceso bloqueado se borró");
    assert!(registro.existe(CLAVE_DESINSTALACION));
    assert!(registro.existe(CLAVE_DESINSTALACION_GO));
}

// Si falla escribir el `.cmd`, la app sigue instalada: no se le pueden quitar
// los accesos ni la entrada de Agregar o quitar programas.
#[test]
fn si_falla_escribir_el_script_no_se_borran_accesos_ni_claves() {
    let temp = tempfile::tempdir().expect("carpeta temporal");
    let (rutas, accesos) = de_prueba(temp.path());
    let registro = RegistroMemoria::default();
    for lnk in accesos.lista() {
        fs::create_dir_all(lnk.parent().expect("carpeta del acceso")).expect("carpeta");
        fs::write(&lnk, b"acceso").expect("acceso");
    }
    registro
        .escribir(
            CLAVE_DESINSTALACION,
            "DisplayName",
            Valor::Texto("Oruscribe Agente".to_owned()),
        )
        .expect("clave canónica");
    registro
        .escribir(
            CLAVE_DESINSTALACION_GO,
            "DisplayName",
            Valor::Texto("Oruscribe Agente".to_owned()),
        )
        .expect("clave de la instalación anterior");

    // `temporal` como archivo: `create_dir_all` falla sin depender de permisos.
    let temporal = temp.path().join("temp");
    fs::write(&temporal, b"no soy carpeta").expect("obstáculo para el script");

    preparar_desinstalacion(&rutas, &accesos, &registro, None, &temporal)
        .expect_err("escribir el script debe fallar");

    for lnk in accesos.lista() {
        assert!(
            lnk.exists(),
            "el acceso {} se borró aunque falló el script",
            lnk.display()
        );
    }
    assert!(
        registro.existe(CLAVE_DESINSTALACION),
        "la entrada de Agregar o quitar programas se borró aunque falló el script"
    );
    assert!(
        registro.existe(CLAVE_DESINSTALACION_GO),
        "la clave anterior se borró aunque falló el script"
    );
}

// El `.cmd` vive en un TEMP compartido: dos corridas no pueden escribir el
// mismo archivo, o la segunda pisa el contenido que la primera va a ejecutar.
#[test]
fn cada_desinstalacion_escribe_un_script_distinto() {
    let temp = tempfile::tempdir().expect("carpeta temporal");
    let (rutas, accesos) = de_prueba(temp.path());
    let registro = RegistroMemoria::default();
    let crear_accesos = || {
        for lnk in accesos.lista() {
            fs::create_dir_all(lnk.parent().expect("carpeta del acceso")).expect("carpeta");
            fs::write(lnk, b"acceso").expect("acceso");
        }
    };

    crear_accesos();
    let primera = preparar_desinstalacion(&rutas, &accesos, &registro, None, temp.path())
        .expect("primera preparación");
    crear_accesos();
    let segunda = preparar_desinstalacion(&rutas, &accesos, &registro, None, temp.path())
        .expect("segunda preparación");

    assert_ne!(
        primera.script, segunda.script,
        "las dos corridas compartieron el mismo .cmd"
    );
    for plan in [&primera, &segunda] {
        assert_eq!(
            fs::read_to_string(&plan.script).expect("script escrito"),
            plan.contenido
        );
    }
}

#[test]
fn las_banderas_de_desinstalacion_son_las_esperadas() {
    let args = |lista: &[&str]| {
        lista
            .iter()
            .map(|arg| (*arg).to_owned())
            .collect::<Vec<_>>()
    };

    assert_eq!(argumentos_desinstalacion(&args(&[])), None);
    assert_eq!(
        argumentos_desinstalacion(&args(&["--uninstall"])),
        Some(false)
    );
    assert_eq!(argumentos_desinstalacion(&args(&["--remove-data"])), None);
    assert_eq!(
        argumentos_desinstalacion(&args(&["--uninstall", "--remove-data"])),
        Some(true)
    );
}

// `cmd` con `chcp 65001` encuentra
// la carpeta con acentos y `%`.
#[cfg(windows)]
#[test]
fn el_cmd_borra_acentos_y_porcentaje() {
    use std::process::Command;

    let temp = tempfile::tempdir().expect("carpeta temporal");
    let carpeta = temp.path().join("Usuario José Núñez 100%");
    fs::create_dir_all(&carpeta).expect("carpeta con acentos");
    fs::write(carpeta.join("dato.txt"), b"x").expect("archivo de prueba");
    let script = temp.path().join("desinstalar.cmd");
    fs::write(&script, guion("rem", std::slice::from_ref(&carpeta))).expect("script");

    let _ = Command::new("cmd.exe")
        .arg("/c")
        .arg(&script)
        .status()
        .expect("ejecutar cmd");

    assert!(
        !carpeta.exists(),
        "la carpeta {} sigue en disco",
        carpeta.display()
    );
}

// Un `.lnk` real en carpeta temporal.
#[cfg(windows)]
#[test]
fn el_acceso_real_se_crea() {
    use std::time::Duration;

    use orus_install::windows::CreadorWsh;

    let temp = tempfile::tempdir().expect("carpeta temporal");
    let lnk = temp.path().join("prueba.lnk");
    let destino = std::env::current_exe().expect("binario de la suite");
    let creador = CreadorWsh;
    let mut ultimo = String::new();
    for _ in 0..3 {
        match creador.crear(&lnk, &destino, temp.path()) {
            Ok(()) => break,
            Err(error) => {
                ultimo = error;
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }
    if !lnk.exists() {
        eprintln!("se omite el acceso real: {ultimo}");
        return;
    }
    let tam = fs::metadata(&lnk).expect("metadata del acceso").len();
    assert!(tam >= 100, "el acceso directo pesa {tam} bytes");
}

// Con el escritorio redirigido, la carpeta la dice el registro del usuario y
// no `USERPROFILE\Desktop`.
#[test]
fn escritorio_redirigido_se_lee_del_registro() {
    let registro = RegistroMemoria::default();
    registro
        .escribir(
            r"Software\Microsoft\Windows\CurrentVersion\Explorer\User Shell Folders",
            "Desktop",
            Valor::Texto(r"%USERPROFILE%\OneDrive\Desktop".to_owned()),
        )
        .expect("valor Desktop del registro");

    let accesos = RutasAccesos::desde_registro(&registro, Some(OsStr::new(r"C:\Users\Ana")));

    // El separador lo pone la plataforma: el valor del registro es el que manda,
    // no una ruta con barras fijas (en Linux este test tambien corre).
    assert_eq!(
        accesos.lista()[1],
        PathBuf::from(r"C:\Users\Ana\OneDrive\Desktop").join(format!("{NOMBRE_APP}.lnk"))
    );
}

// `Shell Folders` manda: Windows ya expandió su valor y es el que lee el
// backend real; `User Shell Folders` solo se usa si aquél falta.
#[test]
fn escritorio_redirigido_shell_folders_gana() {
    let registro = RegistroMemoria::default();
    registro
        .escribir(
            r"Software\Microsoft\Windows\CurrentVersion\Explorer\Shell Folders",
            "Desktop",
            Valor::Texto(r"C:\Users\Ana\OneDrive\Desktop".to_owned()),
        )
        .expect("Shell Folders");
    registro
        .escribir(
            r"Software\Microsoft\Windows\CurrentVersion\Explorer\User Shell Folders",
            "Desktop",
            Valor::Texto(r"%USERPROFILE%\Desktop".to_owned()),
        )
        .expect("User Shell Folders");

    let accesos = RutasAccesos::desde_registro(&registro, Some(OsStr::new(r"C:\Users\Ana")));

    assert_eq!(
        accesos.lista()[1],
        PathBuf::from(r"C:\Users\Ana\OneDrive\Desktop").join(format!("{NOMBRE_APP}.lnk"))
    );
}

#[test]
fn actualizar_version_cambia_solo_el_valor_visible() {
    let temp = tempfile::tempdir().expect("carpeta temporal");
    let (rutas, accesos) = de_prueba(temp.path());
    let registro = RegistroMemoria::default();
    let creador = CreadorMemoria::default();
    let origen = temp.path().join("descarga.exe");
    fs::write(&origen, b"binario").expect("origen");

    instalar(&rutas, &accesos, &registro, &creador, &origen, "0.6.3").expect("instalar");
    let desinstalar = registro.valor(CLAVE_DESINSTALACION, "UninstallString");

    actualizar_version(&registro, "0.6.4").expect("actualizar versión");

    assert_eq!(
        registro.valor(CLAVE_DESINSTALACION, "DisplayVersion"),
        Some(Valor::Texto("0.6.4".to_owned()))
    );
    assert_eq!(
        registro.valor(CLAVE_DESINSTALACION, "UninstallString"),
        desinstalar
    );
}

// El rollback de Windows restaura el `.old`; en Linux no existe esa copia
// (`swap.rs` devuelve Unsupported), así que el test solo corre en Windows.
#[cfg(windows)]
#[test]
fn el_rollback_devuelve_la_copia_anterior() {
    let temp = tempfile::tempdir().expect("carpeta temporal");
    let target = temp.path().join("app.exe");
    let old = temp.path().join("app.exe.old");
    fs::write(&target, b"nueva rota").expect("nueva");
    fs::write(&old, b"vieja buena").expect("vieja");

    orus_install::rollback(&target).expect("rollback");

    assert_eq!(fs::read(&target).expect("target"), b"vieja buena".to_vec());
    assert!(!old.exists());
    assert!(temp.path().join("app.exe.failed").exists());
}

// En Linux el swap no deja `.old`: rollback tiene que decir que no hay copia
// anterior, no tocar el binario ni entrar en pánico.
#[cfg(not(windows))]
#[test]
fn el_rollback_en_linux_dice_que_no_hay_copia_anterior() {
    let temp = tempfile::tempdir().expect("carpeta temporal");
    let target = temp.path().join("app.exe");
    fs::write(&target, b"binario").expect("binario");

    let error = orus_install::rollback(&target).expect_err("en Linux no hay .old");

    assert_eq!(error.kind(), std::io::ErrorKind::Unsupported);
    assert_eq!(fs::read(&target).expect("target"), b"binario".to_vec());
}

// El fallback real de `User Shell Folders`: Windows guarda esos valores como
// `REG_EXPAND_SZ`. Con `Shell Folders` ausente, el escritorio redirigido tiene
// que salir de ahí expandido, no del `USERPROFILE\Desktop` por defecto.
#[test]
fn escritorio_redirigido_user_shell_folders_es_expand_sz() {
    let registro = RegistroMemoria::default();
    registro.escribir_con_tipo(
        r"Software\Microsoft\Windows\CurrentVersion\Explorer\User Shell Folders",
        "Desktop",
        Valor::Texto(r"%USERPROFILE%\OneDrive\Desktop".to_owned()),
        REG_EXPAND_SZ,
    );

    let accesos = RutasAccesos::desde_registro(&registro, Some(OsStr::new(r"C:\Users\Ana")));

    // El separador lo pone la plataforma: lo que importa es que la carpeta
    // salga del valor expandido y no del `USERPROFILE\Desktop` por defecto.
    assert_eq!(
        accesos.lista()[1],
        PathBuf::from(r"C:\Users\Ana\OneDrive\Desktop").join(format!("{NOMBRE_APP}.lnk"))
    );
}

// Un tipo que no es texto no se lee: el escritorio cae al perfil, no al DWORD.
#[test]
fn un_valor_que_no_es_texto_no_se_lee() {
    let registro = RegistroMemoria::default();
    registro.escribir_con_tipo(
        r"Software\Microsoft\Windows\CurrentVersion\Explorer\User Shell Folders",
        "Desktop",
        Valor::Dword(1),
        REG_DWORD,
    );

    let accesos = RutasAccesos::desde_registro(&registro, Some(OsStr::new(r"C:\Users\Ana")));

    // El fallback arma la ruta con `join`: el separador lo pone la plataforma.
    assert_eq!(
        accesos.lista()[1],
        PathBuf::from(r"C:\Users\Ana")
            .join("Desktop")
            .join(format!("{NOMBRE_APP}.lnk"))
    );
}

// Un symlink plantado en `.nuevo` no puede truncar su destino: la copia
// atómica crea el archivo sin seguir enlaces, como `download.rs`. En Windows
// crear symlinks de archivo exige privilegio o Developer Mode; el gate de
// Linux sí puede plantarlo.
#[cfg(unix)]
#[test]
fn instalar_no_sigue_un_symlink_plantado_en_el_nuevo() {
    let temp = tempfile::tempdir().expect("carpeta temporal");
    let (rutas, accesos) = de_prueba(temp.path());
    let registro = RegistroMemoria::default();
    let creador = CreadorMemoria::default();
    let origen = temp.path().join("descarga.exe");
    fs::write(&origen, b"binario nuevo").expect("binario de origen");
    let victima = temp.path().join("victima.txt");
    fs::write(&victima, b"datos de la victima").expect("archivo víctima");
    fs::create_dir_all(rutas.dir()).expect("carpeta de instalación");
    let nuevo = con_sufijo(&rutas.exe(), ".nuevo");
    std::os::unix::fs::symlink(&victima, &nuevo).expect("symlink plantado");

    instalar(&rutas, &accesos, &registro, &creador, &origen, "0.6.0").expect("instalar");

    assert_eq!(
        fs::read(&victima).expect("archivo víctima"),
        b"datos de la victima".to_vec(),
        "el symlink plantado en .nuevo truncó su destino"
    );
    assert!(
        !fs::symlink_metadata(rutas.exe())
            .expect("metadata del exe")
            .file_type()
            .is_symlink(),
        "el exe instalado no puede ser un symlink"
    );
    assert_eq!(
        fs::read(rutas.exe()).expect("exe instalado"),
        b"binario nuevo".to_vec()
    );
}
