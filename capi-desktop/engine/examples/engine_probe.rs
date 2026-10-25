//! Valida la API pública de `capi-engine` contra YouTube real.
//!
//! `cargo run --example engine_probe -- "daft punk"`

use std::time::Instant;

use capi_engine::Engine;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "rustypipe=warn,capi_engine=info".into()),
        )
        .with_target(false)
        .init();

    let query = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "daft punk".to_string());
    let storage = std::env::temp_dir().join("capi-engine-apitest");
    let engine = Engine::new(storage)?;

    let t = Instant::now();
    let songs = engine.search(&query).await?;
    println!("search: {} canciones en {:?}", songs.len(), t.elapsed());
    let first = songs.first().ok_or("sin resultados")?;
    println!("  primera: {first:#?}");
    println!("  json: {}", serde_json::to_string(first)?);

    let t = Instant::now();
    let stream = engine.stream(&first.id).await?;
    println!(
        "stream: itag={} codec={} bitrate={} expires={} en {:?}",
        stream.itag,
        stream.codec,
        stream.bitrate,
        stream.expires_at,
        t.elapsed()
    );
    println!("  stream json: {}", serde_json::to_string(&stream)?);
    let resp = reqwest::Client::new()
        .get(&stream.url)
        .header("Range", "bytes=0-1023")
        .send()
        .await?;
    println!("  range: {} {}B", resp.status(), resp.bytes().await?.len());

    let t = Instant::now();
    let lyrics = engine
        .lyrics(
            &first.id,
            &first.artist,
            &first.title,
            first.duration.unwrap_or(0),
            None,
        )
        .await?;
    println!(
        "lyrics: provider={:?} bytes={} en {:?}",
        lyrics.provider,
        lyrics.lyrics.as_deref().map(str::len).unwrap_or(0),
        t.elapsed()
    );

    if let Some(artist_id) = &first.artist_id {
        let t = Instant::now();
        let artist = engine.artist(artist_id).await?;
        println!(
            "artist: {:?} secciones={} en {:?}",
            artist.name,
            artist.sections.len(),
            t.elapsed()
        );
    }

    let t = Instant::now();
    let home = engine.home().await?;
    println!(
        "home: {} secciones / {} items en {:?}",
        home.sections.len(),
        home.sections.iter().map(|s| s.items.len()).sum::<usize>(),
        t.elapsed()
    );

    let t = Instant::now();
    let explore = engine.explore().await?;
    println!(
        "explore: {} álbumes / {} moods en {:?}",
        explore.new_release_albums.len(),
        explore.mood_and_genres.len(),
        t.elapsed()
    );

    if let Some(album) = explore.new_release_albums.first() {
        let t = Instant::now();
        let tracks = engine.album_tracks(&album.id).await?;
        println!(
            "album {}: {} canciones en {:?}",
            album.title,
            tracks.len(),
            t.elapsed()
        );
    }

    Ok(())
}
