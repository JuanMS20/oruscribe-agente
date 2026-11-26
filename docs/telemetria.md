# Telemetría de errores

El cliente sigue siendo Sentry (POST JSON a `/api/<id>/store/`). No hay OpenTelemetry, OTLP ni métricas en esta tarjeta.

## DSN de release

`.github/workflows/ci.yml` y `scripts/release.ps1` leen `ORUSCRIBE_SENTRY_DSN` del entorno (en CI, del secreto del mismo nombre); el repositorio no guarda un DSN. Si la variable está vacía al compilar, el binario sale sin DSN y ambos caminos lo avisan:

- `dsn_configurado()` devuelve `None`: el binario publicado no tiene DSN horneado.
- `Telemetry::salud()` devuelve `Local` aunque la persona active el interruptor.
- Activado sin DSN no produce ninguna petición de red; el anillo local de 20 eventos sigue disponible en Diagnóstico.
- `GET /api/diagnostics` muestra `Telemetría: local (sin consentimiento o sin DSN configurado).`

## Qué se reporta

| Categoría | Cuándo |
|---|---|
| `shell.panic` | Pánico de Rust (fatal, síncrono, con archivo:línea) |
| `shell.startup` | Cualquier cuadro de error de arranque que ve la persona (servidor, ventana, bandeja, autostart) |
| `shell.server` | El servidor local se detuvo (fatal) |
| `shell.browser`, `shell.update` | No abrió el navegador; falló una actualización |
| `auth.oauth`, `agent.jwt` | Falla el login (la cancelación de la persona no se reporta); token sin formato JWT |
| `agent.chat` | Turno del chat falló: `kind` = `auth`, `rate_limit`, `server`, `empty_response`, `transport`, `local`, `client` y `status` |
| `agent.timeout`, `agent.panic`, `agent.limit`, `agent.tools`, `agent.session` | Otros finales de turno que la persona no provocó |
| `agent.setup`, `agent.save` | El turno falló al prepararse; no se pudo guardar la conversación |
| `mcp.*`, `agent.doorbell` | Herramientas de Oruscribe y timbre |
| `api.http` | Cualquier 5xx del servidor local (`warning` para 429) |
| `ui.js`, `ui.api`, `ui.mic`, `ui.clipboard` | Errores del frontend |

Cupos por hora: 5 fatales, 10 errores y 5 avisos (`warning` e `info`), cada uno aparte, más deduplicación de 5 minutos. El consentimiento se lee al arrancar, antes de crear la sesión, para que un fallo temprano también cuente.

Entrega real verificada con el DSN de producción el 2026-11-26: un `orus-server` real envió un evento sintético `qa.release_check` y el diagnóstico quedó en `configurada; 1 entregados, 0 fallos`. No se enviaron datos personales.

## Cómo configurarlo en un build

La variable se lee primero en tiempo de ejecución (`std::env::var`); si falta o está vacía, se usa el valor horneado por `option_env!` al compilar. Para un build con DSN hay que exportarla antes de compilar, desde el entorno del pipeline, nunca desde un archivo del repo:

```bash
export ORUSCRIBE_SENTRY_DSN='https://<clave>@<org>.ingest.sentry.io/<proyecto>'
cargo build --release --locked -p orus-shell
```

```powershell
$env:ORUSCRIBE_SENTRY_DSN = 'https://<clave>@<org>.ingest.sentry.io/<proyecto>'
cargo build --release --locked -p orus-shell
```

El parser solo acepta HTTPS (o HTTP en loopback para pruebas), proyecto numérico y clave alfanumérica, y desactiva redirecciones para evitar SSRF por 30x.

## Consentimiento

Por defecto la telemetría de errores técnicos redactados está activada (`Settings::telemetry_on()` devuelve `true` cuando el campo no existe) y se apaga desde **Ajustes → Reportar errores automáticamente** (`"telemetry": false` en `settings.json`). Quien la apagó conserva su elección guardada. Sin DSN embebido al compilar ni definido en `ORUSCRIBE_SENTRY_DSN`, nunca sale ninguna petición de red y solo se mantiene el anillo local de 20 eventos en Diagnóstico.

## Estados de entrega

- `Local`: sin consentimiento o sin DSN; solo anillo local.
- `Configurada`: consentimiento y DSN presentes, sin envíos en vuelo y último envío correcto (o sin intentos previos).
- `Entregando`: hay al menos un envío en curso.
- `Fallando`: el último envío falló (401, 403, 404, 429, 5xx, DNS/TLS o cliente no disponible). El diagnóstico muestra el número de fallos y el último error.

## Cierre y redacción

El shell llama a `flush` con un tope de 2 s al salir del bucle de eventos; no hay espera indefinida. El hook de pánico llama al hook anterior antes de tocar telemetría, aísla su cuerpo con `catch_unwind` y nunca hace `block_on` en el hilo del pánico. El payload de un pánico lleva `exception.values[0].stacktrace.frames` cuando se pudo capturar un backtrace, y `platform: native`.

Antes de guardar o enviar se tachan claves `sk-`/`syn_`/`oru_`, JWT, parámetros sensibles en URLs (`api_key`, `sig`, `id_token`, `code`, ...), cabeceras (`Authorization`, `Cookie`, `X-Api-Key`), `/root`, `C:\Users\<nombre>` y `/home/<nombre>`.
