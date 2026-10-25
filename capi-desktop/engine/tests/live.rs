//! Pruebas de integración contra YouTube real.
//!
//! Se ejecutan manualmente porque dependen de red y de que YouTube no cambie:
//! `cargo test --test live -- --ignored --nocapture`

use capi_engine::Engine;

fn engine() -> Engine {
    let dir = std::env::temp_dir().join("capi-engine-live-test");
    Engine::new(dir).expect("engine")
}

#[tokio::test]
#[ignore = "requiere red y YouTube disponible"]
async fn busca_resuelve_y_descarga_un_rango() {
    let engine = engine();
    let songs = engine.search("daft punk").await.expect("search");
    assert!(!songs.is_empty(), "la búsqueda no devolvió canciones");

    let first = &songs[0];
    let stream = engine.stream(&first.id).await.expect("stream");
    assert!(stream.url.starts_with("https://"));
    assert!(stream.expires_at > 0);

    let resp = reqwest::Client::new()
        .get(&stream.url)
        .header("Range", "bytes=0-1023")
        .send()
        .await
        .expect("http");
    assert!(resp.status().is_success(), "status {}", resp.status());
    assert_eq!(resp.bytes().await.expect("body").len(), 1024);

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
    assert!(lyrics.lyrics.is_some() || lyrics.provider.is_none());
}
