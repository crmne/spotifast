//! Diagnostic: the account's playlist library as Spotifast reads it over
//! the local playback session, with the stored playback credential. This is
//! the read that stands in for the shared Web API app's list of the
//! account's playlists. Nothing is written.
//!
//!   cargo run --example library_probe

use librespot_core::{Session, SessionConfig, cache::Cache};

fn main() -> anyhow::Result<()> {
    fastframe_log::Logging::new("spotifast", env!("CARGO_PKG_VERSION"))
        .filter("warn")
        .init()?;

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

        let started = std::time::Instant::now();
        let playlists = match spotifast::session_reads::library(&session).await {
            Ok(playlists) => playlists,
            Err(spotifast::session_reads::Failure::Definitive(error)) => {
                anyhow::bail!("refused: {error}")
            }
            Err(spotifast::session_reads::Failure::Retry(error)) => {
                anyhow::bail!("unavailable: {error:#}")
            }
        };
        let elapsed = started.elapsed();
        let spotify_owned = playlists
            .iter()
            .filter(|playlist| playlist.owner.id.as_deref() == Some("spotify"))
            .count();
        let with_cover = playlists
            .iter()
            .filter(|playlist| !playlist.images.is_empty())
            .count();
        let with_public = playlists
            .iter()
            .filter(|playlist| playlist.public.is_some())
            .count();
        println!(
            "{} playlists in {elapsed:?}: {spotify_owned} owned by Spotify, {with_cover} with a cover, {with_public} with a public flag",
            playlists.len()
        );
        for playlist in &playlists {
            println!(
                "  {:<24} {:<40} owner={:<26} songs={:<5} public={:<5} cover={}",
                playlist.id,
                playlist.name,
                playlist.owner_name(),
                playlist.track_total(),
                playlist
                    .public
                    .map_or_else(|| "?".to_string(), |public| public.to_string()),
                playlist
                    .images
                    .first()
                    .map_or("none", |image| image.url.as_str()),
            );
        }
        anyhow::Ok(())
    })?;
    Ok(())
}
