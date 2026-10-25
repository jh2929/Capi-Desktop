use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rustypipe::client::RustyPipe;
use rustypipe::model::{AlbumItem, ArtistItem, MusicPlaylistItem, Thumbnail, TrackItem};
use serde_json::Value;
use tokio::sync::RwLock;

use crate::error::EngineError;
use crate::lyrics;
use crate::models::{
    ArtistPage, ArtistSection, ExploreResponse, HomeResponse, HomeSection, LyricsResult, MoodGenre,
    Song, StreamInfo, YtItem,
};

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(12);
const VISIONOS_USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 15_7_3) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/26.0 Safari/605.1.15";

struct CachedStream {
    info: StreamInfo,
}

struct CachedLyrics {
    result: LyricsResult,
    cached_at: i64,
}

struct CachedSearch {
    songs: Vec<Song>,
    cached_at: i64,
}

/// Motor de Capi. Es `Send + Sync` y se comparte como `Arc<Engine>` desde Tauri.
pub struct Engine {
    rp: RustyPipe,
    http: reqwest::Client,
    timeout: Duration,
    visitor_data: RwLock<Option<String>>,
    streams: RwLock<HashMap<String, CachedStream>>,
    lyrics: RwLock<HashMap<String, CachedLyrics>>,
    searches: RwLock<HashMap<String, CachedSearch>>,
}

impl Engine {
    pub fn new(storage_dir: PathBuf) -> Result<Self, EngineError> {
        std::fs::create_dir_all(&storage_dir).map_err(|e| {
            EngineError::Internal(format!("no se pudo crear el directorio de caché: {e}"))
        })?;

        let rp = RustyPipe::builder()
            .storage_dir(storage_dir)
            .no_botguard()
            .timeout(DEFAULT_TIMEOUT)
            .n_http_retries(1)
            .build()?;

        let http = reqwest::Client::builder()
            .user_agent(VISIONOS_USER_AGENT)
            .timeout(DEFAULT_TIMEOUT)
            .build()
            .map_err(|e| EngineError::Internal(format!("no se pudo crear el cliente HTTP: {e}")))?;

        Ok(Self {
            rp,
            http,
            timeout: DEFAULT_TIMEOUT,
            visitor_data: RwLock::new(None),
            streams: RwLock::new(HashMap::new()),
            lyrics: RwLock::new(HashMap::new()),
            searches: RwLock::new(HashMap::new()),
        })
    }

    // ─── Catálogo ─────────────────────────────────────────────────────

    pub async fn search(&self, query: &str) -> Result<Vec<Song>, EngineError> {
        let query = query.trim();
        if query.is_empty() {
            return Ok(Vec::new());
        }
        let key = query.to_lowercase();
        if let Some(cached) = self.searches.read().await.get(&key) {
            if now_unix() - cached.cached_at < 5 * 60 {
                return Ok(cached.songs.clone());
            }
        }

        let result = self
            .with_timeout(self.rp.query().music_search_tracks(query))
            .await?;
        let songs: Vec<Song> = result.items.items.iter().map(song_from_track).collect();

        {
            let mut cache = self.searches.write().await;
            if cache.len() >= 64 {
                cache.clear();
            }
            cache.insert(
                key,
                CachedSearch {
                    songs: songs.clone(),
                    cached_at: now_unix(),
                },
            );
        }

        Ok(songs)
    }

    pub async fn album_tracks(&self, album_id: &str) -> Result<Vec<Song>, EngineError> {
        let album = self
            .with_timeout(self.rp.query().music_album(strip_vl(album_id)))
            .await?;
        Ok(album.tracks.iter().map(song_from_track).collect())
    }

    /// Acepta IDs de playlist (`PL…`, `VLPL…`, `RDCLAK…`) o de álbum (`MPREb…`).
    pub async fn playlist_tracks(&self, id: &str) -> Result<Vec<Song>, EngineError> {
        let id = strip_vl(id);
        if id.starts_with("MPREb") {
            return self.album_tracks(id).await;
        }
        let playlist = self
            .with_timeout(self.rp.query().music_playlist(id))
            .await?;
        Ok(playlist.tracks.items.iter().map(song_from_track).collect())
    }

    pub async fn search_playlists(&self, query: &str) -> Result<Vec<YtItem>, EngineError> {
        if query.trim().is_empty() {
            return Ok(Vec::new());
        }
        let result = self
            .with_timeout(self.rp.query().music_search_playlists(query, false))
            .await?;
        Ok(result.items.items.iter().map(yt_from_playlist).collect())
    }

    pub async fn artist(&self, artist_id: &str) -> Result<ArtistPage, EngineError> {
        let artist = self
            .with_timeout(self.rp.query().music_artist(artist_id, false))
            .await?;

        let mut sections = Vec::new();
        if !artist.tracks.is_empty() {
            sections.push(ArtistSection {
                title: "Canciones populares".into(),
                items: artist.tracks.iter().take(12).map(yt_from_track).collect(),
                browse_id: None,
                params: None,
            });
        }
        if !artist.albums.is_empty() {
            sections.push(ArtistSection {
                title: "Álbumes".into(),
                items: artist.albums.iter().take(20).map(yt_from_album).collect(),
                browse_id: None,
                params: None,
            });
        }
        if !artist.playlists.is_empty() {
            sections.push(ArtistSection {
                title: "Playlists".into(),
                items: artist
                    .playlists
                    .iter()
                    .take(12)
                    .map(yt_from_playlist)
                    .collect(),
                browse_id: None,
                params: None,
            });
        }
        if !artist.similar_artists.is_empty() {
            sections.push(ArtistSection {
                title: "Artistas similares".into(),
                items: artist
                    .similar_artists
                    .iter()
                    .take(12)
                    .map(yt_from_artist)
                    .collect(),
                browse_id: None,
                params: None,
            });
        }

        Ok(ArtistPage {
            id: artist.id,
            name: artist.name,
            thumbnail: best_thumbnail(&artist.header_image),
            description: artist.description,
            sections,
        })
    }

    pub async fn new_releases(&self) -> Result<Vec<YtItem>, EngineError> {
        let albums = self
            .with_timeout(self.rp.query().music_new_albums())
            .await?;
        Ok(albums.iter().take(30).map(yt_from_album).collect())
    }

    pub async fn explore(&self) -> Result<ExploreResponse, EngineError> {
        let albums = self
            .with_timeout(self.rp.query().music_new_albums())
            .await?;
        let genres = self.with_timeout(self.rp.query().music_genres()).await?;

        Ok(ExploreResponse {
            new_release_albums: albums.iter().take(30).map(yt_from_album).collect(),
            mood_and_genres: genres
                .iter()
                .map(|g| MoodGenre {
                    title: g.name.clone(),
                    stripe_color: i64::from(g.color),
                    browse_id: g.id.clone(),
                    params: None,
                })
                .collect(),
        })
    }

    pub async fn suggestions(&self, query: &str) -> Result<Vec<String>, EngineError> {
        if query.trim().is_empty() {
            return Ok(Vec::new());
        }
        let result = self
            .with_timeout(self.rp.query().music_search_suggestion(query))
            .await?;
        Ok(result.terms)
    }

    /// Home v1: se compone con novedades. El frontend mantiene sus capas de
    /// recomendaciones locales y búsquedas de respaldo.
    pub async fn home(&self) -> Result<HomeResponse, EngineError> {
        let albums = self
            .with_timeout(self.rp.query().music_new_albums())
            .await?;
        let sections = if albums.is_empty() {
            Vec::new()
        } else {
            vec![HomeSection {
                title: "Novedades".into(),
                label: None,
                thumbnail: None,
                params: None,
                items: albums.iter().take(15).map(yt_from_album).collect(),
            }]
        };
        Ok(HomeResponse {
            sections,
            continuation: None,
        })
    }

    // ─── Streaming ────────────────────────────────────────────────────

    /// Resuelve una URL de audio reproducible.
    ///
    /// Se usa el cliente **VISIONOS** de YouTube, que no requiere PO token y
    /// devuelve URLs que aceptan rangos HTTP completos (el cliente IOS limita
    /// el acceso a ~1 MiB, lo que rompe la reproducción en WebKit/GStreamer).
    pub async fn stream(&self, video_id: &str) -> Result<StreamInfo, EngineError> {
        if let Some(cached) = self.streams.read().await.get(video_id) {
            if cached.info.expires_at > now_unix() + 60 {
                return Ok(cached.info.clone());
            }
        }

        let visitor_data = self.visitor_data(false).await?;
        let info = self.fetch_visionos_stream(video_id, &visitor_data).await?;

        self.streams
            .write()
            .await
            .insert(video_id.to_string(), CachedStream { info: info.clone() });

        Ok(info)
    }

    async fn visitor_data(&self, force_new: bool) -> Result<String, EngineError> {
        if !force_new {
            if let Some(cached) = self.visitor_data.read().await.clone() {
                return Ok(cached);
            }
        }
        let visitor = self
            .rp
            .query()
            .get_visitor_data(force_new)
            .await
            .map_err(|e| EngineError::Network(format!("no se pudo obtener visitorData: {e}")))?;
        *self.visitor_data.write().await = Some(visitor.clone());
        Ok(visitor)
    }

    async fn fetch_visionos_stream(
        &self,
        video_id: &str,
        visitor_data: &str,
    ) -> Result<StreamInfo, EngineError> {
        let body = serde_json::json!({
            "context": {
                "client": {
                    "clientName": "VISIONOS",
                    "clientVersion": "1.02",
                    "deviceMake": "Apple",
                    "deviceModel": "RealityDevice17,1",
                    "userAgent": VISIONOS_USER_AGENT,
                    "osName": "visionOS",
                    "osVersion": "26.5.23O471",
                    "hl": "en",
                    "timeZone": "UTC",
                    "utcOffsetMinutes": 0
                }
            },
            "videoId": video_id,
            "contentCheckOk": true,
            "racyCheckOk": true
        });

        let response = self
            .http
            .post("https://www.youtube.com/youtubei/v1/player?prettyPrint=false")
            .header("X-Youtube-Client-Name", "101")
            .header("X-Youtube-Client-Version", "1.02")
            .header("X-Goog-Visitor-Id", visitor_data)
            .header("Origin", "https://www.youtube.com")
            .json(&body)
            .send()
            .await?;

        if !response.status().is_success() {
            return Err(EngineError::Network(format!(
                "YouTube player respondió HTTP {}",
                response.status().as_u16()
            )));
        }

        let json: Value = response.json().await?;
        select_visionos_stream(&json)
    }

    pub async fn invalidate_stream(&self, video_id: &str) {
        self.streams.write().await.remove(video_id);
    }

    // ─── Letras ───────────────────────────────────────────────────────

    pub async fn lyrics(
        &self,
        video_id: &str,
        artist: &str,
        title: &str,
        duration: u32,
        provider: Option<&str>,
    ) -> Result<LyricsResult, EngineError> {
        if let Some(cached) = self.lyrics.read().await.get(video_id) {
            if now_unix() - cached.cached_at < 24 * 60 * 60 {
                return Ok(cached.result.clone());
            }
        }

        let result = lyrics::fetch(
            &self.rp,
            video_id,
            artist,
            title,
            duration,
            provider,
            self.timeout,
        )
        .await?;

        self.lyrics.write().await.insert(
            video_id.to_string(),
            CachedLyrics {
                result: result.clone(),
                cached_at: now_unix(),
            },
        );

        Ok(result)
    }

    // ─── Helpers ──────────────────────────────────────────────────────

    async fn with_timeout<F, T>(&self, future: F) -> Result<T, EngineError>
    where
        F: std::future::Future<Output = Result<T, rustypipe::error::Error>>,
    {
        match tokio::time::timeout(self.timeout, future).await {
            Ok(result) => result.map_err(EngineError::from),
            Err(_) => Err(EngineError::Network(format!(
                "YouTube no respondió en {} s",
                self.timeout.as_secs()
            ))),
        }
    }
}

fn strip_vl(id: &str) -> &str {
    id.strip_prefix("VL").unwrap_or(id)
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

pub(crate) fn best_thumbnail(thumbs: &[Thumbnail]) -> Option<String> {
    thumbs
        .iter()
        .max_by_key(|t| u64::from(t.width) * u64::from(t.height))
        .map(|t| upgrade_thumbnail(&t.url))
}

/// YouTube sirve portadas en varios tamaños mediante la query `=w…-h…`.
/// Se normaliza a 544 px (el mayor tamaño estable de YT Music) y se cambian
/// los `*default.jpg` de i.ytimg por `maxresdefault.jpg`.
fn upgrade_thumbnail(url: &str) -> String {
    let mut result = url.to_string();

    if url.contains("googleusercontent.com") || url.contains("ggpht.com") {
        if let Some(eq) = result.rfind('=') {
            let suffix = result[eq + 1..].to_string();
            if suffix.starts_with('w') {
                if let Some(hpos) = suffix.find("-h") {
                    let width = suffix[1..hpos].parse::<u32>().unwrap_or(544).max(544);
                    let rest = &suffix[hpos + 2..];
                    let h_end = rest.find('-').unwrap_or(rest.len());
                    let height = rest[..h_end].parse::<u32>().unwrap_or(544).max(544);
                    let tail = &rest[h_end..];
                    result = format!("{}w{}-h{}{}", &result[..eq + 1], width, height, tail);
                }
            } else if suffix.starts_with('s') {
                let end = suffix.find('-').unwrap_or(suffix.len());
                let size = suffix[1..end].parse::<u32>().unwrap_or(544).max(544);
                result = format!("{}=s{}{}", &result[..eq], size, &suffix[end..]);
            }
        }
    }

    if url.contains("ytimg.com") {
        for size in ["default", "mqdefault", "hqdefault", "sddefault"] {
            result = result.replace(&format!("/{size}.jpg"), "/maxresdefault.jpg");
        }
    }

    result
}

fn select_visionos_stream(json: &Value) -> Result<StreamInfo, EngineError> {
    let status = json
        .pointer("/playabilityStatus/status")
        .and_then(Value::as_str)
        .unwrap_or("UNKNOWN");
    if status != "OK" {
        let reason = json
            .pointer("/playabilityStatus/reason")
            .and_then(Value::as_str)
            .unwrap_or("sin motivo");
        return Err(EngineError::Unavailable(format!(
            "YouTube no permite reproducir el vídeo ({status}: {reason})"
        )));
    }

    let formats = json
        .pointer("/streamingData/adaptiveFormats")
        .and_then(Value::as_array)
        .ok_or_else(|| EngineError::Unavailable("YouTube no devolvió formatos de audio".into()))?;

    let best = formats
        .iter()
        .filter(|format| {
            format
                .get("mimeType")
                .and_then(Value::as_str)
                .is_some_and(|mime| mime.starts_with("audio/mp4"))
        })
        .max_by_key(|format| format.get("bitrate").and_then(Value::as_u64).unwrap_or(0))
        .or_else(|| {
            formats.iter().find(|format| {
                format
                    .get("mimeType")
                    .and_then(Value::as_str)
                    .is_some_and(|mime| mime.starts_with("audio/"))
            })
        })
        .ok_or_else(|| EngineError::Unavailable("no hay pistas de audio disponibles".into()))?;

    let url = best
        .get("url")
        .and_then(Value::as_str)
        .ok_or_else(|| EngineError::Unavailable("YouTube devolvió un formato sin URL".into()))?;
    let mime = best
        .get("mimeType")
        .and_then(Value::as_str)
        .unwrap_or("audio/mp4")
        .to_string();
    let expires_at = url
        .parse::<reqwest::Url>()
        .ok()
        .and_then(|parsed| {
            parsed
                .query_pairs()
                .find(|(key, _)| key == "expire")
                .and_then(|(_, value)| value.parse::<i64>().ok())
        })
        .unwrap_or_else(|| now_unix() + 6 * 60 * 60);

    Ok(StreamInfo {
        url: url.to_string(),
        expires_at,
        bitrate: best.get("bitrate").and_then(Value::as_u64).unwrap_or(0) as u32,
        itag: best.get("itag").and_then(Value::as_u64).unwrap_or(0) as u32,
        codec: if mime.contains("mp4") {
            "mp4a".into()
        } else if mime.contains("opus") {
            "opus".into()
        } else {
            "unknown".into()
        },
        mime,
    })
}

pub(crate) fn song_from_track(track: &TrackItem) -> Song {
    let artist = if track.artists.is_empty() {
        "Artista Desconocido".to_string()
    } else {
        track
            .artists
            .iter()
            .map(|a| a.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    };
    let artist_id = track
        .artist_id
        .clone()
        .or_else(|| track.artists.iter().find_map(|a| a.id.clone()));

    Song {
        id: track.id.clone(),
        title: track.name.clone(),
        artist,
        album: track.album.as_ref().map(|a| a.name.clone()),
        duration: track.duration,
        thumbnail: best_thumbnail(&track.cover).or_else(|| {
            Some(upgrade_thumbnail(&format!(
                "https://i.ytimg.com/vi/{}/hqdefault.jpg",
                track.id
            )))
        }),
        explicit: false,
        artist_id,
    }
}

pub(crate) fn yt_from_album(album: &AlbumItem) -> YtItem {
    YtItem {
        item_type: "album".into(),
        id: album.id.clone(),
        title: album.name.clone(),
        thumbnail: best_thumbnail(&album.cover),
        explicit: false,
        artists: Some(album.artists.iter().map(|a| a.name.clone()).collect()),
        album: Some(album.name.clone()),
        duration: None,
        year: album.year,
        playlist_id: None,
        browse_id: Some(album.id.clone()),
        author: None,
    }
}

pub(crate) fn yt_from_playlist(playlist: &MusicPlaylistItem) -> YtItem {
    YtItem {
        item_type: "playlist".into(),
        id: playlist.id.clone(),
        title: playlist.name.clone(),
        thumbnail: best_thumbnail(&playlist.thumbnail),
        explicit: false,
        artists: playlist.channel.as_ref().map(|c| vec![c.name.clone()]),
        album: None,
        duration: None,
        year: None,
        playlist_id: Some(playlist.id.clone()),
        browse_id: None,
        author: playlist.channel.as_ref().map(|c| c.name.clone()),
    }
}

pub(crate) fn yt_from_artist(artist: &ArtistItem) -> YtItem {
    YtItem {
        item_type: "artist".into(),
        id: artist.id.clone(),
        title: artist.name.clone(),
        thumbnail: best_thumbnail(&artist.avatar),
        explicit: false,
        artists: Some(vec![artist.name.clone()]),
        album: None,
        duration: None,
        year: None,
        playlist_id: None,
        browse_id: Some(artist.id.clone()),
        author: None,
    }
}

pub(crate) fn yt_from_track(track: &TrackItem) -> YtItem {
    YtItem {
        item_type: "song".into(),
        id: track.id.clone(),
        title: track.name.clone(),
        thumbnail: best_thumbnail(&track.cover),
        explicit: false,
        artists: Some(track.artists.iter().map(|a| a.name.clone()).collect()),
        album: track.album.as_ref().map(|a| a.name.clone()),
        duration: track.duration,
        year: None,
        playlist_id: None,
        browse_id: None,
        author: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engine_es_send_y_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Engine>();
    }

    #[test]
    fn visionos_prefiere_aac_y_lee_la_expiracion() {
        let json = serde_json::json!({
            "playabilityStatus": { "status": "OK" },
            "streamingData": { "adaptiveFormats": [
                { "itag": 251, "mimeType": "audio/webm; codecs=\"opus\"", "bitrate": 165000,
                  "url": "https://rr1---sn-x.googlevideo.com/videoplayback?expire=1700000000" },
                { "itag": 140, "mimeType": "audio/mp4; codecs=\"mp4a.40.2\"", "bitrate": 131000,
                  "url": "https://rr1---sn-x.googlevideo.com/videoplayback?expire=1800000000" }
            ]}
        });
        let stream = select_visionos_stream(&json).expect("stream");
        assert_eq!(stream.itag, 140);
        assert_eq!(stream.codec, "mp4a");
        assert_eq!(stream.expires_at, 1_800_000_000);
    }

    #[test]
    fn visionos_reporta_playability_no_disponible() {
        let json = serde_json::json!({
            "playabilityStatus": { "status": "LOGIN_REQUIRED", "reason": "bot" }
        });
        let error = select_visionos_stream(&json).expect_err("debe fallar");
        assert!(error.to_string().contains("LOGIN_REQUIRED"));
    }

    #[test]
    fn thumbnails_se_normalizan_a_alta_calidad() {
        assert_eq!(
            upgrade_thumbnail("https://yt3.googleusercontent.com/x=w120-h120-l90-rj"),
            "https://yt3.googleusercontent.com/x=w544-h544-l90-rj"
        );
        assert_eq!(
            upgrade_thumbnail("https://i.ytimg.com/vi/abc/hqdefault.jpg"),
            "https://i.ytimg.com/vi/abc/maxresdefault.jpg"
        );
        assert_eq!(
            upgrade_thumbnail("https://i.ytimg.com/vi/abc/maxresdefault.jpg"),
            "https://i.ytimg.com/vi/abc/maxresdefault.jpg"
        );
    }

    #[test]
    fn strip_vl_quita_el_prefijo() {
        assert_eq!(strip_vl("VLPL123"), "PL123");
        assert_eq!(strip_vl("PL123"), "PL123");
    }

    #[test]
    fn now_unix_es_positivo() {
        assert!(now_unix() > 1_700_000_000);
    }
}
