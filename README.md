# Oruscribe Agente

App de escritorio nativa para **Windows y Linux** de [Oruscribe](https://oruscribe.com/). Un solo ejecutable en Rust —sin Electron, sin Node en ejecución ni permisos de administrador— que vive en la bandeja del sistema, se conecta al servidor MCP de tu organización y pone a trabajar a tu agente de IA junto a tu equipo.

## Qué hace hoy

- **Ventana nativa + bandeja del sistema** (`orus-shell` con `tao` + `wry`: WebView2 en Windows, WebKitGTK 4.1 en Linux). Cerrar la ventana la oculta en la bandeja; el timbre y el agente siguen activos hasta elegir **Salir**.
- **Conexión segura a Oruscribe** (`orus-auth` + `orus-org`): inicio de sesión OAuth 2.1 con PKCE, tokens en el llavero del sistema, cliente MCP Streamable HTTP (`rmcp`) y timbre en vivo (*doorbell*) para menciones, tareas asignadas y alertas sin sondeos agresivos.
- **Agente de IA con dos modos**:
  - **IA de Oruscribe**: corre en los servidores de Oruscribe con tu sesión y herramientas de la organización.
  - **Mi clave** (OpenRouter o proveedor compatible con OpenAI/Anthropic): arnés completo en este equipo con herramientas MCP de Oruscribe + herramientas locales (`read`, `write`, `edit`, `bash`), lectura de `AGENTS.md` / `SKILL.md`, aprobaciones explícitas en pantalla y salida visible de cada herramienta.
- **Trabajo diario**: Hoy/Panel, Mis tareas, Chat del equipo (con adjuntos, reacciones y citas), Avisos, Menciones y Puesta al día.
- **Voz opcional**: dictado y lectura mediante cualquier servidor compatible con OpenAI configurado en Ajustes (sin dependencias C++/CMake en el binario).
- **Instalador y actualizador firmado**: instalador NSIS por usuario en Windows, `.tar.gz` + `install.sh` en Linux, y actualización verificada con firmas Ed25519.

## Requisitos de compilación

- **Rust estable** (`rust-version = 1.88`, edición 2024). **100 % Rust puro** (sin CMake, sin LLVM/libclang, sin Node).
- **Windows:** WebView2 (incluido en Windows 10/11).
- **Linux x86_64:** `libgtk-3-dev`, `libwebkit2gtk-4.1-dev`, `libayatana-appindicator3-dev`, `libdbus-1-dev`, `librsvg2-dev`, `pkg-config`.

## Compatibilidad del binario de Linux

El `.tar.gz` se compila en el runner `ubuntu-latest` del CI. Su glibc mínima es la de ese runner y se guarda en `GLIBC_BASELINE.txt` dentro del paquete; `install.sh` la compara con la del equipo antes de copiar. En una distro con glibc más vieja el binario publicado no arranca: compila desde el fuente en ese equipo. No hay build musl, AppImage ni Flatpak.

## Compilar y ejecutar

```powershell
# Compilar la app de escritorio (release)
cargo build --release --locked -p orus-shell

# Probar la interfaz en navegador sin abrir la ventana nativa (con carpeta temporal)
cargo run -p orus-server -- --addr 127.0.0.1:8099 --data $env:TEMP\oru-dev
```

## Verificación (gate local)

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test -j 1 --workspace --locked
Get-ChildItem frontend\dist\js -Recurse -Filter *.js | ForEach-Object { Get-Content $_.FullName -Raw | node --input-type=module --check }
```

Para el test E2E completo del turno del agente con el proveedor falso:

```powershell
cargo build -p fake-provider --locked
cargo test -p orus-app --test agent_turno --locked -- --ignored
```

## Estructura del workspace (12 crates + 1 helper de test)

```text
crates/orus-shell     Binario de escritorio: ventana, bandeja, instancia única y arranque local
crates/orus-server    Router combinado + frontend/dist embebido (y binario headless de desarrollo)
crates/orus-api       Borde HTTP local (/api/*) y eventos SSE
crates/orus-app       Estado de sesión, login, timbre, agente, aprobaciones y conversaciones
crates/orus-harness   Motor de turno: prompt, AGENTS.md, skills, herramientas MCP/locales y sesiones
crates/orus-agent     Proveedores LLM (OpenAI/Anthropic/CloudBrain), política, runner y compactación
crates/orus-tools     Herramientas locales confinadas (read, write, edit, bash), diario y permisos
crates/orus-org       Cliente MCP de Oruscribe (rmcp) y timbre en vivo
crates/orus-auth      OAuth 2.1 + PKCE y almacén de secretos en llavero del sistema
crates/orus-config    Carpeta de datos, idioma y settings.json
crates/orus-core      Tipos compartidos de mensajes, eventos y errores
crates/orus-install   Instalación por usuario, actualización firmada Ed25519 y signmanifest
tools/fake-provider   Servidor LLM falso para el test E2E de turno
frontend/dist         Interfaz estática HTML/CSS/JS embebida tal cual en el binario
```

## Documentación

- [docs/arquitectura.md](docs/arquitectura.md): flujo real de arranque, seguridad local y arquitectura de crates.
