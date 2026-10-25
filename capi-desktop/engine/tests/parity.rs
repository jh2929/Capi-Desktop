//! Paridad funcional del motor con los comandos del antiguo daemon.
//!
//! Requiere red y YouTube disponible:
//! `cargo test --test parity -- --ignored --nocapture`

use capi_engine::Engine;

fn engine() -> Engine {
    let dir = std::env::temp_dir().join("capi-engine-parity-test");
    Engine::new(dir).expect("engine")
}

#[tokio::test]
#[ignore = "requiere red y YouTube disponible"]
async fn cubre_todos_los_comandos_del_daemon() {
    let engine = engine();

    let songs = engine.search("daft punk").await.expect("search");
    assert!(!songs.is_empty());
    let first = &songs[0];
    assert!(!first.id.is_empty());
    assert!(!first.title.is_empty());
    assert!(!first.artist.is_empty());
    assert!(first.duration.unwrap_or(0) > 0);
    assert!(first.thumbnail.is_some());
    assert!(first.artist_id.is_some());

    let song_json = serde_json::to_value(first).unwrap();
    for key in [
        "id",
        "title",
        "artist",
        "album",
        "duration",
        "thumbnail",
        "explicit",
        "artistId",
    ] {
        assert!(song_json.get(key).is_some(), "falta {key} en Song");
    }

    let stream = engine.stream(&first.id).await.expect("stream");
    assert!(stream.url.starts_with("https://"));
    assert!(stream.expires_at > 0);
    assert_eq!(
        stream.codec, "mp4a",
        "se espera AAC por compatibilidad con WebView"
    );
    assert!(stream.mime.contains("audio/mp4"));

    let resp = reqwest::Client::new()
        .get(&stream.url)
        .header("Range", "bytes=0-1023")
        .send()
        .await
        .expect("range");
    assert_eq!(resp.status(), reqwest::StatusCode::PARTIAL_CONTENT);

    // El cliente iOS limitaba los rangos a ~1 MiB; VISIONOS debe permitir
    // pedir el segundo bloque sin 403.
    let second = reqwest::Client::new()
        .get(&stream.url)
        .header("Range", "bytes=1048576-2097151")
        .send()
        .await
        .expect("second range");
    assert_eq!(second.status(), reqwest::StatusCode::PARTIAL_CONTENT);

    let home = engine.home().await.expect("home");
    assert!(!home.sections.is_empty());
    assert!(!home.sections[0].items.is_empty());

    let explore = engine.explore().await.expect("explore");
    assert!(!explore.new_release_albums.is_empty());
    assert!(!explore.mood_and_genres.is_empty());

    let releases = engine.new_releases().await.expect("new releases");
    assert!(!releases.is_empty());

    let artist_id = first.artist_id.as_ref().expect("artist id");
    let artist = engine.artist(artist_id).await.expect("artist");
    assert_eq!(artist.id, *artist_id);
    assert!(!artist.sections.is_empty());

    let album_id = &explore.new_release_albums[0].id;
    let album_tracks = engine.album_tracks(album_id).await.expect("album");
    assert!(!album_tracks.is_empty());

    let playlists = engine
        .search_playlists("daft punk")
        .await
        .expect("playlists");
    assert!(!playlists.is_empty());
    let playlist = engine
        .playlist_tracks(&playlists[0].id)
        .await
        .expect("playlist tracks");
    assert!(!playlist.is_empty());

    let suggestions = engine.suggestions("daft").await.expect("suggestions");
    assert!(!suggestions.is_empty());

    let lyrics = engine
        .lyrics(
            &first.id,
            &first.artist,
            &first.title,
            first.duration.unwrap_or(0),
            None,
        )
        .await
        .expect("lyrics");
    assert!(lyrics.lyrics.is_some(), "no se encontraron letras");
    assert!(lyrics.provider.is_some());

    println!(
        "paridad OK: {} canciones, artista {}, álbum {}, playlist {}, {} moods, letras {}",
        songs.len(),
        artist.name,
        album_id,
        playlists[0].title,
        explore.mood_and_genres.len(),
        lyrics.provider.unwrap_or_default()
    );
}
