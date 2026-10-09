//! Linux: the power-saver profile, and sleep and wake from logind.
//!
//! Each runs on a thread of its own over D-Bus, as the colour scheme does
//! (`appearance.rs`), so a slow or absent bus never holds up the app.
//!
//! - The power profile comes from the desktop portal's PowerProfileMonitor,
//!   which works inside Flatpak, or else from power-profiles-daemon.
//! - logind announces sleep with `PrepareForSleep`. A "delay" inhibitor lock
//!   gives the app a moment to save first; inside Flatpak it needs
//!   `--system-talk-name=org.freedesktop.login1`, and without it the app
//!   still hears about the wake.
//!
//! <https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.PowerProfileMonitor.html>
//! <https://systemd.io/INHIBITOR_LOCKS/>

use std::sync::Arc;

use zbus::blocking::{Connection, Proxy};

use super::{Event, SUSPEND_ACK, Shared};

/// The threads run for the life of the process; nothing to undo.
pub(super) struct Reader;

impl Reader {
    pub(super) fn start(shared: Arc<Shared>) -> Result<Self, String> {
        let saver = Arc::clone(&shared);
        spawn("spotifast-power-profile", move || {
            if let Err(error) = watch_saver(&saver) {
                log::debug!("the power profile is unavailable: {error}");
            }
        })?;
        spawn("spotifast-sleep", move || {
            if let Err(error) = watch_sleep(&shared) {
                log::debug!("sleep and wake are unavailable: {error}");
            }
        })?;
        Ok(Self)
    }
}

fn spawn(name: &str, run: impl FnOnce() + Send + 'static) -> Result<(), String> {
    std::thread::Builder::new()
        .name(name.to_string())
        .spawn(run)
        .map(|_| ())
        .map_err(|error| error.to_string())
}

fn watch_saver(shared: &Shared) -> zbus::Result<()> {
    match watch_portal_saver(shared) {
        Ok(()) => Ok(()),
        Err(error) => {
            log::debug!("no power profile portal ({error}); asking power-profiles-daemon");
            watch_daemon_profile(shared)
        }
    }
}

/// The portal's `power-saver-enabled` property, and its changes.
fn watch_portal_saver(shared: &Shared) -> zbus::Result<()> {
    let connection = Connection::session()?;
    let proxy = Proxy::new(
        &connection,
        "org.freedesktop.portal.Desktop",
        "/org/freedesktop/portal/desktop",
        "org.freedesktop.portal.PowerProfileMonitor",
    )?;
    let changes = proxy.receive_property_changed::<bool>("power-saver-enabled");
    let current: bool = proxy.get_property("power-saver-enabled")?;
    shared.update(|conditions| conditions.saver = current);
    for change in changes {
        if let Ok(on) = change.get() {
            shared.update(|conditions| conditions.saver = on);
        }
    }
    Ok(())
}

/// power-profiles-daemon's `ActiveProfile`, under its current name or the
/// one it had before 0.20.
fn watch_daemon_profile(shared: &Shared) -> zbus::Result<()> {
    let connection = Connection::system()?;
    let proxy = Proxy::new(
        &connection,
        "org.freedesktop.UPower.PowerProfiles",
        "/org/freedesktop/UPower/PowerProfiles",
        "org.freedesktop.UPower.PowerProfiles",
    )
    .and_then(|proxy| proxy.get_property::<String>("ActiveProfile").map(|_| proxy))
    .or_else(|_| {
        Proxy::new(
            &connection,
            "net.hadess.PowerProfiles",
            "/net/hadess/PowerProfiles",
            "net.hadess.PowerProfiles",
        )
    })?;
    let changes = proxy.receive_property_changed::<String>("ActiveProfile");
    let current: String = proxy.get_property("ActiveProfile")?;
    shared.update(|conditions| conditions.saver = saves(&current));
    for change in changes {
        if let Ok(profile) = change.get() {
            shared.update(|conditions| conditions.saver = saves(&profile));
        }
    }
    Ok(())
}

/// Whether a power-profiles-daemon profile asks to save energy.
fn saves(profile: &str) -> bool {
    profile == "power-saver"
}

/// logind's `PrepareForSleep`, holding a delay lock while awake.
fn watch_sleep(shared: &Shared) -> zbus::Result<()> {
    let connection = Connection::system()?;
    let manager = Proxy::new(
        &connection,
        "org.freedesktop.login1",
        "/org/freedesktop/login1",
        "org.freedesktop.login1.Manager",
    )?;
    let signals = manager.receive_signal("PrepareForSleep")?;
    let mut lock = inhibit(&manager);
    for message in signals {
        let Ok(sleeping) = message.body().deserialize::<bool>() else {
            continue;
        };
        if sleeping {
            shared.push_and_wait(Event::Suspending, SUSPEND_ACK);
            // Closing the lock lets the system sleep.
            lock.take();
        } else {
            shared.push(Event::Resumed);
            lock = inhibit(&manager);
        }
    }
    Ok(())
}

/// Takes a "delay" lock on sleep, or gets on without one.
fn inhibit(manager: &Proxy<'_>) -> Option<zbus::zvariant::OwnedFd> {
    manager
        .call(
            "Inhibit",
            &(
                "sleep",
                "Spotifast",
                "Saves the session before sleeping",
                "delay",
            ),
        )
        .inspect_err(|error| log::debug!("no delay before sleep: {error}"))
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_power_saver_profile_saves() {
        assert!(saves("power-saver"));
        assert!(!saves("balanced"));
        assert!(!saves("performance"));
    }
}
