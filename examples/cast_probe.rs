//! Diagnostic: start Spotify on a Google Cast device and sign it in.
//! Afterwards the device should be listed in Spotify's device pickers.
//!
//!   cargo run --example cast_probe -- 192.168.1.20 8009 "nest audio"

use librespot_core::{Session, SessionConfig};

fn main() -> anyhow::Result<()> {
    fastframe_log::Logging::new("spotifast", env!("CARGO_PKG_VERSION"))
        .filter("info")
        .init()?;
    let mut args = std::env::args().skip(1);
    let (Some(address), Some(port), Some(name)) = (args.next(), args.next(), args.next()) else {
        anyhow::bail!("usage: cast_probe <address> <port> <friendly name>");
    };

    let Some(spotifast::credentials::Grant::Playback(grant)) = stored_grant()? else {
        anyhow::bail!("Enable playback in Spotifast first");
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let session = {
        let _context = runtime.enter();
        Session::new(SessionConfig::default(), None)
    };
    runtime.block_on(session.connect(grant, false))?;
    println!("session connected");

    println!(
        "starting Spotify on {name} ({address}:{port}), device id {} ...",
        spotifast::cast::device_id(&name)
    );
    spotifast::cast::start(address.parse()?, port.parse()?, &name, |client, device| {
        println!("  clientID {client}, exchanging the session token ...");
        let token = runtime.block_on(spotifast::cast::device_token(&session, client, device))?;
        println!("  got a device token ({} chars)", token.len());
        Ok(token)
    })?;
    println!("  accepted: {name} is signed in to Spotify Connect");
    Ok(())
}

fn stored_grant() -> anyhow::Result<Option<spotifast::credentials::Grant>> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let loaded = runtime.block_on(async {
        let store = spotifast::credentials::Store::new(spotifast::paths::AppDirs::discover());
        store
            .lease(spotifast::credentials::Slot::Playback)
            .load()
            .await
    })?;
    if let Some(warning) = loaded.warning {
        eprintln!("{warning}");
    }
    Ok(loaded.grant)
}
