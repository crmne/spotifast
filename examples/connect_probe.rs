//! Diagnostic: discover Spotify Connect receivers on the LAN and hand one an
//! account, then check whether Spotify's device list picks it up.
//!
//!   cargo run --example connect_probe            # just discover
//!   cargo run --example connect_probe -- "House" # discover and activate

use std::time::Duration;

fn main() -> anyhow::Result<()> {
    fastframe_log::Logging::new("spotifast", env!("CARGO_PKG_VERSION"))
        .filter("warn")
        .init()?;
    let wanted = std::env::args().nth(1);

    println!("browsing for _spotify-connect._tcp ...");
    let receivers = spotifast::zeroconf::discover(Duration::from_secs(4))?;
    if receivers.is_empty() {
        println!("  none found");
        return Ok(());
    }
    let http = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(8))
        .build()?;
    for receiver in &receivers {
        print!(
            "  {} at {}:{}",
            receiver.name, receiver.address, receiver.port
        );
        match spotifast::zeroconf::get_info(&http, receiver) {
            Ok(info) => println!(
                "  [{} {} | token={} | active_user={:?}]",
                info.remote_name, info.device_type, info.token_type, info.active_user
            ),
            Err(error) => println!("  (getInfo failed: {error})"),
        }
    }

    let Some(wanted) = wanted else { return Ok(()) };
    let Some(receiver) = receivers
        .iter()
        .find(|r| r.name.to_lowercase().contains(&wanted.to_lowercase()))
    else {
        println!("no receiver matching {wanted:?}");
        return Ok(());
    };

    let Some(spotifast::credentials::Grant::Playback(grant)) =
        stored_grant(spotifast::credentials::Slot::Playback)?
    else {
        anyhow::bail!("Enable playback in Spotifast first");
    };
    let credentials = spotifast::zeroconf::Credentials::from_playback(&grant)?;
    println!("\nhanding the account to {} ...", receiver.name);
    let info = spotifast::zeroconf::get_info(&http, receiver)?;
    match spotifast::zeroconf::add_user(&http, receiver, &info, &credentials, "Spotifast") {
        Ok(()) => println!("  accepted"),
        Err(error) => {
            println!("  refused: {error}");
            return Ok(());
        }
    }

    println!("\nwaiting for it to appear in Spotify's device list ...");
    for attempt in 1..=10 {
        std::thread::sleep(Duration::from_secs(2));
        let listed = devices()?;
        if !listed.is_empty() {
            for name in &listed {
                println!("  [{attempt}] {name}");
            }
            return Ok(());
        }
        println!("  [{attempt}] still empty");
    }
    Ok(())
}

/// The account's devices as Spotify currently sees them.
fn devices() -> anyhow::Result<Vec<String>> {
    let Some(spotifast::credentials::Grant::Web(token)) =
        stored_grant(spotifast::credentials::Slot::Shared)?
    else {
        anyhow::bail!("Sign in to Spotifast first");
    };
    let http = reqwest::blocking::Client::new();
    let body: serde_json::Value = http
        .get("https://api.spotify.com/v1/me/player/devices")
        .bearer_auth(&token.access_token)
        .send()?
        .json()?;
    Ok(body["devices"]
        .as_array()
        .map(|devices| {
            devices
                .iter()
                .map(|d| {
                    format!(
                        "{} ({}, active={})",
                        d["name"].as_str().unwrap_or("?"),
                        d["type"].as_str().unwrap_or("?"),
                        d["is_active"]
                    )
                })
                .collect()
        })
        .unwrap_or_default())
}

fn stored_grant(
    slot: spotifast::credentials::Slot,
) -> anyhow::Result<Option<spotifast::credentials::Grant>> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let loaded = runtime.block_on(async {
        let store = spotifast::credentials::Store::new(spotifast::paths::AppDirs::discover());
        store.lease(slot).load().await
    })?;
    if let Some(warning) = loaded.warning {
        eprintln!("{warning}");
    }
    Ok(loaded.grant)
}
