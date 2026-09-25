//! Diagnostic: which context shapes Spotify still resolves for a song
//! radio, run with the stored playback credential.
//!
//!   cargo run --example station_probe -- spotify:track:4uLU6hMCjMI75M1A2tKUQC

use librespot_core::{Session, SessionConfig, cache::Cache};
use librespot_protocol::autoplay_context_request::AutoplayContextRequest;

fn main() -> anyhow::Result<()> {
    fastframe_log::Logging::new("spotifast", env!("CARGO_PKG_VERSION"))
        .filter("warn")
        .init()?;
    let track = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "spotify:track:4uLU6hMCjMI75M1A2tKUQC".into());
    let id = track.rsplit(':').next().unwrap_or_default().to_string();

    let dirs = spotifast::paths::AppDirs::discover();
    let cache = Cache::new::<&std::path::Path>(None, None, None, None)?.with_memory_credentials();

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async move {
        let store = spotifast::credentials::Store::new(dirs);
        let loaded = store
            .lease(spotifast::credentials::Slot::Playback)
            .load()
            .await?;
        if let Some(warning) = loaded.warning {
            eprintln!("{warning}");
        }
        let Some(spotifast::credentials::Grant::Playback(credentials)) = loaded.grant else {
            anyhow::bail!("Enable playback in Spotifast first");
        };
        let session = Session::new(SessionConfig::default(), Some(cache));
        session.connect(credentials, false).await?;
        println!("connected as {}", session.username());

        let station = format!("spotify:station:track:{id}");
        for uri in [station.as_str(), track.as_str()] {
            print!("get_context({uri}) -> ");
            match session.spclient().get_context(uri).await {
                Ok(ctx) => println!(
                    "ok: uri={:?} pages={} first_page_tracks={}",
                    ctx.uri,
                    ctx.pages.len(),
                    ctx.pages.first().map(|p| p.tracks.len()).unwrap_or(0)
                ),
                Err(error) => println!("ERR: {error}"),
            }
        }
        for uri in [track.as_str(), station.as_str()] {
            let request = AutoplayContextRequest {
                context_uri: Some(uri.to_string()),
                ..Default::default()
            };
            print!("get_autoplay_context({uri}) -> ");
            match session.spclient().get_autoplay_context(&request).await {
                Ok(ctx) => println!(
                    "ok: uri={:?} pages={} first_page_tracks={}",
                    ctx.uri,
                    ctx.pages.len(),
                    ctx.pages.first().map(|p| p.tracks.len()).unwrap_or(0)
                ),
                Err(error) => println!("ERR: {error}"),
            }
        }
        anyhow::Ok(())
    })?;
    Ok(())
}
