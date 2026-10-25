//! Sonda de la Fase 0.
//!
//! Uso:
//!   cargo run --example probe -- "daft punk"
//!
//! Comprueba catálogo, player, clientes y un GET con Range. No forma parte del
//! binario final; sirve para decidir el camino de PO tokens antes de integrar
//! el motor en Tauri.

use std::time::{Duration, Instant};

use reqwest::header::RANGE;
use rustypipe::client::{ClientType, RustyPipe};
use rustypipe::model::AudioFormat;
use rustypipe::param::StreamFilter;

const CLIENTS: &[ClientType] = &[
    ClientType::Android,
    ClientType::Ios,
    ClientType::Tv,
    ClientType::DesktopMusic,
    ClientType::Desktop,
    ClientType::Mobile,
];

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

    let storage = std::env::temp_dir().join("capi-engine-probe");
    std::fs::create_dir_all(&storage)?;

    println!("== Capi engine probe ==");
    println!("query: {query:?}");
    println!("storage: {}", storage.display());

    let t = Instant::now();
    let rp = RustyPipe::builder()
        .storage_dir(&storage)
        .no_botguard()
        .build()?;
    println!("build cliente: {:?}", t.elapsed());

    let t = Instant::now();
    let search = rp.query().music_search_tracks(&query).await?;
    println!(
        "search: {} resultados en {:?}",
        search.items.items.len(),
        t.elapsed()
    );
    for item in search.items.items.iter().take(3) {
        println!("  {item:#?}");
    }

    let Some(first) = search.items.items.first() else {
        eprintln!("sin resultados; no se puede probar el player");
        return Ok(());
    };
    println!("primer video: {} ({})", first.id, first.name);

    let t = Instant::now();
    match rp.query().player(&first.id).await {
        Ok(player) => {
            println!(
                "player default: client={:?} audio_streams={} valid_until={:?} en {:?}",
                player.client_type,
                player.audio_streams.len(),
                player.valid_until,
                t.elapsed()
            );
            if let Some(stream) = player.select_audio_stream(&StreamFilter::default()) {
                println!(
                    "  elegido: itag={} mime={} bitrate={} size={}",
                    stream.itag, stream.mime, stream.bitrate, stream.size
                );
                probe_range(&stream.url).await;
            } else {
                println!("  sin audio seleccionable");
            }
        }
        Err(err) => println!("player default: ERROR {err}"),
    }

    println!("-- streams iOS y formatos --");
    if let Ok(player) = rp
        .query()
        .player_from_client(&first.id, ClientType::Ios)
        .await
    {
        for stream in &player.audio_streams {
            println!(
                "  itag={} format={:?} codec={:?} bitrate={} mime={}",
                stream.itag, stream.format, stream.codec, stream.bitrate, stream.mime
            );
        }
        let m4a_filter = StreamFilter::default().audio_formats(vec![AudioFormat::M4a]);
        println!(
            "  selección M4A: {:?}",
            player
                .select_audio_stream(&m4a_filter)
                .map(|s| (s.itag, s.bitrate, s.mime.as_str()))
        );
    }

    if std::env::var_os("PROBE_MATRIX").is_some() {
        println!("-- matriz de clientes --");
        for &client in CLIENTS {
            let t = Instant::now();
            match rp.query().player_from_client(&first.id, client).await {
                Ok(player) => {
                    let best = player.select_audio_stream(&StreamFilter::default());
                    println!(
                        "  {:?}: OK audio={} expires={}s best={:?} ({:?})",
                        client,
                        player.audio_streams.len(),
                        player.expires_in_seconds,
                        best.map(|s| (s.itag, s.bitrate, s.mime.as_str())),
                        t.elapsed()
                    );
                }
                Err(err) => println!("  {:?}: ERROR {} ({:?})", client, err, t.elapsed()),
            }
        }
    }

    println!("-- 5 pistas con iOS --");
    for item in search.items.items.iter().take(5) {
        let t = Instant::now();
        match rp
            .query()
            .player_from_client(&item.id, ClientType::Ios)
            .await
        {
            Ok(player) => {
                let m4a = player.select_audio_stream(
                    &StreamFilter::default().audio_formats(vec![AudioFormat::M4a]),
                );
                let best = player
                    .select_audio_stream(&StreamFilter::default())
                    .map(|s| (s.itag, s.codec, s.bitrate));
                println!(
                    "  {}: OK streams={} m4a={:?} best={:?} ({:?})",
                    item.id,
                    player.audio_streams.len(),
                    m4a.map(|s| (s.itag, s.bitrate)),
                    best,
                    t.elapsed()
                );
                if let Some(stream) =
                    m4a.or_else(|| player.select_audio_stream(&StreamFilter::default()))
                {
                    probe_range(&stream.url).await;
                }
            }
            Err(err) => println!("  {}: ERROR {err}", item.id),
        }
    }

    println!("-- catálogo relacionado --");
    if let Some(album) = &first.album {
        let t = Instant::now();
        match rp.query().music_album(&album.id).await {
            Ok(page) => println!("album {}: {:?} en {:?}", album.id, page.name, t.elapsed()),
            Err(err) => println!("album {}: ERROR {err}", album.id),
        }
    }
    if let Some(artist_id) = &first.artist_id {
        let t = Instant::now();
        match rp.query().music_artist(artist_id, false).await {
            Ok(artist) => println!(
                "artist {}: {:?} en {:?}",
                artist_id,
                artist.name,
                t.elapsed()
            ),
            Err(err) => println!("artist {}: ERROR {err}", artist_id),
        }
    }
    match rp.query().music_search_playlists("daft punk", false).await {
        Ok(playlists) => {
            println!("playlists: {}", playlists.items.items.len());
            if let Some(pl) = playlists.items.items.first() {
                println!("  primera: {pl:#?}");
                match rp.query().music_playlist(&pl.id).await {
                    Ok(page) => println!("  playlist {}: {:?}", pl.id, page.name),
                    Err(err) => println!("  playlist {}: ERROR {err}", pl.id),
                }
            }
        }
        Err(err) => println!("playlists: ERROR {err}"),
    }

    println!("-- letras --");
    match rp.query().music_details(&first.id).await {
        Ok(details) => {
            println!(
                "details: name={:?} duration={:?}",
                details.track.name, details.track.duration
            );
            if let Some(lyrics_id) = &details.lyrics_id {
                println!("  lyrics_id={lyrics_id}");
                match rp.query().music_lyrics(lyrics_id).await {
                    Ok(lyrics) => println!("  lyrics: {} bytes", lyrics.body.len()),
                    Err(err) => println!("  lyrics: ERROR {err}"),
                }
            } else {
                println!("  sin lyrics_id");
            }
        }
        Err(err) => println!("details: ERROR {err}"),
    }

    println!("-- catálogo auxiliar --");
    let t = Instant::now();
    match rp.query().music_new_albums().await {
        Ok(albums) => println!("new albums: {} en {:?}", albums.len(), t.elapsed()),
        Err(err) => println!("new albums: ERROR {err}"),
    }

    let t = Instant::now();
    match rp.query().music_genres().await {
        Ok(genres) => println!("genres: {} en {:?}", genres.len(), t.elapsed()),
        Err(err) => println!("genres: ERROR {err}"),
    }

    let t = Instant::now();
    match rp.query().music_search_suggestion(&query).await {
        Ok(sugg) => println!("suggestions: {} en {:?}", sugg.terms.len(), t.elapsed()),
        Err(err) => println!("suggestions: ERROR {err}"),
    }

    println!("RSS: {:?} MB", rss_mb());
    Ok(())
}

async fn probe_range(url: &str) {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .expect("cliente reqwest");
    let t = Instant::now();
    match client.get(url).header(RANGE, "bytes=0-1023").send().await {
        Ok(resp) => {
            let status = resp.status();
            let len = resp.content_length();
            let body = resp.bytes().await.map(|b| b.len()).unwrap_or(0);
            println!(
                "  range GET: status={} len={len:?} body={}B en {:?}",
                status,
                body,
                t.elapsed()
            );
        }
        Err(err) => println!("  range GET: ERROR {err}"),
    }
}

fn rss_mb() -> Option<f64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let kb = status
        .lines()
        .find_map(|line| line.strip_prefix("VmRSS:"))?
        .split_whitespace()
        .next()?
        .parse::<f64>()
        .ok()?;
    Some(kb / 1024.0)
}
