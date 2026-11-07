use serde::{Deserialize, Serialize};

/// Canción tal y como la consume el frontend actual (`SongDTO` de Kotlin).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Song {
    pub id: String,
    pub title: String,
    pub artist: String,
    pub primary_artist: Option<String>,
    pub album: Option<String>,
    pub duration: Option<u32>,
    pub thumbnail: Option<String>,
    pub explicit: bool,
    pub artist_id: Option<String>,
}

/// Ítem genérico de YouTube Music (`YTItemDTO`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct YtItem {
    #[serde(rename = "type")]
    pub item_type: String,
    pub id: String,
    pub title: String,
    pub thumbnail: Option<String>,
    pub explicit: bool,
    pub artists: Option<Vec<String>>,
    pub album: Option<String>,
    pub duration: Option<u32>,
    pub year: Option<u16>,
    pub playlist_id: Option<String>,
    pub browse_id: Option<String>,
    pub author: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct HomeSection {
    pub title: String,
    pub label: Option<String>,
    pub thumbnail: Option<String>,
    pub params: Option<String>,
    pub items: Vec<YtItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct HomeResponse {
    pub sections: Vec<HomeSection>,
    pub continuation: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ArtistSection {
    pub title: String,
    pub items: Vec<YtItem>,
    pub browse_id: Option<String>,
    pub params: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ArtistPage {
    pub id: String,
    pub name: String,
    pub thumbnail: Option<String>,
    pub description: Option<String>,
    pub sections: Vec<ArtistSection>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MoodGenre {
    pub title: String,
    pub stripe_color: i64,
    pub browse_id: String,
    pub params: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ExploreResponse {
    pub new_release_albums: Vec<YtItem>,
    pub mood_and_genres: Vec<MoodGenre>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LyricsResult {
    pub lyrics: Option<String>,
    pub provider: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct StreamInfo {
    /// URL directa de googlevideo (sin proxy).
    pub url: String,
    /// Marca UNIX en la que YouTube invalida la URL.
    pub expires_at: i64,
    pub mime: String,
    pub bitrate: u32,
    pub itag: u32,
    pub codec: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SearchPage {
    pub songs: Vec<Song>,
    pub continuation: Option<String>,
}

/// Forma de error que espera el frontend actual (`ErrorDTO`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorDto {
    pub error: String,
}

impl ErrorDto {
    pub fn from_message(message: impl Into<String>) -> Self {
        Self {
            error: message.into(),
        }
    }
}
