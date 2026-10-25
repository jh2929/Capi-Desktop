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

## Siguiente fase

1. Integrar `capi-engine` en Tauri como path dependency.
2. Reemplazar comandos `buscar_cancion`, `obtener_stream`, etc. manteniendo el
   JSON actual para no tocar React.
3. Añadir tests de contrato y pruebas en Windows/macOS.
4. Retirar el daemon solo cuando la paridad esté verificada.
