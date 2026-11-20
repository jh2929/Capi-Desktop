use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use capi_engine::models::Song;
use capi_engine::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::Emitter;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::MediaState;

static MCP_PORT: AtomicU16 = AtomicU16::new(0);

#[derive(Debug, Serialize, Deserialize)]
pub struct McpInfo {
    pub enabled: bool,
    pub port: u16,
    pub http_url: String,
    pub sse_url: String,
    pub tools_count: usize,
}

pub fn get_mcp_port() -> u16 {
    MCP_PORT.load(Ordering::SeqCst)
}

pub fn start_mcp_server(
    engine: Arc<Engine>,
    media_state: Arc<Mutex<MediaState>>,
    app_handle: tauri::AppHandle,
    capi_dir: PathBuf,
) -> Result<u16, String> {
    // Buscar puerto libre: intentar 19840 primero, luego 19841..19850
    let mut chosen_port = None;
    if TcpListener::bind("127.0.0.1:19840").is_ok() {
        chosen_port = Some(19840);
    } else {
        for p in 19841..19855 {
            if TcpListener::bind(format!("127.0.0.1:{}", p)).is_ok() {
                chosen_port = Some(p);
                break;
            }
        }
    }

    let port = chosen_port.ok_or_else(|| {
        "No se pudo encontrar un puerto libre para MCP en el rango 19840-19855".to_string()
    })?;
    MCP_PORT.store(port, Ordering::SeqCst);

    let engine_clone = engine.clone();
    let media_state_clone = media_state.clone();
    let app_handle_clone = app_handle.clone();
    let capi_dir_clone = capi_dir.clone();

    std::thread::Builder::new()
        .name("capi-mcp-server".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
            {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("[MCP] Error al inicializar runtime de Tokio para MCP: {}", e);
                    return;
                }
            };

            rt.block_on(async move {
                let addr = format!("127.0.0.1:{}", port);
                let listener = match tokio::net::TcpListener::bind(&addr).await {
                    Ok(l) => {
                        println!("[MCP] Servidor MCP iniciado con éxito en http://{}", addr);
                        l
                    }
                    Err(e) => {
                        eprintln!("[MCP] Error al iniciar servidor MCP en {}: {}", addr, e);
                        return;
                    }
                };

        loop {
            let (mut socket, _) = match listener.accept().await {
                Ok(conn) => conn,
                Err(_) => continue,
            };

            let engine = engine_clone.clone();
            let media_state = media_state_clone.clone();
            let app_handle = app_handle_clone.clone();
            let capi_dir = capi_dir_clone.clone();

            tokio::spawn(async move {
                let mut buffer = vec![0u8; 65536];
                let mut total_read = 0;

                // Leer encabezados HTTP
                loop {
                    match socket.read(&mut buffer[total_read..]).await {
                        Ok(0) => return,
                        Ok(n) => {
                            total_read += n;
                            if buffer[..total_read].windows(4).any(|w| w == b"\r\n\r\n") {
                                break;
                            }
                            if total_read >= buffer.len() {
                                break;
                            }
                        }
                        Err(_) => return,
                    }
                }

                let req_text = String::from_utf8_lossy(&buffer[..total_read]);
                let mut lines = req_text.lines();
                let request_line = lines.next().unwrap_or("");
                let parts: Vec<&str> = request_line.split_whitespace().collect();
                if parts.len() < 2 {
                    return;
                }

                let method = parts[0];
                let raw_path = parts[1];
                let path = raw_path.split('?').next().unwrap_or(raw_path);

                let cors_headers = "Access-Control-Allow-Origin: *\r\nAccess-Control-Allow-Methods: GET, POST, OPTIONS\r\nAccess-Control-Allow-Headers: Content-Type, Authorization, X-Requested-With\r\n";

                // Manejo de Preflight OPTIONS
                if method == "OPTIONS" {
                    let resp = format!(
                        "HTTP/1.1 204 No Content\r\n{}Access-Control-Max-Age: 86400\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                        cors_headers
                    );
                    let _ = socket.write_all(resp.as_bytes()).await;
                    return;
                }

                // Endpoint de información /mcp/info o /info
                if method == "GET" && (path == "/mcp/info" || path == "/info") {
                    let info = json!({
                        "name": "Capi Desktop MCP Server",
                        "version": env!("CARGO_PKG_VERSION"),
                        "status": "online",
                        "port": port,
                        "endpoints": {
                            "jsonrpc_http": format!("http://127.0.0.1:{}/mcp", port),
                            "sse": format!("http://127.0.0.1:{}/sse", port)
                        },
                        "claude_config": {
                            "capi": {
                                "url": format!("http://127.0.0.1:{}/sse", port)
                            }
                        },
                        "cursor_config": {
                            "capi": {
                                "url": format!("http://127.0.0.1:{}/sse", port)
                            }
                        }
                    });
                    let body = serde_json::to_string_pretty(&info).unwrap_or_default();
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json; charset=utf-8\r\n{}Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                        cors_headers,
                        body.len(),
                        body
                    );
                    let _ = socket.write_all(resp.as_bytes()).await;
                    return;
                }

                // Endpoint SSE (Server-Sent Events) para MCP
                if method == "GET" && (path == "/sse" || path == "/mcp/sse") {
                    let session_id = uuid::Uuid::new_v4().to_string();
                    let sse_header = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: keep-alive\r\n{}Access-Control-Allow-Origin: *\r\n\r\n",
                        cors_headers
                    );
                    if socket.write_all(sse_header.as_bytes()).await.is_err() {
                        return;
                    }

                    // Enviar evento inicial 'endpoint' según especificación MCP SSE
                    let init_event = format!("event: endpoint\ndata: /mcp/message?sessionId={}\n\n", session_id);
                    if socket.write_all(init_event.as_bytes()).await.is_err() {
                        return;
                    }

                    // Mantener vivo con pings periódicos
                    let mut interval = tokio::time::interval(Duration::from_secs(15));
                    loop {
                        interval.tick().await;
                        if socket.write_all(b": ping\n\n").await.is_err() {
                            break;
                        }
                    }
                    return;
                }

                // Extracción del cuerpo JSON para peticiones POST
                let header_end = buffer[..total_read]
                    .windows(4)
                    .position(|w| w == b"\r\n\r\n")
                    .map(|pos| pos + 4)
                    .unwrap_or(total_read);

                let mut body_bytes = buffer[header_end..total_read].to_vec();

                // Buscar Content-Length para asegurarse de tener todo el cuerpo
                let mut content_length = 0;
                for line in lines {
                    if line.to_lowercase().starts_with("content-length:") {
                        if let Some(val_str) = line.split(':').nth(1) {
                            content_length = val_str.trim().parse::<usize>().unwrap_or(0);
                        }
                    }
                }

                while body_bytes.len() < content_length {
                    let mut chunk = vec![0u8; 4096];
                    match socket.read(&mut chunk).await {
                        Ok(0) => break,
                        Ok(n) => body_bytes.extend_from_slice(&chunk[..n]),
                        Err(_) => break,
                    }
                }

                let body_str = String::from_utf8_lossy(&body_bytes);
                let json_req: Value = match serde_json::from_str(&body_str) {
                    Ok(val) => val,
                    Err(_) => {
                        let err_resp = json!({
                            "jsonrpc": "2.0",
                            "error": { "code": -32700, "message": "Parse error" },
                            "id": null
                        });
                        let body = serde_json::to_string(&err_resp).unwrap_or_default();
                        let resp = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json; charset=utf-8\r\n{}Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                            cors_headers, body.len(), body
                        );
                        let _ = socket.write_all(resp.as_bytes()).await;
                        return;
                    }
                };

                let json_resp = handle_mcp_request(
                    json_req,
                    &engine,
                    &media_state,
                    &app_handle,
                    &capi_dir,
                )
                .await;

                if let Some(resp_val) = json_resp {
                    let body = serde_json::to_string(&resp_val).unwrap_or_default();
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json; charset=utf-8\r\n{}Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                        cors_headers,
                        body.len(),
                        body
                    );
                    let _ = socket.write_all(resp.as_bytes()).await;
                } else {
                    let resp = format!(
                        "HTTP/1.1 204 No Content\r\n{}Content-Length: 0\r\nConnection: close\r\n\r\n",
                        cors_headers
                    );
                    let _ = socket.write_all(resp.as_bytes()).await;
                }
            });
        }
    });
})
.map_err(|e| format!("Error al iniciar thread de MCP: {}", e))?;

    Ok(port)
}

pub async fn handle_mcp_request(
    req: Value,
    engine: &Arc<Engine>,
    media_state: &Arc<Mutex<MediaState>>,
    app_handle: &tauri::AppHandle,
    capi_dir: &Path,
) -> Option<Value> {
    let method = req.get("method")?.as_str()?;
    let id = req.get("id").cloned();

    match method {
        "initialize" => Some(json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "protocolVersion": "2024-11-05",
                "capabilities": {
                    "tools": { "listChanged": false }
                },
                "serverInfo": {
                    "name": "capi-desktop-mcp",
                    "version": env!("CARGO_PKG_VERSION")
                }
            }
        })),

        "notifications/initialized" | "initialized" => None,

        "ping" => Some(json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {}
        })),

        "tools/list" => {
            let tools = get_tool_definitions();
            Some(json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "tools": tools
                }
            }))
        }

        "tools/call" => {
            let params = req.get("params")?;
            let tool_name = params.get("name")?.as_str()?;
            let arguments = params.get("arguments").cloned().unwrap_or(json!({}));

            let (result_text, is_error) = execute_tool(
                tool_name,
                arguments,
                engine,
                media_state,
                app_handle,
                capi_dir,
            )
            .await;

            Some(json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "content": [
                        {
                            "type": "text",
                            "text": result_text
                        }
                    ],
                    "isError": is_error
                }
            }))
        }

        _ => Some(json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {
                "code": -32601,
                "message": format!("Method not found: {}", method)
            }
        })),
    }
}

fn get_tool_definitions() -> Vec<Value> {
    vec![
        json!({
            "name": "capi_get_player_state",
            "description": "Obtiene el estado completo de reproducción de Capi: canción actual, artista, álbum, duración, posición en segundos, volumen (0.0 a 1.0) y si está reproduciendo.",
            "inputSchema": { "type": "object", "properties": {} }
        }),
        json!({
            "name": "capi_play",
            "description": "Inicia o reanuda la reproducción del reproductor.",
            "inputSchema": { "type": "object", "properties": {} }
        }),
        json!({
            "name": "capi_pause",
            "description": "Pausa la reproducción actual de música.",
            "inputSchema": { "type": "object", "properties": {} }
        }),
        json!({
            "name": "capi_toggle_play",
            "description": "Alterna entre reproducir y pausar.",
            "inputSchema": { "type": "object", "properties": {} }
        }),
        json!({
            "name": "capi_next",
            "description": "Salta a la siguiente canción en la cola de reproducción.",
            "inputSchema": { "type": "object", "properties": {} }
        }),
        json!({
            "name": "capi_previous",
            "description": "Vuelve a la canción anterior en la cola de reproducción.",
            "inputSchema": { "type": "object", "properties": {} }
        }),
        json!({
            "name": "capi_set_volume",
            "description": "Ajusta el volumen del reproductor de Capi (de 0.0 a 1.0 o de 0 a 100).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "level": {
                        "type": "number",
                        "description": "Nivel de volumen (entre 0 y 1 o entre 0 y 100)"
                    }
                },
                "required": ["level"]
            }
        }),
        json!({
            "name": "capi_seek",
            "description": "Salta a una posición de tiempo en segundos dentro de la canción actual.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "position": {
                        "type": "number",
                        "description": "Posición en segundos a donde saltar"
                    }
                },
                "required": ["position"]
            }
        }),
        json!({
            "name": "capi_search",
            "description": "Busca canciones, artistas, álbumes o playlists en el catálogo de música de Capi / YouTube Music.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Término de búsqueda musical" },
                    "filter": {
                        "type": "string",
                        "enum": ["songs", "playlists", "artists", "albums"],
                        "description": "Tipo de contenido a buscar (por defecto 'songs')"
                    }
                },
                "required": ["query"]
            }
        }),
        json!({
            "name": "capi_play_track",
            "description": "Busca y reproduce inmediatamente una canción por nombre/artista o por su ID de video de YouTube.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Nombre de la canción o ID de video de YouTube (ej. 'Bohemian Rhapsody Queen' o 'fJ9rUzIMcZQ')"
                    }
                },
                "required": ["query"]
            }
        }),
        json!({
            "name": "capi_get_lyrics",
            "description": "Obtiene la letra de la canción actual o de una canción especificada (sincronizada o texto plano).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "track_id": { "type": "string", "description": "ID de la canción (opcional)" },
                    "title": { "type": "string", "description": "Título de la canción (opcional)" },
                    "artist": { "type": "string", "description": "Artista de la canción (opcional)" }
                }
            }
        }),
        json!({
            "name": "capi_get_playlists",
            "description": "Lista todas las playlists del usuario (tanto las sincronizadas automáticamente de YouTube Music como las creadas en la app).",
            "inputSchema": { "type": "object", "properties": {} }
        }),
        json!({
            "name": "capi_get_playlist_tracks",
            "description": "Obtiene la lista de canciones de una playlist dada su ID de playlist.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "playlist_id": { "type": "string", "description": "ID de la playlist (ej. 'PL...' o ID local)" }
                },
                "required": ["playlist_id"]
            }
        }),
        json!({
            "name": "capi_play_playlist",
            "description": "Inicia la reproducción de una playlist completa por su ID.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "playlist_id": { "type": "string", "description": "ID de la playlist a reproducir" }
                },
                "required": ["playlist_id"]
            }
        }),
        json!({
            "name": "capi_get_liked_songs",
            "description": "Obtiene la lista de canciones favoritas (Me Gusta) del usuario sincronizadas de YouTube Music.",
            "inputSchema": { "type": "object", "properties": {} }
        }),
        json!({
            "name": "capi_like_track",
            "description": "Marca o desmarca como favorita (Me Gusta) una canción en YouTube Music.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "video_id": { "type": "string", "description": "ID de video de YouTube de la canción" },
                    "like": { "type": "boolean", "description": "true para Me Gusta, false para quitar Me Gusta" }
                },
                "required": ["video_id"]
            }
        }),
        json!({
            "name": "capi_sync_youtube",
            "description": "Ejecuta una sincronización completa en segundo plano con YouTube Music (playlists, canciones favoritas, artistas suscritos).",
            "inputSchema": { "type": "object", "properties": {} }
        }),
        json!({
            "name": "capi_get_downloads",
            "description": "Lista todas las canciones descargadas localmente en la app para escuchar sin conexión.",
            "inputSchema": { "type": "object", "properties": {} }
        }),
        json!({
            "name": "capi_download_track",
            "description": "Inicia la descarga de una canción para escucharla sin conexión por nombre o ID de YouTube.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Nombre de la canción o ID de YouTube a descargar" }
                },
                "required": ["query"]
            }
        }),
        json!({
            "name": "capi_get_account_info",
            "description": "Obtiene el estado de la cuenta de YouTube Music conectada a Capi (nombre, email y estado de conexión).",
            "inputSchema": { "type": "object", "properties": {} }
        }),
        json!({
            "name": "capi_navigate",
            "description": "Navega a una sección específica de la aplicación gráfica Capi ('home', 'explore', 'buscar', 'biblioteca', 'playlists', 'favoritos', 'download_manager', 'settings').",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "tab": {
                        "type": "string",
                        "enum": ["home", "explore", "buscar", "biblioteca", "playlists", "favoritos", "download_manager", "settings"],
                        "description": "Nombre de la pestaña a la que navegar"
                    }
                },
                "required": ["tab"]
            }
        }),
        json!({
            "name": "capi_show_toast",
            "description": "Muestra una notificación emergente visual (toast) en la interfaz gráfica de Capi.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "message": { "type": "string", "description": "Mensaje que aparecerá en pantalla" }
                },
                "required": ["message"]
            }
        }),
    ]
}

async fn execute_tool(
    name: &str,
    args: Value,
    engine: &Arc<Engine>,
    media_state: &Arc<Mutex<MediaState>>,
    app_handle: &tauri::AppHandle,
    capi_dir: &Path,
) -> (String, bool) {
    match name {
        "capi_get_player_state" => {
            let guard = match media_state.lock() {
                Ok(g) => g,
                Err(e) => return (format!("Error al leer estado: {}", e), true),
            };
            let state_json = json!({
                "is_playing": guard.is_playing,
                "title": guard.title,
                "artist": guard.artist,
                "album": guard.album,
                "thumbnail": guard.thumbnail,
                "duration": guard.duration,
                "position": guard.position,
                "volume": guard.volume,
                "shuffle": guard.shuffle,
                "repeat": guard.repeat,
                "track_id": guard.track_id
            });
            (
                serde_json::to_string_pretty(&state_json).unwrap_or_default(),
                false,
            )
        }

        "capi_play" => {
            let _ = app_handle.emit("mpris-command", json!({ "action": "play" }));
            ("Reproducción iniciada".to_string(), false)
        }

        "capi_pause" => {
            let _ = app_handle.emit("mpris-command", json!({ "action": "pause" }));
            ("Reproducción pausada".to_string(), false)
        }

        "capi_toggle_play" => {
            let _ = app_handle.emit("mpris-command", json!({ "action": "play-pause" }));
            ("Reproducción alternada (play/pause)".to_string(), false)
        }

        "capi_next" => {
            let _ = app_handle.emit("mpris-command", json!({ "action": "next" }));
            ("Saltando a la siguiente canción".to_string(), false)
        }

        "capi_previous" => {
            let _ = app_handle.emit("mpris-command", json!({ "action": "prev" }));
            ("Volviendo a la canción anterior".to_string(), false)
        }

        "capi_set_volume" => {
            let raw_level = args.get("level").and_then(|v| v.as_f64()).unwrap_or(0.8);
            let level = if raw_level > 1.0 {
                raw_level / 100.0
            } else {
                raw_level
            };
            let clamped = level.clamp(0.0, 1.0);
            let _ = app_handle.emit(
                "mpris-command",
                json!({ "action": "volume", "level": clamped }),
            );
            (
                format!("Volumen establecido a {:.0}%", clamped * 100.0),
                false,
            )
        }

        "capi_seek" => {
            let pos = args.get("position").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let _ = app_handle.emit(
                "mpris-command",
                json!({ "action": "seek", "position": pos }),
            );
            (format!("Posición establecida a {:.1}s", pos), false)
        }

        "capi_search" => {
            let query = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
            if query.trim().is_empty() {
                return (
                    "El término de búsqueda no puede estar vacío".to_string(),
                    true,
                );
            }
            let filter = args
                .get("filter")
                .and_then(|v| v.as_str())
                .unwrap_or("songs");

            match filter {
                "playlists" => match engine.search_playlists(query).await {
                    Ok(playlists) => {
                        let json_val = serde_json::to_string_pretty(&playlists).unwrap_or_default();
                        (json_val, false)
                    }
                    Err(e) => (format!("Error en búsqueda de playlists: {}", e), true),
                },
                _ => match engine.search(query).await {
                    Ok(songs) => {
                        let json_val = serde_json::to_string_pretty(&songs).unwrap_or_default();
                        (json_val, false)
                    }
                    Err(e) => (format!("Error en búsqueda: {}", e), true),
                },
            }
        }

        "capi_play_track" => {
            let query = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
            if query.trim().is_empty() {
                return (
                    "El parámetro 'query' no puede estar vacío".to_string(),
                    true,
                );
            }

            // Si es un ID de video directo (11 caracteres alfanuméricos)
            let is_direct_id = query.len() == 11
                && query
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');

            let song_to_play: Option<Song> = if is_direct_id {
                match engine.search(query).await {
                    Ok(songs) => songs.into_iter().find(|s| s.id == query).or_else(|| {
                        Some(Song {
                            id: query.to_string(),
                            title: query.to_string(),
                            artist: "YouTube".to_string(),
                            primary_artist: None,
                            album: None,
                            duration: None,
                            thumbnail: Some(format!(
                                "https://i.ytimg.com/vi/{}/hqdefault.jpg",
                                query
                            )),
                            explicit: false,
                            artist_id: None,
                        })
                    }),
                    Err(_) => Some(Song {
                        id: query.to_string(),
                        title: query.to_string(),
                        artist: "YouTube".to_string(),
                        primary_artist: None,
                        album: None,
                        duration: None,
                        thumbnail: Some(format!("https://i.ytimg.com/vi/{}/hqdefault.jpg", query)),
                        explicit: false,
                        artist_id: None,
                    }),
                }
            } else {
                match engine.search(query).await {
                    Ok(songs) => songs.into_iter().next(),
                    Err(e) => return (format!("Error buscando '{}': {}", query, e), true),
                }
            };

            if let Some(song) = song_to_play {
                let _ = app_handle.emit(
                    "mcp-command",
                    json!({
                        "action": "play-track",
                        "track": song
                    }),
                );
                (
                    format!(
                        "Reproduciendo en Capi: '{}' por '{}'",
                        song.title, song.artist
                    ),
                    false,
                )
            } else {
                (
                    format!("No se encontraron canciones para '{}'", query),
                    true,
                )
            }
        }

        "capi_get_lyrics" => {
            let (track_id, title, artist, duration) = {
                let guard = media_state.lock().ok();
                let current_id = guard
                    .as_ref()
                    .map(|g| g.track_id.clone())
                    .unwrap_or_default();
                let current_title = guard.as_ref().map(|g| g.title.clone()).unwrap_or_default();
                let current_artist = guard.as_ref().map(|g| g.artist.clone()).unwrap_or_default();
                let current_duration = guard.as_ref().map(|g| g.duration as u32).unwrap_or(0);

                let id = args
                    .get("track_id")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or(current_id);
                let t = args
                    .get("title")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or(current_title);
                let a = args
                    .get("artist")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or(current_artist);
                (id, t, a, current_duration)
            };

            if track_id.is_empty() && title.is_empty() {
                return ("No hay ninguna canción reproduciéndose y no se especificó 'title' o 'track_id'".to_string(), true);
            }

            match engine
                .lyrics(&track_id, &title, &artist, duration, None)
                .await
            {
                Ok(lyrics) => {
                    let json_val = serde_json::to_string_pretty(&lyrics).unwrap_or_default();
                    (json_val, false)
                }
                Err(e) => (format!("Error obteniendo letra: {}", e), true),
            }
        }

        "capi_get_playlists" => match engine.sync_ytm().await {
            Ok(data) => {
                let json_val = serde_json::to_string_pretty(&data.playlists).unwrap_or_default();
                (json_val, false)
            }
            Err(e) => (format!("Error obteniendo playlists: {}", e), true),
        },

        "capi_get_playlist_tracks" => {
            let playlist_id = args
                .get("playlist_id")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if playlist_id.trim().is_empty() {
                return (
                    "El parámetro 'playlist_id' es obligatorio".to_string(),
                    true,
                );
            }

            match engine.playlist_tracks(playlist_id).await {
                Ok(tracks) => {
                    let json_val = serde_json::to_string_pretty(&tracks).unwrap_or_default();
                    (json_val, false)
                }
                Err(e) => (
                    format!("Error obteniendo canciones de playlist: {}", e),
                    true,
                ),
            }
        }

        "capi_play_playlist" => {
            let playlist_id = args
                .get("playlist_id")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if playlist_id.trim().is_empty() {
                return (
                    "El parámetro 'playlist_id' es obligatorio".to_string(),
                    true,
                );
            }

            match engine.playlist_tracks(playlist_id).await {
                Ok(tracks) => {
                    if tracks.is_empty() {
                        return (
                            "La playlist está vacía o no tiene canciones accesibles".to_string(),
                            true,
                        );
                    }
                    let _ = app_handle.emit(
                        "mcp-command",
                        json!({
                            "action": "play-playlist",
                            "tracks": tracks,
                            "playlistId": playlist_id
                        }),
                    );
                    (
                        format!("Reproduciendo playlist con {} canciones", tracks.len()),
                        false,
                    )
                }
                Err(e) => (format!("Error al cargar playlist: {}", e), true),
            }
        }

        "capi_get_liked_songs" => match engine.sync_ytm().await {
            Ok(data) => {
                let json_val = serde_json::to_string_pretty(&data.liked_songs).unwrap_or_default();
                (json_val, false)
            }
            Err(e) => (format!("Error obteniendo canciones favoritas: {}", e), true),
        },

        "capi_like_track" => {
            let video_id = args.get("video_id").and_then(|v| v.as_str()).unwrap_or("");
            if video_id.trim().is_empty() {
                return ("El parámetro 'video_id' es obligatorio".to_string(), true);
            }
            let like = args.get("like").and_then(|v| v.as_bool()).unwrap_or(true);

            match engine.like_video(video_id, like).await {
                Ok(res) => {
                    let _ = app_handle.emit(
                        "mcp-command",
                        json!({
                            "action": "like-track",
                            "videoId": video_id,
                            "like": like
                        }),
                    );
                    (
                        format!("Estado de Me Gusta actualizado (éxito: {})", res),
                        false,
                    )
                }
                Err(e) => (format!("Error al actualizar Me Gusta: {}", e), true),
            }
        }

        "capi_sync_youtube" => match engine.sync_ytm().await {
            Ok(sync_res) => {
                let _ = app_handle.emit(
                    "ytm-sync-completed",
                    serde_json::to_value(&sync_res).unwrap_or_default(),
                );
                (format!(
                        "Sincronización completada con éxito: {} canciones favoritas, {} playlists, {} artistas suscritos.",
                        sync_res.liked_songs.len(),
                        sync_res.playlists.len(),
                        sync_res.artists.len()
                    ), false)
            }
            Err(e) => (format!("Error en sincronización: {}", e), true),
        },

        "capi_get_downloads" => {
            let mut tracks = Vec::new();
            scan_downloads_dir(capi_dir, 0, 10, &mut tracks);
            let json_val = serde_json::to_string_pretty(&tracks).unwrap_or_default();
            (json_val, false)
        }

        "capi_download_track" => {
            let query = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
            if query.trim().is_empty() {
                return ("El parámetro 'query' es obligatorio".to_string(), true);
            }

            match engine.search(query).await {
                Ok(songs) => {
                    if let Some(song) = songs.into_iter().next() {
                        let _ = app_handle.emit(
                            "mcp-command",
                            json!({
                                "action": "download-track",
                                "track": song
                            }),
                        );
                        (
                            format!(
                                "Iniciando descarga de '{}' por '{}'",
                                song.title, song.artist
                            ),
                            false,
                        )
                    } else {
                        (
                            format!("No se encontró ninguna canción para '{}'", query),
                            true,
                        )
                    }
                }
                Err(e) => (
                    format!("Error buscando canción para descargar: {}", e),
                    true,
                ),
            }
        }

        "capi_get_account_info" => {
            let state = engine.ytm_account_state().await;
            let json_val = serde_json::to_string_pretty(&state).unwrap_or_default();
            (json_val, false)
        }

        "capi_navigate" => {
            let tab = args.get("tab").and_then(|v| v.as_str()).unwrap_or("home");
            let _ = app_handle.emit(
                "mcp-command",
                json!({
                    "action": "navigate",
                    "tab": tab
                }),
            );
            (format!("Navegando a la pestaña: {}", tab), false)
        }

        "capi_show_toast" => {
            let message = args.get("message").and_then(|v| v.as_str()).unwrap_or("");
            if message.trim().is_empty() {
                return ("El mensaje no puede estar vacío".to_string(), true);
            }
            let _ = app_handle.emit(
                "mcp-command",
                json!({
                    "action": "show-toast",
                    "message": message
                }),
            );
            (format!("Notificación mostrada: {}", message), false)
        }

        _ => (format!("Herramienta no implementada: {}", name), true),
    }
}

fn scan_downloads_dir(dir: &std::path::Path, depth: usize, max_depth: usize, out: &mut Vec<Value>) {
    if depth > max_depth {
        return;
    }
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if name != "cache" && name != "engine" && name != "logs" {
                    scan_downloads_dir(&path, depth + 1, max_depth, out);
                }
            } else if path.is_file() {
                if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                    let ext_lower = ext.to_lowercase();
                    if matches!(
                        ext_lower.as_str(),
                        "mp3" | "m4a" | "opus" | "webm" | "flac" | "ogg" | "wav"
                    ) {
                        let file_stem = path
                            .file_stem()
                            .and_then(|n| n.to_str())
                            .unwrap_or("Canción");
                        let artist_dir = path
                            .parent()
                            .and_then(|p| p.file_name())
                            .and_then(|n| n.to_str())
                            .unwrap_or("Local");
                        out.push(json!({
                            "title": file_stem,
                            "artist": artist_dir,
                            "path": path.to_string_lossy(),
                            "extension": ext_lower
                        }));
                    }
                }
            }
        }
    }
}

pub fn run_stdio_mcp() {
    use std::io::{BufRead, Write};
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap_or_default();

    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();

    let mut port = MCP_PORT.load(Ordering::SeqCst);
    if port == 0 {
        port = 19840;
    }

    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let req_json: Option<Value> = serde_json::from_str(trimmed).ok();
        let method = req_json
            .as_ref()
            .and_then(|v| v.get("method").and_then(|m| m.as_str()));
        let id = req_json.as_ref().and_then(|v| v.get("id").cloned());

        let mut target_port = port;
        let mut resp = client
            .post(format!("http://127.0.0.1:{}/mcp", target_port))
            .header("Content-Type", "application/json")
            .body(trimmed.to_string())
            .send();

        if resp.is_err() {
            for p in 19840..=19855 {
                if p == target_port {
                    continue;
                }
                if let Ok(r) = client
                    .get(format!("http://127.0.0.1:{}/mcp/info", p))
                    .timeout(Duration::from_millis(50))
                    .send()
                {
                    if r.status().is_success() {
                        target_port = p;
                        resp = client
                            .post(format!("http://127.0.0.1:{}/mcp", target_port))
                            .header("Content-Type", "application/json")
                            .body(trimmed.to_string())
                            .send();
                        break;
                    }
                }
            }
        }

        match resp {
            Ok(r) => {
                if let Ok(text) = r.text() {
                    let _ = writeln!(stdout, "{}", text.trim());
                    let _ = stdout.flush();
                }
            }
            Err(e) => {
                if method == Some("initialize") {
                    let init_resp = json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": {
                            "protocolVersion": "2024-11-05",
                            "capabilities": {
                                "tools": { "listChanged": false }
                            },
                            "serverInfo": {
                                "name": "capi-desktop-mcp",
                                "version": env!("CARGO_PKG_VERSION")
                            }
                        }
                    });
                    let _ = writeln!(stdout, "{}", init_resp);
                    let _ = stdout.flush();
                } else if method == Some("notifications/initialized")
                    || method == Some("initialized")
                {
                    // Notificación sin respuesta
                } else if method == Some("tools/list") {
                    let tools = get_tool_definitions();
                    let list_resp = json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": {
                            "tools": tools
                        }
                    });
                    let _ = writeln!(stdout, "{}", list_resp);
                    let _ = stdout.flush();
                } else if method == Some("tools/call") {
                    let call_resp = json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": {
                            "content": [
                                {
                                    "type": "text",
                                    "text": "Error: Capi Desktop no está en ejecución. Por favor inicia la aplicación gráfica Capi para que pueda interactuar con el reproductor."
                                }
                            ],
                            "isError": true
                        }
                    });
                    let _ = writeln!(stdout, "{}", call_resp);
                    let _ = stdout.flush();
                } else {
                    let err_json = json!({
                        "jsonrpc": "2.0",
                        "error": {
                            "code": -32000,
                            "message": format!("Capi Desktop no está en ejecución o el servidor MCP no responde: {}", e)
                        },
                        "id": id
                    });
                    let _ = writeln!(stdout, "{}", err_json);
                    let _ = stdout.flush();
                }
            }
        }
    }
}
