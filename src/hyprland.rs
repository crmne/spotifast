//! Takes Hyprland's border off skinned windows.
//!
//! A skin paints every pixel of its window, and on Hyprland a window with no
//! decorations still gets the compositor's border, rounded corners and
//! shadow: a rectangle around a skin that is not one. Wayland has no way to
//! ask for less, so this asks Hyprland over its own socket.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// A window takes a moment to reach the compositor's list.
const ATTEMPTS: [Duration; 3] = [
    Duration::from_millis(200),
    Duration::from_millis(500),
    Duration::from_millis(1500),
];

/// Moves on whenever the windows wanted change, stopping older attempts: the
/// main window, which shares this process, keeps its border.
static WANTED: AtomicU64 = AtomicU64::new(0);

fn socket() -> Option<PathBuf> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")?;
    let instance = std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE")?;
    Some(
        PathBuf::from(runtime)
            .join("hypr")
            .join(instance)
            .join(".socket.sock"),
    )
}

fn request(socket: &PathBuf, text: &str) -> std::io::Result<String> {
    let mut stream = UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(Duration::from_secs(1)))?;
    stream.set_write_timeout(Some(Duration::from_secs(1)))?;
    stream.write_all(text.as_bytes())?;
    let mut reply = String::new();
    stream.read_to_string(&mut reply)?;
    Ok(reply)
}

/// This process's windows in Hyprland's list of clients.
fn addresses(clients: &str, pid: u32) -> Vec<String> {
    let Ok(serde_json::Value::Array(clients)) = serde_json::from_str(clients) else {
        return Vec::new();
    };
    clients
        .iter()
        .filter(|client| client["pid"].as_u64() == Some(u64::from(pid)))
        .filter_map(|client| client["address"].as_str())
        .filter(|address| address.bytes().all(|byte| byte.is_ascii_alphanumeric()))
        .map(str::to_owned)
        .collect()
}

fn bare(address: &str) -> String {
    let set = |prop: &str, value: &str| {
        format!(
            "hl.dispatch(hl.dsp.window.set_prop({{ prop = \"{prop}\", value = \"{value}\", \
             window = \"address:{address}\" }}))"
        )
    };
    format!(
        "/eval {} {} {}",
        set("border_size", "0"),
        set("rounding", "0"),
        set("no_shadow", "1"),
    )
}

/// Takes the border off this process's windows, off the window's thread.
pub fn bare_skin_windows() {
    if cfg!(test) {
        return;
    }
    let Some(socket) = socket() else {
        return;
    };
    let wanted = WANTED.fetch_add(1, Ordering::Relaxed) + 1;
    std::thread::spawn(move || {
        for wait in ATTEMPTS {
            std::thread::sleep(wait);
            if WANTED.load(Ordering::Relaxed) != wanted {
                return;
            }
            let clients = match request(&socket, "j/clients") {
                Ok(clients) => clients,
                Err(error) => {
                    log::debug!("could not list Hyprland's windows: {error}");
                    return;
                }
            };
            for address in addresses(&clients, std::process::id()) {
                if let Err(error) = request(&socket, &bare(&address)) {
                    log::debug!("could not take the border off a skin: {error}");
                }
            }
        }
    });
}

/// Stops any attempt under way: the next window is the main one.
pub fn keep_borders() {
    WANTED.fetch_add(1, Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_this_process_windows_are_touched() {
        let clients = r#"[
            {"address": "0xa1", "pid": 7},
            {"address": "0xb2", "pid": 8},
            {"address": "0xc3\" }))", "pid": 7}
        ]"#;
        assert_eq!(addresses(clients, 7), ["0xa1"]);
        assert!(addresses("not json", 7).is_empty());
    }

    #[test]
    fn the_request_names_the_window_each_time() {
        let text = bare("0xa1");
        assert!(text.starts_with("/eval "));
        assert_eq!(text.matches("window = \"address:0xa1\"").count(), 3);
    }
}
