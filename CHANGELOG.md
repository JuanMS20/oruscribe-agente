# Cambios por versión

## 0.8.5

Sin migración de datos. Backend: sin cambios.

- Chat que se quedaba pegado: un pánico o un Stop durante la preparación del
  turno dejaba al agente «ocupado» (todo envío daba 409 hasta reiniciar); la
  compactación volvía a resumir su propio resumen en cada paso; enviar un
  mensaje podía dejar la pantalla sin «Trabajando…» ni respuesta si había otra
  consulta de estado en vuelo o el envío fallaba por red.
- Herramientas: `bash` sin `timeout` ya devuelve al modelo la salida parcial y
  el motivo en vez de «context deadline exceeded» sin salida; la salida larga
  conserva el inicio y el final, donde está el error; la búsqueda de texto sigue
  tras una carpeta sin permiso y lo avisa.
- Proveedores compatibles con OpenAI: un stream que ya dijo `stop` o
  `tool_calls` y cierra sin `[DONE]` ya no se pierde.
- Linux: el listado de carpetas marcaba mal el corte a 500 entradas; el
  instalador y el paquete se prueban ahora en Ubuntu 22.04 y 24.04, Debian 12,
  Fedora y Arch, y las pruebas de llavero corren contra gnome-keyring real.

## 0.8.4

Sin migración de datos. Backend: sin cambios.

- Chat: «continua» después de un fallo ya reintenta cuando el proveedor devuelve
  una respuesta vacía; un bloque `orus-tool` roto o con una herramienta
  desconocida vuelve al modelo como error en vez de cerrar el turno sin hacer
  el trabajo; `bash` acepta `timeout` para instalaciones largas y la salida
  larga conserva el final, donde está el error.
- El agente recibe las rutas reales de Descargas, Documentos y Escritorio (en
  Linux, según el idioma del sistema) y la indicación de usar rutas absolutas.
- Telemetría: activada por defecto y apagable en Ajustes. Reporta fallos del
  chat, del arranque, de guardado y del servidor local; los avisos tienen cupo
  propio; el texto redactado ya no filtra `password=` tras una palabra sensible.
- Inicio de sesión: cancelar ya no tarda hasta el plazo completo y no cuenta
  como error.
- Linux: el instalador encuentra `ldconfig` en `/sbin` y `/usr/sbin`.

## 0.7.0

La actualización deja de ser manual. Sin migración de datos: volver a 0.6.10 es
reinstalar y ya.

- La app se actualiza sola también en Linux cuando vive en `~/.local`: baja el
  paquete, comprueba la firma y la huella, verifica que el binario nuevo arranca
  y lo reemplaza. Si está instalada en `/usr/bin`, avisa y no toca nada.
- El aviso de versión nueva aparece antes de iniciar sesión, muestra las
  novedades y el progreso de la descarga.
- Comprueba cada 6 horas mientras la app está abierta, no solo al arrancar.
- Windows: el mismo flujo de siempre, ahora con progreso a la vista.

Arreglos desde 0.6.10 (36 commits):

- Sesión: la reconexión tiene plazo y no deja la ventana en «conectando»; el
  cambio de organización aísla los datos y la sesión aguanta entre procesos.
- Chat: la conversación se ve en ventanas angostas y el botón de volver
  funciona; borrar una conversación confirma; las aprobaciones se repintan.
- Agente: no cierra turnos en blanco, conserva el historial al cancelar, cuenta
  el consumo del subagente, reembolsa el cupo del timbre y el diario del
  despertar no se ve vacío mientras trabaja.
- Organización: paginación real, adjuntos idempotentes, textos y metas toleran
  nulls del servidor y las subidas tienen plazo por tamaño.
- Instalación: desinstalación segura, autostart correcto y un solo script de
  desinstalación por instalación.
- API: las entradas inválidas responden 400 en vez de 500.
- Calidad: el gate de Linux compila y pasa (clippy y tests), y el nivel `smoke`
  de `scripts/test.sh` corre sin pantalla.

## 0.6.10

Arreglos para los primeros pilotos de escritorio. Sin migración de datos. Incluye los cambios de 0.6.9, que no llegaron a publicarse.

- Al añadir una carpeta permitida en Windows, el selector nativo ya no se corta a los 40 s.
- El botón «Oír» (leer en voz alta la respuesta del asistente) vuelve a funcionar.
- «El agente trabaja solo» se guarda también con la IA de Oruscribe y respeta los topes por minuto, día y tokens elegidos.
- Salir de Chat detiene el sondeo aunque una respuesta viniera en camino.
- Un fallo de red al leer herramientas MCP ya no consume el cupo de 15 lecturas por 3 minutos.
- «Instalar» avisa si no puede abrir la copia instalada en vez de reportar éxito.
- El chat usa la organización configurada y la barra lateral permite elegir entre las organizaciones que ofrece el servidor, sin cambiar la de otros clientes.

## 0.6.9

Parche del re-análisis posterior a 0.6.8. Solo arreglos; sin cambios que rompen
ni migración de datos.

- Al añadir una carpeta permitida en Windows, el selector nativo ya no se corta
  a los 40 s: puedes tomarte el tiempo que necesites para elegir la carpeta sin
  que se descarte ni aparezca un error de conexión.
- El botón «Oír» (leer en voz alta la respuesta del asistente) vuelve a funcionar:
  la política de seguridad de la interfaz ahora permite reproducir el audio.

## 0.6.8

Auditoría pre-pilotos: el arnés y la app de escritorio quedan listos para la prueba
piloto. Solo arreglos; sin cambios que rompen ni migración de datos.

- El asistente ya no pierde la respuesta a mitad: un corte de red o del proveedor
  conserva el texto que ya viste en la ventana.
- Una conversación con Anthropic que tuvo un turno fallido ya no queda inservible:
  los mensajes se alternan correctamente y el siguiente turno funciona.
- El cerebro de la organización reintenta ante un fallo pasajero (429/5xx), igual
  que los demás proveedores.
- Las herramientas locales se cancelan de verdad: un comando colgado ya no traba
  el turno ni el botón de cancelar.
- La búsqueda de archivos avisa cuando hubo un error en vez de decir «sin
  coincidencias»; deshacer una escritura ya no traba el diario.
- Las menciones y tareas encoladas se ejecutan al terminar tu turno (la cola de
  despertar ya no se queda trabada). El timbre tiene plazo de conexión: un
  servidor que no responde no deja «conectando» para siempre.
- El detalle de metas tolera campos nulos del servidor (la pantalla ya no se cae).
  Subir un archivo de 25 MiB en conexión lenta ya no se corta a los 120 s.
- Login: un `expires_in` enorme del servidor ya no revienta el canje ni la
  renovación; cerrar sesión durante el canje ya no revive la sesión al próximo
  arranque; cambiar de organización limpia la conversación anterior.
- Actualizar muestra la versión nueva en «Agregar o quitar programas»; desinstalar
  ya no borra tus datos si no puede desinstalar; la descarga de actualización tiene
  plazo (un servidor colgado no deja el candado tomado). En Linux, «Añadir carpeta»
  acepta una ruta manual.
- La ventana del asistente tolera un fallo de sondeo sin apagarse; «Borrar todas»
  pide confirmación; los fallos de notas y salas ya no mienten el estado.
- El gate de tests comprueba de verdad en Windows (tests que se saltan solos o
  quedaban verdes sin comprobar nada, corregidos).

## 0.6.3

Arregla el llavero en Linux y la salida sin Administrador de tareas cuando se
pierde la conexión. Backend: sin cambios.

- Linux guarda el token del login OAuth, el token pegado a mano y la clave del
  agente en Secret Service cuando está disponible; si no lo está, cae en un
  archivo privado con permiso 0600. Antes esas tres entradas fallaban con
  «el llavero solo está implementado en Windows».
- `install.sh` detecta la biblioteca del indicador con `ldconfig -p` antes de
  copiar, además de revisar `ldd`.
- En Fedora 45 o posterior, `install.sh` avisa de `openssl3-libs`: el binario
  usa OpenSSL 3 y esa distro trae OpenSSL 4.
- Al perder la conexión, la ventana muestra «Sin conexión. Reconectando…»,
  reintenta a 4, 8, 15 y 30 segundos y ofrece «Reintentar ahora» y
  «Salir de Oruscribe» sin abrir el Administrador de tareas: el botón llama a
  `POST /api/quit`.
- Un 401 o 403 ya no se reintenta cada 60 segundos: son respuestas definitivas,
  no fallos de red.

La ventana negra de Linux se corrigió en 0.6.1 (`b2f8cfa`) y sigue corregida en
0.6.2 y 0.6.3. Quien venga de 0.6.0 tiene que actualizar: 0.6.0 no se parchea.

## 0.6.2

Cierra cinco fallos del borde que podían dejar la app abierta sin conexión o
gastar la cuota del plan sin necesidad.

- Linux vuelve a abrir después de un cierre forzado o de cerrar sesión: el
  candado de instancia es `flock` y el archivo viejo ya no bloquea.
- El interruptor de arranque con Windows cambia de verdad la entrada `Run` y el
  panel muestra su estado real; en Linux la fila no aparece.
- Los avisos en vivo actualizan las cifras del timbre sin recargar todas las
  listas ni pedir datos con la ventana oculta. El chat abierto, las alertas y
  las menciones se refrescan con un tope de 15 s.
- Si el timbre no puede escribir el latido o pasan 90 s sin tráfico, la app lo
  cierra y reconecta en vez de quedarse «Conectada» para siempre.
- El tope de 15 lecturas MCP cada 3 minutos cuenta solo las respuestas del
  backend: un error de conexión ya no deja al agente sin ventana al volver la
  red.

## 0.6.1

Corrige el arranque en Linux: la ventana ya no queda negra y el instalador avisa
de cualquier biblioteca que falte antes de copiar.

- El WebView se monta en el `GtkBox` que tao crea dentro de la ventana, no en la
  ventana: GTK rechazaba el segundo widget y quedaba solo el fondo del tema.
- `install.sh` revisa todas las líneas `ldd ... => not found`, las lista sin
  repetir y no copia nada si falta alguna.
- CI abre la app en Xvfb y falla si la ventana queda plana o el log trae el
  aviso de GTK; el `.tar.gz` no se publica con una ventana vacía.

## 0.6.0

La app pasa de Go a Rust y retira el árbol Go de `master`. Esta versión publica
para Windows y Linux.

- El shell, la API, el arnés, las herramientas y la instalación viven en
  `crates/`; `frontend/dist` se conserva como interfaz embebida del servidor.
- El arranque restaura la sesión guardada: la app sale de `connecting` y el
  intro desaparece, en vez de quedarse en el splash.
- El firmante de `latest.json` es Rust (`signmanifest`) y verifica la firma
  Ed25519 con las mismas llaves públicas.
- CI, Dependabot y `scripts/install.ps1` usan Cargo; se retiran los flujos Go.
- El historial Go queda en el tag `legacy/go-0.5.4`.
- El timbre en vivo se cablea de punta a punta: el borde conecta el websocket,
  reparte cada aviso al long poll y mantiene el estado «Conectado» en Ajustes.
- El tope de lecturas MCP (15 llamadas por herramienta en 3 minutos) pasa a ser
  compartido por todos los turnos, para que un mensaje nuevo no reinicie la
  ventana de sondeo. La publicación de contadores ya no se solapa.
- Linux abre ventana con GTK3 y WebKitGTK 4.1, guarda la instancia única con un
  archivo de bloqueo y se publica como `tar.gz` con `install.sh`; CI compila y
  prueba los dos sistemas.
- Las fotos de perfil se muestran también cuando vienen como ruta del mismo
  origen o en base64, y el chat abre la primera sala al entrar en vez de quedar
  vacío. Los mensajes se repintan al editarse y el foco sondea como máximo una
  vez por minuto, para no quemar la cuota del plan.

## 0.5.4

La capa de accesibilidad y los estados de fallo dejan de ser decorativos. La
auditoría de la base quita pérdidas silenciosas de datos y de interfaz.

- La salida del arnés sin ventana ya no se pierde en la consola: con
  `Oruscribe Agente run --json > eventos.jsonl` el archivo recibe los eventos
  y `echo pregunta | run` llega entero; `undo` y `rules` vuelven a verse en la
  terminal. La consola solo se toma cuando el sistema no dio manejadores.
- Con `run --json`, un error temprano (sin proveedor configurado) sale como
  evento JSON en el stream, no como texto plano que rompe al consumidor.
- El `install.sh` del tar.gz se prueba en CI con sus costuras: comando exacto
  por familia (pacman, apt, dnf), mensaje genérico en distros desconocidas y
  desinstalación limpia.

- La conversación con IA se llama **Chat** en las dos lenguas, pesa más en la barra y tiene un botón flotante que la abre desde cualquier pantalla.
- El modelo se elige de la lista publicada para el proveedor, con enlace al catálogo; si el catálogo no responde, el campo sigue libre.
- Con **Mi clave** se adjuntan hasta cuatro documentos de texto por mensaje (32 KB cada uno, 64 KB en total); se marcan con su nombre para que el modelo sepa dónde empiezan y terminan.
- Con **Mi clave** también se adjuntan hasta cuatro imágenes por mensaje (2 MB cada una, 6 MB en total), con vista previa y reducción en la ventana. OpenAI y Anthropic las reciben en su formato; el cerebro de la organización avisa que no recibe imágenes. Adjuntar una imagen funciona al arrancar, sin abrir Ajustes primero.
- El chat dicta y lee respuestas con un servidor de voz compatible con OpenAI que la persona configura en su equipo (whisper.cpp, LocalAI u openedai-speech). La app no empaqueta modelos y el audio no sale de esa dirección.
- Los adjuntos del chat del equipo se abren dentro de la ventana: imagen, PDF o texto, sin salir de la app, con su tipo y un enlace de descarga que sigue disponible si la vista previa falla.
- La primera vez que la app está instalada pregunta si debe arrancar con el equipo; la respuesta se guarda. Si hay una versión nueva, avisa al arrancar y se instala desde la misma ventana.
- La IA de la organización sigue oculta y bloqueada para un miembro normal: proveedor, modelo, clave, modo y carpetas solo aparecen con **Mi clave**. El backend ignora y conserva esos ajustes cuando el cerebro es de la organización, y no guarda una clave aunque la petición la traiga.
- Un cuerpo vacío ya no puede borrar: desinstalar conserva los datos salvo un `keepData:false` explícito, borrar conversaciones exige `id` o `all`, y deshacer exige un número. El desinstalador silencioso de Windows ya no borra la carpeta de datos.
- Cerrar sesión borra el token antes que las conversaciones; la cola del despertar se limpia y una ejecución vieja ya no puede cerrar el despertar de la sesión nueva.
- El arranque de Linux reconoce la instalación hecha por el gestor de paquetes; la copia del binario ya no se corta en silencio si falla la lectura; si la carpeta del sistema no se puede escribir, la actualización cae en la copia por usuario.
- Cerrar la ventana deja la app en la bandeja con el agente y el despertar vivos; solo **Salir** termina el proceso. macOS ya no cierra la app al cerrar la última ventana.
- Las notas locales se separan por cuenta del mismo equipo; las notas viejas se adoptan una vez sin perderlas. La copia local de la primera pantalla se recorta para no llenar el almacenamiento.
- Escribir en el bloc exige sesión: una petición sin cuenta ya no puede reescribir el archivo y borrar las notas del formato viejo que faltaban adoptar.
- Guardar ajustes, el agente o las reglas recordadas parte de lo que hay en disco y escribe bajo un solo candado: un guardado simultáneo ya no puede pisar el cambio del otro con una copia vieja.
- El tope de tokens cierra las llamadas a herramientas antes de cortar, así el turno siguiente no queda inválido.

- El toast y la pill de conexión anuncian su contenido a lectores de pantalla (`role="status"`), y el menú declara el ítem activo con `aria-current`.
- Los atributos visibles —menú, recargar, tema, controles del mapa, búsquedas, adjuntar, emoji— se traducen al cambiar a inglés; el aria-label del menú ya no se queda en el idioma del arranque.
- Enter y Espacio activan filas de tarea, tarjetas kanban y nodos del mapa.
- Si la actualización falla, un aviso único con Reintentar lo dice; la vista del agente nunca queda en blanco si su primera carga falla; Notas muestra esqueleto y error con salida; el Mapa con cero datos muestra mensaje.
- Adjuntar archivo en el chat conserva el texto escrito si la subida falla; el fallo de estado de tarea avisa con toast, no con `alert`.
- Aprobar una acción del agente exige dos clics (armar y confirmar; Escape o seis segundos desarma) y la decisión se toma en la vista del agente, no desde el inicio.
- Todos los modales reciben el foco al abrir, lo atrapan con Tab y lo devuelven al control que los abrió.
- El token `--soft` existe en los dos temas; el atajo Ctrl+K sube contraste; el splash respeta movimiento reducido también en sus temporizadores.
- Cerrar sesión borra el temporal que puede traer el token; el diario y el instalador llevan sus respaldos al disco antes de reemplazar.
- El arnés sin ventana ya no manda la clave guardada a un `--base-url` distinto, y la subida de archivos exige https (http solo en loopback).
- Un argumento de herramienta inválido ya no rompe el guardado de la conversación, y una respuesta cortada por el proveedor se avisa en vez de presentarse completa.
- Las validaciones de la API contestan 400; un doble clic no duplica tareas ni avisos; los campos de token se ocultan en pantalla.
- El cerebro de la organización vuelve a compactar las conversaciones largas en vez de crecer sin tope.

## 0.5.3

Las máquinas de estado que ya estaban en la app quedan escritas y con guardas.
La sección Agente deja probar el proveedor antes de guardarlo, y las fotos de
perfil sobreviven a una actualización o a un arranque lento.

- `docs/maquinas-de-estado.md` reúne las catorce máquinas que estaban implícitas, cada una con sus estados, sus guardas y su prueba.
- Guardar la configuración mientras el agente contesta devuelve un error claro en vez de dejar la conversación con un modelo que no fue el que respondió.
- Cerrar sesión no deja una ejecución colgada; la sesión nueva manda su primer mensaje sin esperar el desenrollo de la anterior.
- Dos clics en "Actualizar" ya no escriben el mismo archivo a medias.
- Catálogo de proveedores al día con pi: direcciones nuevas de OpenCode, OpenCode Go y Kimi, y Qwen Token Plan individual de China.
- "Probar conexión" en Ajustes del agente hace una llamada mínima con lo escrito, dice si funciona y cuánto tardó; no guarda nada ni devuelve la clave.
- Las fotos de perfil se guardan por organización en la carpeta de datos; un fallo conserva las caras que ya estaban y la ventana reintenta unos 66 segundos.
- Si la sesión se importó con token del MCP, no puede leer fotos nuevas; el diagnóstico lo dice en vez de fallar en silencio.

## 0.5.2

La sala abierta ya no se queda congelada cuando el timbre no avisa, los avisos
aparecen al enviarlos y cerrar y abrir sesión no duplica los refrescos.

- La sala abierta y la lista de salas se repasan cada 10 segundos y al recuperar el foco: si el aviso en vivo se pierde, la conversación sigue viva.
- El aviso que envías aparece de inmediato en Avisos, con su contador.
- Cerrar y abrir sesión deja un solo oyente del foco: un foco ya no dispara varias peticiones iguales.
- Las propiedades del `.exe` muestran la versión real, no `0.1.0`.
- El arnés suma su sistema inmunológico: todas las rutas HTTP, las pantallas y las escrituras quedan bajo comprobación.

## 0.5.0

La ventana se ordena como arnés: primero lo que necesita una decisión, después el día, el agente y la organización.

- La entrada abre con "Nada urgente", "Tu día", "Tu agente" y el pulso de la organización. Se retira la barra de estado que mezclaba escalas y el aviso de instalación pasa a un botón pequeño.
- `Ctrl+K` abre una paleta que busca pantallas y acciones, con una línea que explica cada una.
- Los nombres ambiguos cambian: Cerebro pasa a Mapa, Puesta al día a Resumen y Equipos a Personas.
- El Agente muestra el cerebro y el modo, las sesiones guardadas a un lado, un estado vacío con sugerencias y el compositor fijo con el botón de enviar visible.
- Las fotos de las personas se refrescan cuando llegan y las iniciales quedan de respaldo. La actividad también muestra los rostros.
- El Mapa gana leyenda de colores y una línea de uso.
- Avisos usa "Desvío del plan" y "Logro", y ya no muestra claves de traducción crudas.
- El seguimiento etiqueta las fechas "Desde/Hasta" y muestra "Sin fecha" cuando están vacías.
- Los ajustes del agente se agrupan en Cerebro, Permisos y Autonomía, y esconden lo que no aplica al cerebro de la organización.
- La bienvenida explica en tres líneas qué hace el arnés y que no hace falta instalar OpenCode, Node ni nada aparte.
- Windows se instala con `Oruscribe-Agente-Setup-0.5.0.exe`: no pide administrador, crea los accesos y ofrece abrir la app al terminar. Linux usa los paquetes nativos, que instalan GTK y dejan el lanzador en el menú.
- Para Linux sin paquete o sin sudo, la release trae `oruscribe-agente-0.5.0-linux-x86_64.tar.gz`: conserva el permiso de ejecución y su `install.sh` deja la app en la cuenta con acceso en el menú.
- Los paquetes de Linux validan sus metadatos AppStream y refrescan el menú al desinstalar.

## 0.4.5

El agente de la ventana y el CLI comparten el motor.

- El CLI usa las herramientas de Oruscribe (tareas, chat, metas, avisos) con la
  sesión que la app ya guardó, además de las locales, `AGENTS.md`, skills y
  `task`. Si no hay sesión, sigue con las locales y lo dice; `--no-org` apaga
  las de Oruscribe.
- La ventana carga `AGENTS.md` y `ORUSCRIBE.md`, skills y la herramienta `task`
  cuando el cerebro es "Mi clave". La raíz es la primera carpeta permitida; sin
  carpetas, el hogar.
- `internal/mcptools` concentra las herramientas MCP, las prohibidas y las de
  lectura; `internal/subagent` concentra `task`; `internal/sessions` concentra
  el formato y el guardado de sesiones. La ventana y el CLI ya no tienen copias
  separadas de esas listas.
- Las conversaciones viejas de la ventana se migran al formato compartido sin
  perder mensajes.
- El README usa el repositorio de descargas correcto
  (`oruscribe-agente-descargas`) en el ejemplo del manifiesto.

## 0.4.4

El arnés se puede usar sin la ventana.

- `Oruscribe Agente run "..."` corre el agente en la terminal con las
  herramientas locales de la carpeta actual. No abre la ventana ni necesita
  bibliotecas gráficas.
- Las sesiones se guardan en la carpeta de datos con permiso `0600`, se reanudan
  con `--session` o `--continue` y se listan con `--list-sessions`.
- El arnés lee `AGENTS.md` y `ORUSCRIBE.md` desde la raíz hasta la carpeta de
  trabajo, primero el global, con topes de tamaño y sin seguir enlaces.
- Las skills de `SKILL.md` se leen de `.agents/skills`, `skills`,
  `~/.agents/skills` y la carpeta de datos; la herramienta `skill` las lista y
  carga, y una skill del proyecto pisa a una global con el mismo nombre.
- La herramienta `task` delega una exploración de solo lectura a un subagente
  con contexto limpio, sin recursión y con tope de pasos.
- `--json` emite un evento JSONL por paso (`start`, `delta`, `tool_call`,
  `tool_result`, `denied`, `answer`, `usage`, `done`, `error`).
- En `ask`, las escrituras piden permiso en la terminal y las ejecuciones lo
  piden siempre salvo `--allow-exec`; sin terminal se niegan. `--yes` aprueba
  escrituras y no ejecuciones.
- En Windows, el ejecutable gráfico se engancha a la consola del proceso padre
  para que `run` pueda imprimir.

## 0.4.3

Primera versión con actualizaciones firmadas y aprobación interactiva del agente.

- El manifiesto de actualización se verifica con firma ed25519 antes de leerlo.
  Las llaves públicas van embebidas; la privada nunca entra al repositorio.
- `cmd/signmanifest -verify` comprueba un manifiesto publicado contra una llave
  pública determinada.
- El script de publicación ya no puede saltarse la comprobación de las llaves
  de firma, ni con `-SkipTests`.
- El agente arranca en "Pedir permiso": las escrituras esperan aprobación en
  pantalla, con un plazo de cinco minutos que cuenta como no. "Solo lectura" y
  "Autónomo" siguen disponibles.
- El agente muestra la respuesta mientras llega (streaming) y corta la conexión
  al cancelar.
- Las conversaciones se guardan en la carpeta de datos con permiso `0600`, se
  listan en la pantalla, se borran de a una o todas, y se borran al cerrar
  sesión. La clave del proveedor se conserva.
- Las conversaciones largas se resumen con el propio modelo y la pantalla mide
  el consumo de tokens.
- El agente puede usar herramientas locales solo si eliges carpetas permitidas:
  listar, leer y buscar archivos dentro de ellas, crear o editar archivos y
  ejecutar programas de una lista. Toda ruta se resuelve a su ruta real (enlaces y
  junctions incluidos) y se rechaza si sale de las carpetas; los comandos van sin
  shell, con entorno limpio y tope de tiempo, y al vencer se mata el árbol de
  procesos. Ejecutar siempre pide aprobación, también en modo Autónomo.
- "El agente trabaja solo": el timbre puede despertarlo con menciones, tareas
  asignadas y alertas. Una ejecución a la vez, sin repetir eventos ni responder a
  lo que escribe la propia persona, con topes por minuto, por día y de tokens, y
  con registro visible.
- La pantalla de inicio pasa a llamarse "Hoy" y muestra arriba las aprobaciones
  pendientes y lo que el agente hizo solo en el día.
- El timbre no arranca una conexión nueva para la misma sesión y limita las
  reaperturas por minuto.
- El comentario del paquete de Linux nombra las distribuciones medidas.
- Se quita el flag `--disable-gpu` de WebView2: medido, el arranque no cambia, la
  memoria sube unos 65 MB y el CPU en reposo baja; la razón quedó en el código.
- Registro local con rotación que tacha claves y tokens y no guarda mensajes, y
  botón "Copiar diagnóstico" con versión, sistema y últimos errores, sin datos
  personales.
- El CI fija las acciones de GitHub por commit, genera un SBOM, comprueba que
  dos compilaciones dan el mismo hash y corre `govulncheck` cada semana.
  Dependabot vigila Go y las acciones.
- El agente no ve ni puede llamar las herramientas que devuelven un secreto o
  cambian la cuenta entera (`create_agent`, `get_my_doorbell`, `invite_member`,
  `switch_org`, `create_organization`, `move_task_to_org` y `move_goal_to_org`),
  en ningún modo.
- Crear o editar un archivo con la herramienta del agente ya no puede dejarlo
  vacío ni a medias si falla el cambio de permisos o se llena el disco: el
  original queda intacto y el error llega al agente.
- Buscar texto en una carpeta respeta Cancelar y el tope de tiempo de la
  herramienta.
- En Linux y Mac, cada comando del agente recibe su propia carpeta `HOME`, que se
  borra al terminar, en lugar de `/tmp`, que cualquiera del equipo puede escribir.
- Una conversación guardada ya no se pierde si falla la escritura del archivo
  temporal.
- La carpeta de datos se crea con permiso `0700`.
- El selector de proveedores usa un catálogo real: OpenAI, xAI y los demás
  servicios que hablan chat completions aparecen disponibles, y los que el
  arnés todavía no habla explican por qué.
- El agente reintenta los errores pasajeros del proveedor (429 y 5xx) con una
  espera acotada, sin repetir el texto que ya se mostró en streaming.
- La ventana no se bloquea esperando los avatares: se piden en segundo plano,
  con caché de cinco minutos y un reintento por minuto tras un fallo.
- La conexión con Oruscribe reintenta tres veces los fallos de red antes de
  darse por vencida, y el timbre espera con una variación para que todos los
  equipos no reconecten al mismo tiempo.
- El grafo de la organización se dibuja sin simulación, sin nodos encimados y
  con los enlaces entre equipos, tareas, metas y personas.
- En Linux, la entrada `.desktop` se corrige si quedó apuntando al binario
  viejo después de una actualización.
- "Copiar diagnóstico" sigue funcionando cuando el portapapeles del sistema
  no está disponible.

## 0.4.2

- El agente se conecta a la app: clave en el llavero, herramientas de Oruscribe
  con lista explícita de lectura, solo lectura por defecto, sin mandar la clave
  a otro proveedor y borrando la conversación al cerrar sesión.
- Pantalla "Agente": conectar un modelo, conversar, ver los pasos de
  herramientas y avisos de lo que se envía al proveedor.
- El binario suelto de Linux explica qué bibliotecas faltan y con qué comando
  instalarlas, también en una ventana.
- CI con detector de carreras y paquete instalado en seis distribuciones.
