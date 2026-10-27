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
use std::sync::Mutex;

type UpdaterCache = Mutex<Option<(crate::settings::ProxyConfig, Updater)>>;
static UPDATER: UpdaterCache = Mutex::new(None);

pub const CONFIG: UpdateConfig = UpdateConfig {
    macos: MacConfig {
        bundle_ids: &["rocks.spotifast.Spotifast"],
        executable_names: &["Spotifast"],
        legacy_bundle_names: &[],
    },
    // Releases publish an Ed25519 signature for checksums.txt. Require it
    // before downloading an update, using the release workflow's public key.
    publisher_key: Some(include_str!("../assets/update-public-key.hex")),
    // Releases build their AppImages before publishing, so the signed
    // checksums.txt lists them (#737): an AppImage replaces itself with the
    // next one.
    appimage: true,
    ..UpdateConfig::new(
        "crmne/spotifast",
        "Spotifast",
        "spotifast",
        env!("CARGO_PKG_VERSION"),
    )
};

/// An updater on Spotifast's HTTP client, through the configured proxy.
pub fn updater(proxy: &crate::settings::ProxyConfig) -> anyhow::Result<Updater> {
    cached_updater(&UPDATER, proxy, || {
        let builder = crate::http::blocking_builder(proxy).map_err(anyhow::Error::msg)?;
        Ok(Updater::new(CONFIG, ReqwestTransport::new(builder)?))
    })
}

// Check, download, and handoff share the HTTP runtime, certificate store,
// and connection pool. A changed proxy policy discards the previous client;
// a failed replacement must never fall back to that client's route.
fn cached_updater(
    cache: &UpdaterCache,
    proxy: &crate::settings::ProxyConfig,
    create: impl FnOnce() -> anyhow::Result<Updater>,
) -> anyhow::Result<Updater> {
    let mut cached = cache.lock().unwrap_or_else(|lock| lock.into_inner());
    if let Some((policy, updater)) = cached.as_ref()
        && policy == proxy
    {
        return Ok(updater.clone());
    }
    *cached = None;
    let updater = create()?;
    *cached = Some((proxy.clone(), updater.clone()));
    Ok(updater)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct NoNetwork;

    impl fastframe_update::Transport for NoNetwork {
        fn get(
            &self,
            _: &fastframe_update::Request<'_>,
        ) -> anyhow::Result<fastframe_update::Response> {
            panic!("cache tests must never send a request")
        }
    }

    #[test]
    fn update_operations_reuse_the_client_for_the_same_proxy() {
        let cache = Mutex::new(None);
        let proxy = crate::settings::ProxyConfig::Off;
        let first = cached_updater(&cache, &proxy, || Ok(Updater::new(CONFIG, NoNetwork))).unwrap();
        let second = cached_updater(&cache, &proxy, || panic!("client was rebuilt")).unwrap();
        // Setting a test feed affects only the returned clone, not the cache.
        let local = Source::local("http://127.0.0.1:8123/").unwrap();
        assert_eq!(first.with_source(local.clone()).source(), &local);
        assert!(second.source().is_github());
    }

    #[test]
    fn proxy_changes_replace_the_client_and_failures_do_not_reuse_it() {
        let cache = Mutex::new(None);
        let off = crate::settings::ProxyConfig::Off;
        let system = crate::settings::ProxyConfig::System;
        cached_updater(&cache, &off, || Ok(Updater::new(CONFIG, NoNetwork))).unwrap();
        let error = cached_updater(&cache, &system, || anyhow::bail!("invalid proxy")).unwrap_err();
        assert_eq!(error.to_string(), "invalid proxy");
        assert!(cache.lock().unwrap().is_none());
        cached_updater(&cache, &system, || Ok(Updater::new(CONFIG, NoNetwork))).unwrap();
        cached_updater(&cache, &system, || panic!("replacement was not cached")).unwrap();
    }

    #[test]
    fn update_config_is_valid() {
        CONFIG.validate().unwrap();
        assert_eq!(CONFIG.current_version, env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn updates_require_the_release_signing_key() {
        assert!(
            CONFIG.publisher_key.is_some(),
            "updates must require a publisher signature"
        );
        CONFIG.validate().unwrap();
    }

    /// An AppImage may replace itself only because the release workflow
    /// builds the AppImages into the signed checksums.txt;
    /// packaging/test-release-names.py checks that side.
    #[test]
    fn appimages_update_themselves() {
        const { assert!(CONFIG.appimage) };
    }
}
