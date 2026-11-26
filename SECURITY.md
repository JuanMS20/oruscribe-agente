# Seguridad de Oruscribe Agente

Este documento dice qué protege la app, de quién, y qué queda fuera. Si algo de
aquí no coincide con el código, gana el código; el documento se corrige.

## Modelo de amenazas

Lo que se defiende, en orden de gravedad.

1. **La sesión y la clave del proveedor de IA.** En Windows viven en Credential
   Manager y no hay respaldo en archivo. En Linux se intenta Secret Service;
   si el llavero no existe o está bloqueado, el binario publicado escribe el
   refresh token en `session.json` y la clave en `secret-agente-clave.json`
   dentro de la carpeta de datos (`~/.config/Oruscribe` por defecto), con
   directorio `0700` y archivos `0600`. Con `ORUSCRIBE_ALLOW_FILE_SECRETS=1`
   el archivo es el store principal en cualquier sistema. El respaldo solo se
   lee si el propio guardado lo marcó: un llavero que falla después no revive
   un token viejo. Nunca se imprimen los secretos (el aviso de respaldo solo
   da la ruta), ni se muestran en la interfaz (solo los últimos cuatro
   caracteres de la clave), ni aparecen en un error.
2. **La cuenta de Oruscribe de la persona.** La app actúa en su nombre. Por eso
   el modo por defecto es "Pedir permiso": una herramienta de escritura no corre
   hasta que la persona la aprueba en pantalla, con un plazo de cinco minutos
   que cuenta como no. "Solo lectura" niega toda escritura sin preguntar.
   "Autónomo" las deja correr y avisa en pantalla. En ningún modo ve ni puede
   llamar `create_agent`, `get_my_doorbell`, `invite_member`, `switch_org`,
   `create_organization`, `move_task_to_org` ni `move_goal_to_org`: devuelven un
   secreto o cambian la cuenta entera. Se quitan antes de llegar al modelo y se
   rechazan también al ejecutarse.
3. **Los datos de otras personas que llegan del servidor.** Todo texto ajeno se
   escapa antes de entrar al HTML (`esc()`), no hay manejadores en línea ni
   `eval`, y la política de seguridad de contenido (CSP) de
   `frontend/dist/index.html` limita qué puede cargar la ventana.
4. **El despertar automático.** Apagado por defecto. Cuando se enciende, solo
   atiende menciones, tareas asignadas y alertas; ignora lo que escribe la propia
   persona (que hoy es también la identidad del agente, bloqueo B1), recuerda los
   eventos para no repetirlos, corre una ejecución a la vez con topes por minuto,
   por día y de tokens, y deja un registro visible. El texto del evento es dato,
   nunca una orden, y en modo "Pedir permiso" toda escritura sigue esperando
   aprobación.
5. **Las instrucciones inyectadas en datos.** Lo que devuelven las herramientas
   (mensajes, tareas, archivos) es información, no órdenes. El mensaje de
   sistema lo dice, y en modo "Pedir permiso" una orden escondida en un dato no
   salta la aprobación.
6. **Las actualizaciones.** El manifiesto `latest.json` se verifica con su firma
   ed25519 antes de leerlo; la descarga se verifica con su SHA-256; la URL debe
   pertenecer al repositorio de descargas. Un manifiesto alterado o sin firma se
   rechaza. Las llaves públicas están en
   `crates/orus-install/src/update/keys.rs`; las privadas nunca entran al
   repositorio.
7. **La red.** La app abre un servidor HTTP efímero en `127.0.0.1` (puerto
   asignado por el sistema) para que la ventana hable con la API; solo escucha
   en loopback y exige el token de la sesión local. El inicio de sesión con
   OAuth usa `127.0.0.1:47823` mientras dura. El actualizador consulta el
   manifiesto en GitHub (`raw.githubusercontent.com`) y descarga del repositorio
   de descargas. La telemetría de errores va a Sentry, activada por defecto y
   apagable en Ajustes: envía solo fallos técnicos redactados (sin chat, sin
   tokens, sin rutas de usuario), con tope por hora. Las otras salidas son el
   servidor de Oruscribe configurado y el proveedor de IA que elija la persona.
8. **El cierre de sesión.** Borra el token, cancela el agente, borra la
   conversación y sus archivos, y deja la clave del proveedor (es de la
   persona).

De quién se defiende: de otro programa o persona con la misma cuenta de Windows
o de Linux, de un servidor comprometido que manda texto malicioso, de un
manifiesto de actualización falsificado y de una red que intenta cambiar el
tráfico. No se defiende de alguien con permisos de administrador en el equipo:
esa persona puede leer la memoria del proceso.

## Qué NO está protegido todavía

- **Respaldo en claro en Linux.** Sin llavero funcional, el refresh token y la
  clave del proveedor quedan en JSON `0600` en la carpeta de datos. Sincronizar
  `~/.config/Oruscribe` (dotfiles, Syncthing, Nextcloud) sube esos secretos.
  Cerrar sesión borra el respaldo y su marca (`*.fallback`, solo un SHA-256,
  sin secreto); el llavero bloqueado no lee un respaldo que el guardado no marcó.
- **Conexión como agente trabajador (B1).** Hoy la app entra con la cuenta de la
  persona. Hasta que el servidor OAuth permita elegir un agente, todo lo que
  haga queda a nombre de la persona.
- **Arnés completo con IA de la organización (B2).** El cerebro de la
  organización ya atiende el chat por defecto (modelo y herramientas del
  servidor, pagados por la organización), pero no ejecuta el arnés local ni
  permite elegir modelo. Para eso falta un punto de acceso compatible con OpenAI
  y una clave de la organización.
- **Términos versionados en el servidor (B3).** El texto y la versión existen en
  el cliente, pero no hay tablas que guarden la aceptación.
- **Certificado de firma de Windows (B5).** El ejecutable no está firmado, así
  que SmartScreen puede avisar.
- **Fotos de perfil de direcciones arbitrarias.** La interfaz carga imágenes
  `https` que pone otra persona; eso revela la IP de quien mira. Está pendiente
  de decisión (sección 7 del prompt del arnés).
- **Autónomo junto con "El agente trabaja solo".** Si se combinan, el texto de
  un mensaje ajeno que despierta al agente puede llevarlo a usar sin aprobación
  cualquier herramienta de escritura de Oruscribe que no esté prohibida, y a leer
  archivos de las carpetas permitidas. El único freno es el mensaje de sistema
  que dice que el texto es información. Por eso el despertar viene apagado y el
  modo por defecto es "Pedir permiso". No los combines si las carpetas permitidas
  tienen datos sensibles.
- **Herramientas locales.** No existen sin carpetas permitidas. Con ellas, cada
  ruta se resuelve a su ruta real (enlaces, junctions y puntos de unión incluidos)
  y se rechaza si queda fuera; también se rechazan UNC, flujos de datos alternos,
  nombres de dispositivo y rutas relativas. La comparación no distingue mayúsculas
  en Windows. El archivo se abre después de validar y se vuelve a validar abierto.
  La lectura se corta en 1 MiB y no se leen binarios. Los programas salen de una
  lista, se ejecutan sin shell, con un entorno limpio y un `HOME` propio que se
  borra al terminar (Linux, Mac y Windows), y con tope de tiempo; al
  agotarse se mata el árbol completo. `bash` reenvía solo rutas puntuales de
  configuración (`SSH_AUTH_SOCK`, `GH_CONFIG_DIR`, `CARGO_HOME` y similares),
  nunca el perfil completo. Ejecutar siempre pide aprobación, incluso en
  Autónomo. Límite conocido: el proceso se asigna al objeto de trabajo justo
  después de arrancar, así que un hijo creado en esa ventana mínima podría escapar
  del trabajo; en ese caso se usa `taskkill /T` como respaldo al cortar.

## Cómo se firma una actualización

1. El script `scripts/release.ps1` compila `orus-shell`, calcula el SHA-256 del
   ejecutable y del instalador, y escribe `latest.json` con saltos de línea LF.
2. Con `ORUSCRIBE_SIGNING_KEY` apuntando a la llave privada principal, corre
   `cargo run --release --locked -p orus-install --bin signmanifest` con
   `-key <llave> -sign build\release\latest.json`; la firma queda en
   `latest.json.sig`, en base64.
3. Antes de publicar se comprueba la firma con la misma herramienta
   (`... -- -verify build\release\latest.json -pub <llave pública>`); el script
   además corre `llaves_embebidas_estan_bien_formadas`, que valida las llaves
   embebidas de `crates/orus-install/src/update/keys.rs`.
4. Se sube primero el `.sig` y después el manifiesto, para que ningún cliente
   vea un manifiesto sin su firma.
5. El cliente verifica la firma con las llaves públicas embebidas antes de leer
   el manifiesto, y después el hash del binario descargado.

La llave privada de respaldo se guarda fuera de línea. Si se pierde la
principal, la de respaldo permite publicar una versión que rote las llaves.

## Dónde viven los secretos

| Secreto | Dónde | Quién lo puede leer |
|---|---|---|
| Token de sesión de OAuth | Windows: Credential Manager. Linux: Secret Service, con respaldo `session.json` (`0600`) si el llavero falta o está bloqueado; con `ORUSCRIBE_ALLOW_FILE_SECRETS=1`, archivo principal | La persona y la app; en Linux, otra persona con la misma cuenta puede leer el archivo |
| Clave del proveedor de IA | Windows: Credential Manager. Linux: Secret Service, con respaldo `secret-agente-clave.json` (`0600`); con `ORUSCRIBE_ALLOW_FILE_SECRETS=1`, archivo principal | La persona y la app; en Linux, otra persona con la misma cuenta puede leer el archivo |
| Llave privada de firma | La ruta que indique `ORUSCRIBE_SIGNING_KEY`; por convención, `C:\Users\<persona>\.oruscribe\update-signing.key` | Solo la persona que publica |
| Llave privada de respaldo | Medio fuera de línea | Solo la persona |

Ningún secreto entra al repositorio, a un registro, a un diagnóstico copiable, a
una captura ni a un dato de prueba.

## Cómo reportar una falla

Escríbele a Juan (JuanMS20) por el canal privado del equipo, no en un issue
público. Incluye: versión de la app, sistema operativo, qué pasó y cómo
reproducirlo. No adjuntes claves, tokens ni datos de clientes. Si el reporte
necesita un dato sensible, dilo primero y acuerden un canal.
