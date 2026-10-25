use std::time::Duration;

use rustypipe::client::RustyPipe;
use serde::Deserialize;

use crate::error::EngineError;
use crate::models::LyricsResult;

#[derive(Debug, Deserialize)]
struct LrcLibTrack {
    #[allow(dead_code)]
    #[serde(rename = "trackName")]
    track_name: String,
    #[allow(dead_code)]
    #[serde(rename = "artistName")]
    artist_name: String,
    duration: Option<f64>,
    #[serde(rename = "syncedLyrics")]
    synced_lyrics: Option<String>,
    #[serde(rename = "plainLyrics")]
    plain_lyrics: Option<String>,
}

/// Busca letras primero en LrcLib (rápido y sin autenticación) y luego en
/// YouTube Music. Devuelve `Ok(None)` cuando no hay letra, no cuando falla la
/// red, para que el frontend muestre "no disponible" en lugar de un error.
pub async fn fetch(
    rp: &RustyPipe,
    video_id: &str,
    artist: &str,
    title: &str,
    duration: u32,
    provider: Option<&str>,
    timeout: Duration,
) -> Result<LyricsResult, EngineError> {
    // Los proveedores que aún no están implementados (KuGou, SimpMusic,
    // BetterLyrics) se tratan como "auto" para no dejar la UI sin letra.
    let (use_lrclib, use_youtube_music) = match provider {
        Some("LrcLib") => (true, false),
        Some("YouTube Music") => (false, true),
        _ => (true, true),
    };

    if use_lrclib {
        if let Some(found) = fetch_lrclib(artist, title, duration).await {
            return Ok(LyricsResult {
                lyrics: Some(found),
                provider: Some("LrcLib".into()),
            });
        }
    }

    if !use_youtube_music {
        return Ok(LyricsResult {
            lyrics: None,
            provider: None,
        });
    }

    match tokio::time::timeout(timeout, fetch_youtube_music(rp, video_id)).await {
        Ok(Ok(Some(found))) => Ok(LyricsResult {
            lyrics: Some(found),
            provider: Some("YouTube Music".into()),
        }),
        Ok(Ok(None)) => Ok(LyricsResult {
            lyrics: None,
            provider: None,
        }),
        Ok(Err(err)) => {
            tracing::warn!(%video_id, "letras de YouTube Music no disponibles: {err}");
            Ok(LyricsResult {
                lyrics: None,
                provider: None,
            })
        }
        Err(_) => {
            tracing::warn!(%video_id, "timeout esperando letras de YouTube Music");
            Ok(LyricsResult {
                lyrics: None,
                provider: None,
            })
        }
    }
}

async fn fetch_lrclib(artist: &str, title: &str, duration: u32) -> Option<String> {
    if artist.trim().is_empty() || title.trim().is_empty() {
        return None;
    }

    let client = reqwest::Client::builder()
        .user_agent(concat!("Capi/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(8))
        .build()
        .ok()?;

    let url = reqwest::Url::parse_with_params(
        "https://lrclib.net/api/search",
        &[("track_name", title), ("artist_name", artist)],
    )
    .ok()?;

    let response = client.get(url).send().await.ok()?;
    if !response.status().is_success() {
        tracing::warn!(status = %response.status(), "LrcLib respondió con error");
        return None;
    }

    let tracks: Vec<LrcLibTrack> = response.json().await.ok()?;
    pick_lrclib(tracks, duration)
}

fn pick_lrclib(mut tracks: Vec<LrcLibTrack>, duration: u32) -> Option<String> {
    tracks.retain(|t| {
        t.synced_lyrics
            .as_deref()
            .is_some_and(|s| !s.trim().is_empty())
            || t.plain_lyrics
                .as_deref()
                .is_some_and(|s| !s.trim().is_empty())
    });

    if tracks.is_empty() {
        return None;
    }

    let target = f64::from(duration);
    tracks.sort_by(|a, b| {
        let da = (a.duration.unwrap_or(target) - target).abs();
        let db = (b.duration.unwrap_or(target) - target).abs();
        da.total_cmp(&db)
    });

    tracks.into_iter().find_map(|track| {
        track
            .synced_lyrics
            .filter(|s| !s.trim().is_empty())
            .or(track.plain_lyrics)
    })
}

async fn fetch_youtube_music(
    rp: &RustyPipe,
    video_id: &str,
) -> Result<Option<String>, EngineError> {
    let details = rp.query().music_details(video_id).await?;
    let Some(lyrics_id) = details.lyrics_id else {
        return Ok(None);
    };
    let lyrics = rp.query().music_lyrics(&lyrics_id).await?;
    if lyrics.body.trim().is_empty() {
        Ok(None)
    } else {
        Ok(Some(lyrics.body))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(duration: Option<f64>, synced: Option<&str>, plain: Option<&str>) -> LrcLibTrack {
        LrcLibTrack {
            track_name: "Track".into(),
            artist_name: "Artist".into(),
            duration,
            synced_lyrics: synced.map(str::to_string),
            plain_lyrics: plain.map(str::to_string),
        }
    }

    #[test]
    fn prefiere_la_letra_sincronizada_mas_cercana() {
        let tracks = vec![
            track(Some(100.0), Some("[00:01.00]lejana"), None),
            track(Some(200.0), Some("[00:01.00]cercana"), None),
        ];
        assert_eq!(
            pick_lrclib(tracks, 202),
            Some("[00:01.00]cercana".to_string())
        );
    }

    #[test]
    fn usa_letra_plana_si_no_hay_sincronizada() {
        let tracks = vec![track(Some(200.0), None, Some("letra plana"))];
        assert_eq!(pick_lrclib(tracks, 200), Some("letra plana".to_string()));
    }

    #[test]
    fn devuelve_none_sin_candidatos() {
        assert_eq!(pick_lrclib(Vec::new(), 200), None);
        let tracks = vec![track(Some(200.0), Some("   "), None)];
        assert_eq!(pick_lrclib(tracks, 200), None);
    }
}
