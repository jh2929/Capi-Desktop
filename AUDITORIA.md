# Auditoría técnica de Capi Desktop

- **Repositorio:** `/home/emixdy/Proyectos/Capi` (`master` @ `bba526d`, release `v0.1.9`)
- **Fecha:** 2026-10-25
- **Alcance:** daemon Kotlin (`capi-core`), librerías `innertube`/`lrclib`, backend Tauri/Rust, frontend React/TS, build, CI/CD, packaging y documentación.
- **Método:** lectura estática completa de fuentes, `git ls-files`/historial, `npx tsc --noEmit` (0 errores), `cargo check --all-targets` (0 errores/0 warnings), `cargo clippy` (11 warnings), `npm audit` (4 high + 1 low), API de GitHub/AUR y verificación de semántica de Tauri 2.11 en el código de la crate.
- **No se modificó ninguna fuente.**

## Veredicto general

El producto funciona y la arquitectura (Tauri + React + daemon nativo Kotlin) es coherente con el objetivo de bajo consumo. La deuda principal no es de funcionalidad sino de **seguridad del servidor local**, **robustez del ciclo de vida del daemon**, **gestión de estado/errores en el frontend** y **proceso de release**. Hay dos hallazgos que considero bloqueantes antes de distribuir otra versión: el proxy local expuesto a la LAN y el reinicio del daemon que no reemplaza el estado (la app no se recupera y genera procesos huérfanos).

---

## Top 12 riesgos

| # | Riesgo | Severidad |
|---|--------|-----------|
| 1 | Proxy HTTP local en `0.0.0.0`, sin auth, con control de reproducción, SSRF y lectura de audio arbitrario | Crítico |
| 2 | `restart_daemon` no reemplaza el estado en Tauri 2 → recuperación rota + daemons huérfanos | Crítico |
| 3 | El daemon **no termina al cerrar stdin** (coroutine de refresh infinita) y deja procesos huérfanos | Crítico |
| 4 | Daemon secuencial de hasta 19 clientes excede el timeout de 25 s → se mata a sí mismo en bucle | Crítico |
| 5 | `borrar_cancion`, `cache_audio` y `/play?path=` aceptan rutas/IDs arbitrarios (borrado, escritura y lectura fuera de sandbox) | Crítico |
| 6 | Descargas que se corrompen silenciosamente (200 tras Range, streams truncados) y no se pueden cancelar | Alto |
| 7 | Frontend: `JSON.parse` sin protección en el arranque → pantalla blanca irrecuperable | Crítico |
| 8 | Carrera en `playTrack`: respuestas viejas pisan la canción actual | Crítico |
| 9 | Versiones desincronizadas (tag 0.1.9 / instalador 0.1.4 / manifiestos 0.1.0) y AUR roto (MIT, source 404) | Crítico |
| 10 | Cualquier `println` de librería/log contamina stdout y desalinea el protocolo sin recuperación | Crítico |
| 11 | 61 MB de binario versionado y ~704 MB de historial Git | Alto |
| 12 | Cero tests, cero lint/format y ausencia de CI de calidad; release automático sin firma ni gates | Alto |

---

## 1. Críticos

### C-01. Servidor HTTP local expuesto, sin autenticación y con capacidades peligrosas
- **Dónde:** `capi-desktop/src-tauri/src/lib.rs:68,70` (bind `0.0.0.0:12761-12770`), `:286-354` (`/api/play-pause`, `/api/next`, `/api/prev`, `/api/volume`, `/api/seek`, `/api/state`), `:356-469` (`/image?url=`, `/play?url=`), `:470-505` (`/play?path=`), CORS `*`.
- **Problema:** el proxy escucha en todas las interfaces y no exige token. Cualquier dispositivo de la LAN puede controlar la reproducción, leer el estado (incluida `stream_url` firmada) y usar la máquina como proxy HTTP (SSRF contra routers, servicios en `localhost`, metadata cloud). Además, cualquier web abierta en el mismo equipo puede hacer peticiones a `http://127.0.0.1:12761/api/...` sin preflight (GET) y alterar la reproducción.
- **Impacto:** crítico de seguridad y privacidad. Es el vector más grave del proyecto y se distribuye activamente en las releases.
- **Fix:** bind a `127.0.0.1`; token aleatorio por sesión en todas las rutas (incluido `/play`, `/image`, `/api`, `/tv`); mutaciones por `POST`; validar `Origin`; CORS restringido; allowlist de hosts (`*.googlevideo.com`, `*.ytimg.com`) y bloqueo de IP privadas/loopback tras resolver DNS; timeouts totales y límite de bytes. Para cast, levantar un listener LAN **solo** cuando el usuario active "Recibir en TV", con token en la URL y caducidad.

### C-02. `restart_daemon` no reemplaza el estado gestionado (Tauri 2)
- **Dónde:** `lib.rs:795-813` (`restart_daemon`), `:824-830` y `:1046-1050` (reintentos que leen `app.state`), `:2303-2333` (watchdog).
- **Problema:** `restart_daemon` hace `app.manage(Arc::new(daemon))` y `app.manage(Arc::new(Mutex::new(child)))` sobre tipos ya gestionados. En Tauri 2.11/2.12 `StateManager::set` **no reemplaza** si el tipo ya existe (devuelve `false`; verificado en `tauri-2.11.2/src/state.rs:118-133`). El estado sigue apuntando a los pipes del daemon muerto, mientras el proceso nuevo queda vivo pero inaccesible.
- **Impacto:** tras el primer timeout/caída, todos los comandos fallan para siempre; el watchdog reintenta cada 30 s y crea un daemon huérfano por intento. La app solo se recupera reiniciándola por completo.
- **Fix:** gestionar una sola vez un `AppState { daemon: RwLock<Option<DaemonProcess>>, child: Mutex<Option<Child>>, restarting: Mutex<()> }`; intercambiar bajo lock, matar/reapear el anterior y serializar reinicios. Probar con un test de integración que mate el daemon y verifique que el siguiente comando funciona.

### C-03. Resolución de stream secuencial incompatible con el timeout de 25 s
- **Dónde:** `capi-core/src/main/kotlin/com/arturo254/capi/core/Main.kt:283-345` (`STREAM_CLIENTS`, 19 clientes) y `:541-560`; `lib.rs:676-716` (timeout global de 25 s que mata el daemon).
- **Problema:** se prueban hasta 19 clientes de YouTube en serie, cada uno con timeouts de 15 s y reintentos. Un vídeo con restricciones puede tardar minutos; al superar 25 s Rust mata el daemon. Eso dispara el reinicio roto (C-02) y convierte un fallo de reproducción en la pérdida total del backend.
- **Impacto:** reproducciones que dejan la app inservible; CPU/red y procesos desperdiciados.
- **Fix:** presupuesto total (p. ej. 12 s) con `withTimeout`; probar clientes en paralelo con `select`/`coroutineScope` y cancelar el resto; cachear el cliente/format ganador por vídeo; devolver un error tipado rápido en lugar de agotar clientes; alinear el timeout de Rust y no matar el proceso por una operación lenta.

### C-04. Rutas y IDs sin sandbox: borrado, escritura y lectura arbitrarias
- **Dónde:** `lib.rs:1028-1035` (`borrar_cancion` borra cualquier ruta), `:470-505` (`/play?path=` acepta cualquier archivo con extensión de audio, y `starts_with(&capi_dir)` es léxico, no canónico), `:1978-1996` (`cache_audio` concatena `track_id` sin sanear → `../`), `:1817-1824` (`listar_archivos_locales` lista cualquier directorio).
- **Problema:** el frontend puede enviar cualquier ruta (y una web/LAN puede llamar al proxy). Un `track_id` malicioso escribe fuera del directorio de caché; una ruta con `../` o un symlink escapa de `capi_dir`; `borrar_cancion` elimina archivos arbitrarios del usuario.
- **Impacto:** pérdida de datos y exfiltración/escritura arbitraria. Combinado con C-01, explotable desde la red local.
- **Fix:** canonicalizar con `std::fs::canonicalize` y exigir prefijo real dentro de raíces permitidas (appdata y carpeta elegida); hash/validar `track_id` (`^[A-Za-z0-9_-]{6,20}$`); no permitir `..`; separar "biblioteca local" de "descargas" con raíces explícitas; para audio local, registrar previamente los archivos escaneados y servir solo por ID, nunca por ruta.

### C-05. Frontend: `JSON.parse` sin protección en el arranque
- **Dónde:** `capi-desktop/src/App.tsx:341-344, 431-445, 446-457, 458-462, 465-467, 670-678`; `main.tsx` sin ErrorBoundary.
- **Problema:** cualquier valor corrupto o de una versión anterior en `capi_hidden_artists`, `capi_favorites`, `capi_playlists`, `capi_history`, `capi_downloads`, `capi_downloaded_metadata`, `capi_listening_stats` o `capi_playlist_covers` lanza durante el primer render. No hay boundary ni recuperación: pantalla blanca y el usuario debe borrar el almacenamiento a mano.
- **Impacto:** app inutilizable por un fallo trivial (cuota, cierre abrupto, migración, edición manual, backup defectuoso).
- **Fix:** `safeJsonParse(raw, validator)` con `zod`/validadores manuales y valores por defecto; versionado de esquema y migraciones; `ErrorBoundary` global con botón "restablecer datos"; envolver todos los `setItem` en try/catch (cuota) y mover historiales grandes a IndexedDB.

### C-06. Carrera en `playTrack`: una petición vieja pisa la canción actual
- **Dónde:** `App.tsx:2134-2350` (`invoke("obtener_stream")` en `2247`, `audio.src`/`play()` en `2312-2339`).
- **Problema:** no hay token de petición. Con clics rápidos (o preload + clic), la respuesta de la canción A puede llegar después de seleccionar B: se hace `play()` de A cuando `currentTrack` ya es B, `setIsPlaying(true)`/`setBuffering(false)` de promesas obsoletas y el `AbortError` de `play()` abre un `DiagnosticModal` falso.
- **Impacto:** se reproduce la canción equivocada, estado de UI inconsistente y diagnósticos engañosos; muy fácil de reproducir.
- **Fix:** `playReqIdRef.current++` al inicio y comprobar el token antes de cada `setState`, `audio.src` y `play()`; ignorar `AbortError`; cancelar/invalidar el preload de la pista anterior.

### C-07. Versiones y release desincronizados
- **Dónde:** `tauri.conf.json:4` (0.1.4), `package.json:4`/`Cargo.toml:3` (0.1.0), `PKGBUILD:5`/`.SRCINFO:3` (0.1.0), `main.rs:4` (v0.1.1), pantalla "Acerca de" `App.tsx:6425` ("1.0.0").
- **Problema:** el tag `v0.1.9` publica instaladores `Capi_0.1.4_*`, los manifiestos dicen 0.1.0 y la UI 1.0.0. El workflow no deriva ni valida la versión desde el tag.
- **Impacto:** soporte, telemetría, updater y AUR imposibles; releases formalmente incorrectas desde v0.1.5.
- **Fix:** fuente única de verdad (p. ej. `package.json`), script de sincronización/`tauri build --config`, guard en CI (`tag == versión`), mostrar la versión real desde `package_info` (ya existe `obtener_info_sistema`).

### C-08. Paquete AUR roto y licencia contradictoria
- **Dónde:** `PKGBUILD:10` (`MIT`), `:25` (source `v0.1.0-linux`, no existe → 404), `:5-6` (0.1.0-3), `.SRCINFO` (0.1.0-1, otro checksum), `:32-36` (instala en `/usr/lib/capi/bin`, que la app no consulta).
- **Problema:** el paquete no se puede construir, declara MIT cuando el proyecto es GPL-3.0 y, aun con source válido, el daemon quedaría en una ruta no resuelta por `get_binary_path`.
- **Impacto:** instalación Linux inexistente y contradicción legal.
- **Fix:** source real o tarball estable generado en CI; `license=('GPL-3.0-only')`; versión/checksum sincronizados; layout compatible (`externalBin`/`/usr/lib/Capi`); `LICENSE` incluido; probar `makepkg -sir` en Arch.

### C-09. El daemon no termina al cerrar stdin (procesos huérfanos)
- **Dónde:** `capi-core/src/main/kotlin/com/arturo254/capi/core/Main.kt:490-508` (refresh infinito `launch { while (true) { delay(30m) ... } }`) y `:514-517` (`return@runBlocking` al EOF).
- **Problema:** `runBlocking` no retorna hasta que terminan sus hijos; la coroutine de refresh nunca termina, así que `return@runBlocking` no cierra el proceso. Reproducido con el binario existente: tras “stdin closed, exiting” el proceso seguía vivo a los 20 s (`timeout` → exit 124). Rust no mata al hijo al terminar (`Child` no tiene `kill_on_drop`).
- **Impacto:** cada cierre de la app deja un daemon consumiendo RAM/red; al reabrir se acumulan procesos; el daemon viejo puede seguir usando la red y compitiendo por cuotas.
- **Fix:** guardar los `Job` (refresh/prefetch), cancelarlos en `finally` con `cancelAndJoin()` y llamar `exitProcess(0)` al EOF; en Rust, matar y `wait()` al hijo en `RunEvent::Exit`/`Drop`; test de integración: EOF ⇒ salida en <2 s.

### C-10. stdout contaminado: cualquier log rompe el protocolo para siempre
- **Dónde:** `Main.kt:409-411` y `Innertube.kt:70-72` (serializers a stdout con variables de entorno), `YouTube.kt:939, 985`, `InnerTube.kt:596`, y el `printError` del catch global (`Main.kt:925-934`).
- **Problema:** el protocolo es “una línea JSON de stdout = una respuesta”, sin id ni framing. Con `OPENTUNE_DEBUG=1` se emiten 63 líneas antes de `{"status":"ready"}` (reproducido) y el arranque falla al leer la primera. Un `println` en mitad de sesión desplaza todas las respuestas una posición y no hay forma de resincronizar; `printError` en el catch global también escribe JSON de error a stdout antes de morir.
- **Impacto:** daemon inarrancable bajo flags de diagnóstico y corrupción total del canal si una ruta de librería imprime; errores imposibles de diagnosticar.
- **Fix:** stdout **exclusivamente** para JSON de protocolo; logs a stderr con logger y niveles; eliminar prints de librería; añadir `id` de petición y test que verifique que toda línea de stdout es JSON válido.

---

## 2. Altos

### Seguridad

**A-01. Proxy SSRF y lectura de audio sin allowlist.**
`lib.rs:356-505`. `/image` y `/play` hacen GET a cualquier URL http(s), sin allowlist, sin timeout total (solo `connect_timeout` de 8 s) ni límite de bytes; pueden alcanzar servicios internos (`127.0.0.1`, `192.168.x.x`, `169.254.169.254`) y reflejar contenido. **Fix:** allowlist de dominios, bloquear IP privadas/loopback tras resolver DNS, sin redirects automáticos, timeout total y `max_bytes`.

**A-02. `Range` con underflow y `Content-Length` incorrecto.**
`lib.rs:500` (`file_size - 1` con archivo vacío), `:521` (`range_end - range_start + 1` con rangos invertidos/fuera de límite), `:431-433` (`Content-Length: 0` si el upstream no la envía) y `:528-529` (todo lo que no es `.m4a` se sirve como `audio/webm`, incluidos mp3/wav/ogg/flac/aac). **Impacto:** pánico del hilo en debug, cuerpos truncados o colgados, reproducción local rota según el formato. **Fix:** aritmética comprobada, clamp a `file_size-1`, 416 para rangos inválidos, omitir `Content-Length` si no existe (o chunked), mapa MIME real.

**A-03. `cache_audio` sin estado y con carreras.**
`lib.rs:1978-2046`. Devuelve un archivo existente aunque sea parcial, no valida el status HTTP, escribe directamente sobre el destino final y guarda metadata sin lock ni escritura atómica; `get_cached_audio` borra el archivo pero no la entrada (`:1964-1970`); `set_cache_config` acepta `max_bytes=0`. **Impacto:** audio corrupto servido como válido, JSON de metadata truncado, archivos huérfanos. **Fix:** descargar a `.part`, validar `2xx`/tamaño, `rename` atómico, lock, TTL y limpieza coherente.

**A-04. `borrar_cancion` y `deleteLocalTrack` confían en la ruta persistida.**
`lib.rs:1028`, `App.tsx:2461-2474`. Ver C-04. **Fix:** endpoint `borrar_descarga(track_id)` que resuelva la ruta en backend y valide que está bajo `appdata/Capi`; nada de rutas del frontend.

**A-05. Hijacking del binario del daemon.**
`lib.rs:588-670` busca primero `cwd/bin/capi-core`, luego una ruta absoluta del desarrollador `/home/emixdy/Proyectos/Capi/...` (sin `debug_assertions`). **Impacto:** un `bin/capi-core` malicioso en el directorio de trabajo se ejecuta antes que el binario empaquetado; además filtra información privada. **Fix:** en release, solo `resource_dir`/`current_exe` verificados; en dev, `CAPI_CORE_PATH` bajo `cfg(debug_assertions)`; eliminar la ruta absoluta y el `get_binary_path` duplicado en `po_token.rs`.

**A-06. Cast sin validación y siempre "exitoso".**
`lib.rs:1506-1708` termina con `Ok(true)` aunque todos los protocolos fallen; usa `control_url` del frontend y `LOCATION` de SSDP para hacer POST/GET sin validar esquema/host (SSRF); el descubrimiento lanza cientos de peticiones con 3 s cada una. `castManager.ts:124-168` fija `connected` tras 500 ms sin comprobar el resultado y no pausa el audio local al transmitir (suenan ambos). **Fix:** devolver error real, validar IP privada y esquema, timeout global, estado `error`, pausar el reproductor local al conectar y reanudar al desconectar.

**A-07. CSP demasiado laxa e inoperante para fuentes.**
`tauri.conf.json:21` permite `img-src https:` y `connect-src https:` (exfiltración si hay XSS), `http://127.0.0.1:*` (sondeo de puertos), y no define `object-src`, `base-uri`, `frame-ancestors` ni `form-action`. A la vez, `index.html:8-9` carga Google Fonts que la propia CSP bloquea (no hay `style-src`/`font-src` externos). **Fix:** CSP por allowlist, `font-src 'self'`, fuentes empaquetadas, `nosniff`; `connect-src` solo al puerto del proxy y dominios necesarios.

**A-08. `GITHUB_TOKEN` con escritura en todo el workflow y acciones sin pin.**
`.github/workflows/desktop-release.yml:3-4`, 23 `uses` por tag, `persist-credentials` por defecto. **Impacto:** cualquier acción/dependencia comprometida puede publicar releases. **Fix:** `contents: read` global y `write` solo en `release`; pin a SHA; `environment` con revisores; `persist-credentials: false`.

### Correctitud y estabilidad

**A-09. Arranque del daemon sin timeout y sin limpieza del hijo.**
`lib.rs:718-787`. `spawn_daemon_process` bloquea en `read_line` esperando `"ready"` sin timeout; el daemon tarda hasta 20 s en `visitorData` más la generación de tres PO tokens antes de escribir `ready` (`Main.kt:449-487`). Si falla, retorna sin `kill()`/`wait()`. **Impacto:** la ventana no aparece (setup bloqueado) o queda un proceso huérfano; en Linux el error es silencioso. **Fix:** handshake con timeout, `kill`+`wait` en error, no bloquear el setup (spawn asíncrono con estado "iniciando"), y exponer el error en UI.

**A-10. Timeout global que mata al daemon por comandos en cola.**
`lib.rs:676-716`. El timeout de 25 s empieza antes de adquirir los locks de stdin/stdout; un comando lento hace que otro en espera expire y mate el daemon sin haber enviado nada. `spawn_blocking` no se cancela. **Impacto:** caídas en cascada con búsquedas/preloads concurrentes. **Fix:** lector dedicado con request-id y canal; timeout por operación, no por espera de lock; supervisor único; cancelación real.

**A-11. Descargas que se corrompen en silencio.**
`lib.rs:946-1017`. Acepta un `200` en respuesta a un `Range` y concatena el archivo completo sobre el parcial; si el stream termina antes de `total_size` sin error hace `break`; solo valida `>1 KB`; timeout total de 30 s; el frontend no puede cancelar. **Impacto:** audio duplicado/truncado marcado como descargado, archivos huérfanos al cancelar, fallos en canciones largas. **Fix:** exigir `206` + `Content-Range` coherente, verificar bytes, `.part` + `rename`, timeout por lectura (no total), token de cancelación y limpieza.

**A-12. Caché de streams inmortal y fin de cola roto (frontend).**
`App.tsx:411, 1236-1250, 2234-2287`: `streamCacheRef` nunca expira ni se invalida; `onError` (`3188-3208`) no borra la entrada ni reintenta, así que una URL firmada caducada deja la canción rota hasta reiniciar. `onEnded` (`3209-3223`) + `getNextTrackToPlay` (`1207-1228`): al agotar la cola no se pausa ni se resetea el tiempo y la UI sigue "reproduciendo". **Fix:** TTL por URL (p. ej. 2 h), invalidar en `MediaError` y reintentar una vez, pausar al final de la cola.

**A-13. `emitMediaState` destruye su propio intervalo.**
`App.tsx:982-1007`. El callback depende de `currentTime` y el efecto que crea el `setInterval(3000)` depende del callback; cada tick (~1,2 s) limpia y recrea el intervalo, que nunca llega a 3 s. Se envía IPC en cada actualización de tiempo. **Fix:** separar el envío del reloj (refs) o depender solo de `[isPlaying, currentTrack]` y leer el tiempo desde `audioRef`/ref.

**A-14. Búsqueda, artista, relacionados y carpeta local sin control de carrera.**
`App.tsx:2477-2521` (búsqueda), `1365-1389` (artista), `1330-1337`/`1623-1637` (relacionados), `786-790` (carpeta). Respuestas lentas pisan el estado nuevo; los errores solo van a `console`. **Fix:** id de petición/`AbortController` por flujo y estado de error visible.

**A-15. "Cancelar descarga" no cancela y se duplican descargas.**
`App.tsx:6640-6660` solo borra estado; `invoke("descargar_cancion")` sigue y al terminar descarta la metadata, dejando archivo huérfano. `downloadTrack` (`2398-2422`) no consulta `activeDownloadsRef` antes de empezar. **Fix:** comando backend de cancelación, guard por ref al inicio, borrar el archivo parcial.

**A-16. Sleep Timer se congela al reprogramar.**
`App.tsx:1713-1719` hace `cancel()` (mode=null) y `set("time")` en el mismo batch; el efecto depende solo de `[sleepMode]` (`1739-1760`), así que no se crea un intervalo nuevo y la cuenta queda parada. **Fix:** guardar un `deadline` o depender de `sleepTimerSeconds > 0`.

**A-17. Diagnóstico cierra la sesión demasiado pronto.**
`App.tsx:2323` marca `onPlaybackStarted` al resolver `play()`, lo que completa la sesión (`diagnostics/index.ts:184-186`, `PlaybackPipelineDetector.ts:192-197`); los errores posteriores ya no generan reporte porque `onError` exige `running` (`3199-3205`). **Fix:** completar en `player.playing`/`ended` y no en la promesa de `play()`.

**A-18. `Range`/puerto y ciclo de vida del servidor.**
Si no hay puerto libre en `12761-12770` el setup entero falla y la app no arranca (`lib.rs:68-71, 2299-2300`). El servidor es thread-per-connection y lee solo 4096 bytes una vez (`:82-91`), sin timeouts ni límite de conexiones: cabeceras largas se truncan y conexiones lentas agotan hilos (DoS LAN). **Fix:** degradar sin proxy/cast, parsear cabeceras completas, límites, timeouts y pool.

**A-19. Módulos de PO tokens/pipeline muertos.**
`bg_pipeline.rs`, `po_token.rs` y `crypto_cache.rs` no están declarados con `mod` en `lib.rs`; `cargo check` no los compila y `start_refill_worker`/`init_background_pipeline` nunca se llaman. El README afirma PO-Tokens automáticos, pero el pool no existe en la app. Si se activan, no tienen TTL, timeouts ni escritura atómica. **Fix:** cablearlos con TTL/timeout o eliminarlos y ajustar el README.

**A-20. Windows/macOS: stderr del daemon acumulado sin límite.**
`lib.rs:729-763` lee stderr a un `String` durante toda la vida del proceso y solo se usa al arrancar. **Fix:** ring buffer o dejar de acumular tras `ready`.

**A-21. I/O bloqueante en comandos async y sync.**
`lib.rs` usa `std::fs` en `cache_audio`/`get_cached_audio`/`clean_audio_cache`/`descargar_cancion` y comandos sync con recorrido recursivo (`dir_size`, `listar_archivos_locales`). `discord_connect`/`discord_update` retienen un `std::sync::Mutex` mientras hacen I/O bloqueante (`1729-1786`). **Impacto:** UI congelada o workers agotados. **Fix:** `tokio::fs`/`spawn_blocking`, locks async y timeouts; `dir_size` sin seguir symlinks y con límites.

### Daemon Kotlin / InnerTube

**A-32. PO tokens sintéticos presentados como "genuine" y sobrescritura de tokens externos.**
`innertube/src/main/kotlin/.../utils/PoTokenGenerator.kt:26-58` construye un token casero (clave aleatoria de 16 bytes + XOR), sin BotGuard/WebPO. `Main.kt:444-470` acepta `OPENTUNE_PO_TOKEN` externo, pero el refresh de `:490-502` lo sobrescribe igualmente con tokens sintéticos y asigna el mismo valor a GVS/player (`:544-548, 700-704`). **Impacto:** 403/`LOGIN_REQUIRED` intermitentes, falsa sensación de bypass y destrucción de tokens válidos. **Fix:** eliminar el generador o implementar BotGuard real; no sobrescribir tokens externos; tokens separados por contexto con expiración; feature flag y telemetría de playability.

**A-33. `runCatching`/`catch (Exception)` se tragan `CancellationException`.**
`Main.kt:335`; `YouTube.kt:189-190` y casi todas las APIs; `LrcLib.kt:70`. **Impacto:** el timeout de letras o un shutdown no cancelan el trabajo; los proveedores siguen hasta 15 s y se fugan conexiones. **Fix:** helper `runCatchingCancellable` que rethrow `CancellationException`; comprobar `if (e is CancellationException) throw e`.

**A-34. `Thread.sleep` bloqueante en ruta `suspend`.**
`innertube/.../pages/NewPipe.kt:134-165` (retry con `Thread.sleep`), invocado desde `Main.kt:550` en el hilo de `runBlocking` que también lee stdin. **Impacto:** bloquea el protocolo; un `ping` no responde y el frontend puede matar el daemon. **Fix:** `delay`/`withContext(Dispatchers.IO)`, presupuesto global y límite de reintentos.

**A-35. La URL de stream descarta su expiración.**
`StreamDTO` solo tiene `url` (`Main.kt:49-52`) e ignora `expiresInSeconds` de `PlayerResponse.kt:50`. **Impacto:** caché/preload pueden reutilizar una URL caducada y dar 403 sin posibilidad de refrescar. **Fix:** añadir `expiresAt`, `mimeType`, `contentLength` e `itag` al DTO; TTL y re-resolución proactiva.

**A-36. `!!` y supuestos de esquema en el parsing de YouTube.**
`YouTube.kt:274, 326, 372, 388, 413, 575, 659, 1102, 1190`; `pages/AlbumPage.kt:105-114`; `NextPage.kt:57`; `SearchPage.kt:61`; `ThumbnailRenderer.kt:32`. **Impacto:** un álbum sin columna de thumbnail o una respuesta con campos opcionales lanza NPE y rompe la pantalla completa. **Fix:** eliminar `!!`, defaults + errores contextuales, `mapNotNull` y fixtures reales.

**A-37. Sin manejo de 429/5xx ni rate limiting; reintentos sobre mutaciones.**
`InnerTube.kt:164-182` solo reintenta `IOException`; `expectSuccess=true` (`:93`) convierte 429/5xx en excepción no reintentada; `withRetry` envuelve también like/editar playlist; las letras lanzan 6 proveedores a la vez y el stream hasta 19 clientes. **Impacto:** se ignoran `Retry-After`, se pueden duplicar "me gusta"/altas de playlist y se satura la red. **Fix:** clasificar retryables con jitter, reintentar solo GET, rate limiter global e idempotency keys.

**A-38. `albumSongs` pierde la continuación en `onResponseReceivedActions`.**
`YouTube.kt:342-359` lee las canciones del append, pero el siguiente token solo de `continuationContents.musicPlaylistShelfContinuation` (PlaylistPage sí tiene fallback en `:538-556`). **Impacto:** álbumes/playlists largos se truncan tras el primer lote. **Fix:** obtener el token de los mismos `continuationItems` y cubrir ambos formatos.

**A-39. Recursión sin límite en `next` (automix).**
`YouTube.kt:1115-1126` se llama recursivamente cuando existe `automixPreviewVideoRenderer`. **Impacto:** endpoint repetido/malformado → stack overflow; el `runCatching` exterior no lo evita. **Fix:** bucle iterativo con `visitedEndpoints` y profundidad máxima.

**A-40. Tokens y `cachedSignatureTimestamp` sin sincronización ni TTL.**
`YouTube.kt:106-128` expone `visitorData`/`poToken` como `var` sin `@Volatile`/mutex; `Main.kt:280, 475, 550` cachea el signature timestamp para siempre y lo prefetcha con un vídeo fijo. **Impacto:** carreras de visibilidad, tokens mezclados (visitorData nuevo + token viejo) y fallos de descifrado hasta reiniciar. **Fix:** snapshot inmutable por petición, `Mutex`/`AtomicReference`, TTL y refresco al fallar el descifrado.

### Calidad, release y cadena de suministro

**A-22. Sin firma ni notarización (macOS/Windows).**
No hay pasos de firma en el workflow ni `signingIdentity`/entitlements/hardened runtime en `tauri.conf.json`. Gatekeeper/SmartScreen bloquean los instaladores. **Fix:** secretos de firma, `notarytool`, `codesign --verify`/`spctl`, `signtool verify`.

**A-23. Sin checksums, SBOM ni procedencia.**
El release solo publica instaladores. **Fix:** `SHA256SUMS`, SBOM CycloneDX/SPDX, `actions/attest-build-provenance`, reporte de licencias.

**A-24. Arquitecturas incompletas.**
No hay matriz: macOS solo arm64, Windows/Linux solo x64. **Fix:** matriz OS/arch, universal macOS, runners ARM.

**A-25. Sin CI de tests/lint/format.**
Solo existe el workflow de release; no hay tests en ningún módulo, ni scripts `test/lint/format/typecheck`. `cargo clippy` da 11 warnings. **Fix:** CI en PR/push con `tsc --noEmit`, Biome/ESLint, Vitest, `cargo fmt/clippy/test`, `gradle test` + ktlint/detekt; hacerlos checks requeridos.

**A-26. Toolchains flotantes y builds no reproducibles.**
GraalVM/Rust/Node sin pin exacto, sin `rust-toolchain.toml`, sin `SOURCE_DATE_EPOCH`; `mise.toml` no coincide con CI. **Fix:** pin de parches, caches, `--locked/--frozen`, `strip`, perfil release con LTO.

**A-27. Release automático sin compuerta.**
Cualquier tag `v*` publica (`draft: false`), se cancelan releases en curso y no hay `environment` con aprobación. **Fix:** validar SemVer, `draft: true`, aprobación manual, `prerelease` para `-rc/-beta`, `fail_on_unmatched_files`.

**A-28. 61 MB de binario versionado y ~704 MB de historial.**
`capi-desktop/bin/capi-core` está trackeado pese a `.gitignore`; `.git` pesa ~705 MB (7 revisiones del binario y un blob de 92,8 MB de `squashfs-root`). El bundle Windows empaqueta además el ELF Linux por `resources: ["../bin/*"]`. **Fix:** `git rm --cached`, ignorar `bin/*`, construir en CI, publicar con checksum, purgar historial con `git filter-repo`/BFG, `externalBin` por plataforma.

**A-29. Cadena de suministro Gradle/npm.**
JitPack para NewPipeExtractor sin commit/checksum, wrapper sin `distributionSha256Sum`, sin dependency verification/locking, `npm audit` con 4 high + 1 low. **Fix:** locking+verification, fijar commit, actualizar Vite/postcss/nanoid, auditorías en CI.

**A-30. Licencia y metadatos inconsistentes.**
`PKGBUILD` MIT vs GPL-3.0; `Cargo.toml` sin `license`, `description = "A Tauri App"`, `authors = ["you"]`; `package.json` sin license; fuentes propias sin cabecera SPDX. **Fix:** unificar GPL-3.0-only y completar metadatos en todos los manifiestos.

**A-31. Documentación de contribución y plantilla de bug obsoletas.**
`CONTRIBUTING.md` describe Android Studio/JDK 11/rama `main`/Crowdin; la plantilla de issue habla de APK/adb/Play Store. **Fix:** reescribir para Tauri/Rust/Node/GraalVM y añadir plantilla desktop (SO, arquitectura, versión, logs, diagnóstico).

---

## 3. Medios

### Frontend

- **M-01. i18n casi inexistente.** 31 locales declarados, 14 usos reales de `t()`; el sidebar hardcodea "Home/Explorar/Buscar" (`App.tsx:325-336, 3319-3325`); `index.html:2` fija `lang="en"` y no se actualiza al cambiar idioma. **Fix:** extraer textos, actualizar `document.documentElement.lang/dir`, o retirar el selector.
- **M-02. Modo claro inalcanzable.** `{false && theme === "light-mode"}` (`5912`) y `.filter(t => t.id !== "light-mode")` (`5940`), aunque CSS y efecto lo soportan. **Fix:** quitar ambos.
- **M-03. Accesibilidad sistémica.** 0 `role=`, 6 `aria-*`, 0 `tabIndex` para 202 botones y 28 divs clicables; modales sin `dialog`/Escape/focus trap (`7658, 8121, 8405, 8433`); `user-select:none` global; sin `:focus-visible` ni `prefers-reduced-motion`. **Fix:** componente Modal accesible, navegación por teclado y estilos de foco.
- **M-04. Borrado en transacción IndexedDB readonly.** `thumbnailCache.ts:37-48` llama `store.delete()` dentro de una tx `readonly`; el error es asíncrono y `resolve(null)` no se ejecuta, dejando `SafeImage` en "loading" para siempre al expirar el TTL (60 días). `cleanExpiredThumbnails` nunca se invoca y no se cierran conexiones. **Fix:** tx `readwrite`/borrado diferido y limpieza al arrancar.
- **M-05. Fuga de blob URLs y cacheo de errores en SafeImage.** `SafeImage.tsx:49-70` sobrescribe `objectUrlRef` sin revocar al cambiar `trackId`; `86-91` solo revoca al desmontar; `90-100` no valida `resp.ok`/`Content-Type` y puede cachear HTML de error (además `fetch` cross-origin a `i.ytimg.com` falla por CORS, por lo que la caché probablemente nunca se llena). **Fix:** revocar antes de reasignar, validar respuesta y cachear vía `/image` propio.
- **M-06. Traducción de letras: privacidad y carrera.** `App.tsx:2072-2125` envía la letra completa a `api.mymemory.translated.net` por GET, sin timeout/abort y sin id de pista; una respuesta vieja puede activar la traducción en la canción nueva. No hay aviso de privacidad ni caché persistente. **Fix:** `AbortController` + id, consentimiento y caché.
- **M-07. Import de backup sin validar.** `App.tsx:847-917` escribe cualquier clave/valor de `parsed.data` en localStorage y aplica estados sin validar; un backup malformado rompe el render o agota la cuota. **Fix:** whitelist `capi_*`, validadores y confirmación.
- **M-08. localStorage sin try/catch y crecimiento sin límite.** Escrituras `1041-1064`, `2530`, avatar base64 `6331-6332`; `QuotaExceededError` no se captura; `searchThumbnails` crece sin tope. **Fix:** wrapper tolerante a fallos, LRU y mover datos grandes a IndexedDB.
- **M-09. Home sin estado de error/vacío.** Si fallan los 4 tiers, queda el skeleton infinito (`3514-3516`); `parsed.sections` se usa sin validar (`1419-1421`) y `section.items.map` (`3792`) revienta si falta `items`. **Fix:** estados `loading/error/empty` y validación de DTOs.
- **M-10. Cast siempre "conectado" y doble audio.** Ver A-06.
- **M-11. Teclado global con closure obsoleto.** `App.tsx:1089-1205` depende de `[isPlayerExpanded,isPlaying,streamUrl]` pero usa `playNext/playPrev/togglePlay`, que capturan cola/pista; cambiar la cola no re-registra el listener. Escape corta la reproducción aunque haya un modal abierto. **Fix:** refs o dependencias completas y prioridad de modales.
- **M-12. `play()` sin catch.** `togglePlay` (`2590`) y repeat-one (`3219`, catch vacío) pueden producir rechazos no manejados sin diagnóstico.
- **M-13. Doble lógica de letras y re-render global.** `handleTimeUpdate` (`2607-2653`) muta el DOM a mano y `currentTime` también se usa en JSX (`6749, 7177, 7320`); dos sincronizadores con umbrales 800/350 ms. Cada tick re-renderiza un árbol de 8500 líneas con 83 `useState`, 49 efectos y 0 `useMemo`. **Fix:** reloj aislado, un solo sincronizador, memoización y virtualización.
- **M-14. Fechas UTC en estadísticas.** `toISOString().split('T')[0]` (`1867, 2006, 2017`) atribuye escuchas al día equivocado fuera de UTC (Venezuela UTC-4). **Fix:** fecha local.
- **M-15. `getPlaylistCover` parsea localStorage en render.** `465-467`; se llama 6 veces por render (`3643, 4469, 4866-4867, 5078-5079`). **Fix:** cargar una vez en estado.
- **M-16. Numpad muerto y `getBannerUrl` divide por cero.** `1189-1200` compara `e.key === "Numpad0"` (los navegadores entregan `"0"`); `71-87` genera `hInfinity` con `=w0-h0`.
- **M-17. `locale`, `volume` y `pillRadius` sin validar.** `as Locale` (`325`), `parseFloat/parseInt` (`563-567, 660-663`) pueden producir `undefined`/`NaN`. **Fix:** validar y clampear.
- **M-18. `navigator.clipboard.writeText` sin catch** (`3019-3020, 3043, 8482`); 60 `console.*` en producción; imágenes sin `loading="lazy"`/`decoding`; `key={i}` en listas reordenables (`3796, 4193, 4262, 5321`).
- **M-19. Timers sin cleanup:** toast (`315`), debounce de sugerencias (`632/2532`), autoScroll (`1296-1301`), blur (`3395`).

### Backend

- **M-20. API sin validar valores.** `/api/volume` y `/api/seek` aceptan `NaN`/`inf`/negativos/fuera de rango (`lib.rs:307-331`); `/api/state` escribe body en HEAD (`:344`). **Fix:** clamp y 400.
- **M-21. Superficie de red y cabeceras.** `/image` refleja `Content-Type` del upstream y no envía `nosniff` (`:356-395`), puede servir HTML/SVG desde el origen local; `urlencoding::decode` interpreta `+` como espacio y corrompe URLs firmadas (`:359, 401`). **Fix:** percent-decoding, solo `image/*`, `nosniff`.
- **M-22. Descubrimiento cast frágil.** Asume IPv4 /24 y usa `8.8.8.8` (`:1209-1228`), sin IPv6/mDNS; parseo XML por substrings (`:1281-1314, 1415-1458`). **Fix:** crates `ssdp`/`mdns`, parser XML, no escanear 254 IPs al arrancar.
- **M-23. Caché: stats y configuración.** `get_cache_stats` calcula `oldest` antes de limpiar y recarga (`:2125-2139`); `set_cache_config` permite `max_bytes=0`; `remove_cached_track` sustituye `.webm`/`.m4a` sobre toda la ruta y puede borrar archivos no relacionados (`:2168-2171`); el máximo solo se aplica después de escribir (`:2038-2042`). **Fix:** validar config, limpiar antes de stats, borrar por ruta exacta, aplicar límite antes de descargar.
- **M-24. Ciclo de vida de ventanas y tray.** Cerrar siempre oculta y no se puede cerrar (`:2265-2270`); si el tray falla no hay forma de salir (`:2362-2415`); `handle.exit(0)` no hace shutdown ordenado; las ventanas `main-*` (`:2380-2387`) no están en `capabilities/default.json:5` (solo `"main"`), así que "Abrir otra ventana" abre una ventana sin permisos IPC. **Fix:** opción "minimizar a bandeja", fallback de salida, shutdown ordenado, capability con patrón `main*`.
- **M-25. Errores de arranque silenciosos en Linux.** `main.rs:4-18` instala el panic hook solo en non-Linux y usa versión hardcodeada 0.1.1. **Fix:** hook multiplataforma y versión desde `package_info`.
- **M-26. `obtener_sugerencias` sin timeout/status** y cliente nuevo por llamada (`lib.rs:1154-1177`). **Fix:** cliente compartido, timeout, validar 2xx.
- **M-27. `dir_size` sigue symlinks y resta lecturas inconsistentes** (`:2186-2213`), posible underflow/loop. **Fix:** una pasada, sin seguir enlaces.
- **M-28. `spawn_blocking` y locks:** ver A-10/C-02.
- **M-29. Sin logs estructurados ni IDs de petición;** `println!` con URLs firmadas (`lib.rs:412, 694`), riesgo de privacidad y cortes UTF-8 que pueden envenenar los mutex. **Fix:** `tracing`, `chars().take()`, no loguear URLs completas.
- **M-30. Clippy:** 11 warnings (`lib.rs:8, 83, 289, 295, 301, 490, 545, 729, 1102, 1506, 1826`). **Fix:** `cargo clippy --fix`/fmt.

### Daemon Kotlin

- **M-31. Protocolo sin request-id ni framing.** Una sola tubería stdin/stdout, un comando a la vez; cualquier `println` accidental (logs, `printError` en el catch global) se interpreta como respuesta y desalinea todo. **Fix:** JSON Lines con `id`, respuestas correlacionadas, logs solo a stderr, y tests de protocolo.
- **M-32. Arranque lento antes de `ready`.** `Main.kt:449-487` hace `visitorData` (timeout 20 s) + tres generaciones de token antes de avisar; Rust no tiene timeout (A-09). **Fix:** responder `ready` primero y generar tokens en background, con estado "degradado" si fallan.
- **M-33. Tokens mutables sin sincronización.** `visitorData`/`poToken*` son globales mutables; el refresh cada 30 min (`:492-508`) asigna `visitorData` antes de generar los tokens, por lo que una petición concurrente puede usar visitorData nuevo con tokens viejos. **Fix:** snapshot inmutable por petición y `Mutex`/atomics.
- **M-34. `cachedSignatureTimestamp` nunca caduca.** `Main.kt:283, 551-553`: si YouTube cambia la firma, se siguen usando valores viejos hasta reiniciar. **Fix:** TTL y refresco al fallar el descifrado.
- **M-35. Letras: fan-out de 6 proveedores en cada petición.** `Main.kt:584-633` lanza YouTube Music, YouTube Subtitle, LrcLib, KuGou, SimpMusic y BetterLyrics en paralelo, sin caché, con 8 s de espera; un proveedor desconocido hace esperar 8 s completos. Depende de APIs de terceros sin SLA (y envía título/artista a varios servicios). **Fix:** LrcLib primero, caché persistente por pista, timeout corto, proveedor desconocido = error inmediato, y aviso de privacidad.
- **M-36. TTML con horas mal convertido.** `normalizeLyrics` (`Main.kt:938-953`) solo entiende `mm:ss`; un `begin="00:01:23.45"` cae a `toDoubleOrNull()` → 0. **Fix:** mismo parser que el frontend (`lyricsParser.ts`) o librería.
- **M-37. Código duplicado CLI/daemon.** Los handlers `--search`/`--get-stream`/`--lyrics` repiten el mapeo de DTO del bucle; riesgo de divergencia. **Fix:** funciones de caso de uso compartidas.
- **M-38. Dependencias pesadas y packaging.** NewPipeExtractor + Rhino + RE2J + Ktor en un nativo de 61 MB sin `strip`; configuraciones `META-INF/native-image/{lyrics,search,stream}` no está claro que se apliquen. **Fix:** perfil release stripped, reachability metadata verificada, revisar/eliminar Rhino si no se usa.
- **M-39. Sin tests en `capi-core`/`innertube`/`lrclib`.** Los `testImplementation(junit)` existen pero no hay fuentes de test. **Fix:** tests de parser de letras, DTOs, resolución de cliente y protocolo.
- **M-40. Unidades inconsistentes en KuGou.** `Main.kt:183` compara `song.duration` (ms) con `duration` (s); `:192` sí multiplica por 1000. El filtro de candidatos casi nunca coincide y cae al fallback por keyword. **Fix:** normalizar a ms y test.
- **M-41. Las letras "plain" de LrcLib nunca se usan.** `LrcLib.kt:63` filtra `syncedLyrics != null` y `:126-134` intenta usar `plainLyrics`, que ya no está en la lista. **Fix:** no filtrar en `queryLyrics` y preferir synced con fallback plain explícito.
- **M-42. Proveedor de letras inválido espera 8 s.** `Main.kt:593-618`: si `provider` no coincide, `active` queda vacío, no se lanza ningún `launch`, el `Channel` nunca se cierra y `withTimeoutOrNull(8000)` agota el tiempo. **Fix:** validar contra un mapa y devolver error inmediato.
- **M-43. Transcript frágil.** `YouTube.kt:1184-1190` usa `cues[0]` y `!!` ignorando el resto de cues del grupo; `InnerTube.kt:355, 363` tiene la clave hardcodeada y un `11.toChar()` mágico. **Fix:** recorrer `cues` con `mapNotNull`, validar grupos y mover la clave a configuración.
- **M-44. Cookies y `pot` sin higiene.** `YouTube.kt:1069-1083` toma `playbackTracking` de la respuesta y solo reemplaza `https://s.youtube.com`; `InnerTube.kt:142-153` añade la cookie a cualquier URL con `setLogin=true`; `YouTube.kt:181-187` concatena `pot=$token` sin encodear. **Fix:** allowlist estricta de hosts, `URLBuilder` para el token y no adjuntar cookies a tracking.
- **M-45. Sin límites de memoria por línea/respuesta.** `Main.kt:514` y `lib.rs:693` usan `read_line` sin tope; `Main.kt:767` escribe la respuesta completa. **Fix:** máximo por línea (1 MB), límite de respuesta y backpressure.
- **M-46. `assert` deshabilitado y `visitorData` frágil.** `YouTube.kt:1171-1173` usa `assert` para el límite de cola (no se evalúa en JVM por defecto); `YouTube.kt:1193-1202, 1242` hace `substring(5)` y regex `^Cg[t|s]` (el `|` es literal). **Fix:** `require`/truncado y parseo JSON validado con `^Cg[ts]`.
- **M-47. Divergencias CLI/daemon y cierre de clientes.** `Main.kt:850-861` (`--lyrics` solo LrcLib y duración obligatoria frente a 6 proveedores); `pages/NewPipe.kt:88-90` captura el proxy al init; los `HttpClient` de `InnerTube.kt:92`, `LrcLib.kt:28` no se cierran; `stdinErrorCount` (`Main.kt:511, 772-776`) nunca se reinicia. **Fix:** unificar casos de uso, inyectar proxy, `close()` en shutdown y resetear el contador.

### Build y repo

- **M-48. Gradle obsoleto y contradictorio.** `gradle.properties:17-22` conserva flags Android/Jetifier/KSP, `:39` desactiva caching, CI pasa `--no-configuration-cache`; `build.gradle.kts:6-8` limpia solo el build raíz. **Fix:** limpiar flags, decidir configuration cache, cachear, clean por subproyecto.
- **M-49. Renovate no funcional.** `renovate.json:6` apunta a `main`/`dev` inexistentes; sin pin de acciones, agrupación ni workflow/Dependabot. **Fix:** solo `master`, `pinDigests`, grupos y alertas.
- **M-50. Assets pesados/muertos.** `Capi.jpeg` 492 KB, `assets/Logo.png` 2,7 MB sin referencias, `src/assets/Logo.png` 1,8 MB, 35 iconos Android/iOS, `tauri.svg`/`vite.svg` de plantilla. **Fix:** optimizar (WebP/AVIF/SVG), borrar duplicados/móviles.
- **M-51. `.gitignore` incompleto y sin `.gitattributes`.** Solo ignora nombres exactos de `capi-core`; `.vscode/` global impide versionar `extensions.json`; sin normalización LF. **Fix:** `bin/*`, artefactos de instaladores, `.gitattributes` y `.editorconfig`.
- **M-52. Metadatos pobres.** `Cargo.toml` "A Tauri App"/"you"; `package.json` sin description/license/engines; `tauri.conf` sin publisher/category. **Fix:** completar.
- **M-53. Sin updater.** No hay `plugins.updater` ni `latest.json`. **Fix:** Tauri Updater firmado con canal stable/beta.
- **M-54. README con afirmaciones no verificadas.** <130 MB sin benchmark, "M4A" con fallback WebM, traducción vía MyMemory sin aviso, "gráfico Top Artistas no completado", instrucciones `chmod +x Capi-Desktop` cuando se publican `.deb/.rpm/.AppImage/.msi/.dmg`. **Fix:** documentar formatos reales, instalación por formato, benchmark o retirar la cifra, privacidad de traducción.
- **M-55. Identidad difusa.** README/CONTRIBUTING/PKGBUILD apuntan a `jh2929/Capi`; el remoto es `Capi-Desktop`; producto `Capi`, paquete `capi-desktop`, AUR `capi-desktop-bin`, restos OpenTune. **Fix:** unificar nombre/repo.

---

## 4. Bajos (selección)

- Sin `.editorconfig`, `.gitattributes`, plantilla de PR, `CODEOWNERS` ni `SECURITY.md`.
- `mise.toml` solo fija Java y no se usa en CI; falta guía de build desde fuente.
- `package.json` conserva `build:win` con target GNU que CI no usa.
- `navigator.clipboard` sin fallback; en WebKit puede rechazar.
- Logs de depuración (`console.*`, `println!`, `System.err`) en producción, con timing y URLs.
- `getBannerUrl` puede generar URLs inválidas; `SafeImage` sin `loading="lazy"`.
- `i18n.ts` rellena en silencio con inglés las claves ausentes (no hay validación de completitud).
- `play_events` de IndexedDB crece sin poda; `thumbnailCache` no cierra conexiones.
- `formatTime`/slider pueden mostrar `NaN` si `duration=0` en algunos flujos locales.
- `on_window_event` oculta en vez de cerrar sin explicar al usuario; primer cierre "no hace nada".
- Dependencias de dev con vulnerabilidades conocidas (Vite/esbuild/postcss/nanoid) y `lucide-react ^1.17.0` con rango abierto.
- `frontendDist`/`devUrl` y CSP: en `dev` el HMR WebSocket de `localhost:1420` no está cubierto por `connect-src` (solo `127.0.0.1`).

---

## 5. Nuevas funciones recomendadas

Priorizadas por relación impacto/esfuerzo, alineadas con la arquitectura actual.

### Producto y reproducción
1. **Cola y reproducción robustas:** gapless/crossfade opcional, "radio infinita" al terminar la cola, historial de cola, undo de "quitar de la cola", y un solo flujo de reproducción con request-id.
2. **MPRIS/Media Session + teclas multimedia:** `souvlaki` (Rust) o `mpris-server`; controles del sistema, carátula y posición. Hoy solo hay un endpoint HTTP que imita MPRIS.
3. **Ecualizador y normalización:** EQ de 10 bandas + ReplayGain/loudness con Web Audio o DSP en Rust; presets y "night mode".
4. **Descargas de verdad:** cola con pausa/reanudación/cancelación real, límite de concurrencia, selección de formato (Opus/M4A), metadatos y carátula embebidos, plantilla de carpetas (`{artist}/{album}/{track}`), verificación de integridad, reconciliación al arrancar (archivos borrados/renombrados) y exportar/importar M3U.
5. **Modo offline explícito:** descargar albums/playlists completas, indicador de disponibilidad, reproducción sin red y sincronización de "me gusta" pendiente.
6. **Biblioteca local completa:** escaneo recursivo, lectura de tags (title/artist/album/año/género), carátula embebida, ordenación, búsqueda y "watched folders"; hoy solo lista el primer nivel y llama a todo "Archivo Local".

### Cuenta, sincronización y recomendaciones
7. **Login de YouTube Music (OAuth/cookies):** biblioteca, likes, playlists y "mi mix" reales; hoy todo es anónimo. Con almacenamiento seguro de tokens (keyring del SO, nunca en localStorage).
8. **Scrobbling:** Last.fm y ListenBrainz, con cola offline y deduplicación.
9. **Recomendaciones explicables:** usar `recommendations.ts` para "porque escuchaste X", radio por artista, mezclas diarias, y permitir ocultar/afinar artistas (ya existe `hiddenArtists`). Añadir "no me gusta" y peso por finalización de canción.
10. **Estadísticas tipo Wrapped:** ya hay base; añadir resumen semanal/mensual exportable como imagen, rachas y comparación, con fechas locales y poda de eventos.

### Letras y accesibilidad
11. **Letras de calidad:** LrcLib como primera fuente, caché local por pista, ajuste de offset ±, letras por palabra (karaoke), búsqueda manual de candidatos, edición/guardado de `.lrc`, y traducción **local** (LibreTranslate/Argos) u opcional con consentimiento explícito.
12. **Accesibilidad:** componente Modal con focus trap/Escape/ARIA, navegación completa por teclado (incluido espacio sin conflicto), `prefers-reduced-motion`, contraste configurable y escalado de UI.

### Sistema, plataforma y seguridad
13. **Auto-actualización firmada** (Tauri Updater) con canal beta, notas y verificación de firma.
14. **Servidor local endurecido:** token por sesión, loopback por defecto, listener LAN opcional para cast, allowlist de hosts, rate limit y consentimiento para "Recibir en TV".
15. **Cast real:** Chromecast (protocolo CASTV2) o DLNA con control de volumen/transporte, pausa del audio local, reconexión y descubrimiento mDNS; validar IP/host.
16. **Miniaturas y caché de imágenes propias:** proxy `/image` con allowlist, cache en IndexedDB/`tauri-plugin-store`, no depender de CORS de `ytimg`.
17. **Ajustes avanzados:** ruta de descargas, límite de caché, proxy, tema del sistema, arranque minimizado, atajos configurables, exportar/importar toda la configuración con validación de esquema.
18. **Command palette (Ctrl/Cmd+K)** y búsqueda global unificada (biblioteca + YouTube + playlists + ajustes).

### Ingeniería
19. **Capa de datos tipada:** cliente IPC único con DTOs, reintentos, timeouts, cancelación y validación (`zod`), en lugar de `invoke` + `JSON.parse` disperso; Zustand/TanStack Query para estado.
20. **Observabilidad y soporte:** logs estructurados con rotación, crash reporting local opt-in, "exportar diagnóstico" (ya existe) + copiar al portapapeles y abrir carpeta.
21. **Suite de tests:** Vitest para `lyricsParser`, `urlUtils`, `recommendations`, `thumbnailCache`; `cargo test` para rangos/caché/paths; integración del protocolo Kotlin; E2E con `tauri-driver`/WebDriver para reproducir y descargar.
22. **Multiplataforma real:** builds firmados x64/arm64 (macOS universal), AppImage/deb/rpm probados, Flatpak y AUR automatizado.

---

## 6. Plan sugerido

### Semana 1 — bloqueantes
1. Bind a loopback + token + allowlist + restringir `path=`/`borrar_cancion`/`cache_audio`.
2. Arreglar `restart_daemon` con estado mutable y matar/reapear; añadir test del reinicio.
3. Cerrar el daemon al EOF, mover logs a stderr y añadir `id` de petición al protocolo.
4. Timeout presupuestado y clientes en paralelo en `resolveStreamUrl`; no matar el daemon por un comando lento.
5. `safeJsonParse` + ErrorBoundary; request-id en `playTrack`; invalidar `streamCacheRef` y pausar al final de la cola.
6. Corregir descargas (206/Content-Range, `.part`, cancelación) y `Range`/MIME del servidor.

### Semana 2–3 — calidad y release
7. CI con `tsc`, Biome, Vitest, `cargo fmt/clippy/test`, `gradle test`; primeros tests de parsers y protocolo.
8. Fuente única de versión + guard en CI; AUR y metadatos/licencia; `SECURITY.md`.
9. `git rm --cached` del binario, `externalBin` por plataforma y plan de purga de historial.
10. Firma/notarización, checksums, SBOM, updater; `draft`/aprobación.

### Mes 2 — producto
11. Refactor de `App.tsx` en features + capa IPC tipada + memoización/virtualización.
12. Descargas con cola real, MPRIS, escaneo local con tags, letras con caché/offset.
13. Login de YouTube Music, scrobbling y accesibilidad.

---

## 7. Quick wins (bajo esfuerzo, alto retorno)

1. `git rm --cached capi-desktop/bin/capi-core` e ignorar `capi-desktop/bin/*`.
2. `permissions: contents: read` + pin de acciones a SHA + `persist-credentials: false`.
3. Script/guard de versión desde el tag y sincronización de manifiestos.
4. `draft: true`, `environment: release`, no cancelar releases, `fail_on_unmatched_files`.
5. `chmod +x` en macOS e `if-no-files-found: error`.
6. `SHA256SUMS` + SBOM + attestations.
7. `SECURITY.md`, plantilla PR y plantilla de bug desktop.
8. Corregir AUR (source, GPL-3.0, versión, checksum, ruta, `.SRCINFO`).
9. Eliminar `/home/emixdy/...` y el `get_binary_path` duplicado; `strip` del daemon.
10. Self-host de fuentes, favicon propio, `lang="es"` y CSP coherente.
11. Biome + scripts `lint/format/typecheck/test`; `cargo fmt/clippy`; ktlint.
12. Invalidar caché y reintentar en `onError`; arreglar `emitMediaState`, sleep timer y fin de cola.
13. Revocar blob URLs, transacción `readwrite`, `loading="lazy"`.
14. Borrar `{false && ...}` del modo claro y el filtro `light-mode`.
15. Optimizar imágenes y borrar iconos móviles/SVGs de plantilla.
16. Al EOF del stdin: cancelar refresh/prefetch y `exitProcess(0)`; matar/esperar el hijo en Rust.
17. Mover todos los `println` de diagnóstico a stderr y añadir `id` a las respuestas del protocolo.
18. No sobrescribir tokens externos y retirar el generador de PO tokens sintéticos (o marcarlo experimental).

---

## 8. Aspectos positivos verificados

- La arquitectura daemon nativo + Tauri es válida y el daemon responde por JSON sobre stdin, sin `shell` ni interpolación de comandos (no hay command injection).
- `npx tsc --noEmit` pasa limpio (strict + `noUnusedLocals`), `cargo check --all-targets` pasa sin errores ni warnings y `:capi-core:compileKotlin` compila sin warnings de fuente.
- No hay `dangerouslySetInnerHTML`, `innerHTML`, `eval` ni `new Function` en el frontend; las URLs pasan por `encodeURIComponent` y `validateStreamUrl`; los reportes sanean firmas.
- No hay `unsafe` en Rust; el scope del protocolo `asset` se limita a `$APPDATA`.
- La capa de diagnóstico está razonablemente separada (motor, detectores, timeline, reporte) y es una buena base para soporte.
- `node_modules`, `dist`, `target` y `build` **no** están versionados; los lockfiles son consistentes; el workflow de release produce instaladores para las 3 plataformas.
- No se encontraron secretos en HEAD ni cambios sin commitear.
- Existen `LICENSE` GPL-3.0, `CODE_OF_CONDUCT.md`, `CONTRIBUTING.md` y una UI cuidada con temas, i18n base y funciones diferenciales (cast, letras, estadísticas, Discord RPC).

---

## Anexo: comandos de verificación

```bash
npx tsc --noEmit                       # frontend: 0 errores
cargo check --all-targets              # rust: 0 errores, 0 warnings
cargo clippy --all-targets             # rust: 11 warnings
./gradlew :capi-core:compileKotlin --rerun-tasks   # JVM: BUILD SUCCESSFUL
npm audit                              # 4 high, 1 low
git ls-files -ci --exclude-standard    # capi-desktop/bin/capi-core
git count-objects -vH                  # size-pack ~703.82 MiB
```

**Smoke tests del daemon (reproducidos):**

```bash
printf '{"action":"ping"}\nnot-json\n{"action":"nope"}\n\n' | ./bin/capi-core --daemon
# -> {"status":"ready"} / {"status":"pong"} / {"error":"Unknown action: null"} / {"error":"Unknown action: nope"}

# EOF no termina el proceso:
printf '' | timeout 20 ./bin/capi-core --daemon; echo $?   # -> 124

# Los logs de debug contaminan stdout (63 líneas antes de ready):
OPENTUNE_DEBUG=1 ./bin/capi-core --daemon | head -1        # -> serializer, no JSON de ready
```

> Nota: la auditoría es estática más compilación. No se ejecutó la app completa ni pruebas dinámicas del servidor HTTP; sí se hizo smoke test del daemon y se verificó la semántica de `StateManager::set` de Tauri 2.11 en el código de la crate.
