use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use reqwest::header::{
    HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE, COOKIE, ORIGIN, REFERER,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha1::{Digest, Sha1};
use tokio::sync::RwLock;

use crate::error::EngineError;
use crate::models::{Song, YtItem};

const YTM_CLIENT_NAME: &str = "67";
const YTM_CLIENT_VERSION: &str = "1.20241111.01.00";
const YTM_ORIGIN: &str = "https://music.youtube.com";
const YTM_USER_AGENT: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct MediaInfo {
    pub video_id: String,
    pub view_count: Option<u64>,
    pub likes: Option<u64>,
    pub dislikes: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct YtmSession {
    pub cookie: String,
    pub visitor_data: Option<String>,
    pub data_sync_id: Option<String>,
    pub account_name: Option<String>,
    pub account_email: Option<String>,
    pub account_photo: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct YtmAccountState {
    pub logged_in: bool,
    pub name: Option<String>,
    pub email: Option<String>,
    pub photo: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct YtmPlaylist {
    pub id: String,
    pub title: String,
    pub thumbnail: Option<String>,
    pub song_count: Option<u32>,
    pub is_editable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct YtmArtist {
    pub id: String,
    pub name: String,
    pub thumbnail: Option<String>,
    pub subscribers: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct YtmSyncData {
    pub liked_songs: Vec<Song>,
    pub playlists: Vec<YtmPlaylist>,
    pub artists: Vec<YtmArtist>,
    pub albums: Vec<YtItem>,
}

/// Gestor de sincronización y peticiones autenticadas a YouTube Music.
pub struct YtmClient {
    http: reqwest::Client,
    storage_file: PathBuf,
    session: RwLock<YtmSession>,
    media_cache: RwLock<HashMap<String, (MediaInfo, Instant)>>,
}

impl YtmClient {
    pub fn new(storage_dir: PathBuf) -> Self {
        let storage_file = storage_dir.join("ytm_session.json");
        let initial_session = if storage_file.exists() {
            std::fs::read_to_string(&storage_file)
                .ok()
                .and_then(|data| serde_json::from_str::<YtmSession>(&data).ok())
                .unwrap_or_default()
        } else {
            YtmSession::default()
        };

        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .user_agent(YTM_USER_AGENT)
            .build()
            .unwrap_or_default();

        Self {
            http,
            storage_file,
            session: RwLock::new(initial_session),
            media_cache: RwLock::new(HashMap::new()),
        }
    }

    pub async fn is_logged_in(&self) -> bool {
        let session = self.session.read().await;
        extract_sapisid(&session.cookie).is_some()
    }

    pub async fn get_account_state(&self) -> YtmAccountState {
        let session = self.session.read().await;
        let logged_in = extract_sapisid(&session.cookie).is_some();
        YtmAccountState {
            logged_in,
            name: session.account_name.clone(),
            email: session.account_email.clone(),
            photo: session.account_photo.clone(),
        }
    }

    pub async fn save_session(
        &self,
        cookie: String,
        visitor_data: Option<String>,
        data_sync_id: Option<String>,
    ) -> Result<YtmAccountState, EngineError> {
        {
            let mut session = self.session.write().await;
            session.cookie = cookie;
            if visitor_data.is_some() {
                session.visitor_data = visitor_data;
            }
            if data_sync_id.is_some() {
                session.data_sync_id = data_sync_id;
            }
        }
        self.persist_session().await?;

        // Intentar actualizar la info del perfil automáticamente
        let _ = self.refresh_account_info().await;
        Ok(self.get_account_state().await)
    }

    pub async fn clear_session(&self) -> Result<(), EngineError> {
        {
            let mut session = self.session.write().await;
            *session = YtmSession::default();
        }
        if self.storage_file.exists() {
            let _ = std::fs::remove_file(&self.storage_file);
        }
        Ok(())
    }

    async fn persist_session(&self) -> Result<(), EngineError> {
        let session = self.session.read().await;
        if let Ok(json) = serde_json::to_string_pretty(&*session) {
            let _ = std::fs::write(&self.storage_file, json);
        }
        Ok(())
    }

    // ─── Cabeceras y Peticiones InnerTube ─────────────────────────────

    fn build_headers(&self, session: &YtmSession) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert("X-Goog-Api-Format-Version", HeaderValue::from_static("1"));
        headers.insert(
            "X-YouTube-Client-Name",
            HeaderValue::from_static(YTM_CLIENT_NAME),
        );
        headers.insert(
            "X-YouTube-Client-Version",
            HeaderValue::from_static(YTM_CLIENT_VERSION),
        );
        headers.insert(ORIGIN, HeaderValue::from_static(YTM_ORIGIN));
        headers.insert(
            REFERER,
            HeaderValue::from_static("https://music.youtube.com/"),
        );

        if let Some(ref vd) = session.visitor_data {
            if let Ok(val) = HeaderValue::from_str(vd) {
                headers.insert("X-Goog-Visitor-Id", val);
            }
        }

        if let Some(sapisid) = extract_sapisid(&session.cookie) {
            if let Ok(cookie_val) = HeaderValue::from_str(&session.cookie) {
                headers.insert(COOKIE, cookie_val);
            }
            let (timestamp, hash) = generate_sapisid_hash(sapisid, YTM_ORIGIN);
            let auth_str = format!("SAPISIDHASH {}_{}", timestamp, hash);
            if let Ok(auth_val) = HeaderValue::from_str(&auth_str) {
                headers.insert(AUTHORIZATION, auth_val);
            }
        }

        headers
    }

    fn build_context_json(&self, session: &YtmSession) -> Value {
        let on_behalf_of = session.data_sync_id.as_deref().map(|id| {
            id.split("||").next().unwrap_or(id).trim()
        });
        serde_json::json!({
            "client": {
                "clientName": "WEB_REMIX",
                "clientVersion": YTM_CLIENT_VERSION,
                "hl": "es",
                "gl": "ES",
                "visitorData": session.visitor_data
            },
            "user": {
                "onBehalfOfUser": on_behalf_of,
                "dataSyncId": on_behalf_of,
                "lockedSafetyMode": false
            }
        })
    }

    // ─── Cantidad de Reproducciones (Media Info) ──────────────────────

    pub async fn get_media_info(&self, video_id: &str) -> Result<MediaInfo, EngineError> {
        let clean_id = if video_id.contains("v=") {
            video_id
                .split("v=")
                .nth(1)
                .and_then(|s| s.split('&').next())
                .unwrap_or(video_id)
        } else if video_id.contains('/') {
            video_id.split('/').next_back().unwrap_or(video_id)
        } else {
            video_id
        }
        .trim();

        if clean_id.is_empty()
            || clean_id.starts_with('/')
            || clean_id.contains('\\')
            || clean_id.starts_with("local:")
        {
            return Ok(MediaInfo {
                video_id: clean_id.to_string(),
                view_count: None,
                likes: None,
                dislikes: None,
            });
        }

        // Verificar caché en memoria (TTL de 10 minutos, sólo si tiene datos reales)
        {
            let cache = self.media_cache.read().await;
            if let Some((info, timestamp)) = cache.get(clean_id) {
                if timestamp.elapsed() < Duration::from_secs(600) && info.view_count.is_some() {
                    return Ok(info.clone());
                }
            }
        }

        // 1. Petición oficial a YouTube Music player API para el viewCount real
        let ytm_fut = async {
            let session = self.session.read().await;
            let body = serde_json::json!({
                "context": self.build_context_json(&session),
                "videoId": clean_id
            });
            let headers = self.build_headers(&session);

            let res = self
                .http
                .post("https://music.youtube.com/youtubei/v1/player?prettyPrint=false")
                .timeout(Duration::from_secs(5))
                .headers(headers)
                .json(&body)
                .send()
                .await;

            if let Ok(res) = res {
                if let Ok(json) = res.json::<Value>().await {
                    let view_str = json["videoDetails"]["viewCount"].as_str();
                    return view_str.and_then(|s| s.parse::<u64>().ok());
                }
            }
            None
        };

        // 2. Petición a ReturnYouTubeDislike para likes, dislikes y fallback de vistas
        let ryd_fut = async {
            let url = format!("https://returnyoutubedislikeapi.com/Votes?videoId={clean_id}");
            let res = self
                .http
                .get(&url)
                .timeout(Duration::from_secs(5))
                .send()
                .await;

            if let Ok(res) = res {
                if res.status().is_success() {
                    if let Ok(json) = res.json::<Value>().await {
                        let view_count = json["viewCount"].as_u64();
                        let likes = json["likes"].as_u64();
                        let dislikes = json["dislikes"].as_u64();
                        return (view_count, likes, dislikes);
                    }
                }
            }
            (None, None, None)
        };

        let (ytm_views, (ryd_views, likes, dislikes)) = tokio::join!(ytm_fut, ryd_fut);

        let view_count = ytm_views.or(ryd_views);

        let info = MediaInfo {
            video_id: clean_id.to_string(),
            view_count,
            likes,
            dislikes,
        };

        if info.view_count.is_some() {
            let mut cache = self.media_cache.write().await;
            cache.insert(clean_id.to_string(), (info.clone(), Instant::now()));
        }

        Ok(info)
    }

    // ─── Suscribirse / Desuscribirse a Canal ─────────────────────────

    pub async fn subscribe_channel(
        &self,
        channel_id: &str,
        subscribe: bool,
    ) -> Result<bool, EngineError> {
        let endpoint = if subscribe {
            "https://music.youtube.com/youtubei/v1/subscription/subscribe"
        } else {
            "https://music.youtube.com/youtubei/v1/subscription/unsubscribe"
        };

        let session = self.session.read().await;
        if extract_sapisid(&session.cookie).is_none() {
            return Err(EngineError::Unavailable(
                "Inicia sesión con YouTube Music para suscribirte a canales.".into(),
            ));
        }

        let body = serde_json::json!({
            "context": self.build_context_json(&session),
            "channelIds": [channel_id]
        });
        let headers = self.build_headers(&session);

        let response = self
            .http
            .post(endpoint)
            .headers(headers)
            .json(&body)
            .send()
            .await
            .map_err(|e| EngineError::Network(format!("Error al suscribirse: {e}")))?;

        Ok(response.status().is_success())
    }

    // ─── Like / Dislike / RemoveLike a Canción ────────────────────────

    pub async fn like_video(&self, video_id: &str, like: bool) -> Result<bool, EngineError> {
        let endpoint = if like {
            "https://music.youtube.com/youtubei/v1/like/like"
        } else {
            "https://music.youtube.com/youtubei/v1/like/removelike"
        };

        let session = self.session.read().await;
        if extract_sapisid(&session.cookie).is_none() {
            return Err(EngineError::Unavailable(
                "Inicia sesión con YouTube Music para dar Me Gusta.".into(),
            ));
        }

        let body = serde_json::json!({
            "context": self.build_context_json(&session),
            "target": {
                "videoId": video_id
            }
        });
        let headers = self.build_headers(&session);

        let response = self
            .http
            .post(endpoint)
            .headers(headers)
            .json(&body)
            .send()
            .await
            .map_err(|e| EngineError::Network(format!("Error al modificar like: {e}")))?;

        Ok(response.status().is_success())
    }

    // ─── Info de la Cuenta ───────────────────────────────────────────

    pub async fn refresh_account_info(&self) -> Result<YtmAccountState, EngineError> {
        let session = self.session.read().await;
        if extract_sapisid(&session.cookie).is_none() {
            return Ok(YtmAccountState {
                logged_in: false,
                name: None,
                email: None,
                photo: None,
            });
        }

        let body = serde_json::json!({
            "context": self.build_context_json(&session)
        });
        let headers = self.build_headers(&session);
        drop(session);

        let response = self
            .http
            .post("https://music.youtube.com/youtubei/v1/account/account_menu")
            .headers(headers)
            .json(&body)
            .send()
            .await
            .map_err(|e| EngineError::Network(format!("Error al obtener menú de cuenta: {e}")))?;

        if !response.status().is_success() {
            return Err(EngineError::Unavailable(format!(
                "YouTube devolvió HTTP {}",
                response.status()
            )));
        }

        let json: Value = response
            .json()
            .await
            .map_err(|e| EngineError::Network(e.to_string()))?;

        let header = json["actions"]
            .as_array()
            .and_then(|a| a.first())
            .and_then(|a| {
                a["openPopupAction"]["popup"]["multiPageMenuRenderer"]["header"]
                    ["activeAccountHeaderRenderer"]
                    .as_object()
            });

        if let Some(h) = header {
            let name = h
                .get("accountName")
                .and_then(|r| r["runs"].as_array())
                .and_then(|a| a.first())
                .and_then(|r| r["text"].as_str())
                .map(|s| s.to_string());

            let email = h
                .get("email")
                .and_then(|r| r["runs"].as_array())
                .and_then(|a| a.first())
                .and_then(|r| r["text"].as_str())
                .map(|s| s.to_string());

            let photo = h
                .get("accountPhoto")
                .and_then(|p| p["thumbnails"].as_array())
                .and_then(|t| t.last())
                .and_then(|th| th["url"].as_str())
                .map(|s| s.to_string());

            {
                let mut session = self.session.write().await;
                session.account_name = name.clone();
                session.account_email = email.clone();
                session.account_photo = photo.clone();
            }
            let _ = self.persist_session().await;

            return Ok(YtmAccountState {
                logged_in: true,
                name,
                email,
                photo,
            });
        }

        Ok(self.get_account_state().await)
    }

    // ─── Sincronización: Canciones Favoritas (Liked Music "LM") ───────

    pub async fn fetch_liked_songs(&self) -> Result<Vec<Song>, EngineError> {
        let session = self.session.read().await;
        if extract_sapisid(&session.cookie).is_none() {
            return Err(EngineError::Unavailable(
                "Inicia sesión para sincronizar tus canciones favoritas.".into(),
            ));
        }

        let context_json = self.build_context_json(&session);
        let headers = self.build_headers(&session);
        drop(session);

        let body = serde_json::json!({
            "context": context_json,
            "browseId": "LM"
        });

        let response = self
            .http
            .post("https://music.youtube.com/youtubei/v1/browse")
            .headers(headers.clone())
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                EngineError::Network(format!("Error al sincronizar canciones favoritas: {e}"))
            })?;

        let json: Value = response
            .json()
            .await
            .map_err(|e| EngineError::Network(e.to_string()))?;

        let mut songs = Vec::new();
        collect_songs_from_browse(&json, &mut songs);

        // Continuación para páginas adicionales de favoritos (hasta 15 páginas)
        let mut continuation = find_continuation_token(&json);
        let mut pages = 0;
        while let Some(token) = continuation {
            if pages >= 15 {
                break;
            }
            pages += 1;

            let cont_body = serde_json::json!({
                "context": context_json,
                "continuation": token
            });

            if let Ok(cont_resp) = self
                .http
                .post("https://music.youtube.com/youtubei/v1/browse")
                .headers(headers.clone())
                .json(&cont_body)
                .send()
                .await
            {
                if let Ok(cont_json) = cont_resp.json::<Value>().await {
                    let prev_count = songs.len();
                    collect_songs_from_browse(&cont_json, &mut songs);
                    continuation = find_continuation_token(&cont_json);
                    if songs.len() == prev_count {
                        break;
                    }
                } else {
                    break;
                }
            } else {
                break;
            }
        }

        Ok(songs)
    }

    // ─── Sincronización: Playlists del Usuario ─────────────────────────

    pub async fn fetch_saved_playlists(&self) -> Result<Vec<YtmPlaylist>, EngineError> {
        let session = self.session.read().await;
        if extract_sapisid(&session.cookie).is_none() {
            return Err(EngineError::Unavailable(
                "Inicia sesión para sincronizar tus playlists.".into(),
            ));
        }

        let context_json = self.build_context_json(&session);
        let headers = self.build_headers(&session);
        drop(session);

        let body = serde_json::json!({
            "context": context_json,
            "browseId": "FEmusic_liked_playlists"
        });

        let response = self
            .http
            .post("https://music.youtube.com/youtubei/v1/browse")
            .headers(headers.clone())
            .json(&body)
            .send()
            .await
            .map_err(|e| EngineError::Network(format!("Error al sincronizar playlists: {e}")))?;

        let json: Value = response
            .json()
            .await
            .map_err(|e| EngineError::Network(e.to_string()))?;

        let mut playlists = Vec::new();
        collect_playlists_from_browse(&json, &mut playlists);

        // Continuación para páginas adicionales de playlists
        let mut continuation = find_continuation_token(&json);
        let mut pages = 0;
        while let Some(token) = continuation {
            if pages >= 25 {
                break;
            }
            pages += 1;

            let cont_body = serde_json::json!({
                "context": context_json,
                "continuation": token
            });

            if let Ok(cont_resp) = self
                .http
                .post("https://music.youtube.com/youtubei/v1/browse")
                .headers(headers.clone())
                .json(&cont_body)
                .send()
                .await
            {
                if let Ok(cont_json) = cont_resp.json::<Value>().await {
                    let prev_count = playlists.len();
                    collect_playlists_from_browse(&cont_json, &mut playlists);
                    continuation = find_continuation_token(&cont_json);
                    if playlists.len() == prev_count {
                        break;
                    }
                } else {
                    break;
                }
            } else {
                break;
            }
        }

        // Fallback: si FEmusic_liked_playlists no devuelve listas, consultar FEmusic_library_landing
        if playlists.is_empty() {
            let body_landing = serde_json::json!({
                "context": context_json,
                "browseId": "FEmusic_library_landing"
            });
            if let Ok(resp_landing) = self
                .http
                .post("https://music.youtube.com/youtubei/v1/browse")
                .headers(headers.clone())
                .json(&body_landing)
                .send()
                .await
            {
                if let Ok(json_landing) = resp_landing.json::<Value>().await {
                    collect_playlists_from_browse(&json_landing, &mut playlists);
                }
            }
        }

        // Normalizar IDs y filtrar playlists duplicadas o no deseadas (LM / SE)
        playlists.retain(|p| p.id != "LM" && p.id != "SE" && !p.title.trim().is_empty());

        Ok(playlists)
    }

    // ─── Obtener canciones de una playlist autenticada ────────────────

    pub async fn fetch_playlist_tracks(&self, playlist_id: &str) -> Result<Vec<Song>, EngineError> {
        let session = self.session.read().await;
        if extract_sapisid(&session.cookie).is_none() {
            return Err(EngineError::Unavailable(
                "Inicia sesión para sincronizar la playlist con tu cuenta.".into(),
            ));
        }

        let clean_id = strip_vl(playlist_id);
        let browse_id = if clean_id.starts_with("VL") {
            clean_id.to_string()
        } else {
            format!("VL{}", clean_id)
        };

        let context_json = self.build_context_json(&session);
        let headers = self.build_headers(&session);
        drop(session);

        let body = serde_json::json!({
            "context": context_json,
            "browseId": browse_id
        });

        let response = self
            .http
            .post("https://music.youtube.com/youtubei/v1/browse")
            .headers(headers.clone())
            .json(&body)
            .send()
            .await
            .map_err(|e| EngineError::Network(format!("Error al obtener playlist autenticada: {e}")))?;

        let json: Value = response
            .json()
            .await
            .map_err(|e| EngineError::Network(e.to_string()))?;

        let mut songs = Vec::new();
        collect_songs_from_browse(&json, &mut songs);

        // Seguir continuaciones si la playlist tiene más de 100 canciones
        let mut continuation = find_continuation_token(&json);
        let mut pages = 0;
        while let Some(token) = continuation {
            if pages >= 25 {
                break;
            }
            pages += 1;

            let cont_body = serde_json::json!({
                "context": context_json,
                "continuation": token
            });

            if let Ok(cont_resp) = self
                .http
                .post("https://music.youtube.com/youtubei/v1/browse")
                .headers(headers.clone())
                .json(&cont_body)
                .send()
                .await
            {
                if let Ok(cont_json) = cont_resp.json::<Value>().await {
                    let prev_count = songs.len();
                    collect_songs_from_browse(&cont_json, &mut songs);
                    continuation = find_continuation_token(&cont_json);
                    if songs.len() == prev_count {
                        break;
                    }
                } else {
                    break;
                }
            } else {
                break;
            }
        }

        Ok(songs)
    }

    // ─── Sincronización: Artistas Suscritos ───────────────────────────

    pub async fn fetch_subscribed_artists(&self) -> Result<Vec<YtmArtist>, EngineError> {
        let session = self.session.read().await;
        if extract_sapisid(&session.cookie).is_none() {
            return Err(EngineError::Unavailable(
                "Inicia sesión para sincronizar tus artistas suscritos.".into(),
            ));
        }

        let body = serde_json::json!({
            "context": self.build_context_json(&session),
            "browseId": "FEmusic_library_corpus_artists"
        });
        let headers = self.build_headers(&session);
        drop(session);

        let response = self
            .http
            .post("https://music.youtube.com/youtubei/v1/browse")
            .headers(headers)
            .json(&body)
            .send()
            .await
            .map_err(|e| EngineError::Network(format!("Error al sincronizar artistas: {e}")))?;

        let json: Value = response
            .json()
            .await
            .map_err(|e| EngineError::Network(e.to_string()))?;

        let mut artists = Vec::new();
        collect_artists_from_browse(&json, &mut artists);
        Ok(artists)
    }

    // ─── Sincronización Completa ─────────────────────────────────────

    pub async fn full_sync(&self) -> Result<YtmSyncData, EngineError> {
        let liked_songs = self.fetch_liked_songs().await.unwrap_or_default();
        let playlists = self.fetch_saved_playlists().await.unwrap_or_default();
        let artists = self.fetch_subscribed_artists().await.unwrap_or_default();

        Ok(YtmSyncData {
            liked_songs,
            playlists,
            artists,
            albums: Vec::new(),
        })
    }
}

// ─── Helpers de Hash y Parsing ────────────────────────────────────────

pub fn strip_vl(id: &str) -> &str {
    id.strip_prefix("VL").unwrap_or(id)
}

fn find_continuation_token(val: &Value) -> Option<String> {
    if let Value::Object(map) = val {
        if let Some(next_c) = map.get("nextContinuationData") {
            if let Some(token) = next_c["continuation"].as_str() {
                return Some(token.to_string());
            }
        }
        if let Some(reload_c) = map.get("reloadContinuationData") {
            if let Some(token) = reload_c["continuation"].as_str() {
                return Some(token.to_string());
            }
        }
        for (_, v) in map {
            if let Some(token) = find_continuation_token(v) {
                return Some(token);
            }
        }
    } else if let Value::Array(arr) = val {
        for v in arr {
            if let Some(token) = find_continuation_token(v) {
                return Some(token);
            }
        }
    }
    None
}

pub fn extract_sapisid(cookie: &str) -> Option<&str> {
    let mut fallback = None;
    for part in cookie.split(';') {
        let part = part.trim();
        if let Some((k, v)) = part.split_once('=') {
            let key = k.trim();
            if key == "SAPISID" {
                return Some(v.trim());
            } else if key == "__Secure-3PAPISID" || key == "__Secure-1PAPISID" {
                fallback = Some(v.trim());
            }
        }
    }
    fallback
}

pub fn generate_sapisid_hash(sapisid: &str, origin: &str) -> (u64, String) {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let mut hasher = Sha1::new();
    hasher.update(format!("{timestamp} {sapisid} {origin}").as_bytes());
    let hash = format!("{:x}", hasher.finalize());
    (timestamp, hash)
}

fn parse_duration_str(s: &str) -> Option<u32> {
    let parts: Vec<&str> = s.split(':').collect();
    match parts.len() {
        2 => {
            let m: u32 = parts[0].trim().parse().ok()?;
            let s: u32 = parts[1].trim().parse().ok()?;
            Some(m * 60 + s)
        }
        3 => {
            let h: u32 = parts[0].trim().parse().ok()?;
            let m: u32 = parts[1].trim().parse().ok()?;
            let s: u32 = parts[2].trim().parse().ok()?;
            Some(h * 3600 + m * 60 + s)
        }
        _ => None,
    }
}

fn collect_songs_from_browse(val: &Value, songs: &mut Vec<Song>) {
    if let Value::Object(map) = val {
        if let Some(renderer) = map.get("musicResponsiveListItemRenderer") {
            if let Some(song) = parse_song_renderer(renderer) {
                if !songs.iter().any(|s| s.id == song.id) {
                    songs.push(song);
                }
            }
            return;
        }

        for (_, v) in map {
            collect_songs_from_browse(v, songs);
        }
    } else if let Value::Array(arr) = val {
        for v in arr {
            collect_songs_from_browse(v, songs);
        }
    }
}

fn parse_song_renderer(r: &Value) -> Option<Song> {
    let id = r["playlistItemData"]["videoId"]
        .as_str()
        .or_else(|| {
            r["overlay"]["musicItemThumbnailOverlayRenderer"]["content"]["musicPlayButtonRenderer"]
                ["playNavigationEndpoint"]["watchEndpoint"]["videoId"]
                .as_str()
        })?
        .to_string();

    let flex = r["flexColumns"].as_array()?;
    let col0 = flex.first()?;
    let title = col0["musicResponsiveListItemFlexColumnRenderer"]["text"]["runs"]
        .as_array()?
        .first()?["text"]
        .as_str()?
        .to_string();

    let mut artist = String::new();
    let mut album = None;
    let mut artist_id = None;

    if let Some(col1) = flex.get(1) {
        if let Some(runs) =
            col1["musicResponsiveListItemFlexColumnRenderer"]["text"]["runs"].as_array()
        {
            if let Some(first_run) = runs.first() {
                if let Some(t) = first_run["text"].as_str() {
                    artist = t.to_string();
                }
                artist_id = first_run["navigationEndpoint"]["browseEndpoint"]["browseId"]
                    .as_str()
                    .map(|s| s.to_string());
            }
            if runs.len() >= 3 {
                if let Some(alb_run) = runs.get(2) {
                    album = alb_run["text"].as_str().map(|s| s.to_string());
                }
            }
        }
    }

    let duration = r["fixedColumns"]
        .as_array()
        .and_then(|f| f.first())
        .and_then(|c| c["musicResponsiveListItemFlexColumnRenderer"]["text"]["runs"].as_array())
        .and_then(|runs| runs.first())
        .and_then(|r| r["text"].as_str())
        .and_then(parse_duration_str);

    let thumbnail = r["thumbnail"]["musicThumbnailRenderer"]["thumbnail"]["thumbnails"]
        .as_array()
        .and_then(|arr| arr.last())
        .and_then(|th| th["url"].as_str())
        .map(|s| s.to_string());

    let explicit = r["badges"]
        .as_array()
        .map(|b| {
            b.iter().any(|badge| {
                badge["musicInlineBadgeRenderer"]["icon"]["iconType"].as_str()
                    == Some("MUSIC_EXPLICIT_BADGE")
            })
        })
        .unwrap_or(false);

    Some(Song {
        id,
        title,
        artist,
        primary_artist: None,
        album,
        duration,
        thumbnail,
        explicit,
        artist_id,
    })
}

fn collect_playlists_from_browse(val: &Value, playlists: &mut Vec<YtmPlaylist>) {
    if let Value::Object(map) = val {
        if let Some(renderer) = map.get("musicTwoRowItemRenderer") {
            if let Some(pl) = parse_playlist_two_row(renderer) {
                if !playlists.iter().any(|p| p.id == pl.id) {
                    playlists.push(pl);
                }
            }
            return;
        }

        if let Some(renderer) = map.get("musicResponsiveListItemRenderer") {
            if let Some(pl) = parse_playlist_responsive(renderer) {
                if !playlists.iter().any(|p| p.id == pl.id) {
                    playlists.push(pl);
                }
            }
            return;
        }

        for (_, v) in map {
            collect_playlists_from_browse(v, playlists);
        }
    } else if let Value::Array(arr) = val {
        for v in arr {
            collect_playlists_from_browse(v, playlists);
        }
    }
}

fn parse_playlist_two_row(r: &Value) -> Option<YtmPlaylist> {
    let raw_browse_id = r["navigationEndpoint"]["browseEndpoint"]["browseId"]
        .as_str()
        .or_else(|| r["navigationEndpoint"]["watchEndpoint"]["playlistId"].as_str())?
        .to_string();

    let clean_id = strip_vl(&raw_browse_id).to_string();
    if clean_id == "LM" || clean_id == "SE" {
        return None;
    }

    let title = r["title"]["runs"]
        .as_array()
        .and_then(|a| a.first())
        .and_then(|t| t["text"].as_str())
        .or_else(|| r["title"]["simpleText"].as_str())?
        .to_string();

    let thumbnail = r["thumbnailRenderer"]["musicThumbnailRenderer"]["thumbnail"]["thumbnails"]
        .as_array()
        .or_else(|| r["thumbnail"]["musicThumbnailRenderer"]["thumbnail"]["thumbnails"].as_array())
        .and_then(|arr| arr.last())
        .and_then(|th| th["url"].as_str())
        .map(|s| s.to_string());

    let song_count = r["subtitle"]["runs"].as_array().and_then(|runs| {
        for run in runs {
            if let Some(text) = run["text"].as_str() {
                let digits: String = text.chars().filter(|c| c.is_ascii_digit()).collect();
                if let Ok(count) = digits.parse::<u32>() {
                    return Some(count);
                }
            }
        }
        None
    });

    Some(YtmPlaylist {
        id: clean_id,
        title,
        thumbnail,
        song_count,
        is_editable: true,
    })
}

fn parse_playlist_responsive(r: &Value) -> Option<YtmPlaylist> {
    let raw_browse_id = r["navigationEndpoint"]["browseEndpoint"]["browseId"]
        .as_str()
        .or_else(|| r["playlistItemData"]["playlistId"].as_str())
        .or_else(|| r["navigationEndpoint"]["watchEndpoint"]["playlistId"].as_str())?
        .to_string();

    let clean_id = strip_vl(&raw_browse_id).to_string();
    if clean_id == "LM" || clean_id == "SE" {
        return None;
    }

    if !raw_browse_id.starts_with("VL") && !clean_id.starts_with("PL") && !clean_id.starts_with("RD")
    {
        return None;
    }

    let flex = r["flexColumns"].as_array()?;
    let col0 = flex.first()?;
    let title = col0["musicResponsiveListItemFlexColumnRenderer"]["text"]["runs"]
        .as_array()
        .and_then(|a| a.first())
        .and_then(|t| t["text"].as_str())
        .or_else(|| {
            col0["musicResponsiveListItemFlexColumnRenderer"]["text"]["simpleText"].as_str()
        })?
        .to_string();

    let thumbnail = r["thumbnail"]["musicThumbnailRenderer"]["thumbnail"]["thumbnails"]
        .as_array()
        .or_else(|| {
            r["thumbnailRenderer"]["musicThumbnailRenderer"]["thumbnail"]["thumbnails"].as_array()
        })
        .and_then(|arr| arr.last())
        .and_then(|th| th["url"].as_str())
        .map(|s| s.to_string());

    let song_count = flex.get(1).and_then(|col| {
        col["musicResponsiveListItemFlexColumnRenderer"]["text"]["runs"]
            .as_array()
            .and_then(|runs| {
                for run in runs {
                    if let Some(text) = run["text"].as_str() {
                        let digits: String = text.chars().filter(|c| c.is_ascii_digit()).collect();
                        if let Ok(count) = digits.parse::<u32>() {
                            return Some(count);
                        }
                    }
                }
                None
            })
    });

    Some(YtmPlaylist {
        id: clean_id,
        title,
        thumbnail,
        song_count,
        is_editable: true,
    })
}

fn collect_artists_from_browse(val: &Value, artists: &mut Vec<YtmArtist>) {
    if let Value::Object(map) = val {
        if let Some(renderer) = map.get("musicTwoRowItemRenderer") {
            if let Some(art) = parse_artist_two_row(renderer) {
                if !artists.iter().any(|a| a.id == art.id) {
                    artists.push(art);
                }
            }
            return;
        }

        for (_, v) in map {
            collect_artists_from_browse(v, artists);
        }
    } else if let Value::Array(arr) = val {
        for v in arr {
            collect_artists_from_browse(v, artists);
        }
    }
}

fn parse_artist_two_row(r: &Value) -> Option<YtmArtist> {
    let browse_id = r["navigationEndpoint"]["browseEndpoint"]["browseId"]
        .as_str()?
        .to_string();

    let name = r["title"]["runs"].as_array()?.first()?["text"]
        .as_str()?
        .to_string();

    let thumbnail = r["thumbnailRenderer"]["musicThumbnailRenderer"]["thumbnail"]["thumbnails"]
        .as_array()
        .and_then(|arr| arr.last())
        .and_then(|th| th["url"].as_str())
        .map(|s| s.to_string());

    let subscribers = r["subtitle"]["runs"]
        .as_array()
        .and_then(|runs| runs.first())
        .and_then(|r| r["text"].as_str())
        .map(|s| s.to_string());

    Some(YtmArtist {
        id: browse_id,
        name,
        thumbnail,
        subscribers,
    })
}
