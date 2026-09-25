//! Self-update from GitHub releases, through fastframe-update.
//!
//! The previous release's helper installs the first release built on the
//! crate, so what it relies on stays the same: `--version` prints
//! `<command> <version>`, the new app accepts `--update-receipt` and
//! `--update-error` (stripped by [`fastframe_update::intercept`] before the
//! argument parser), and the handoff and receipt files keep their format.

pub use fastframe_update::{
    CHECK_INTERVAL, DownloadState, Installation, Kind, Prepared, Release, Source, Unsupported,
    Updater,
};
use fastframe_update::{MacConfig, ReqwestTransport, UpdateConfig};

pub const CONFIG: UpdateConfig = UpdateConfig {
    // Fastpotify, the name before the rename: its marker files, `fastpotify
    // <version>` answers, and its Windows setup program's location.
    legacy_names: &["fastpotify"],
    legacy_windows_installs: &["Programs/Fastpotify/fastpotify.exe"],
    macos: MacConfig {
        bundle_ids: &["rocks.spotifast.Spotifast", "me.paolino.fastpotify"],
        // 0.9.1 kept "fastpotify" for older clients' validation; later
        // releases may rename it to "Spotifast" (#538).
        executable_names: &["fastpotify", "Spotifast"],
        legacy_bundle_names: &["Fastpotify.app"],
    },
    // Releases are verified against checksums.txt alone until they are
    // signed. Only a version shipped after the first signed release may
    // carry the key: from then on an unsigned release is refused.
    publisher_key: None,
    ..UpdateConfig::new(
        "crmne/spotifast",
        "Spotifast",
        "spotifast",
        env!("CARGO_PKG_VERSION"),
    )
};

/// An updater on Spotifast's HTTP client, through the configured proxy.
pub fn updater(proxy: &crate::settings::ProxyConfig) -> anyhow::Result<Updater> {
    let builder = crate::http::blocking_builder(proxy).map_err(anyhow::Error::msg)?;
    Ok(Updater::new(CONFIG, ReqwestTransport::new(builder)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_config_is_valid() {
        CONFIG.validate().unwrap();
        assert_eq!(CONFIG.current_version, env!("CARGO_PKG_VERSION"));
    }
}
