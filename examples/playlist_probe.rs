//! Diagnostic: what Spotify's playlist4 read sends for one playlist, run
//! with the stored playback credential. Prints the list's format
//! attributes, a tally of the rows' format attributes, and every readable
//! string in the raw answer that is not a URI, to find where Spotify's
//! playlist filter chips (concepts) come from.
//!
//!   cargo run --example playlist_probe -- 37i9dQZEVXboowTAJbz0HN

use std::collections::BTreeMap;

use librespot_core::{Session, SessionConfig, SpotifyId, cache::Cache};
use librespot_protocol::playlist4_external::SelectedListContent;
use protobuf::Message as _;

fn main() -> anyhow::Result<()> {
    fastframe_log::Logging::new("spotifast", env!("CARGO_PKG_VERSION"))
        .filter("warn")
        .init()?;
    let id = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "37i9dQZEVXboowTAJbz0HN".to_string());

    let dirs = spotifast::paths::AppDirs::discover().dirs;
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
        let Some(spotifast::credentials::Grant::Playback(credentials)) = loaded.grant else {
            anyhow::bail!("Enable playback in Spotifast first");
        };
        let session = Session::new(SessionConfig::default(), Some(cache));
        session.connect(credentials, false).await?;

        let playlist = SpotifyId::from_base62(&id)?;
        if let Some(signal) = std::env::args().nth(2) {
            let current = SelectedListContent::parse_from_bytes(
                &session.spclient().get_playlist(&playlist).await?,
            )?;
            let mut body = librespot_protocol::playlist4_external::ListSignals::new();
            body.set_base_revision(current.revision().to_vec());
            let mut emitted = librespot_protocol::signal_model::Signal::new();
            emitted.identifier = signal.clone();
            body.emitted_signals.push(emitted);
            let endpoint = format!("/playlist/v2/playlist/{id}/signals");
            let answer = session
                .spclient()
                .request(
                    &reqwest::Method::POST,
                    &endpoint,
                    None,
                    Some(&body.write_to_bytes()?),
                )
                .await;
            match answer {
                Ok(bytes) => println!("signal {signal}: ok, {} bytes back", bytes.len()),
                Err(error) => println!("signal {signal}: {error:?}"),
            }
        }
        let range = SelectedListContent::parse_from_bytes(
            &session
                .spclient()
                .get_playlist_range(&playlist, 0, 0)
                .await?,
        )?;
        let names = range
            .attributes
            .as_ref()
            .map(|a| {
                a.format_attributes
                    .iter()
                    .filter(|f| f.key().starts_with("session_control_display.displayName."))
                    .count()
            })
            .unwrap_or_default();
        println!(
            "header read: {} format attributes, {names} chip names",
            range
                .attributes
                .as_ref()
                .map_or(0, |a| a.format_attributes.len())
        );
        println!(
            "header read: {} signals, {} rows",
            range
                .contents
                .as_ref()
                .map_or(0, |c| c.available_signals.len()),
            range.contents.as_ref().map_or(0, |c| c.items.len()),
        );
        let bytes = session.spclient().get_playlist(&playlist).await?;
        println!("raw answer: {} bytes", bytes.len());
        let content = SelectedListContent::parse_from_bytes(&bytes)?;

        if let Some(attributes) = content.attributes.as_ref() {
            println!("list name: {:?}", attributes.name());
            println!("list format: {:?}", attributes.format());
            for attribute in &attributes.format_attributes {
                println!(
                    "  list attr {} = {}",
                    attribute.key(),
                    clip(attribute.value())
                );
            }
            println!(
                "  list unknown fields: {:?}",
                attributes.special_fields.unknown_fields()
            );
        }
        println!(
            "top-level unknown fields: {:?}",
            content.special_fields.unknown_fields()
        );

        let mut tally: BTreeMap<String, usize> = BTreeMap::new();
        let mut signals = 0usize;
        if let Some(contents) = content.contents.as_ref() {
            println!("rows: {}", contents.items.len());
            for signal in &contents.available_signals {
                println!("  available signal {}", signal.identifier);
            }
            for item in &contents.items {
                let Some(attributes) = item.attributes.as_ref() else {
                    continue;
                };
                signals += attributes.available_signals.len();
                for attribute in &attributes.format_attributes {
                    let value = attribute.value();
                    if value == "false"
                        || attribute.key().contains("uri")
                        || attribute.key().contains("_id")
                    {
                        continue;
                    }
                    let shown = if value == "true" || value == "1" {
                        String::new()
                    } else {
                        format!("={}", clip(value))
                    };
                    *tally
                        .entry(format!("{}{shown}", attribute.key()))
                        .or_default() += 1;
                }
            }
        }
        println!("row signals: {signals}");
        for (key, count) in tally {
            println!("  {count:>3}  {key}");
        }

        println!("readable strings that are not URIs:");
        let mut seen = std::collections::BTreeSet::new();
        for text in strings(&bytes) {
            if !text.starts_with("spotify:")
                && !text.starts_with("http")
                && seen.insert(text.clone())
            {
                println!("  {text}");
            }
        }
        anyhow::Ok(())
    })?;
    Ok(())
}

fn clip(text: &str) -> String {
    text.chars().take(80).collect()
}

/// Runs of at least six printable UTF-8 characters.
fn strings(bytes: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(bytes)
        .split(|c: char| c.is_control() || c == '\u{fffd}')
        .map(str::trim)
        .filter(|run| run.chars().count() >= 6)
        .map(str::to_string)
        .collect()
}
