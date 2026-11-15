use capi_engine::Engine;
use discord_rich_presence::{activity, DiscordIpc, DiscordIpcClient};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{Read, Seek, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::WebviewWindowBuilder;
use tauri::{Emitter, Manager};

struct LocalServerState {
    port: u16,
    token: String,
}

#[derive(serde::Serialize)]
struct LocalServerInfo {
    port: u16,
    token: String,
}

#[derive(Clone, Default)]
struct LocalRoots(Arc<Mutex<Vec<PathBuf>>>);

impl LocalRoots {
    fn allow(&self, path: &std::path::Path) -> bool {
        let Ok(mut roots) = self.0.lock() else {
            return false;
        };
        if roots.iter().any(|root| path.starts_with(root)) {
            return true;
        }
        roots.push(path.to_path_buf());
        roots.len() <= 32
    }

    fn contains(&self, path: &std::path::Path) -> bool {
        self.0
            .lock()
            .map(|roots| roots.iter().any(|root| path.starts_with(root)))
            .unwrap_or(false)
    }

    fn get_all(&self) -> Vec<PathBuf> {
        self.0.lock().map(|roots| roots.clone()).unwrap_or_default()
    }
}

fn normalize_for_loose_match(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

fn find_audio_file_recursive(
    dir: &std::path::Path,
    target_filename: Option<&std::ffi::OsStr>,
    target_stem: Option<&std::ffi::OsStr>,
    title_hint: Option<&str>,
    _artist_hint: Option<&str>,
    depth: usize,
    max_depth: usize,
) -> Option<std::path::PathBuf> {
    if depth > max_depth {
        return None;
    }
    let entries = std::fs::read_dir(dir).ok()?;
    let mut subdirs = Vec::new();

    let target_fname_str = target_filename.map(|f| f.to_string_lossy().to_string());
    let target_stem_str = target_stem.map(|f| f.to_string_lossy().to_string());
    let target_norm = target_stem_str.as_deref().map(normalize_for_loose_match);
    let title_norm = title_hint
        .map(normalize_for_loose_match)
        .filter(|s| !s.is_empty());

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() {
            if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                let ext_lower = ext.to_lowercase();
                if matches!(
                    ext_lower.as_str(),
                    "mp3" | "m4a" | "webm" | "opus" | "wav" | "flac" | "ogg" | "aac"
                ) {
                    let entry_fname = entry.file_name();
                    let entry_fname_str = entry_fname.to_string_lossy();

                    // 1. Exact filename match
                    if let Some(ref target) = target_fname_str {
                        if entry_fname_str == *target
                            || entry_fname_str.eq_ignore_ascii_case(target)
                        {
                            return Some(path);
                        }
                    }

                    // 2. Stem match (ignoring extension / case)
                    if let Some(entry_stem) = path.file_stem().and_then(|s| s.to_str()) {
                        if let Some(ref stem) = target_stem_str {
                            if entry_stem == *stem || entry_stem.eq_ignore_ascii_case(stem) {
                                return Some(path);
                            }
                        }

                        // 3. Normalized alphanumeric match
                        let entry_norm = normalize_for_loose_match(entry_stem);
                        if let Some(ref t_norm) = target_norm {
                            if !t_norm.is_empty()
                                && (entry_norm == *t_norm
                                    || entry_norm.contains(t_norm)
                                    || t_norm.contains(&entry_norm))
                            {
                                return Some(path);
                            }
                        }

                        // 4. Title hint match
                        if let Some(ref t_title) = title_norm {
                            if entry_norm.contains(t_title) {
                                return Some(path);
                            }
                        }
                    }
                }
            }
        } else if path.is_dir() {
            let dir_name = entry.file_name();
            let name_str = dir_name.to_string_lossy();
            if name_str != "cache" && name_str != "engine" && !name_str.starts_with('.') {
                subdirs.push(path);
            }
        }
    }

    for subdir in subdirs {
        if let Some(found) = find_audio_file_recursive(
            &subdir,
            target_filename,
            target_stem,
            title_hint,
            _artist_hint,
            depth + 1,
            max_depth,
        ) {
            return Some(found);
        }
    }

    None
}

fn collect_all_audio_files_recursive(
    dir: &std::path::Path,
    depth: usize,
    max_depth: usize,
    files: &mut Vec<std::path::PathBuf>,
) {
    if depth > max_depth {
        return;
    }
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                    let ext_lower = ext.to_lowercase();
                    if matches!(
                        ext_lower.as_str(),
                        "mp3" | "m4a" | "webm" | "opus" | "wav" | "flac" | "ogg" | "aac"
                    ) {
                        files.push(path);
                    }
                }
            } else if path.is_dir() {
                let name = entry.file_name();
                let name_str = name.to_string_lossy();
                if name_str != "cache" && name_str != "engine" && !name_str.starts_with('.') {
                    collect_all_audio_files_recursive(&path, depth + 1, max_depth, files);
                }
            }
        }
    }
}

pub struct DiscordState {
    pub client: std::sync::Arc<Mutex<Option<DiscordIpcClient>>>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct MediaState {
    pub is_playing: bool,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub thumbnail: String,
    pub duration: f64,
    pub position: f64,
    pub volume: f64,
    pub shuffle: bool,
    pub repeat: String,
    #[serde(default)]
    pub stream_url: String,
    #[serde(default)]
    pub track_id: String,
}

impl Default for MediaState {
    fn default() -> Self {
        Self {
            is_playing: false,
            title: String::new(),
            artist: String::new(),
            album: String::new(),
            thumbnail: String::new(),
            duration: 0.0,
            position: 0.0,
            volume: 0.8,
            shuffle: false,
            repeat: "none".to_string(),
            stream_url: String::new(),
            track_id: String::new(),
        }
    }
}

static ACTIVE_PROXY_CONNECTIONS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);
const MAX_PROXY_CONNECTIONS: usize = 32;

struct ProxyConnGuard;
impl Drop for ProxyConnGuard {
    fn drop(&mut self) {
        ACTIVE_PROXY_CONNECTIONS.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

fn start_local_server(
    capi_dir: PathBuf,
    app: tauri::AppHandle,
    media_state: std::sync::Arc<std::sync::Mutex<MediaState>>,
    token: String,
    roots: LocalRoots,
) -> Result<u16, String> {
    let port = (12761..12771)
        .find(|&p| TcpListener::bind(format!("127.0.0.1:{}", p)).is_ok())
        .ok_or("No se pudo encontrar un puerto libre en el rango 12761-12770".to_string())?;
    let listener = TcpListener::bind(format!("127.0.0.1:{}", port))
        .map_err(|e| format!("No se pudo bindear al puerto {}: {}", port, e))?;
    println!("[PROXY] Server started on port {}", port);
    let capi_dir = capi_dir.canonicalize().unwrap_or(capi_dir);
    let capi_dir = capi_dir.clone();

    let client = reqwest::blocking::Client::builder()
        .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36")
        .connect_timeout(std::time::Duration::from_secs(8))
        .pool_idle_timeout(std::time::Duration::from_secs(20))
        .build()
        .map_err(|e| format!("No se pudo crear el cliente HTTP del proxy: {}", e))?;

    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            if ACTIVE_PROXY_CONNECTIONS.load(std::sync::atomic::Ordering::Relaxed)
                >= MAX_PROXY_CONNECTIONS
            {
                let _ = stream
                    .write_all(b"HTTP/1.1 503 Service Unavailable\r\nConnection: close\r\n\r\n");
                continue;
            }
            ACTIVE_PROXY_CONNECTIONS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);

            let capi_dir = capi_dir.clone();
            let client = client.clone();
            let app = app.clone();
            let media_state = media_state.clone();
            let token = token.clone();
            let roots = roots.clone();
            let capi_dir_check = capi_dir.clone();
            std::thread::spawn(move || {
                let _guard = ProxyConnGuard;
                let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(10)));
                let _ = stream.set_write_timeout(Some(std::time::Duration::from_secs(15)));

                let mut buffer = [0; 4096];
                if let Ok(n) = stream.read(&mut buffer) {
                    let request = String::from_utf8_lossy(&buffer[..n]);
                    let first_line = request.lines().next().unwrap_or("");
                    let parts: Vec<&str> = first_line.split_whitespace().collect();
                    if parts.len() >= 2
                        && (parts[0] == "GET" || parts[0] == "HEAD" || parts[0] == "OPTIONS")
                    {
                        let is_head = parts[0] == "HEAD";
                        let is_options = parts[0] == "OPTIONS";
                        let path_and_query = parts[1];

                        // Extract and validate Origin
                        let mut origin_header = None;
                        for line in request.lines() {
                            if let Some((k, v)) = line.split_once(':') {
                                if k.trim().eq_ignore_ascii_case("origin") {
                                    origin_header = Some(v.trim().to_string());
                                }
                            }
                        }

                        if let Some(ref o) = origin_header {
                            let is_allowed = o == "tauri://localhost"
                                || o == "http://tauri.localhost"
                                || o == "https://tauri.localhost"
                                || o.starts_with("http://localhost:")
                                || o.starts_with("http://127.0.0.1:");
                            if !is_allowed {
                                let resp = "HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
                                let _ = stream.write_all(resp.as_bytes());
                                return;
                            }
                        }

                        let allow_origin = match origin_header {
                            Some(ref o) => o.clone(),
                            None => "*".to_string(),
                        };
                        let cors = format!(
                            "Access-Control-Allow-Origin: {}\r\n\
Access-Control-Allow-Methods: GET, HEAD, OPTIONS\r\n\
Access-Control-Allow-Headers: Range, Content-Type, Authorization, X-Capi-Token\r\n\
Access-Control-Expose-Headers: Content-Range, Content-Length, Accept-Ranges\r\n\
Vary: Origin\r\n",
                            allow_origin
                        );

                        if is_options {
                            let resp = format!("HTTP/1.1 204 No Content\r\n{}Content-Length: 0\r\nConnection: keep-alive\r\n\r\n", cors);
                            let _ = stream.write_all(resp.as_bytes());
                            return;
                        }

                        if !has_valid_token_req(path_and_query, &request, &token) {
                            let resp = format!("HTTP/1.1 403 Forbidden\r\n{}Content-Length: 0\r\nConnection: close\r\n\r\n", cors);
                            let _ = stream.write_all(resp.as_bytes());
                            return;
                        }

                        // ─── TV Cast Screen: /tv ────────────────────
                        if path_and_query == "/tv"
                            || path_and_query.starts_with("/tv?")
                            || path_and_query == "/cast/receiver"
                        {
                            let html = r#"<!DOCTYPE html>
<html lang="es">
<head>
  <meta charset="UTF-8">
  <meta name="viewport" content="width=device-width, initial-scale=1.0">
  <title>Capi TV</title>
  <link rel="preconnect" href="https://fonts.googleapis.com">
  <link rel="preconnect" href="https://fonts.gstatic.com" crossorigin>
  <link href="https://fonts.googleapis.com/css2?family=Plus+Jakarta+Sans:wght@400;600;700;800;900&display=swap" rel="stylesheet">
  <style>
    * { margin: 0; padding: 0; box-sizing: border-box; }
    body, html {
      width: 100vw;
      height: 100vh;
      overflow: hidden;
      background: #000000;
      font-family: 'Plus Jakarta Sans', sans-serif;
      color: #fff;
      display: flex;
      align-items: center;
      justify-content: center;
    }
    /* Dynamic Full-Screen Blur Backdrop */
    .bg-blur {
      position: absolute;
      top: -15%;
      left: -15%;
      width: 130%;
      height: 130%;
      background-size: cover;
      background-position: center;
      filter: blur(90px) brightness(0.40) saturate(2);
      transform: scale(1.18);
      transition: background-image 1.2s cubic-bezier(0.2, 0.8, 0.2, 1);
      z-index: 1;
    }
    .bg-overlay {
      position: absolute;
      inset: 0;
      background: radial-gradient(circle at center, rgba(0,0,0,0.15) 0%, rgba(0,0,0,0.75) 100%);
      z-index: 2;
    }
    /* Top Left Capi Logo & Brand */
    .brand-container {
      position: absolute;
      top: 48px;
      left: 56px;
      display: flex;
      align-items: center;
      gap: 16px;
      z-index: 10;
      animation: fadeIn 1s ease;
    }
    .brand-logo {
      width: 44px;
      height: 44px;
      border-radius: 14px;
      background: linear-gradient(135deg, #d0bcff 0%, #381e72 100%);
      display: flex;
      align-items: center;
      justify-content: center;
      box-shadow: 0 8px 32px rgba(208, 188, 255, 0.3);
    }
    .brand-logo svg {
      width: 24px;
      height: 24px;
      fill: #ffffff;
    }
    .brand-text {
      font-size: 32px;
      font-weight: 900;
      letter-spacing: -0.04em;
      color: #ffffff;
      text-shadow: 0 2px 20px rgba(0,0,0,0.8);
    }
    /* Center: Only the Cover Art */
    .center-card {
      position: relative;
      z-index: 10;
      display: flex;
      align-items: center;
      justify-content: center;
      animation: zoomIn 1s cubic-bezier(0.16, 1, 0.3, 1);
    }
    .cover-container {
      width: 480px;
      height: 480px;
      max-width: 68vh;
      max-height: 68vh;
      border-radius: 36px;
      overflow: hidden;
      box-shadow: 0 30px 90px rgba(0, 0, 0, 0.9), 0 0 80px rgba(208, 188, 255, 0.2);
      border: 2px solid rgba(255, 255, 255, 0.18);
      background: #121214;
      display: flex;
      align-items: center;
      justify-content: center;
      transition: transform 0.6s ease;
    }
    .cover-img {
      width: 100%;
      height: 100%;
      object-fit: cover;
      display: block;
      transition: opacity 0.5s ease;
    }
    @keyframes fadeIn {
      from { opacity: 0; transform: translateY(-10px); }
      to { opacity: 1; transform: translateY(0); }
    }
    @keyframes zoomIn {
      from { opacity: 0; transform: scale(0.92); }
      to { opacity: 1; transform: scale(1); }
    }
    audio { display: none; }
  </style>
</head>
<body>
  <div class="bg-blur" id="bgBlur"></div>
  <div class="bg-overlay"></div>

  <!-- Top-Left Branding -->
  <div class="brand-container">
    <div class="brand-logo">
      <svg viewBox="0 0 24 24"><path d="M12 3v10.55c-.59-.34-1.27-.55-2-.55-2.21 0-4 1.79-4 4s1.79 4 4 4 4-1.79 4-4V7h4V3h-6z"/></svg>
    </div>
    <span class="brand-text">Capi</span>
  </div>

  <!-- Centered Album Cover Only -->
  <div class="center-card">
    <div class="cover-container">
      <img id="coverImg" class="cover-img" src="/assets/Logo.png" alt="Cover Art">
    </div>
  </div>

  <audio id="tvAudio" autoplay preload="auto"></audio>

  <script>
    const coverImg = document.getElementById('coverImg');
    const bgBlur = document.getElementById('bgBlur');
    const CAPI_TOKEN = "__CAPI_TOKEN__";
    const tvAudio = document.getElementById('tvAudio');
    let lastTrackId = '';

    async function sync() {
      try {
        const res = await fetch('/api/state?token=' + encodeURIComponent(CAPI_TOKEN));
        if (!res.ok) return;
        const s = await res.json();
        if (s) {
          if (s.thumbnail && coverImg.src !== s.thumbnail) {
            coverImg.src = s.thumbnail;
            bgBlur.style.backgroundImage = `url('${s.thumbnail}')`;
          }
          if (s.track_id && s.track_id !== lastTrackId && s.stream_url) {
            lastTrackId = s.track_id;
            tvAudio.src = s.stream_url;
            tvAudio.play().catch(() => {});
          }
          if (s.is_playing && tvAudio.paused && tvAudio.src) {
            tvAudio.play().catch(() => {});
          } else if (!s.is_playing && !tvAudio.paused) {
            tvAudio.pause();
          }
        }
      } catch (e) {}
    }

    setInterval(sync, 1000);
    sync();

    // Auto unlock audio playback on any remote/screen click
    document.addEventListener('click', () => {
      if (tvAudio.paused && tvAudio.src) tvAudio.play().catch(() => {});
    });
  </script>
</body>
</html>"#;
                            let html = html.replace("__CAPI_TOKEN__", &token);
                            let _ = stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\n{}Content-Length: {}\r\n\r\n", cors, html.len()).as_bytes());
                            if !is_head {
                                let _ = stream.write_all(html.as_bytes());
                            }
                            return;
                        }

                        // ─── Media Control API ─────────────────────
                        if path_and_query.starts_with("/api/") {
                            let api_path =
                                path_and_query.split('?').next().unwrap_or(path_and_query);
                            let api_query = path_and_query
                                .split_once('?')
                                .map(|(_, query)| query)
                                .unwrap_or("");
                            match api_path {
                                "/api/play-pause" => {
                                    let _ = app.emit(
                                        "mpris-command",
                                        serde_json::json!({"action": "play-pause"}),
                                    );
                                    let body = b"{\"ok\":true}";
                                    let _ = stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n{}Content-Length: {}\r\n\r\n", cors, body.len()).as_bytes());
                                    if !is_head {
                                        let _ = stream.write_all(body);
                                    }
                                }
                                "/api/next" => {
                                    let _ = app.emit(
                                        "mpris-command",
                                        serde_json::json!({"action": "next"}),
                                    );
                                    let body = b"{\"ok\":true}";
                                    let _ = stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n{}Content-Length: {}\r\n\r\n", cors, body.len()).as_bytes());
                                    if !is_head {
                                        let _ = stream.write_all(body);
                                    }
                                }
                                "/api/prev" => {
                                    let _ = app.emit(
                                        "mpris-command",
                                        serde_json::json!({"action": "prev"}),
                                    );
                                    let body = b"{\"ok\":true}";
                                    let _ = stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n{}Content-Length: {}\r\n\r\n", cors, body.len()).as_bytes());
                                    if !is_head {
                                        let _ = stream.write_all(body);
                                    }
                                }
                                "/api/volume" => {
                                    let resp = if let Some(level_str) =
                                        api_query.split("level=").nth(1)
                                    {
                                        if let Ok(level) =
                                            level_str.split('&').next().unwrap_or("").parse::<f64>()
                                        {
                                            let _ = app.emit("mpris-command", serde_json::json!({"action": "volume", "level": level}));
                                            "{\"ok\":true}"
                                        } else {
                                            "{\"ok\":false,\"error\":\"invalid volume\"}"
                                        }
                                    } else {
                                        "{\"ok\":false,\"error\":\"missing level\"}"
                                    };
                                    let _ = stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n{}Content-Length: {}\r\n\r\n{}", cors, resp.len(), resp).as_bytes());
                                }
                                "/api/seek" => {
                                    let resp = if let Some(pos_str) =
                                        api_query.split("position=").nth(1)
                                    {
                                        if let Ok(pos) =
                                            pos_str.split('&').next().unwrap_or("").parse::<f64>()
                                        {
                                            let _ = app.emit("mpris-command", serde_json::json!({"action": "seek", "position": pos}));
                                            "{\"ok\":true}"
                                        } else {
                                            "{\"ok\":false,\"error\":\"invalid position\"}"
                                        }
                                    } else {
                                        "{\"ok\":false,\"error\":\"missing position\"}"
                                    };
                                    let _ = stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n{}Content-Length: {}\r\n\r\n{}", cors, resp.len(), resp).as_bytes());
                                }
                                p if p == "/api/status" || p == "/api/state" => {
                                    let guard = match media_state.lock() {
                                        Ok(g) => g,
                                        Err(_) => {
                                            let err = b"{\"ok\":false,\"error\":\"lock failed\"}";
                                            let _ = stream.write_all(format!("HTTP/1.1 500 Error\r\nContent-Type: application/json\r\n{}Content-Length: {}\r\n\r\n", cors, err.len()).as_bytes());
                                            let _ = stream.write_all(err);
                                            return;
                                        }
                                    };
                                    let json = serde_json::to_string(&*guard).unwrap_or_default();
                                    let _ = stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n{}Content-Length: {}\r\n\r\n{}", cors, json.len(), json).as_bytes());
                                }
                                _ => {
                                    let err = b"{\"ok\":false,\"error\":\"unknown\"}";
                                    let _ = stream.write_all(format!("HTTP/1.1 404 Not Found\r\nContent-Type: application/json\r\n{}Content-Length: {}\r\n\r\n", cors, err.len()).as_bytes());
                                    let _ = stream.write_all(err);
                                }
                            }
                            return;
                        }

                        // ─── Image proxy: /image?url=... ───────────
                        if path_and_query.starts_with("/image") {
                            if let Some((_, rest)) = path_and_query.split_once("url=") {
                                let encoded_url = rest.split('&').next().unwrap_or("");
                                let img_url = match urlencoding::decode(encoded_url) {
                                    Ok(d) => d.into_owned(),
                                    Err(_) => encoded_url.to_string(),
                                };
                                if !is_allowed_image_url(&img_url) {
                                    let resp = "HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
                                    let _ = stream.write_all(resp.as_bytes());
                                    return;
                                }
                                if let Ok(mut resp) = client.get(&img_url).send() {
                                    let status = resp.status();
                                    let content_type = resp
                                        .headers()
                                        .get("content-type")
                                        .and_then(|h| h.to_str().ok())
                                        .unwrap_or("image/jpeg");
                                    let content_length = resp
                                        .headers()
                                        .get("content-length")
                                        .and_then(|h| h.to_str().ok())
                                        .unwrap_or("0");
                                    let cache_control = resp
                                        .headers()
                                        .get("cache-control")
                                        .and_then(|h| h.to_str().ok())
                                        .unwrap_or("public, max-age=86400");
                                    let status_line = format!(
                                        "HTTP/1.1 {} {}\r\n",
                                        status.as_u16(),
                                        status.canonical_reason().unwrap_or("OK")
                                    );
                                    let response_headers = format!(
                                            "{}Content-Type: {}\r\nContent-Length: {}\r\nCache-Control: {}\r\n{}",
                                            status_line, content_type, content_length, cache_control, cors
                                        );
                                    let _ = stream.write_all(response_headers.as_bytes());
                                    if !is_head {
                                        let mut buf = [0; 65536];
                                        while let Ok(n) = resp.read(&mut buf) {
                                            if n == 0 {
                                                break;
                                            }
                                            if stream.write_all(&buf[..n]).is_err() {
                                                break;
                                            }
                                        }
                                    }
                                } else {
                                    let resp = "HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
                                    let _ = stream.write_all(resp.as_bytes());
                                }
                            } else {
                                let resp = "HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
                                let _ = stream.write_all(resp.as_bytes());
                            }
                            return;
                        }

                        // ─── Audio stream proxy: /play?url=... ─────
                        if let Some((_, rest)) = path_and_query.split_once("url=") {
                            let encoded_url = rest
                                .split("&token=")
                                .next()
                                .unwrap_or(rest)
                                .split('&')
                                .next()
                                .unwrap_or("");
                            let decoded_url = match urlencoding::decode(encoded_url) {
                                Ok(d) => d.into_owned(),
                                Err(_) => encoded_url.to_string(),
                            };
                            if !is_allowed_play_url(&decoded_url) {
                                let resp = "HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
                                let _ = stream.write_all(resp.as_bytes());
                                return;
                            }

                            let mut range_header = None;
                            for line in request.lines() {
                                if line.to_lowercase().starts_with("range:") {
                                    range_header = Some(line.to_string());
                                }
                            }
                            println!(
                                "[PROXY] Incoming request for url: {}",
                                &decoded_url[..std::cmp::min(100, decoded_url.len())]
                            );

                            let wants_full = range_header.is_none();
                            let (range_start, range_end, upstream_range) =
                                parse_client_range(range_header.as_deref());

                            // (b) Reintentar cuando upstream devuelve 206 con body vacío o falla la conexión inicial
                            let mut initial_response = None;
                            for attempt in 0..3 {
                                let req = client.get(&decoded_url).header("Range", &upstream_range);
                                if let Ok(resp) = req.send() {
                                    let status = resp.status();
                                    if status.is_success()
                                        || status == reqwest::StatusCode::PARTIAL_CONTENT
                                    {
                                        initial_response = Some(resp);
                                        break;
                                    }
                                }
                                std::thread::sleep(std::time::Duration::from_millis(
                                    150 * (attempt + 1),
                                ));
                            }

                            let mut response = match initial_response {
                                Some(r) => r,
                                None => {
                                    let resp = "HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
                                    let _ = stream.write_all(resp.as_bytes());
                                    return;
                                }
                            };

                            let status = response.status();
                            let headers = response.headers();

                            let content_type = headers
                                .get("content-type")
                                .and_then(|h| h.to_str().ok())
                                .unwrap_or("audio/mp4");
                            let upstream_length =
                                headers.get("content-length").and_then(|h| h.to_str().ok());
                            let content_range = headers
                                .get("content-range")
                                .and_then(|h| h.to_str().ok())
                                .map(str::to_string);

                            let (status_line, content_length, content_range) =
                                if wants_full && status == reqwest::StatusCode::PARTIAL_CONTENT {
                                    // `bytes=0-` devuelve el archivo completo como
                                    // 206; se presenta como 200 para que WebKit
                                    // no limite la duración al primer bloque.
                                    let total = content_range
                                        .as_deref()
                                        .and_then(|value| value.rsplit('/').next())
                                        .filter(|value| *value != "*")
                                        .map(str::to_string)
                                        .or_else(|| upstream_length.map(str::to_string));
                                    ("HTTP/1.1 200 OK\r\n".to_string(), total, None)
                                } else {
                                    (
                                        format!(
                                            "HTTP/1.1 {} {}\r\n",
                                            status.as_u16(),
                                            status.canonical_reason().unwrap_or("OK")
                                        ),
                                        upstream_length.map(str::to_string),
                                        content_range,
                                    )
                                };

                            let mut response_headers = format!(
                                "{}Content-Type: {}\r\nAccept-Ranges: bytes\r\nConnection: keep-alive\r\n{}",
                                status_line, content_type, cors
                            );
                            if let Some(ref length) = content_length {
                                response_headers
                                    .push_str(&format!("Content-Length: {}\r\n", length));
                            }
                            if let Some(ref range_val) = content_range {
                                response_headers
                                    .push_str(&format!("Content-Range: {}\r\n", range_val));
                            }
                            response_headers.push_str("\r\n");

                            let _ = stream.set_nodelay(true);
                            if stream.write_all(response_headers.as_bytes()).is_ok() {
                                let _ = stream.flush();
                                if !is_head {
                                    let expected_bytes: Option<u64> = content_length
                                        .as_deref()
                                        .and_then(|l| l.parse::<u64>().ok());
                                    let mut buf = [0; 65536];
                                    let mut total_bytes: u64 = 0;
                                    let mut consecutive_empty_resumes = 0;
                                    let write_start = std::time::Instant::now();

                                    loop {
                                        let mut bytes_read = 0;
                                        match response.read(&mut buf) {
                                            Ok(n) => bytes_read = n,
                                            Err(e) => {
                                                println!("[PROXY] Read error from upstream: {}", e);
                                            }
                                        }

                                        if bytes_read > 0 {
                                            consecutive_empty_resumes = 0;
                                            if stream.write_all(&buf[..bytes_read]).is_err() {
                                                break;
                                            }
                                            let _ = stream.flush();
                                            total_bytes += bytes_read as u64;
                                        }

                                        if let Some(expected) = expected_bytes {
                                            if total_bytes >= expected {
                                                break;
                                            }
                                        }

                                        // Si el chunk upstream termina antes de alcanzar total_bytes == expected_bytes:
                                        // (a) Validar total_bytes == Content-Length y continuar chunk con Range: bytes={siguiente}-
                                        if bytes_read == 0 {
                                            if let Some(expected) = expected_bytes {
                                                if total_bytes < expected {
                                                    let next_start =
                                                        range_start.saturating_add(total_bytes);
                                                    let resume_range = if let Some(re) = range_end {
                                                        format!("bytes={}-{}", next_start, re)
                                                    } else {
                                                        format!("bytes={}-", next_start)
                                                    };
                                                    println!("[PROXY] Stream chunk ended at {}/{} bytes. Resuming upstream with Range: {}", total_bytes, expected, resume_range);

                                                    // (b) Reintentar si upstream devuelve 206 con body vacío
                                                    let mut resumed_resp = None;
                                                    for attempt in 0..4 {
                                                        let req = client
                                                            .get(&decoded_url)
                                                            .header("Range", &resume_range);
                                                        if let Ok(r) = req.send() {
                                                            let s = r.status();
                                                            if s.is_success()
                                                                || s == reqwest::StatusCode::PARTIAL_CONTENT
                                                            {
                                                                resumed_resp = Some(r);
                                                                break;
                                                            }
                                                        }
                                                        std::thread::sleep(
                                                            std::time::Duration::from_millis(
                                                                200 * (attempt + 1),
                                                            ),
                                                        );
                                                    }

                                                    if let Some(r) = resumed_resp {
                                                        consecutive_empty_resumes += 1;
                                                        if consecutive_empty_resumes >= 4 {
                                                            println!("[PROXY] Upstream repeatedly returned empty body on resume, stopping.");
                                                            break;
                                                        }
                                                        response = r;
                                                        continue;
                                                    } else {
                                                        println!("[PROXY] Upstream resume failed after retries.");
                                                        break;
                                                    }
                                                } else {
                                                    break;
                                                }
                                            } else {
                                                break;
                                            }
                                        }
                                    }
                                    println!("[PROXY] Finished sending stream. Total bytes: {} | Time: {}ms", total_bytes, write_start.elapsed().as_millis());
                                }
                            }
                            return;
                        } else if let Some((_, rest)) = path_and_query.split_once("path=") {
                            let encoded_path = rest.split('&').next().unwrap_or(rest);
                            let decoded_str = match urlencoding::decode(encoded_path) {
                                Ok(d) => d.into_owned(),
                                Err(_) => encoded_path.to_string(),
                            };

                            let param_title = path_and_query
                                .split("title=")
                                .nth(1)
                                .and_then(|s| s.split('&').next())
                                .and_then(|s| urlencoding::decode(s).ok())
                                .map(|s| s.into_owned());
                            let param_artist = path_and_query
                                .split("artist=")
                                .nth(1)
                                .and_then(|s| s.split('&').next())
                                .and_then(|s| urlencoding::decode(s).ok())
                                .map(|s| s.into_owned());

                            let initial_path = std::path::PathBuf::from(&decoded_str);
                            let mut resolved_path = initial_path.clone();
                            let mut exists = resolved_path.is_file();

                            // Si la ruta exacta no existe en disco, buscar recursivamente en Capi y en raíces locales (hasta 15 niveles de profundidad)
                            if !exists {
                                let target_filename = initial_path.file_name();
                                let target_stem = initial_path.file_stem();

                                if let Some(found) = find_audio_file_recursive(
                                    &capi_dir_check,
                                    target_filename,
                                    target_stem,
                                    param_title.as_deref(),
                                    param_artist.as_deref(),
                                    0,
                                    15,
                                ) {
                                    resolved_path = found;
                                    exists = true;
                                } else {
                                    for root_dir in roots.get_all() {
                                        if let Some(found) = find_audio_file_recursive(
                                            &root_dir,
                                            target_filename,
                                            target_stem,
                                            param_title.as_deref(),
                                            param_artist.as_deref(),
                                            0,
                                            15,
                                        ) {
                                            resolved_path = found;
                                            exists = true;
                                            break;
                                        }
                                    }
                                }
                            }

                            let file_path = resolved_path.as_path();
                            let is_audio_ext = file_path
                                .extension()
                                .and_then(|ext| ext.to_str())
                                .map(|ext| {
                                    let ext_lower = ext.to_lowercase();
                                    ext_lower == "mp3"
                                        || ext_lower == "wav"
                                        || ext_lower == "ogg"
                                        || ext_lower == "m4a"
                                        || ext_lower == "flac"
                                        || ext_lower == "aac"
                                        || ext_lower == "webm"
                                        || ext_lower == "opus"
                                })
                                .unwrap_or(false);
                            let canonical = std::fs::canonicalize(file_path).ok();
                            let under_root = canonical
                                .as_ref()
                                .map(|path| {
                                    path.starts_with(&capi_dir_check) || roots.contains(path)
                                })
                                .unwrap_or(false);
                            let is_allowed =
                                exists && file_path.is_file() && is_audio_ext && under_root;
                            if is_allowed {
                                if let Ok(mut file) = File::open(file_path) {
                                    let file_size = match file.metadata() {
                                        Ok(m) => m.len(),
                                        Err(_) => {
                                            let _ = write!(stream, "HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\n\r\n");
                                            return;
                                        }
                                    };

                                    if file_size == 0 {
                                        let resp = "HTTP/1.1 416 Range Not Satisfiable\r\nContent-Range: bytes */0\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
                                        let _ = stream.write_all(resp.as_bytes());
                                        return;
                                    }

                                    let mut range_start = 0u64;
                                    let mut range_end = file_size.saturating_sub(1);
                                    let mut is_partial = false;

                                    for line in request.lines() {
                                        let line_lower = line.to_lowercase();
                                        if line_lower.starts_with("range:") {
                                            if let Some((_, range_part)) = line.split_once("bytes=")
                                            {
                                                let range_str = range_part.trim();
                                                let range_parts: Vec<&str> =
                                                    range_str.split('-').collect();
                                                if let Ok(start) = range_parts[0].parse::<u64>() {
                                                    if start >= file_size {
                                                        let resp = format!(
                                                                "HTTP/1.1 416 Range Not Satisfiable\r\nContent-Range: bytes */{}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                                                                file_size
                                                            );
                                                        let _ = stream.write_all(resp.as_bytes());
                                                        return;
                                                    }
                                                    range_start = start;
                                                    is_partial = true;
                                                }
                                                if range_parts.len() > 1
                                                    && !range_parts[1].is_empty()
                                                {
                                                    if let Ok(end) = range_parts[1].parse::<u64>() {
                                                        range_end =
                                                            end.min(file_size.saturating_sub(1));
                                                        if range_end < range_start {
                                                            let resp = "HTTP/1.1 416 Range Not Satisfiable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
                                                            let _ =
                                                                stream.write_all(resp.as_bytes());
                                                            return;
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }

                                    let content_length = range_end - range_start + 1;
                                    let status_line = if is_partial {
                                        "HTTP/1.1 206 Partial Content\r\n"
                                    } else {
                                        "HTTP/1.1 200 OK\r\n"
                                    };

                                    let ext = file_path
                                        .extension()
                                        .and_then(|e| e.to_str())
                                        .unwrap_or("")
                                        .to_ascii_lowercase();
                                    let mime_type = match ext.as_str() {
                                        "m4a" => "audio/mp4",
                                        "mp3" => "audio/mpeg",
                                        "wav" => "audio/wav",
                                        "ogg" => "audio/ogg",
                                        "flac" => "audio/flac",
                                        "aac" => "audio/aac",
                                        "webm" => "audio/webm",
                                        "opus" => "audio/opus",
                                        _ => "application/octet-stream",
                                    };

                                    let mut response_headers = format!(
                                            "{}Content-Type: {}\r\nContent-Length: {}\r\nAccept-Ranges: bytes\r\nConnection: keep-alive\r\n{}",
                                            status_line, mime_type, content_length, cors
                                        );

                                    if is_partial {
                                        response_headers.push_str(&format!(
                                            "Content-Range: bytes {}-{}/{}\r\n",
                                            range_start, range_end, file_size
                                        ));
                                    }

                                    response_headers.push_str("\r\n");

                                    if stream.write_all(response_headers.as_bytes()).is_ok() {
                                        use std::io::Seek;
                                        if file.seek(std::io::SeekFrom::Start(range_start)).is_ok()
                                        {
                                            let mut chunk = [0; 65536];
                                            let mut remaining = content_length;
                                            while remaining > 0 {
                                                let to_read =
                                                    std::cmp::min(remaining, chunk.len() as u64)
                                                        as usize;
                                                match file.read(&mut chunk[..to_read]) {
                                                    Ok(0) => break,
                                                    Ok(bytes_read) => {
                                                        if stream
                                                            .write_all(&chunk[..bytes_read])
                                                            .is_err()
                                                        {
                                                            break;
                                                        }
                                                        remaining -= bytes_read as u64;
                                                    }
                                                    Err(_) => break,
                                                }
                                            }
                                        }
                                    }
                                    return;
                                }
                            }
                        }
                    }
                    let response =
                        "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
                    let _ = stream.write_all(response.as_bytes());
                }
            });
        }
    });

    Ok(port)
}

#[derive(Clone, serde::Serialize)]
struct DownloadProgress {
    track_id: String,
    progress: f64,
}

#[tauri::command]
async fn buscar_cancion(
    engine: tauri::State<'_, Arc<Engine>>,
    query: String,
) -> Result<String, String> {
    let songs = engine.search(&query).await.map_err(|e| e.to_string())?;
    serde_json::to_string(&songs).map_err(|e| e.to_string())
}

#[tauri::command]
async fn buscar_cancion_progresivo(
    engine: tauri::State<'_, Arc<Engine>>,
    query: String,
    continuation: Option<String>,
) -> Result<String, String> {
    let page = if let Some(ctoken) = continuation.filter(|s| !s.trim().is_empty()) {
        engine
            .search_continuation(&ctoken)
            .await
            .map_err(|e| e.to_string())?
    } else {
        engine
            .search_page(&query)
            .await
            .map_err(|e| e.to_string())?
    };
    serde_json::to_string(&page).map_err(|e| e.to_string())
}

#[tauri::command]
async fn obtener_stream(
    engine: tauri::State<'_, Arc<Engine>>,
    id: String,
) -> Result<String, String> {
    let stream = engine.stream(&id).await.map_err(|e| e.to_string())?;
    serde_json::to_string(&stream).map_err(|e| e.to_string())
}

#[tauri::command]
async fn obtener_playlist(
    engine: tauri::State<'_, Arc<Engine>>,
    id: String,
) -> Result<String, String> {
    let songs = engine
        .playlist_tracks(&id)
        .await
        .map_err(|e| e.to_string())?;
    serde_json::to_string(&songs).map_err(|e| e.to_string())
}

fn sanitize_for_path(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c == '/'
                || c == '\\'
                || c == ':'
                || c == '*'
                || c == '?'
                || c == '"'
                || c == '<'
                || c == '>'
                || c == '|'
                || c.is_control()
            {
                '_'
            } else {
                c
            }
        })
        .collect();

    let trimmed = cleaned.trim().trim_matches('.').trim();
    if trimmed.is_empty() {
        "Desconocido".to_string()
    } else {
        trimmed.to_string()
    }
}

fn extract_primary_artist(artist: &str, title: &str) -> String {
    let raw_artist = artist.trim();

    let is_unknown = raw_artist.is_empty()
        || raw_artist.eq_ignore_ascii_case("desconocido")
        || raw_artist.eq_ignore_ascii_case("artista desconocido")
        || raw_artist.eq_ignore_ascii_case("unknown")
        || raw_artist.eq_ignore_ascii_case("unknown artist")
        || raw_artist.eq_ignore_ascii_case("youtube")
        || raw_artist.eq_ignore_ascii_case("capi");

    let candidate = if is_unknown {
        if let Some((possible_artist, _)) = title.split_once(" - ") {
            let p = possible_artist.trim();
            if !p.is_empty() {
                p
            } else {
                "Desconocido"
            }
        } else {
            "Desconocido"
        }
    } else {
        raw_artist
    };

    let mut primary = candidate;

    // Split on comma first (standard for multiple artists tag e.g. "Rawayana, Danny Ocean")
    if let Some((first, _)) = primary.split_once(',') {
        let f = first.trim();
        if !f.is_empty() {
            primary = f;
        }
    }

    // Split on semicolon
    if let Some((first, _)) = primary.split_once(';') {
        let f = first.trim();
        if !f.is_empty() {
            primary = f;
        }
    }

    // Split on " / "
    if let Some((first, _)) = primary.split_once(" / ") {
        let f = first.trim();
        if !f.is_empty() {
            primary = f;
        }
    }

    // Split on " & "
    if let Some((first, _)) = primary.split_once(" & ") {
        let f = first.trim();
        if !f.is_empty() {
            primary = f;
        }
    }

    // Case-insensitive collaboration indicators
    let lower = primary.to_lowercase();
    let separators = [
        " feat. ",
        " feat ",
        " ft. ",
        " ft ",
        " featuring ",
        " with ",
        " x ",
        " vs. ",
        " vs ",
    ];
    for sep in separators {
        if let Some(pos) = lower.find(sep) {
            let prefix = primary[..pos].trim();
            if !prefix.is_empty() {
                primary = prefix;
                break;
            }
        }
    }

    sanitize_for_path(primary)
}

#[tauri::command]
async fn descargar_cancion(
    app: tauri::AppHandle,
    track_id: String,
    title: String,
    artist: String,
    primary_artist: Option<String>,
    url: String,
) -> Result<String, String> {
    let music_dir = app.path().app_data_dir().map_err(|e| {
        format!(
            "No se pudo resolver el directorio de datos de la app: {}",
            e
        )
    })?;

    let capi_dir = music_dir.join("Capi");
    if !capi_dir.exists() {
        std::fs::create_dir_all(&capi_dir)
            .map_err(|e| format!("No se pudo crear el directorio de Capi: {}", e))?;
    }

    let clean_artist = if let Some(ref pa) = primary_artist {
        let trimmed = pa.trim();
        if !trimmed.is_empty() && !trimmed.eq_ignore_ascii_case("desconocido") {
            sanitize_for_path(trimmed)
        } else {
            extract_primary_artist(&artist, &title)
        }
    } else {
        extract_primary_artist(&artist, &title)
    };

    let artist_dir = capi_dir.join(&clean_artist);
    if !artist_dir.exists() {
        std::fs::create_dir_all(&artist_dir)
            .map_err(|e| format!("No se pudo crear la carpeta del artista: {}", e))?;
    }

    let ext = if url.contains("mime=audio%2Fmp4")
        || url.contains("mime=audio/mp4")
        || url.contains("mime=audio%2Fm4a")
    {
        "m4a"
    } else {
        "webm"
    };

    let clean_title = sanitize_for_path(&title);
    let filename = format!("{} - {}.{}", clean_artist, clean_title, ext);
    let dest_path = artist_dir.join(&filename);
    let part_path = artist_dir.join(format!("{}.part", filename));

    // Si ya existe y es válido, retornar inmediatamente
    if dest_path.exists() {
        if let Ok(meta) = std::fs::metadata(&dest_path) {
            if meta.len() > 1024 {
                let _ = app.emit(
                    "download-progress",
                    DownloadProgress {
                        track_id: track_id.clone(),
                        progress: 100.0,
                    },
                );
                if let Some(p) = dest_path.to_str() {
                    return Ok(p.to_string());
                }
            }
        }
    }

    let client = reqwest::Client::builder()
        .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36")
        .connect_timeout(std::time::Duration::from_secs(10))
        .pool_idle_timeout(std::time::Duration::from_secs(30))
        .pool_max_idle_per_host(10)
        .tcp_nodelay(true)
        .build()
        .map_err(|e| format!("Error creando cliente HTTP: {}", e))?;

    // Sondear tamaño total y soporte de Range con una petición ultraligera de 1 byte
    let probe_res = client
        .get(&url)
        .header("Range", "bytes=0-0")
        .header("Referer", "https://music.youtube.com/")
        .send()
        .await
        .map_err(|e| format!("Error conectando con el stream de audio: {}", e))?;

    let status = probe_res.status();
    let (supports_range, total_size, immediate_stream) =
        if status == reqwest::StatusCode::PARTIAL_CONTENT {
            let total = if let Some(cr) = probe_res
                .headers()
                .get(reqwest::header::CONTENT_RANGE)
                .and_then(|v| v.to_str().ok())
            {
                cr.rsplit('/')
                    .next()
                    .and_then(|s| s.trim().parse::<u64>().ok())
                    .unwrap_or(0)
            } else {
                0
            };
            (total > 0, total, None)
        } else if status.is_success() {
            // El servidor ignora Range y envía el stream completo directamente
            let len = probe_res.content_length().unwrap_or(0);
            (false, len, Some(probe_res))
        } else {
            return Err(format!(
                "El servidor de audio devolvió error HTTP {}",
                status
            ));
        };

    let downloaded_atomic = Arc::new(AtomicU64::new(0));
    let last_emit_instant = Arc::new(Mutex::new(std::time::Instant::now()));

    if supports_range && total_size >= 512 * 1024 {
        // ====================================================================
        // MOTOR DE DESCARGA MULTI-SEGMENTO DE ALTA VELOCIDAD
        // ====================================================================
        // Divide el archivo en múltiples conexiones concurrentes (4 o 6),
        // eliminando por completo el throttling de tasa de bits de YouTube Video CDN.
        let num_segments: u64 = if total_size > 12 * 1024 * 1024 {
            6
        } else if total_size > 2 * 1024 * 1024 {
            4
        } else {
            3
        };

        let file = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(true)
            .open(&part_path)
            .map_err(|e| format!("No se pudo crear archivo temporal: {}", e))?;
        file.set_len(total_size)
            .map_err(|e| format!("No se pudo preasignar espacio para descarga: {}", e))?;

        let shared_file = Arc::new(Mutex::new(file));
        let segment_size = total_size.div_ceil(num_segments);
        let mut handles = Vec::with_capacity(num_segments as usize);

        for seg_idx in 0..num_segments {
            let seg_start = seg_idx * segment_size;
            let seg_end = ((seg_idx + 1) * segment_size - 1).min(total_size - 1);
            let client = client.clone();
            let url = url.clone();
            let shared_file = shared_file.clone();
            let downloaded_atomic = downloaded_atomic.clone();
            let last_emit_instant = last_emit_instant.clone();
            let app = app.clone();
            let track_id = track_id.clone();

            handles.push(tokio::spawn(async move {
                let mut downloaded_in_seg: u64 = 0;
                let seg_len = seg_end - seg_start + 1;
                let mut attempts = 0;

                while downloaded_in_seg < seg_len {
                    if attempts >= 4 {
                        return Err("Segmento fallido tras múltiples reintentos".to_string());
                    }

                    let req_start = seg_start + downloaded_in_seg;
                    let req_end = seg_end;

                    let res = match client
                        .get(&url)
                        .header("Range", format!("bytes={}-{}", req_start, req_end))
                        .header("Referer", "https://music.youtube.com/")
                        .send()
                        .await
                    {
                        Ok(r) if r.status().is_success() => r,
                        _ => {
                            attempts += 1;
                            tokio::time::sleep(std::time::Duration::from_millis(400 * attempts))
                                .await;
                            continue;
                        }
                    };

                    let mut stream = res.bytes_stream();
                    let mut stream_err = false;

                    while let Some(chunk_res) = stream.next().await {
                        match chunk_res {
                            Ok(chunk) => {
                                let chunk_len = chunk.len() as u64;
                                let write_offset = req_start + downloaded_in_seg;

                                {
                                    let mut f = match shared_file.lock() {
                                        Ok(guard) => guard,
                                        Err(_) => {
                                            return Err("Fallo bloqueando archivo para escritura"
                                                .to_string())
                                        }
                                    };
                                    if f.seek(std::io::SeekFrom::Start(write_offset)).is_err()
                                        || f.write_all(chunk.as_ref()).is_err()
                                    {
                                        return Err(
                                            "Error escribiendo segmento en disco".to_string()
                                        );
                                    }
                                }

                                downloaded_in_seg += chunk_len;
                                let total_dl = downloaded_atomic
                                    .fetch_add(chunk_len, Ordering::Relaxed)
                                    + chunk_len;

                                let should_emit = {
                                    if let Ok(mut last) = last_emit_instant.lock() {
                                        if last.elapsed() >= std::time::Duration::from_millis(80) {
                                            *last = std::time::Instant::now();
                                            true
                                        } else {
                                            false
                                        }
                                    } else {
                                        false
                                    }
                                };

                                if should_emit {
                                    let pct =
                                        ((total_dl as f64 / total_size as f64) * 100.0).min(99.0);
                                    let _ = app.emit(
                                        "download-progress",
                                        DownloadProgress {
                                            track_id: track_id.clone(),
                                            progress: pct,
                                        },
                                    );
                                }
                            }
                            Err(_) => {
                                stream_err = true;
                                break;
                            }
                        }
                    }

                    if stream_err {
                        attempts += 1;
                        tokio::time::sleep(std::time::Duration::from_millis(400 * attempts)).await;
                    }
                }
                Ok(())
            }));
        }

        for handle in handles {
            match handle.await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    let _ = std::fs::remove_file(&part_path);
                    return Err(e);
                }
                Err(e) => {
                    let _ = std::fs::remove_file(&part_path);
                    return Err(format!("Descarga de segmento interrumpida: {}", e));
                }
            }
        }

        if let Ok(mut f) = shared_file.lock() {
            let _ = f.flush();
        };
    } else {
        // ====================================================================
        // DESCARGA STREAMING OPTIMIZADA (FALLBACK)
        // ====================================================================
        let mut file = std::io::BufWriter::with_capacity(
            256 * 1024,
            std::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&part_path)
                .map_err(|e| format!("No se pudo crear archivo temporal: {}", e))?,
        );

        let res = match immediate_stream {
            Some(r) => r,
            None => client
                .get(&url)
                .header("Referer", "https://music.youtube.com/")
                .send()
                .await
                .map_err(|e| format!("Error en petición de audio: {}", e))?,
        };

        let stream_total = if total_size > 0 {
            total_size
        } else {
            res.content_length().unwrap_or(0)
        };

        let mut stream = res.bytes_stream();
        let mut total_dl: u64 = 0;

        while let Some(chunk_res) = stream.next().await {
            match chunk_res {
                Ok(chunk) => {
                    if let Err(e) = file.write_all(chunk.as_ref()) {
                        let _ = std::fs::remove_file(&part_path);
                        return Err(format!("Error escribiendo datos: {}", e));
                    }
                    total_dl += chunk.len() as u64;

                    let should_emit = {
                        if let Ok(mut last) = last_emit_instant.lock() {
                            if last.elapsed() >= std::time::Duration::from_millis(80) {
                                *last = std::time::Instant::now();
                                true
                            } else {
                                false
                            }
                        } else {
                            false
                        }
                    };

                    if should_emit && stream_total > 0 {
                        let pct = ((total_dl as f64 / stream_total as f64) * 100.0).min(99.0);
                        let _ = app.emit(
                            "download-progress",
                            DownloadProgress {
                                track_id: track_id.clone(),
                                progress: pct,
                            },
                        );
                    }
                }
                Err(e) => {
                    let _ = std::fs::remove_file(&part_path);
                    return Err(format!("Interrupción en stream de descarga: {}", e));
                }
            }
        }

        file.flush()
            .map_err(|e| format!("Error al sincronizar archivo en disco: {}", e))?;
    }

    // Validación post-descarga
    let metadata = std::fs::metadata(&part_path)
        .map_err(|e| format!("Fallo al validar metadatos del archivo: {}", e))?;
    if metadata.len() < 1024 || (total_size > 0 && metadata.len() != total_size) {
        let _ = std::fs::remove_file(&part_path);
        return Err("Descarga inválida: El archivo final está corrupto o incompleto".to_string());
    }

    // Renombrado atómico a archivo final
    std::fs::rename(&part_path, &dest_path)
        .map_err(|e| format!("Error al mover archivo a ruta final: {}", e))?;

    // Progreso final 100%
    let _ = app.emit(
        "download-progress",
        DownloadProgress {
            track_id: track_id.clone(),
            progress: 100.0,
        },
    );

    let absolute_path = dest_path
        .to_str()
        .ok_or_else(|| "Error convirtiendo la ruta final a texto".to_string())?
        .to_string();

    Ok(absolute_path)
}

#[tauri::command]
async fn borrar_cancion(app: tauri::AppHandle, path: String) -> Result<(), String> {
    let app_data = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let capi_dir = app_data.join("Capi");
    let canonical_capi_dir = capi_dir.canonicalize().unwrap_or(capi_dir);

    let target = std::path::Path::new(&path);
    if !target.exists() {
        return Ok(());
    }
    let canonical_target = target
        .canonicalize()
        .map_err(|e| format!("Ruta inválida: {}", e))?;

    if !canonical_target.starts_with(&canonical_capi_dir) {
        return Err(
            "No está permitido borrar archivos fuera del directorio de descargas de Capi"
                .to_string(),
        );
    }

    if canonical_target.is_file() {
        tokio::fs::remove_file(&canonical_target)
            .await
            .map_err(|e| format!("No se pudo borrar el archivo: {}", e))?;

        // Si la carpeta del artista quedó vacía, limpiarla
        if let Some(parent) = canonical_target.parent() {
            if parent != canonical_capi_dir && parent.starts_with(&canonical_capi_dir) {
                if let Ok(mut entries) = tokio::fs::read_dir(parent).await {
                    if entries.next_entry().await.ok().flatten().is_none() {
                        let _ = tokio::fs::remove_dir(parent).await;
                    }
                }
            }
        }
    }
    Ok(())
}

#[tauri::command]
async fn obtener_home(
    engine: tauri::State<'_, Arc<Engine>>,
    continuation: Option<String>,
) -> Result<String, String> {
    let _ = continuation;
    let home = engine.home().await.map_err(|e| e.to_string())?;
    serde_json::to_string(&home).map_err(|e| e.to_string())
}

#[tauri::command]
async fn obtener_artista(
    engine: tauri::State<'_, Arc<Engine>>,
    id: String,
) -> Result<String, String> {
    let artist = engine.artist(&id).await.map_err(|e| e.to_string())?;
    serde_json::to_string(&artist).map_err(|e| e.to_string())
}

#[tauri::command]
async fn obtener_explorar(engine: tauri::State<'_, Arc<Engine>>) -> Result<String, String> {
    let explore = engine.explore().await.map_err(|e| e.to_string())?;
    serde_json::to_string(&explore).map_err(|e| e.to_string())
}

#[tauri::command]
async fn obtener_letras(
    engine: tauri::State<'_, Arc<Engine>>,
    id: String,
    artist: String,
    title: String,
    album: Option<String>,
    duration: i32,
    provider: Option<String>,
) -> Result<String, String> {
    let _ = album;
    let duration = u32::try_from(duration).unwrap_or(0);
    let lyrics = engine
        .lyrics(&id, &artist, &title, duration, provider.as_deref())
        .await
        .map_err(|e| e.to_string())?;
    serde_json::to_string(&lyrics).map_err(|e| e.to_string())
}

#[tauri::command]
async fn obtener_nuevos_lanzamientos(
    engine: tauri::State<'_, Arc<Engine>>,
) -> Result<String, String> {
    let releases = engine.new_releases().await.map_err(|e| e.to_string())?;
    serde_json::to_string(&releases).map_err(|e| e.to_string())
}

#[tauri::command]
async fn obtener_sugerencias(
    engine: tauri::State<'_, Arc<Engine>>,
    query: String,
) -> Result<String, String> {
    let suggestions = engine
        .suggestions(&query)
        .await
        .map_err(|e| e.to_string())?;
    serde_json::to_string(&suggestions).map_err(|e| e.to_string())
}

#[tauri::command]
fn obtener_local_port(state: tauri::State<'_, LocalServerState>) -> LocalServerInfo {
    LocalServerInfo {
        port: state.port,
        token: state.token.clone(),
    }
}

fn parse_client_range(raw: Option<&str>) -> (u64, Option<u64>, String) {
    if let Some(line) = raw {
        let value = line
            .split_once(':')
            .map(|(_, v)| v.trim())
            .unwrap_or_else(|| line.trim());
        if let Some(spec) = value.to_lowercase().strip_prefix("bytes=") {
            let spec_trimmed = spec.trim();
            if let Some((start_s, end_s)) = spec_trimmed.split_once('-') {
                let start = start_s.trim().parse::<u64>().unwrap_or(0);
                let end = end_s.trim().parse::<u64>().ok();
                if let Some(e) = end {
                    if e < start {
                        return (0, None, "bytes=0-".to_string());
                    }
                    return (start, Some(e), format!("bytes={}-{}", start, e));
                }
                return (start, None, format!("bytes={}-", start));
            }
        }
    }
    (0, None, "bytes=0-".to_string())
}

fn has_valid_token_req(path_and_query: &str, request_text: &str, expected: &str) -> bool {
    for line in request_text.lines() {
        if let Some((k, v)) = line.split_once(':') {
            let key = k.trim().to_ascii_lowercase();
            let val = v.trim();
            if key == "x-capi-token" && val == expected {
                return true;
            }
            if key == "authorization" {
                if let Some(t) = val.strip_prefix("Bearer ") {
                    if t.trim() == expected {
                        return true;
                    }
                }
            }
        }
    }
    has_valid_token(path_and_query, expected)
}

fn has_valid_token(path_and_query: &str, expected: &str) -> bool {
    let Some((_, query)) = path_and_query.split_once('?') else {
        return false;
    };
    query
        .split('&')
        .find_map(|pair| pair.strip_prefix("token="))
        == Some(expected)
}

fn is_allowed_play_url(raw: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(raw) else {
        return false;
    };
    if url.scheme() != "https" {
        return false;
    }
    matches!(
        url.host_str(),
        Some(host)
            if host == "googlevideo.com" || host.ends_with(".googlevideo.com")
    )
}

fn is_allowed_image_url(raw: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(raw) else {
        return false;
    };
    if url.scheme() != "https" {
        return false;
    }
    matches!(
        url.host_str(),
        Some(host)
            if host == "ytimg.com"
                || host.ends_with(".ytimg.com")
                || host == "googleusercontent.com"
                || host.ends_with(".googleusercontent.com")
                || host == "ggpht.com"
                || host.ends_with(".ggpht.com")
    )
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
pub struct DispositivoCastDTO {
    pub id: String,
    pub name: String,
    pub r#type: String,
    pub model: String,
    pub ip: String,
    pub port: u16,
    pub location: Option<String>,
    pub control_url: Option<String>,
}

fn resolver_ip_local_para_tv(target_ip: &str) -> String {
    if let Ok(target_addr) = format!("{}:80", target_ip).parse::<std::net::SocketAddr>() {
        if let Ok(socket) = std::net::UdpSocket::bind("0.0.0.0:0") {
            if socket.connect(target_addr).is_ok() {
                if let Ok(local) = socket.local_addr() {
                    return local.ip().to_string();
                }
            }
        }
    }
    "127.0.0.1".to_string()
}

fn get_local_subnet_ips() -> (String, Vec<String>) {
    let mut local_ip = "127.0.0.1".to_string();
    if let Ok(socket) = std::net::UdpSocket::bind("0.0.0.0:0") {
        if socket.connect("8.8.8.8:80").is_ok() {
            if let Ok(addr) = socket.local_addr() {
                local_ip = addr.ip().to_string();
            }
        }
    }

    let mut ips = Vec::new();
    let parts: Vec<&str> = local_ip.split('.').collect();
    if parts.len() == 4 {
        let prefix = format!("{}.{}.{}", parts[0], parts[1], parts[2]);
        for i in 1..=254 {
            ips.push(format!("{}.{}", prefix, i));
        }
    }
    (local_ip, ips)
}

#[tauri::command]
async fn descubrir_dispositivos_cast() -> Result<Vec<DispositivoCastDTO>, String> {
    let mut devices: Vec<DispositivoCastDTO> = Vec::new();
    let mut seen_ips: std::collections::HashSet<String> = std::collections::HashSet::new();

    // 1. SSDP Discovery via UDP broadcast / multicast
    let ssdp_requests = [
        "M-SEARCH * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\nMAN: \"ssdp:discover\"\r\nMX: 2\r\nST: ssdp:all\r\n\r\n",
        "M-SEARCH * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\nMAN: \"ssdp:discover\"\r\nMX: 2\r\nST: urn:schemas-upnp-org:device:MediaRenderer:1\r\n\r\n",
        "M-SEARCH * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\nMAN: \"ssdp:discover\"\r\nMX: 2\r\nST: urn:schemas-upnp-org:service:AVTransport:1\r\n\r\n",
        "M-SEARCH * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\nMAN: \"ssdp:discover\"\r\nMX: 2\r\nST: urn:dial-multiscreen-org:service:dial:1\r\n\r\n",
    ];

    if let Ok(socket) = tokio::net::UdpSocket::bind("0.0.0.0:0").await {
        let _ = socket.set_broadcast(true);
        if let Ok(target_addr) = "239.255.255.250:1900".parse::<std::net::SocketAddr>() {
            for req in ssdp_requests {
                let _ = socket.send_to(req.as_bytes(), target_addr).await;
            }

            let mut buf = [0u8; 4096];
            let start = std::time::Instant::now();
            let timeout_dur = std::time::Duration::from_millis(1600);

            while start.elapsed() < timeout_dur {
                let remaining = timeout_dur.saturating_sub(start.elapsed());
                if remaining.is_zero() {
                    break;
                }

                if let Ok(Ok((len, peer))) =
                    tokio::time::timeout(remaining, socket.recv_from(&mut buf)).await
                {
                    if let Ok(resp_str) = std::str::from_utf8(&buf[..len]) {
                        let mut location: Option<String> = None;

                        for line in resp_str.lines() {
                            let lower = line.to_lowercase();
                            if lower.starts_with("location:") {
                                location = Some(line[9..].trim().to_string());
                            }
                        }

                        let ip = peer.ip().to_string();
                        if !seen_ips.contains(&ip) {
                            seen_ips.insert(ip.clone());
                            let mut dev_name = format!("Smart TV ({})", ip);
                            let mut dev_type = "smart_tv".to_string();
                            let mut dev_model = "Dispositivo de red".to_string();
                            let mut control_url: Option<String> = None;

                            if let Some(loc) = &location {
                                if let Ok(client) = reqwest::Client::builder()
                                    .timeout(std::time::Duration::from_millis(700))
                                    .build()
                                {
                                    if let Ok(xml_resp) = client.get(loc).send().await {
                                        if let Ok(xml_body) = xml_resp.text().await {
                                            if let Some(s) = xml_body.find("<friendlyName>") {
                                                if let Some(e) =
                                                    xml_body[s..].find("</friendlyName>")
                                                {
                                                    dev_name =
                                                        xml_body[s + 14..s + e].trim().to_string();
                                                }
                                            }
                                            if let Some(s) = xml_body.find("<modelName>") {
                                                if let Some(e) = xml_body[s..].find("</modelName>")
                                                {
                                                    dev_model =
                                                        xml_body[s + 11..s + e].trim().to_string();
                                                }
                                            }
                                            if let Some(av_idx) = xml_body
                                                .find("urn:schemas-upnp-org:service:AVTransport:1")
                                            {
                                                if let Some(ctrl_start) =
                                                    xml_body[av_idx..].find("<controlURL>")
                                                {
                                                    if let Some(ctrl_end) = xml_body
                                                        [av_idx + ctrl_start..]
                                                        .find("</controlURL>")
                                                    {
                                                        let raw_ctrl =
                                                            xml_body[av_idx + ctrl_start + 12
                                                                ..av_idx + ctrl_start + ctrl_end]
                                                                .trim();
                                                        if raw_ctrl.starts_with("http") {
                                                            control_url =
                                                                Some(raw_ctrl.to_string());
                                                        } else {
                                                            let base = if let Ok(parsed_url) =
                                                                reqwest::Url::parse(loc)
                                                            {
                                                                format!(
                                                                    "{}://{}:{}",
                                                                    parsed_url.scheme(),
                                                                    parsed_url
                                                                        .host_str()
                                                                        .unwrap_or(&ip),
                                                                    parsed_url
                                                                        .port_or_known_default()
                                                                        .unwrap_or(peer.port())
                                                                )
                                                            } else {
                                                                format!(
                                                                    "http://{}:{}",
                                                                    ip,
                                                                    peer.port()
                                                                )
                                                            };
                                                            let formatted_ctrl =
                                                                if raw_ctrl.starts_with('/') {
                                                                    format!("{}{}", base, raw_ctrl)
                                                                } else {
                                                                    format!("{}/{}", base, raw_ctrl)
                                                                };
                                                            control_url = Some(formatted_ctrl);
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }

                            let name_lower = dev_name.to_lowercase();
                            if name_lower.contains("chromecast")
                                || name_lower.contains("google cast")
                            {
                                dev_type = "chromecast".to_string();
                            } else if name_lower.contains("android tv")
                                || name_lower.contains("google tv")
                                || name_lower.contains("bravia")
                            {
                                dev_type = "android_tv".to_string();
                            } else if name_lower.contains("nest")
                                || name_lower.contains("speaker")
                                || name_lower.contains("audio")
                                || name_lower.contains("sonos")
                            {
                                dev_type = "speaker".to_string();
                            } else if name_lower.contains("samsung")
                                || name_lower.contains("lg")
                                || name_lower.contains("roku")
                                || name_lower.contains("tizen")
                                || name_lower.contains("webos")
                            {
                                dev_type = "smart_tv".to_string();
                            }

                            devices.push(DispositivoCastDTO {
                                id: format!("{}_{}", ip, peer.port()),
                                name: dev_name,
                                r#type: dev_type,
                                model: dev_model,
                                ip,
                                port: peer.port(),
                                location,
                                control_url,
                            });
                        }
                    }
                }
            }
        }
    }

    // 2. High-speed concurrent subnet scan for Smart TVs & Chromecasts
    let (my_ip, subnet_ips) = get_local_subnet_ips();
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_millis(600))
        .build()
        .unwrap_or_default();

    let mut scan_tasks = Vec::new();
    for ip in subnet_ips {
        if ip == my_ip || seen_ips.contains(&ip) {
            continue;
        }
        let client_clone = client.clone();
        scan_tasks.push(tokio::spawn(async move {
            let ports = [8008, 8001, 8060, 3000, 7676, 49152, 49153, 52235, 1400];
            for port in ports {
                let addr: Result<std::net::SocketAddr, _> = format!("{}:{}", ip, port).parse();
                if let Ok(sa) = addr {
                    if let Ok(Ok(_)) = tokio::time::timeout(
                        std::time::Duration::from_millis(180),
                        tokio::net::TcpStream::connect(&sa),
                    )
                    .await
                    {
                        // Check Chromecast / Google Cast (8008)
                        if port == 8008 {
                            let eureka_url = format!("http://{}:8008/setup/eureka_info", ip);
                            if let Ok(resp) = client_clone.get(&eureka_url).send().await {
                                if let Ok(text) = resp.text().await {
                                    if let Ok(json_val) =
                                        serde_json::from_str::<serde_json::Value>(&text)
                                    {
                                        let name = json_val
                                            .get("name")
                                            .and_then(|n| n.as_str())
                                            .unwrap_or("Chromecast / Google TV")
                                            .to_string();
                                        let model = json_val
                                            .get("device_info")
                                            .and_then(|d| d.get("model_name"))
                                            .and_then(|m| m.as_str())
                                            .unwrap_or("Chromecast")
                                            .to_string();
                                        return Some(DispositivoCastDTO {
                                            id: format!("{}_{}", ip, port),
                                            name,
                                            r#type: "chromecast".to_string(),
                                            model,
                                            ip: ip.clone(),
                                            port,
                                            location: Some(eureka_url),
                                            control_url: None,
                                        });
                                    }
                                }
                            }
                        }

                        // Check Samsung Smart TV (8001)
                        if port == 8001 {
                            let samsung_url = format!("http://{}:8001/api/v2/", ip);
                            if let Ok(resp) = client_clone.get(&samsung_url).send().await {
                                if let Ok(text) = resp.text().await {
                                    if let Ok(json_val) =
                                        serde_json::from_str::<serde_json::Value>(&text)
                                    {
                                        let name = json_val
                                            .get("name")
                                            .and_then(|n| n.as_str())
                                            .unwrap_or("Samsung Smart TV")
                                            .to_string();
                                        let model = json_val
                                            .get("device")
                                            .and_then(|d| d.get("modelName"))
                                            .and_then(|m| m.as_str())
                                            .unwrap_or("Samsung Tizen")
                                            .to_string();
                                        return Some(DispositivoCastDTO {
                                            id: format!("{}_{}", ip, port),
                                            name,
                                            r#type: "smart_tv".to_string(),
                                            model,
                                            ip: ip.clone(),
                                            port,
                                            location: Some(samsung_url),
                                            control_url: None,
                                        });
                                    }
                                }
                            }
                        }

                        // Check Roku TV (8060)
                        if port == 8060 {
                            let roku_url = format!("http://{}:8060/query/device-info", ip);
                            if let Ok(resp) = client_clone.get(&roku_url).send().await {
                                if let Ok(xml) = resp.text().await {
                                    let mut name = "Roku TV".to_string();
                                    if let Some(s) = xml.find("<friendly-device-name>") {
                                        if let Some(e) = xml[s..].find("</friendly-device-name>") {
                                            name = xml[s + 22..s + e].trim().to_string();
                                        }
                                    }
                                    return Some(DispositivoCastDTO {
                                        id: format!("{}_{}", ip, port),
                                        name,
                                        r#type: "smart_tv".to_string(),
                                        model: "Roku OS".to_string(),
                                        ip: ip.clone(),
                                        port,
                                        location: Some(roku_url),
                                        control_url: None,
                                    });
                                }
                            }
                        }

                        // Generic Smart TV / DLNA MediaRenderer detection
                        let desc_urls = [
                            format!("http://{}:{}/description.xml", ip, port),
                            format!("http://{}:{}/dev/0/desc.xml", ip, port),
                            format!("http://{}:{}/upnp/desc.xml", ip, port),
                        ];
                        for d_url in desc_urls {
                            if let Ok(resp) = client_clone.get(&d_url).send().await {
                                if let Ok(xml) = resp.text().await {
                                    let mut name = format!("Smart TV ({})", ip);
                                    if let Some(s) = xml.find("<friendlyName>") {
                                        if let Some(e) = xml[s..].find("</friendlyName>") {
                                            name = xml[s + 14..s + e].trim().to_string();
                                        }
                                    }
                                    let mut control_url = None;
                                    if let Some(av_idx) =
                                        xml.find("urn:schemas-upnp-org:service:AVTransport:1")
                                    {
                                        if let Some(ctrl_start) = xml[av_idx..].find("<controlURL>")
                                        {
                                            if let Some(ctrl_end) =
                                                xml[av_idx + ctrl_start..].find("</controlURL>")
                                            {
                                                let raw_ctrl = xml[av_idx + ctrl_start + 12
                                                    ..av_idx + ctrl_start + ctrl_end]
                                                    .trim();
                                                control_url = Some(if raw_ctrl.starts_with('/') {
                                                    format!("http://{}:{}{}", ip, port, raw_ctrl)
                                                } else {
                                                    raw_ctrl.to_string()
                                                });
                                            }
                                        }
                                    }
                                    return Some(DispositivoCastDTO {
                                        id: format!("{}_{}", ip, port),
                                        name,
                                        r#type: "smart_tv".to_string(),
                                        model: "Smart TV / DLNA".to_string(),
                                        ip: ip.clone(),
                                        port,
                                        location: Some(d_url),
                                        control_url,
                                    });
                                }
                            }
                        }

                        return Some(DispositivoCastDTO {
                            id: format!("{}_{}", ip, port),
                            name: format!("Dispositivo de Red ({})", ip),
                            r#type: "smart_tv".to_string(),
                            model: format!("Puerto {}", port),
                            ip: ip.clone(),
                            port,
                            location: None,
                            control_url: None,
                        });
                    }
                }
            }
            None
        }));
    }

    for task in futures_util::future::join_all(scan_tasks).await {
        if let Ok(Some(dev)) = task {
            if !seen_ips.contains(&dev.ip) {
                seen_ips.insert(dev.ip.clone());
                devices.push(dev);
            }
        }
    }

    Ok(devices)
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
async fn transmitir_a_dispositivo(
    ip: String,
    port: u16,
    stream_url: String,
    title: String,
    artist: String,
    thumbnail: Option<String>,
    control_url: Option<String>,
    track_id: Option<String>,
) -> Result<bool, String> {
    println!(
        "[CAST] Transmitiendo '{}' por '{}' a {}:{} (Track: {:?}, Thumb: {:?})",
        title, artist, ip, port, track_id, thumbnail
    );

    // 1. Resolve host computer's real LAN IP so the TV doesn't try to connect to localhost/127.0.0.1
    let local_lan_ip = resolver_ip_local_para_tv(&ip);
    let mut resolved_stream_url = stream_url.clone();
    if resolved_stream_url.contains("127.0.0.1") {
        resolved_stream_url = resolved_stream_url.replace("127.0.0.1", &local_lan_ip);
    } else if resolved_stream_url.contains("localhost") {
        resolved_stream_url = resolved_stream_url.replace("localhost", &local_lan_ip);
    }

    let mut resolved_thumbnail = thumbnail.unwrap_or_default();
    if resolved_thumbnail.contains("127.0.0.1") {
        resolved_thumbnail = resolved_thumbnail.replace("127.0.0.1", &local_lan_ip);
    } else if resolved_thumbnail.contains("localhost") {
        resolved_thumbnail = resolved_thumbnail.replace("localhost", &local_lan_ip);
    }

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .build()
        .map_err(|e| e.to_string())?;

    // 2. Specialized Roku TV Direct Cast (Port 8060)
    if port == 8060 {
        let encoded_u = urlencoding::encode(&resolved_stream_url);
        let encoded_t = urlencoding::encode(&title);
        let encoded_a = urlencoding::encode(&artist);
        let encoded_thumb = urlencoding::encode(&resolved_thumbnail);
        let roku_urls = [
            format!("http://{}:8060/launch/15667?u={}&format=mp3&songTitle={}&artistName={}&albumArtUrl={}", ip, encoded_u, encoded_t, encoded_a, encoded_thumb),
            format!("http://{}:8060/input/15667?u={}&format=mp3", ip, encoded_u),
            format!("http://{}:8060/launch/playonroku?u={}", ip, encoded_u),
        ];
        for r_url in roku_urls {
            if let Ok(res) = client.post(&r_url).send().await {
                if res.status().is_success() {
                    println!("[CAST] Transmisión exitosa a Roku TV: {}", r_url);
                    return Ok(true);
                }
            }
        }
    }

    // 3. DIAL App Launch (Chromecast, Android TV, Google TV, Samsung / LG / Sony TV YouTube)
    if let Some(ref tid) = track_id {
        let dial_urls = [
            format!("http://{}:8008/apps/YouTube", ip),
            format!("http://{}:{}/apps/YouTube", ip, port),
            format!("http://{}:8008/apps/YouTube?v={}", ip, tid),
        ];
        for d_url in dial_urls {
            let res = client
                .post(&d_url)
                .header("Content-Type", "application/x-www-form-urlencoded")
                .body(format!("v={}", tid))
                .send()
                .await;
            if let Ok(r) = res {
                if r.status().is_success() || r.status().as_u16() == 201 {
                    println!("[CAST] Transmisión DIAL/YouTube exitosa a {}", d_url);
                    return Ok(true);
                }
            }
        }
    }

    // 4. DLNA UPnP AVTransport MediaRenderer SOAP Playback
    let mut dlna_endpoints = Vec::new();
    if let Some(ctrl) = control_url {
        dlna_endpoints.push(ctrl);
    }

    // Probe candidate descriptors to find active AVTransport control URLs
    let candidate_desc_urls = [
        format!("http://{}:{}/description.xml", ip, port),
        format!("http://{}:{}/dev/0/desc.xml", ip, port),
        format!("http://{}:{}/upnp/desc.xml", ip, port),
        format!("http://{}:7676/smp_4_", ip),
        format!("http://{}:7676/smp_23_", ip),
        format!("http://{}:9197/dmr", ip),
        format!("http://{}:52235/dmr.xml", ip),
        format!("http://{}:49152/description.xml", ip),
        format!("http://{}:49153/description.xml", ip),
        format!("http://{}:1400/xml/device_description.xml", ip),
    ];

    for desc_url in candidate_desc_urls {
        if let Ok(resp) = client.get(&desc_url).send().await {
            if let Ok(xml) = resp.text().await {
                if let Some(av_idx) = xml.find("urn:schemas-upnp-org:service:AVTransport:1") {
                    if let Some(ctrl_start) = xml[av_idx..].find("<controlURL>") {
                        if let Some(ctrl_end) = xml[av_idx + ctrl_start..].find("</controlURL>") {
                            let raw_ctrl = xml
                                [av_idx + ctrl_start + 12..av_idx + ctrl_start + ctrl_end]
                                .trim();
                            let formatted = if raw_ctrl.starts_with("http") {
                                raw_ctrl.to_string()
                            } else {
                                let base = if let Ok(parsed_url) = reqwest::Url::parse(&desc_url) {
                                    format!(
                                        "{}://{}:{}",
                                        parsed_url.scheme(),
                                        parsed_url.host_str().unwrap_or(&ip),
                                        parsed_url.port_or_known_default().unwrap_or(port)
                                    )
                                } else {
                                    format!("http://{}:{}", ip, port)
                                };
                                if raw_ctrl.starts_with('/') {
                                    format!("{}{}", base, raw_ctrl)
                                } else {
                                    format!("{}/{}", base, raw_ctrl)
                                }
                            };
                            if !dlna_endpoints.contains(&formatted) {
                                dlna_endpoints.push(formatted);
                            }
                        }
                    }
                }
            }
        }
    }

    // Default standard endpoints
    dlna_endpoints.push(format!("http://{}:{}/AVTransport/control", ip, port));
    dlna_endpoints.push(format!("http://{}:{}/upnp/control/AVTransport1", ip, port));
    dlna_endpoints.push(format!("http://{}:{}/upnp/control/AVTransport", ip, port));
    dlna_endpoints.push(format!("http://{}:7676/smp_4_", ip));
    dlna_endpoints.push(format!(
        "http://{}:1400/MediaRenderer/AVTransport/Control",
        ip
    ));

    let escaped_title = title
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    let escaped_artist = artist
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    let escaped_url = resolved_stream_url
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    let escaped_thumb = resolved_thumbnail
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");

    let stop_body = r#"<?xml version="1.0" encoding="utf-8"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/" s:encodingStyle="http://schemas.xmlsoap.org/soap/encoding/">
  <s:Body>
    <u:Stop xmlns:u="urn:schemas-upnp-org:service:AVTransport:1">
      <InstanceID>0</InstanceID>
    </u:Stop>
  </s:Body>
</s:Envelope>"#;

    let set_uri_body = format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/" s:encodingStyle="http://schemas.xmlsoap.org/soap/encoding/">
  <s:Body>
    <u:SetAVTransportURI xmlns:u="urn:schemas-upnp-org:service:AVTransport:1">
      <InstanceID>0</InstanceID>
      <CurrentURI>{}</CurrentURI>
      <CurrentURIMetaData>&lt;DIDL-Lite xmlns="urn:schemas-upnp-org:metadata-1-0/DIDL-Lite/" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:upnp="urn:schemas-upnp-org:metadata-1-0/upnp/" xmlns:dlna="urn:schemas-dlna-org:metadata-1-0/"&gt;&lt;item id="0" parentID="-1" restricted="1"&gt;&lt;dc:title&gt;{}&lt;/dc:title&gt;&lt;dc:creator&gt;{}&lt;/dc:creator&gt;&lt;upnp:artist&gt;{}&lt;/upnp:artist&gt;&lt;upnp:albumArtURI dlna:profileID="JPEG_MED"&gt;{}&lt;/upnp:albumArtURI&gt;&lt;upnp:icon&gt;{}&lt;/upnp:icon&gt;&lt;upnp:class&gt;object.item.audioItem.musicTrack&lt;/upnp:class&gt;&lt;res protocolInfo="http-get:*:audio/mpeg:DLNA.ORG_OP=01;DLNA.ORG_CI=0;DLNA.ORG_FLAGS=01700000000000000000000000000000"&gt;{}&lt;/res&gt;&lt;res protocolInfo="http-get:*:audio/mp4:*"&gt;{}&lt;/res&gt;&lt;res protocolInfo="http-get:*:audio/*:*"&gt;{}&lt;/res&gt;&lt;/item&gt;&lt;/DIDL-Lite&gt;</CurrentURIMetaData>
    </u:SetAVTransportURI>
  </s:Body>
</s:Envelope>"#,
        escaped_url,
        escaped_title,
        escaped_artist,
        escaped_artist,
        escaped_thumb,
        escaped_thumb,
        escaped_url,
        escaped_url,
        escaped_url
    );

    let play_body = r#"<?xml version="1.0" encoding="utf-8"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/" s:encodingStyle="http://schemas.xmlsoap.org/soap/encoding/">
  <s:Body>
    <u:Play xmlns:u="urn:schemas-upnp-org:service:AVTransport:1">
      <InstanceID>0</InstanceID>
      <Speed>1</Speed>
    </u:Play>
  </s:Body>
</s:Envelope>"#;

    for target_endpoint in dlna_endpoints {
        let _ = client
            .post(&target_endpoint)
            .header("Content-Type", "text/xml; charset=\"utf-8\"")
            .header(
                "SOAPAction",
                "\"urn:schemas-upnp-org:service:AVTransport:1#Stop\"",
            )
            .body(stop_body)
            .send()
            .await;

        let set_res = client
            .post(&target_endpoint)
            .header("Content-Type", "text/xml; charset=\"utf-8\"")
            .header(
                "SOAPAction",
                "\"urn:schemas-upnp-org:service:AVTransport:1#SetAVTransportURI\"",
            )
            .body(set_uri_body.clone())
            .send()
            .await;

        if let Ok(resp) = set_res {
            if resp.status().is_success() {
                let _ = client
                    .post(&target_endpoint)
                    .header("Content-Type", "text/xml; charset=\"utf-8\"")
                    .header(
                        "SOAPAction",
                        "\"urn:schemas-upnp-org:service:AVTransport:1#Play\"",
                    )
                    .body(play_body)
                    .send()
                    .await;
                println!("[CAST] Transmisión DLNA exitosa a {}", target_endpoint);
                return Ok(true);
            }
        }
    }

    Ok(true)
}

#[tauri::command]
fn obtener_info_sistema(app: tauri::AppHandle) -> String {
    let version = app.package_info().version.to_string();
    serde_json::json!({
        "app_version": version,
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "daemon_pid": 0,
        "backend": "capi-engine (Rust)",
        "backend_version": capi_engine::VERSION
    })
    .to_string()
}

#[tauri::command]
async fn discord_connect(state: tauri::State<'_, DiscordState>) -> Result<bool, String> {
    let client_arc = state.client.clone();
    tokio::task::spawn_blocking(move || {
        let mut client_guard = client_arc.lock().map_err(|e| e.to_string())?;
        if client_guard.is_some() {
            return Ok(true);
        }
        let mut client = DiscordIpcClient::new("1249392288333889638").map_err(|e| e.to_string())?;
        client
            .connect()
            .map_err(|e| format!("Discord no está abierto: {}", e))?;
        *client_guard = Some(client);
        Ok(true)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn discord_update(
    state: tauri::State<'_, DiscordState>,
    title: String,
    artist: String,
    thumbnail: String,
    elapsed: i64,
    duration: i64,
    is_playing: bool,
) -> Result<(), String> {
    let client_arc = state.client.clone();
    tokio::task::spawn_blocking(move || {
        let mut client_guard = client_arc.lock().map_err(|e| e.to_string())?;
        if let Some(client) = client_guard.as_mut() {
            if !is_playing {
                if !title.is_empty() {
                    let mut assets = activity::Assets::new().large_text(&title);
                    if thumbnail.starts_with("http") {
                        assets = assets.large_image(&thumbnail);
                    } else {
                        assets = assets.large_image("logo");
                    }
                    let pause_state = format!("En pausa • {}", artist);
                    let act = activity::Activity::new()
                        .state(&pause_state)
                        .details(&title)
                        .assets(assets);
                    let _ = client.set_activity(act);
                } else {
                    let _ = client.clear_activity();
                }
                return Ok(());
            }

            let mut assets = activity::Assets::new().large_text(&title);

            if thumbnail.starts_with("http") {
                assets = assets.large_image(&thumbnail);
            } else {
                assets = assets.large_image("logo");
            }
            assets = assets.small_image("logo").small_text("Capi");

            let mut act = activity::Activity::new()
                .state(&artist)
                .details(&title)
                .assets(assets);

            if duration > 0 {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs() as i64;
                let start = now - elapsed;
                let end = start + duration;
                let timestamps = activity::Timestamps::new().start(start).end(end);
                act = act.timestamps(timestamps);
            }

            let _ = client.set_activity(act);
        }
        Ok(())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn discord_disconnect(state: tauri::State<'_, DiscordState>) -> Result<(), String> {
    let client_arc = state.client.clone();
    tokio::task::spawn_blocking(move || {
        let mut client_guard = client_arc.lock().map_err(|e| e.to_string())?;
        if let Some(mut client) = client_guard.take() {
            let _ = client.clear_activity();
            let _ = client.close();
        }
        Ok(())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[derive(serde::Serialize, Debug)]
struct LocalTrack {
    id: String,
    title: String,
    artist: String,
    thumbnail: String,
    duration: u32,
}

#[tauri::command]
fn seleccionar_carpeta() -> Option<String> {
    let result = rfd::FileDialog::new().pick_folder();
    result.map(|path| path.to_string_lossy().into_owned())
}

fn scan_audio_files_recursive(
    dir: &std::path::Path,
    depth: usize,
    max_depth: usize,
    tracks: &mut Vec<LocalTrack>,
) {
    if depth > max_depth {
        return;
    }
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                scan_audio_files_recursive(&path, depth + 1, max_depth, tracks);
            } else if path.is_file() {
                if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                    let ext_lower = ext.to_lowercase();
                    if ext_lower == "mp3"
                        || ext_lower == "wav"
                        || ext_lower == "m4a"
                        || ext_lower == "ogg"
                        || ext_lower == "flac"
                        || ext_lower == "aac"
                        || ext_lower == "webm"
                        || ext_lower == "opus"
                    {
                        let file_name = path
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or("Audio Local")
                            .to_string();

                        let title = path
                            .file_stem()
                            .and_then(|n| n.to_str())
                            .unwrap_or(&file_name)
                            .to_string();

                        let artist = path
                            .parent()
                            .and_then(|p| p.file_name())
                            .and_then(|n| n.to_str())
                            .unwrap_or("Archivo Local")
                            .to_string();

                        let path_str = path.to_string_lossy().into_owned();
                        let id = format!("local://{}", path_str);

                        tracks.push(LocalTrack {
                            id,
                            title,
                            artist: if artist == "Capi" || artist.is_empty() {
                                "Archivo Local".to_string()
                            } else {
                                artist
                            },
                            thumbnail: "".to_string(),
                            duration: 0,
                        });
                    }
                }
            }
        }
    }
}

#[tauri::command]
fn listar_archivos_locales(
    ruta: String,
    roots: tauri::State<'_, LocalRoots>,
) -> Result<Vec<LocalTrack>, String> {
    let dir_path = std::path::Path::new(&ruta);
    if !dir_path.exists() || !dir_path.is_dir() {
        return Err("La ruta no existe o no es un directorio".to_string());
    }
    if let Ok(canonical) = dir_path.canonicalize() {
        let _ = roots.allow(&canonical);
    }

    let mut tracks = Vec::new();
    scan_audio_files_recursive(dir_path, 0, 15, &mut tracks);

    Ok(tracks)
}

#[tauri::command]
async fn consolidar_carpetas_descargas(
    app: tauri::AppHandle,
) -> Result<std::collections::HashMap<String, String>, String> {
    let music_dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let capi_dir = music_dir.join("Capi");
    if !capi_dir.exists() || !capi_dir.is_dir() {
        return Ok(std::collections::HashMap::new());
    }

    let mut path_mappings = std::collections::HashMap::new();

    // 1. Mover archivos sueltos en la raíz de Capi a la carpeta de su artista correspondiente
    if let Ok(entries) = std::fs::read_dir(&capi_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                    let ext_lower = ext.to_lowercase();
                    if matches!(
                        ext_lower.as_str(),
                        "mp3" | "m4a" | "webm" | "opus" | "wav" | "flac" | "ogg" | "aac"
                    ) {
                        let file_name = entry.file_name();
                        let file_name_str = file_name.to_string_lossy();
                        let primary = extract_primary_artist(&file_name_str, "");
                        let target_dir = if !primary.is_empty() && primary != "Desconocido" {
                            capi_dir.join(&primary)
                        } else {
                            capi_dir.join("Desconocido")
                        };
                        let _ = std::fs::create_dir_all(&target_dir);
                        let target_path = target_dir.join(&file_name);
                        let old_path_str = path.to_string_lossy().to_string();
                        if !target_path.exists() {
                            if std::fs::rename(&path, &target_path).is_ok() {
                                path_mappings.insert(
                                    old_path_str,
                                    target_path.to_string_lossy().to_string(),
                                );
                            }
                        } else {
                            path_mappings
                                .insert(old_path_str, target_path.to_string_lossy().to_string());
                            let _ = std::fs::remove_file(&path);
                        }
                    }
                }
            }
        }
    }

    // 2. Indexar recursivamente todos los archivos de audio en Capi (hasta 15 niveles) para mapear rutas antiguas y nombres de archivo
    let mut all_files = Vec::new();
    collect_all_audio_files_recursive(&capi_dir, 0, 15, &mut all_files);

    for actual_path in all_files {
        if let Some(file_name) = actual_path.file_name() {
            let file_name_str = file_name.to_string_lossy().to_string();
            let actual_path_str = actual_path.to_string_lossy().to_string();

            // Mapear la ruta antigua al estilo raíz: Capi/Cancion.m4a -> Capi/.../Cancion.m4a
            let old_root_path = capi_dir.join(file_name).to_string_lossy().to_string();
            if old_root_path != actual_path_str {
                path_mappings.insert(old_root_path, actual_path_str.clone());
            }

            // Mapear también por nombre simple de archivo
            path_mappings.insert(file_name_str, actual_path_str);
        }
    }

    Ok(path_mappings)
}

#[tauri::command]
async fn abrir_carpeta_descargas(app: tauri::AppHandle) -> Result<(), String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("Capi");
    if !dir.exists() {
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    }
    open::that(&dir).map_err(|e| format!("No se pudo abrir: {}", e))
}

#[derive(Serialize)]
struct StorageInfo {
    app_size: u64,
    downloads_size: u64,
    cache_size: u64,
    free_space: u64,
}

fn dir_size(path: &PathBuf) -> u64 {
    let mut total = 0u64;
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                total += dir_size(&p);
            } else if let Ok(meta) = p.metadata() {
                total += meta.len();
            }
        }
    }
    total
}

#[tauri::command]
fn obtener_espacio_almacenamiento(app: tauri::AppHandle) -> Result<StorageInfo, String> {
    let app_data = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let capi_dir = app_data.join("Capi");
    let cache_dir = capi_dir.join("cache");

    let downloads_size = if capi_dir.exists() {
        let mut total = dir_size(&capi_dir);
        if cache_dir.exists() {
            total -= dir_size(&cache_dir);
        }
        total
    } else {
        0
    };

    let cache_size = if cache_dir.exists() {
        dir_size(&cache_dir)
    } else {
        0
    };

    let app_size = std::env::current_exe()
        .ok()
        .and_then(|p| p.metadata().ok())
        .map(|m| m.len())
        .unwrap_or(0);

    let free_space = Some(
        capi_dir
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("/")),
    )
    .and_then(|p| fs2::available_space(&p).ok())
    .unwrap_or(0);

    Ok(StorageInfo {
        app_size,
        downloads_size,
        cache_size,
        free_space,
    })
}

#[tauri::command]
fn media_update_state(
    state: tauri::State<'_, Arc<Mutex<MediaState>>>,
    new_state: MediaState,
) -> Result<(), String> {
    let mut guard = state.lock().map_err(|e| e.to_string())?;
    *guard = new_state;
    Ok(())
}

#[tauri::command]
fn media_get_state(state: tauri::State<'_, Arc<Mutex<MediaState>>>) -> Result<MediaState, String> {
    let guard = state.lock().map_err(|e| e.to_string())?;
    Ok(guard.clone())
}

#[tauri::command]
async fn obtener_reproducciones(
    engine: tauri::State<'_, Arc<Engine>>,
    video_id: String,
) -> Result<String, String> {
    let info = engine
        .get_media_info(&video_id)
        .await
        .map_err(|e| e.to_string())?;
    serde_json::to_string(&info).map_err(|e| e.to_string())
}

#[tauri::command]
async fn suscribirse_canal(
    engine: tauri::State<'_, Arc<Engine>>,
    channel_id: String,
    suscribir: bool,
) -> Result<bool, String> {
    engine
        .subscribe_channel(&channel_id, suscribir)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn like_cancion_ytm(
    engine: tauri::State<'_, Arc<Engine>>,
    video_id: String,
    like: bool,
) -> Result<bool, String> {
    engine
        .like_video(&video_id, like)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn obtener_estado_sesion_ytm(
    engine: tauri::State<'_, Arc<Engine>>,
) -> Result<String, String> {
    let state = engine.ytm_account_state().await;
    serde_json::to_string(&state).map_err(|e| e.to_string())
}

#[tauri::command]
async fn guardar_sesion_ytm(
    engine: tauri::State<'_, Arc<Engine>>,
    cookie: String,
    visitor_data: Option<String>,
    data_sync_id: Option<String>,
) -> Result<String, String> {
    let state = engine
        .save_ytm_session(cookie, visitor_data, data_sync_id)
        .await
        .map_err(|e| e.to_string())?;
    serde_json::to_string(&state).map_err(|e| e.to_string())
}

#[tauri::command]
async fn cerrar_sesion_ytm(
    app: tauri::AppHandle,
    engine: tauri::State<'_, Arc<Engine>>,
) -> Result<(), String> {
    engine
        .clear_ytm_session()
        .await
        .map_err(|e| e.to_string())?;

    // Limpiar archivos de cookies de WebKit si existen para evitar recordar sesiones
    if let Ok(app_data) = app.path().app_data_dir() {
        let _ = std::fs::remove_file(app_data.join("cookies"));
        let _ = std::fs::remove_file(app_data.join("cookies.sqlite"));
    }

    Ok(())
}

#[tauri::command]
async fn sincronizar_ytm(engine: tauri::State<'_, Arc<Engine>>) -> Result<String, String> {
    let sync_data = engine.sync_ytm().await.map_err(|e| e.to_string())?;
    serde_json::to_string(&sync_data).map_err(|e| e.to_string())
}

#[tauri::command]
async fn abrir_login_ytm(
    app: tauri::AppHandle,
    engine: tauri::State<'_, Arc<Engine>>,
) -> Result<(), String> {
    if let Some(win) = app.get_webview_window("login-ytm") {
        let _ = win.close();
    }

    let login_url =
        "https://accounts.google.com/ServiceLogin?continue=https%3A%2F%2Fmusic.youtube.com";
    let auth_done = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let auth_done_nav = auth_done.clone();
    let app_clone_nav = app.clone();
    let engine_clone_nav = engine.inner().clone();
    let rt_handle = tokio::runtime::Handle::current();
    let rt_handle_nav = rt_handle.clone();

    let init_script = r#"
        (function() {
            function checkYtmAuth() {
                try {
                    if (window !== window.top) return;
                    if (window.__capi_auth_sent) return;
                    var h = window.location.hostname || "";
                    if (h === "music.youtube.com" || h === "www.youtube.com") {
                        var ck = document.cookie || "";
                        if (ck.indexOf("SAPISID") !== -1 || ck.indexOf("__Secure-3PAPISID") !== -1 || ck.indexOf("__Secure-1PAPISID") !== -1) {
                            window.__capi_auth_sent = true;
                            var vd = "";
                            var ds = "";
                            try {
                                if (window.yt && window.yt.config_) {
                                    vd = window.yt.config_.VISITOR_DATA || "";
                                    ds = window.yt.config_.DATASYNC_ID || "";
                                }
                                if (!vd && window.ytcfg && typeof window.ytcfg.get === 'function') {
                                    vd = window.ytcfg.get('VISITOR_DATA') || "";
                                    ds = window.ytcfg.get('DATASYNC_ID') || "";
                                }
                            } catch(e) {}
                            var payload = {
                                cookie: ck,
                                visitor_data: vd || null,
                                data_sync_id: ds || null
                            };
                            window.location.href = "https://capi.local/auth_success?payload=" + encodeURIComponent(JSON.stringify(payload));
                        }
                    }
                } catch(e) {}
            }
            setInterval(checkYtmAuth, 600);
            checkYtmAuth();
        })();
    "#;

    let _win = WebviewWindowBuilder::new(
        &app,
        "login-ytm",
        tauri::WebviewUrl::External(reqwest::Url::parse(login_url).map_err(|e| e.to_string())?),
    )
    .title("Iniciar sesión en YouTube Music - Capi")
    .inner_size(600.0, 750.0)
    .incognito(false)
    .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36")
    .initialization_script(init_script)
    .on_navigation(move |url| {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if url.host_str() == Some("capi.local") && url.path() == "/auth_success" {
                if let Some(payload_str) = url.query_pairs().find(|(k, _)| k == "payload").map(|(_, v)| v.into_owned()) {
                    if !auth_done_nav.swap(true, std::sync::atomic::Ordering::SeqCst) {
                        let app_h = app_clone_nav.clone();
                        let eng = engine_clone_nav.clone();
                        rt_handle_nav.spawn(async move {
                            #[derive(serde::Deserialize)]
                            struct AuthPayload {
                                cookie: String,
                                visitor_data: Option<String>,
                                data_sync_id: Option<String>,
                            }
                            if let Ok(payload) = serde_json::from_str::<AuthPayload>(&payload_str) {
                                let mut cookie_map = std::collections::HashMap::new();
                                for part in payload.cookie.split(';') {
                                    if let Some((k, v)) = part.trim().split_once('=') {
                                        cookie_map.insert(k.trim().to_string(), v.trim().to_string());
                                    }
                                }
                                if let Some(w) = app_h.get_webview_window("login-ytm") {
                                    if let Ok(native_cookies) = w.cookies() {
                                        for c in native_cookies {
                                            cookie_map.insert(c.name().to_string(), c.value().to_string());
                                        }
                                    }
                                }
                                let merged_cookie = cookie_map
                                    .into_iter()
                                    .map(|(k, v)| format!("{k}={v}"))
                                    .collect::<Vec<_>>()
                                    .join("; ");

                                match eng.save_ytm_session(merged_cookie, payload.visitor_data, payload.data_sync_id).await {
                                    Ok(account_state) => {
                                        let _ = app_h.emit("ytm-auth-result", serde_json::json!({
                                            "success": true,
                                            "name": account_state.name,
                                            "email": account_state.email
                                        }));
                                        // Sincronización automática inmediata en segundo plano (sin descargar música)
                                        if let Ok(sync_res) = eng.sync_ytm().await {
                                            let _ = app_h.emit("ytm-sync-completed", serde_json::to_value(&sync_res).unwrap_or_default());
                                        }
                                    }
                                    Err(err) => {
                                        let _ = app_h.emit("ytm-auth-result", serde_json::json!({
                                            "success": false,
                                            "cancelled": false,
                                            "message": format!("Error al guardar sesión: {}", err)
                                        }));
                                    }
                                }
                            }
                            if let Some(w) = app_h.get_webview_window("login-ytm") {
                                let _ = w.close();
                            }
                        });
                    }
                }
                return false;
            }
            true
        }));
        result.unwrap_or(true)
    })
    .build()
    .map_err(|e| e.to_string())?;

    let app_clone = app.clone();
    let auth_done_monitor = auth_done.clone();

    // Tarea asíncrona para inyectar evaluación periódica y vigilar cierre
    tokio::spawn(async move {
        for _ in 0..300 {
            // Monitorear hasta 5 minutos
            tokio::time::sleep(tokio::time::Duration::from_millis(800)).await;

            if auth_done_monitor.load(std::sync::atomic::Ordering::SeqCst) {
                break;
            }

            let current_win = match app_clone.get_webview_window("login-ytm") {
                Some(w) => w,
                None => {
                    // Ventana cerrada por el usuario antes de autenticarse
                    if !auth_done_monitor.load(std::sync::atomic::Ordering::SeqCst) {
                        let _ = app_clone.emit(
                            "ytm-auth-result",
                            serde_json::json!({
                                "success": false,
                                "cancelled": true,
                                "message": "Ventana de autenticación cerrada."
                            }),
                        );
                    }
                    break;
                }
            };

            // Inyectar script continuamente en caso de navegación SPA o redirección
            let _ = current_win.eval(init_script);
        }
    });

    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
                let _ = window.set_focus();
            }
        }))
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec!["--hidden"]),
        ))
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .setup(|app| {
            let handle = app.handle();

            if let Err(e) = (|| -> Result<(), String> {
                // Start localhost HTTP server for local music playback
                let app_data = handle
                    .path()
                    .app_data_dir()
                    .map_err(|e| format!("No se pudo resolver el directorio de datos: {}", e))?;
                let capi_dir = app_data.join("Capi");
                if !capi_dir.exists() {
                    std::fs::create_dir_all(&capi_dir)
                        .map_err(|e| format!("No se pudo crear el directorio de Capi: {}", e))?;
                }
                // Create cache directory
                let cache_dir = capi_dir.join("cache").join("audio");
                if !cache_dir.exists() {
                    std::fs::create_dir_all(&cache_dir)
                        .map_err(|e| format!("No se pudo crear el directorio de caché: {}", e))?;
                }

                // Motor Rust: se inicializa sin red ni tokens; el catálogo y
                // los streams se resuelven bajo demanda.
                let engine = Engine::new(capi_dir.join("engine"))
                    .map_err(|e| format!("No se pudo inicializar capi-engine: {e}"))?;
                app.manage(Arc::new(engine));

                let media_state = Arc::new(Mutex::new(MediaState::default()));
                let proxy_media_state = media_state.clone();
                app.manage(media_state);

                let token = std::env::var("CAPI_PROXY_TOKEN")
                    .ok()
                    .filter(|t| t.len() >= 16)
                    .unwrap_or_else(|| uuid::Uuid::new_v4().simple().to_string());
                let local_roots = LocalRoots::default();
                let local_port = start_local_server(
                    capi_dir,
                    handle.clone(),
                    proxy_media_state,
                    token.clone(),
                    local_roots.clone(),
                )?;
                app.manage(LocalServerState {
                    port: local_port,
                    token,
                });
                app.manage(local_roots);
                app.manage(DiscordState {
                    client: std::sync::Arc::new(Mutex::new(None)),
                });
                Ok(())
            })() {
                #[cfg(not(target_os = "linux"))]
                {
                    let _ = std::fs::create_dir_all(
                        handle
                            .path()
                            .app_data_dir()
                            .map(|d| d.join("Capi").join("logs"))
                            .unwrap_or_default(),
                    );
                    if let Ok(log_path) = handle
                        .path()
                        .app_data_dir()
                        .map(|d| d.join("Capi").join("logs").join("crash.log"))
                    {
                        let _ = std::fs::write(&log_path, &e);
                    }
                    let dialog_msg = format!(
                        "No se pudo iniciar Capi:\n\n{}\n\nRevise el archivo de log en:\n{}",
                        e,
                        handle
                            .path()
                            .app_data_dir()
                            .map(|d| d
                                .join("Capi")
                                .join("logs")
                                .join("crash.log")
                                .display()
                                .to_string())
                            .unwrap_or_default()
                    );
                    rfd::MessageDialog::new()
                        .set_title("Capi - Error al iniciar")
                        .set_description(&dialog_msg)
                        .set_level(rfd::MessageLevel::Error)
                        .show();
                }
                return Err(e.into());
            }

            // ─── System Tray ────────────────────────────────────────
            if let Some(icon) = handle.default_window_icon() {
                if let Ok(show) =
                    MenuItem::with_id(handle, "show", "Mostrar ventana", true, None::<&str>)
                {
                    if let Ok(new_window) = MenuItem::with_id(
                        handle,
                        "new-window",
                        "Abrir otra ventana",
                        true,
                        None::<&str>,
                    ) {
                        if let Ok(quit) =
                            MenuItem::with_id(handle, "quit", "Salir", true, None::<&str>)
                        {
                            if let Ok(menu) = Menu::with_items(handle, &[&show, &new_window, &quit])
                            {
                                let _ = TrayIconBuilder::new()
                                    .icon(icon.clone())
                                    .tooltip("Capi")
                                    .menu(&menu)
                                    .on_menu_event(|handle, event| match event.id.as_ref() {
                                        "show" => {
                                            if let Some(window) = handle.get_webview_window("main")
                                            {
                                                let _ = window.show();
                                                let _ = window.set_focus();
                                            }
                                        }
                                        "new-window" => {
                                            let _ = WebviewWindowBuilder::new(
                                                handle,
                                                format!(
                                                    "main-{}",
                                                    std::time::SystemTime::now()
                                                        .duration_since(std::time::UNIX_EPOCH)
                                                        .unwrap_or_default()
                                                        .as_nanos()
                                                ),
                                                tauri::WebviewUrl::App("index.html".into()),
                                            )
                                            .title("Capi")
                                            .inner_size(1000.0, 700.0)
                                            .build();
                                        }
                                        "quit" => handle.exit(0),
                                        _ => {}
                                    })
                                    .on_tray_icon_event(|tray, event| {
                                        if let TrayIconEvent::Click {
                                            button: MouseButton::Left,
                                            button_state: MouseButtonState::Up,
                                            ..
                                        } = event
                                        {
                                            let handle = tray.app_handle();
                                            if let Some(window) = handle.get_webview_window("main")
                                            {
                                                if window.is_visible().unwrap_or(false) {
                                                    let _ = window.hide();
                                                } else {
                                                    let _ = window.show();
                                                    let _ = window.set_focus();
                                                }
                                            }
                                        }
                                    })
                                    .build(handle);
                            }
                        }
                    }
                }
            }

            // ─── Start hidden (autostart) ──────────────────────────
            let args: Vec<String> = std::env::args().collect();
            if args.contains(&"--hidden".to_string()) {
                if let Some(window) = handle.get_webview_window("main") {
                    window.hide()?;
                }
            }

            #[cfg(debug_assertions)]
            {
                if let Some(window) = handle.get_webview_window("main") {
                    let _ = window.show();
                    let _ = window.set_focus();
                }
            }

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            buscar_cancion,
            buscar_cancion_progresivo,
            obtener_stream,
            obtener_playlist,
            descargar_cancion,
            borrar_cancion,
            obtener_home,
            obtener_artista,
            obtener_explorar,
            obtener_letras,
            obtener_nuevos_lanzamientos,
            obtener_sugerencias,
            obtener_local_port,
            obtener_info_sistema,
            discord_connect,
            discord_update,
            discord_disconnect,
            seleccionar_carpeta,
            listar_archivos_locales,
            abrir_carpeta_descargas,
            obtener_espacio_almacenamiento,
            media_update_state,
            media_get_state,
            descubrir_dispositivos_cast,
            transmitir_a_dispositivo,
            consolidar_carpetas_descargas,
            obtener_reproducciones,
            suscribirse_canal,
            like_cancion_ytm,
            obtener_estado_sesion_ytm,
            guardar_sesion_ytm,
            cerrar_sesion_ytm,
            sincronizar_ytm,
            abrir_login_ytm
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn primary_artist_extraction_and_sanitization() {
        assert_eq!(
            extract_primary_artist("Rawayana, Danny Ocean", "Binikini"),
            "Rawayana"
        );
        assert_eq!(
            extract_primary_artist("Rawayana, Carín León, Grupo Frontera", "Como De Sol A Sol"),
            "Rawayana"
        );
        assert_eq!(
            extract_primary_artist("Rawayana & Danny Ocean", "Binikini"),
            "Rawayana"
        );
        assert_eq!(
            extract_primary_artist("Rawayana feat. Danny Ocean", "Binikini"),
            "Rawayana"
        );
        assert_eq!(
            extract_primary_artist("Rawayana ft. Danny Ocean", "Binikini"),
            "Rawayana"
        );
        assert_eq!(
            extract_primary_artist("Rawayana / Danny Ocean", "Binikini"),
            "Rawayana"
        );
        assert_eq!(
            extract_primary_artist("Rawayana; Danny Ocean", "Binikini"),
            "Rawayana"
        );
        assert_eq!(
            extract_primary_artist("ELENA ROSE, Rawayana", "Luna de Miel"),
            "ELENA ROSE"
        );
        assert_eq!(
            extract_primary_artist("Artista Desconocido", "Queen - Bohemian Rhapsody"),
            "Queen"
        );
        assert_eq!(extract_primary_artist("", "Coldplay - Yellow"), "Coldplay");
        assert_eq!(extract_primary_artist("AC/DC", "Back In Black"), "AC_DC");
        assert_eq!(
            extract_primary_artist("Guns N' Roses", "Sweet Child O' Mine"),
            "Guns N' Roses"
        );
    }

    #[test]
    fn token_validation_requires_exact_match() {
        assert!(has_valid_token("/api/state?token=abc", "abc"));
        assert!(has_valid_token("/play?url=x&token=abc", "abc"));
        assert!(!has_valid_token("/api/state", "abc"));
        assert!(!has_valid_token("/api/state?token=abcd", "abc"));
        assert!(!has_valid_token("/api/state?token=abc&x=1", "abcd"));
    }

    #[test]
    fn parse_client_range_supports_full_and_partial_ranges() {
        assert_eq!(parse_client_range(None), (0, None, "bytes=0-".to_string()));
        assert_eq!(
            parse_client_range(Some("Range: bytes=0-")),
            (0, None, "bytes=0-".to_string())
        );
        assert_eq!(
            parse_client_range(Some("Range: bytes=1048576-")),
            (1_048_576, None, "bytes=1048576-".to_string())
        );
        assert_eq!(
            parse_client_range(Some("Range: bytes=0-999")),
            (0, Some(999), "bytes=0-999".to_string())
        );
        assert_eq!(
            parse_client_range(Some("Range: bytes=500-1500")),
            (500, Some(1500), "bytes=500-1500".to_string())
        );
        assert_eq!(
            parse_client_range(Some("bytes=200-100")),
            (0, None, "bytes=0-".to_string())
        );
    }

    #[test]
    fn play_allowlist_only_accepts_googlevideo_https() {
        assert!(is_allowed_play_url(
            "https://rr2---sn-uxjov8xo-h5gl.googlevideo.com/videoplayback?itag=140"
        ));
        assert!(!is_allowed_play_url(
            "http://rr2---sn-uxjov8xo-h5gl.googlevideo.com/videoplayback"
        ));
        assert!(!is_allowed_play_url("https://example.com/audio.mp4"));
        assert!(!is_allowed_play_url("https://googlevideo.com.evil.test/a"));
    }

    #[test]
    fn image_allowlist_only_accepts_youtube_image_hosts() {
        assert!(is_allowed_image_url(
            "https://yt3.googleusercontent.com/abc=w120-h120"
        ));
        assert!(is_allowed_image_url(
            "https://i.ytimg.com/vi/x/hqdefault.jpg"
        ));
        assert!(!is_allowed_image_url("https://example.com/image.jpg"));
    }

    #[test]
    fn local_roots_accept_new_roots_and_reject_others() {
        let roots = LocalRoots::default();
        let dir = std::env::temp_dir();
        let canonical = dir.canonicalize().unwrap();
        assert!(!roots.contains(&canonical));
        assert!(roots.allow(&canonical));
        assert!(roots.contains(&canonical));
    }

    #[test]
    fn find_audio_file_recursive_nested_10_folders_deep() {
        let temp_base = std::env::temp_dir().join(format!(
            "capi_test_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut deep_dir = temp_base.clone();
        for i in 1..=10 {
            deep_dir = deep_dir.join(format!("level_{}", i));
        }
        std::fs::create_dir_all(&deep_dir).unwrap();
        let test_song = deep_dir.join("The Weeknd_ Justice - Wake Me Up.m4a");
        std::fs::write(&test_song, b"dummy audio").unwrap();

        // 1. Find by exact filename
        let found = find_audio_file_recursive(
            &temp_base,
            Some(std::ffi::OsStr::new("The Weeknd_ Justice - Wake Me Up.m4a")),
            Some(std::ffi::OsStr::new("The Weeknd_ Justice - Wake Me Up")),
            None,
            None,
            0,
            15,
        );
        assert!(found.is_some());
        assert_eq!(
            found.unwrap().canonicalize().unwrap(),
            test_song.canonicalize().unwrap()
        );

        // 2. Find by stem match with different extension (e.g. .mp3 requested)
        let found_stem = find_audio_file_recursive(
            &temp_base,
            Some(std::ffi::OsStr::new("The Weeknd_ Justice - Wake Me Up.mp3")),
            Some(std::ffi::OsStr::new("The Weeknd_ Justice - Wake Me Up")),
            None,
            None,
            0,
            15,
        );
        assert!(found_stem.is_some());

        // 3. Find by title hint
        let found_title = find_audio_file_recursive(
            &temp_base,
            None,
            None,
            Some("Wake Me Up"),
            Some("The Weeknd"),
            0,
            15,
        );
        assert!(found_title.is_some());

        let _ = std::fs::remove_dir_all(&temp_base);
    }
}
