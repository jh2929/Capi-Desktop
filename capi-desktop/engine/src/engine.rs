use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rustypipe::client::{ClientType, RustyPipe};
use rustypipe::model::{AlbumItem, ArtistItem, MusicPlaylistItem, Thumbnail, TrackItem};
use tokio::sync::RwLock;

use crate::error::EngineError;
use crate::lyrics;
use crate::models::{
    ArtistPage, ArtistSection, ExploreResponse, HomeResponse, HomeSection, LyricsResult, MoodGenre,
    Song, StreamInfo, YtItem,
};

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(12);

struct CachedStream {
    info: StreamInfo,
}

struct CachedLyrics {
    result: LyricsResult,
    cached_at: i64,
}

/// Motor de Capi. Es `Send + Sync` y se comparte como `Arc<Engine>` desde Tauri.
pub struct Engine {
    rp: RustyPipe,
    timeout: Duration,
    streams: RwLock<HashMap<String, CachedStream>>,
    lyrics: RwLock<HashMap<String, CachedLyrics>>,
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

        Ok(Self {
            rp,
            timeout: DEFAULT_TIMEOUT,
            streams: RwLock::new(HashMap::new()),
            lyrics: RwLock::new(HashMap::new()),
        })
    }

    // ─── Catálogo ─────────────────────────────────────────────────────

    pub async fn search(&self, query: &str) -> Result<Vec<Song>, EngineError> {
        if query.trim().is_empty() {
            return Ok(Vec::new());
        }
        let result = self
            .with_timeout(self.rp.query().music_search_tracks(query))
            .await?;
        Ok(result.items.items.iter().map(song_from_track).collect())
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
    /// Fase 0 demostró que el cliente iOS devuelve streams sin PO token y con
    /// AAC (`itag 140`), que es el formato más compatible con WebView. Se
    /// prefiere AAC y se cae a Opus si no existe.
    pub async fn stream(&self, video_id: &str) -> Result<StreamInfo, EngineError> {
        if let Some(cached) = self.streams.read().await.get(video_id) {
            if cached.info.expires_at > now_unix() + 60 {
                return Ok(cached.info.clone());
            }
        }

        let player = self
            .with_timeout(
                self.rp
                    .query()
                    .player_from_client(video_id, ClientType::Ios),
            )
            .await?;

        let stream = player
            .audio_streams
            .iter()
            .filter(|s| s.codec == rustypipe::model::AudioCodec::Mp4a)
            .max_by_key(|s| s.bitrate)
            .or_else(|| player.audio_streams.iter().max_by_key(|s| s.bitrate))
            .ok_or_else(|| {
                EngineError::Unavailable(format!(
                    "YouTube no devolvió audio para el vídeo {video_id}"
                ))
            })?;

        let info = StreamInfo {
            url: stream.url.clone(),
            expires_at: player.valid_until.unix_timestamp(),
            mime: stream.mime.clone(),
            bitrate: stream.bitrate,
            itag: stream.itag,
            codec: format!("{:?}", stream.codec).to_lowercase(),
        };

        self.streams
            .write()
            .await
            .insert(video_id.to_string(), CachedStream { info: info.clone() });

        Ok(info)
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
        .map(|t| t.url.clone())
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
        thumbnail: best_thumbnail(&track.cover),
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
    fn strip_vl_quita_el_prefijo() {
        assert_eq!(strip_vl("VLPL123"), "PL123");
        assert_eq!(strip_vl("PL123"), "PL123");
    }

    #[test]
    fn now_unix_es_positivo() {
        assert!(now_unix() > 1_700_000_000);
    }
}
