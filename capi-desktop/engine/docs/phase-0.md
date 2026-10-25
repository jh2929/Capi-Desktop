# Fase 0 — Spike del motor Rust

- **Fecha:** 2026-10-25
- **Rama:** `rewrite/rust-engine`
- **Entorno:** Arch Linux x86_64, Rust 1.97.1, `rustypipe` 0.11.4
- **Alcance:** validar catálogo, resolución de streams, formatos, letras y
  viabilidad de eliminar el daemon Kotlin y los PO tokens.

## Resultado: GO

`rustypipe` cubre el catálogo y el ítem crítico —la resolución de audio— sin
necesidad de Botguard ni de un binario sidecar.

### Medición (red doméstica, primera ejecución)

| Operación | Tiempo | Resultado |
|---|---:|---|
| Construcción del cliente | 23 ms | Sin red, sin tokens |
| Búsqueda "daft punk" | 1,25–1,78 s | 20 canciones |
| Player iOS (primera) | 244–584 ms | 5 streams |
| `Range GET` del stream | 140–746 ms | HTTP 206, 1 KB |
| Álbum | 981 ms | pistas correctas |
| Artista | 503–528 ms | 4 secciones |
| Playlist / radio | ~ | 1 página de pistas |
| Letras LrcLib | 741 ms | 3.820 bytes |
| Novedades | 717 ms | 103–130 álbumes |
| Moods/géneros | 196 ms | 46 |
| Sugerencias | 187 ms | 6 |
| RSS del proceso | 39 MB | incluye runtime de pruebas |

### Player: cliente iOS, sin PO token

La matriz de clientes fue concluyente:

| Cliente | Resultado |
|---|---|
| `Ios` | **OK**, 5 streams, ~250–500 ms |
| `Android` | Error de deobfuscación tras ~29 s |
| `Tv` | Error: sin datos de deobfuscación |
| `DesktopMusic` / `Desktop` / `Mobile` | Error: requieren Botguard |

El cliente iOS devuelve ambos formatos:

- `itag=140`, `audio/mp4; codecs="mp4a.40.2"`, AAC ~131 kbps
- `itag=251`, `audio/webm; codecs="opus"`, Opus ~165 kbps
- `itag=139` (AAC 48 kbps) y `249/250` (Opus)

**Decisión:** preferir siempre `itag 140` (AAC/M4A) y usar Opus solo como
fallback. Es el formato con mejor compatibilidad con WebView (especialmente
macOS) y evita el problema de duración del AAC/SBR que ya se parchea en el
frontend.

`valid_until` de la URL es ~6–8 h; se cachea hasta 60 s antes de expirar.

### Letras

- LrcLib responde con letras sincronizadas para la primera prueba.
- YouTube Music (`music_details` → `lyrics_id` → `music_lyrics`) funciona como
  fallback.
- No se necesitan KuGou, SimpMusic ni BetterLyrics para la paridad inicial.

### Lo que esto elimina

- Daemon Kotlin y binario GraalVM de 61 MB.
- Protocolo stdin/stdout, `ready`, timeout de 25 s, watchdog y reinicios.
- Generación de PO tokens sintéticos y el pool de tokens.
- Dependencia de Botguard/Deno/V8 y de un sidecar.

## Riesgos abiertos

1. El cliente iOS es no oficial y puede cambiar. Mitigación: un solo punto de
   resolución (`Engine::stream`), logs de playability y capacidad de añadir
   Botguard como fallback.
2. Windows/macOS no se han probado todavía.
3. El `home` de YouTube Music no existe como método en `rustypipe`; la v1 se
   compone con novedades y el frontend conserva sus capas de respaldo.
4. `TrackItem` no expone `explicit`; se envía `false` por ahora.
5. Las URLs de iOS pueden requerir `visitorData` en algunos vídeos; hay que
   probar edad restringida/regiones.

## Fase 0.5 — integración en Tauri (completada)

- `capi-engine` se añadió como path dependency de `src-tauri`.
- Los comandos `buscar_cancion`, `obtener_stream`, `obtener_playlist`,
  `obtener_home`, `obtener_artista`, `obtener_explorar`, `obtener_letras`,
  `obtener_nuevos_lanzamientos` y `obtener_sugerencias` ya usan el motor Rust
  conservando nombres y forma JSON.
- Se eliminaron el spawn del daemon, el protocolo stdin/stdout, el timeout de
  25 s, el reinicio, el watchdog y la ruta hardcodeada del binario.
- `cargo check` y `cargo build` de la app Tauri pasan.
- El ejemplo release del motor pesa **8,3 MB** (frente a los 61 MB del binario
  Kotlin), sin contar el shell Tauri.
- El binario Kotlin dejó de empaquetarse (`tauri.conf.json` y `.gitignore`).

### Retirada de Kotlin (completada)

- `capi-core/`, `innertube/`, `lrclib/`, Gradle y los jobs GraalVM fueron
  eliminados del repositorio.
- El binario sidecar ya no se empaqueta ni se versiona.
- El CI construye directamente la app Tauri con el motor embebido.

### Métricas finales del release (Linux)

- Binario release de la app: **21 MB** (stripped, LTO thin).
- Instalador `.deb`: **9,6 MB**.
- Contenido del `.deb`: solo `usr/bin/capi-desktop`, iconos y `.desktop`.
  **0 entradas `capi-core`**.
- Tiempo de compilación release en esta máquina: ~16 min.
- El binario Kotlin anterior pesaba 61 MB por sí solo.

### Verificación de la retirada

- `npx tsc --noEmit` y `npm run build`: correctos.
- `cargo test` del motor: 6/6; test live contra YouTube: 1/1 (manual).
- `cargo check` y `npm run tauri build -- --debug --no-bundle`: correctos.
- Arranque real en modo `--hidden` con el identificador de desarrollo:
  el proceso no creó ningún hijo `capi-core`; solo aparecieron los procesos
  de WebKit. El proxy arrancó en el siguiente puerto libre (12762 porque la
  app instalada antigua ocupaba 12761).
- La app instalada antigua y su daemon siguen corriendo aparte; no forman
  parte del build nuevo.

### Endurecimiento del proxy local (completado)

- Token aleatorio de 128 bits por sesión, obligatorio en `/play`, `/image`,
  `/api/*` y `/tv`. Sin token se responde **403**.
- `/play?url=` solo acepta hosts `*.googlevideo.com` y HTTPS.
- `/image?url=` solo acepta `*.ytimg.com`, `*.googleusercontent.com` y
  `*.ggpht.com`.
- `/play?path=` canonicaliza la ruta y solo sirve archivos dentro de
  `appdata/Capi` o de una carpeta registrada por `listar_archivos_locales`.
- La página de TV recibe el token inyectado para su polling de `/api/state`.
- El frontend obtiene `{ port, token }` de `obtener_local_port` y lo añade a
  las URLs de reproducción.
- `CAPI_PROXY_TOKEN` permite fijar el token en pruebas.

Verificado con la app real y `curl`:

| Petición | Resultado |
|---|---|
| `/api/state` sin token | 403 |
| `/api/state?token=…` | 200 |
| `/tv` sin token | 403 |
| `/tv?token=…` | 200, token inyectado |
| `/play?url=https://example.com/...` con token | 403 (host no permitido) |
| `/play?url=<googlevideo>&token=…` + Range | **206**, 1 KB |
| mismo stream sin token | 403 |
| `/play?path=` dentro de `appdata/Capi` + token | 206, `audio/mpeg` |
| `/play?path=` fuera de las raíces registradas | 404 |
| archivo vacío | 416 |
| `Range` con inicio fuera del archivo | 416 |
| `Range: bytes=0-` (estilo WebKit/GStreamer) a través del proxy | 206, 1 MiB |
| reproducción completa a través del proxy con `gst-play` | OK |

### Incidencia: «The operation is not supported»

La reproducción fallaba porque WebKit enviaba `Range: bytes=0-` y el proxy lo
reenviaba tal cual a googlevideo, que responde **403** a rangos abiertos
(el cliente IOS además limita el resto a ~1 MiB). El `<audio>` recibía un 403
y lanzaba `NotSupportedError`.

Corrección doble:

1. El proxy pide siempre un bloque acotado de **1 MiB** y deja que el cliente
   solicite el siguiente.
2. El motor resuelve streams con el cliente **VISIONOS** (visitorData, sin PO
   token), que devuelve URLs con soporte de rangos completos y abiertos. Se
   descartó IOS para reproducción porque solo sirve ~1 MiB.

Verificado con `curl` a través del proxy (`bytes=0-` → 206, 1 MiB;
`bytes=1048576-2097151` → 206) y con `gst-play` a través del proxy.

### Reproducción: final prematuro y saltos de pista

WebKit hace la primera petición de audio **sin cabecera `Range`**. El proxy
devolvía un `206` de 1 MiB; el elemento `<audio>` interpretaba que el archivo
duraba eso y disparaba `ended` a mitad de canción, saltando a la siguiente.

Corrección: si la petición no trae `Range`, el proxy pide `bytes=0-`, reescribe
el `206` upstream a un **`200` con la longitud total** y transmite el archivo
completo. Las peticiones con `Range` siguen usando bloques de 1 MiB.

Verificado: sin `Range` → `200`, `Content-Length: 3230877`, cuerpo completo;
`Range: bytes=1048576-2097151` → `206`, 1 MiB.

### Thumbnails

- El motor normaliza las portadas a **544 px** (`=w544-h544`) y convierte
  `default/hqdefault/sddefault` en `maxresdefault`.
- Si una pista no trae cover, se usa `i.ytimg.com/vi/<id>/maxresdefault.jpg`.
- El frontend pide 544 px (no 600, que devolvía 404), usa `maxresdefault` con
  fallback a `hqdefault` y estrena una caché IndexedDB nueva para descartar los
  blobs de baja resolución.

### Búsquedas

- Las búsquedas repetidas se cachean 5 minutos en memoria (máx. 64 consultas).

### Pendiente antes del release

1. QA de runtime con un perfil limpio: búsqueda, play, álbum, playlist, artista,
   letras y descargas.
2. Probar Windows y macOS (especialmente M4A/AAC en WebView).
3. Migrar descargas/caché a Rust o dejarlas como están hasta la siguiente fase.

## Siguiente fase

1. QA de runtime y pruebas en las tres plataformas.
2. Endurecer el servidor local y quitar el proxy de imagen/stream abierto.
3. Descargas atómicas y caché persistente.

