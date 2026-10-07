//! The desktop's light or dark preference on Linux.
//!
//! winit reports no system theme on X11 and none on most Wayland desktops,
//! so "Follow system" would always stay dark. The desktop portal publishes
//! the preference as `org.freedesktop.appearance color-scheme` on GNOME, KDE
//! and inside Flatpak. A thread of its own reads it and follows its change
//! signal, so a slow or absent session bus never holds up the window.

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use zbus::zvariant::{OwnedValue, Value};

const UNKNOWN: u8 = 0;
const DARK: u8 = 1;
const LIGHT: u8 = 2;

const NAMESPACE: &str = "org.freedesktop.appearance";
const KEY: &str = "color-scheme";

/// The latest preference the portal reported.
pub struct SystemAppearance {
    scheme: Arc<AtomicU8>,
}

impl SystemAppearance {
    pub fn spawn(wake: impl Fn() + Send + 'static) -> Self {
        let scheme = Arc::new(AtomicU8::new(UNKNOWN));
        let shared = scheme.clone();
        let spawned = std::thread::Builder::new()
            .name("spotizgeg-appearance".to_string())
            .spawn(move || {
                if let Err(error) = watch(&shared, &wake) {
                    log::debug!("the desktop's colour scheme is unavailable: {error}");
                }
            });
        if let Err(error) = spawned {
            log::warn!("unable to watch the desktop's colour scheme: {error}");
        }
        Self { scheme }
    }

    /// A desktop that always prefers `dark`, for tests elsewhere.
    #[cfg(test)]
    pub fn fixed(dark: bool) -> Self {
        let scheme = Arc::new(AtomicU8::new(if dark { DARK } else { LIGHT }));
        Self { scheme }
    }

    /// Whether the desktop prefers dark, once the portal has answered.
    pub fn dark(&self) -> Option<bool> {
        match self.scheme.load(Ordering::Relaxed) {
            DARK => Some(true),
            LIGHT => Some(false),
            _ => None,
        }
    }
}

fn watch(scheme: &AtomicU8, wake: &dyn Fn()) -> zbus::Result<()> {
    let connection = zbus::blocking::Connection::session()?;
    let proxy = zbus::blocking::Proxy::new(
        &connection,
        "org.freedesktop.portal.Desktop",
        "/org/freedesktop/portal/desktop",
        "org.freedesktop.portal.Settings",
    )?;
    // Subscribe before reading, so a change in between is not missed.
    let changes = proxy.receive_signal("SettingChanged")?;
    // `ReadOne` arrived in version 2 of the interface; `Read` wraps the
    // same answer in one more variant.
    let current: zbus::Result<OwnedValue> = proxy
        .call("ReadOne", &(NAMESPACE, KEY))
        .or_else(|_| proxy.call("Read", &(NAMESPACE, KEY)));
    if let Some(value) = current.ok().as_deref().and_then(color_scheme) {
        publish(scheme, value, wake);
    }
    for message in changes {
        let Ok((namespace, key, value)) =
            message.body().deserialize::<(String, String, OwnedValue)>()
        else {
            continue;
        };
        if namespace == NAMESPACE
            && key == KEY
            && let Some(value) = color_scheme(&value)
        {
            publish(scheme, value, wake);
        }
    }
    Ok(())
}

fn publish(scheme: &AtomicU8, value: u32, wake: &dyn Fn()) {
    let next = stored(value);
    if scheme.swap(next, Ordering::Relaxed) != next {
        wake();
    }
}

/// The portal's value: 1 prefers dark, 2 prefers light, 0 states no
/// preference, which desktops draw light.
fn stored(value: u32) -> u8 {
    if value == 1 { DARK } else { LIGHT }
}

/// Unwraps the number from the one or two variants it arrives in.
fn color_scheme(value: &Value<'_>) -> Option<u32> {
    match value {
        Value::U32(value) => Some(*value),
        Value::Value(inner) => color_scheme(inner),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_portals_answer_is_read_through_either_wrapping() {
        assert_eq!(color_scheme(&Value::U32(1)), Some(1));
        assert_eq!(
            color_scheme(&Value::Value(Box::new(Value::U32(2)))),
            Some(2)
        );
        assert_eq!(color_scheme(&Value::Str("dark".into())), None);
    }

    /// Reads the running desktop's portal. Needs a session bus with
    /// xdg-desktop-portal, so it only runs when asked for.
    #[test]
    #[ignore]
    fn the_desktop_portal_answers() {
        let appearance = SystemAppearance::spawn(|| {});
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while appearance.dark().is_none() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(
            appearance.dark().is_some(),
            "the portal gave no colour scheme"
        );
    }

    #[test]
    fn no_preference_is_drawn_light() {
        assert_eq!(stored(1), DARK);
        assert_eq!(stored(2), LIGHT);
        assert_eq!(stored(0), LIGHT);
    }

    #[test]
    fn a_change_wakes_the_window_once() {
        let scheme = AtomicU8::new(UNKNOWN);
        let woken = std::cell::Cell::new(0);
        let wake = || woken.set(woken.get() + 1);
        publish(&scheme, 1, &wake);
        publish(&scheme, 1, &wake);
        publish(&scheme, 2, &wake);
        assert_eq!(woken.get(), 2);
        let appearance = SystemAppearance {
            scheme: Arc::new(AtomicU8::new(scheme.load(Ordering::Relaxed))),
        };
        assert_eq!(appearance.dark(), Some(false));
    }
}
