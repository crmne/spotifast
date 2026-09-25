//! Diagnostic: what Spotify's own permission service says about each
//! playlist in the account's rootlist, run with the stored playback
//! credential. The Web API's `collaborative` flag misses playlists shared by
//! invitation; the rootlist's decorated `capabilities` do not.
//!
//!   cargo run --example rootlist_probe

use librespot_core::{Session, SessionConfig, cache::Cache};
use librespot_protocol::playlist4_external::SelectedListContent;
use protobuf::Message as _;

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
        let loaded = store.lease(spotifast::credentials::Slot::Playback).load().await?;
        if let Some(warning) = loaded.warning {
            eprintln!("{warning}");
        }
        let Some(spotifast::credentials::Grant::Playback(credentials)) = loaded.grant else {
            anyhow::bail!("Enable playback in Spotifast first");
        };
        let session = Session::new(SessionConfig::default(), Some(cache));
        session.connect(credentials, false).await?;
        println!("connected as {}", session.username());

        let mut from = 0usize;
        loop {
            let bytes = session.spclient().get_rootlist(from, Some(500)).await?;
            let content = SelectedListContent::parse_from_bytes(&bytes)?;
            let Some(contents) = content.contents.into_option() else {
                break;
            };
            let count = contents.items.len();
            let truncated = contents.truncated();
            println!(
                "page from={from}: {count} items, {} meta items",
                contents.meta_items.len()
            );
            for (item, meta) in contents.items.iter().zip(contents.meta_items.iter()) {
                let caps = meta.capabilities.as_ref();
                println!(
                    "{:<40} owner={:<24} collaborative={:<5} can_view={:?} can_edit_items={:?} can_edit_metadata={:?} name={:?}",
                    item.uri(),
                    meta.owner_username(),
                    meta.attributes.collaborative(),
                    caps.map(|c| c.can_view()),
                    caps.map(|c| c.can_edit_items()),
                    caps.map(|c| c.can_edit_metadata()),
                    meta.attributes.name(),
                );
            }
            if !truncated || count == 0 {
                break;
            }
            from += count;
        }
        anyhow::Ok(())
    })?;
    Ok(())
}
