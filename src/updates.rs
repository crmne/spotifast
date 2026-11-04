//! Self-update from GitHub releases, through fastframe-update.
//!
//! The previous release's helper installs the first release built on the
//! crate, so what it relies on stays the same: `--version` prints
//! `<command> <version>`, the new app accepts `--update-receipt` and
//! `--update-error` (stripped by [`fastframe_update::intercept`] before the
//! argument parser), and the handoff and receipt files keep their format.
//!
//! A copy that can't replace itself (a package manager's, a Flatpak, an
//! AppImage someone else owns) still hears about each release, once its
//! signed `checksums.txt` verifies ([`Updater::verify`]). The update window
//! then says how it was installed and shows the command that updates it,
//! with a Copy button ([`Notice`]); Spotifast never runs it.

//!
//! A copy that can replace itself updates on its own, by fastframe-update's
//! shared rules ([`policy`]): a newer release on the same major version
//! downloads in the background (not on a metered connection or without
//! internet) and installs at the next start, before the window opens,
//! unless the person turned **Update automatically** off or went back to an
//! older version on purpose. Restart to update still installs it at once.

pub use fastframe_update::policy::{self, Offer};
pub use fastframe_update::{
    CHECK_INTERVAL, DownloadState, Installation, Kind, Manager, Prepared, Release, Source,
    Unsupported, Updater,
};
use fastframe_update::{MacConfig, ManagedUpdate, ReqwestTransport, UpdateConfig};
use std::path::Path;
use std::sync::Mutex;

/// The version this build is.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Where the release files are.
const DOWNLOADS: &str = "https://github.com/crmne/spotifast/releases/download";

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

/// Rebuilds the updater on its next use after proxy settings are reapplied.
pub fn forget_updater() {
    forget_cached_updater(&UPDATER);
}

fn forget_cached_updater(cache: &UpdaterCache) {
    *cache.lock().unwrap_or_else(|lock| lock.into_inner()) = None;
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

/// How `release` reaches a copy that can install it: on its own
/// ([`Offer::Automatic`]) or when the person asks. `newest_run` is the
/// newest version that ran here ([`Settings::newest_run`](crate::settings::Settings::newest_run)).
pub fn offer(release: &str, automatic: bool, newest_run: Option<&str>) -> Offer {
    policy::offer(VERSION, release, automatic, Ok(()), newest_run)
}

/// At a start, before anything opens: hands the update an earlier run
/// downloaded to the helper when it installs on its own. `true` when the
/// helper took it and this process must quit now; it relaunches Spotifast
/// with `arguments` once the new version is in place.
pub fn install_kept(automatic: bool, newest_run: Option<&str>, arguments: Vec<String>) -> bool {
    let updater = Updater::new(CONFIG, NoNetwork);
    let prepared = match updater.kept() {
        Ok(Some(prepared)) => prepared,
        Ok(None) => return false,
        Err(error) => {
            log::warn!("the update downloaded earlier failed its checks: {error:#}");
            return false;
        }
    };
    let version = prepared.version().to_owned();
    if !policy::installs_at_start(VERSION, &version, automatic, newest_run, true) {
        log::info!("keeping the downloaded update {version} for when it is asked for");
        return false;
    }
    log::info!("installing the update {version} downloaded earlier");
    match updater.handoff(prepared, arguments) {
        Ok(()) => true,
        Err(error) => {
            log::warn!("could not install the update {version} at start: {error:#}");
            false
        }
    }
}

/// A transport for work that never reaches the network: a kept download
/// and its handoff are checked against what was stored beside the app.
struct NoNetwork;

impl fastframe_update::Transport for NoNetwork {
    fn get(&self, _: &fastframe_update::Request<'_>) -> anyhow::Result<fastframe_update::Response> {
        anyhow::bail!("installing at start reads nothing from the network")
    }
}

/// Whether this copy can install the release it was checked against, or
/// what it says instead.
pub type Support = Result<Installation, Notice>;

/// What the update window says for a copy that can't install a release.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notice {
    /// Why, in fastframe-update's English sentence, or the error that kept
    /// the app from finding out. Shown when there is no [`Route`].
    pub reason: String,
    /// How this copy gets the release instead, when the computer tells.
    pub route: Option<Route>,
}

impl Notice {
    /// A failure to find out whether this copy can update itself.
    pub fn failed(error: impl Into<String>) -> Self {
        Self {
            reason: error.into(),
            route: None,
        }
    }
}

/// How a copy that can't replace itself gets a release.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Route {
    /// A package manager with a repository: pacman and the AUR, COPR,
    /// Homebrew, Nix, Snap, cargo. `command` is `None` where there is no
    /// one command (NixOS, Home Manager).
    Managed {
        manager: Manager,
        command: Option<String>,
    },
    /// The release's DEB package: there is no APT repository to upgrade
    /// from, so the new one is downloaded and installed with apt.
    Deb { download: String, command: String },
    /// Fedora COPR (paolino/spotifast), which dnf upgrades from. `command`
    /// is `None` when dnf can't tell the package (no `/etc/os-release`).
    Copr { command: Option<String> },
    /// The release's own RPM package, installed from its address.
    Rpm { command: String },
    /// The release's Flatpak bundle: no Flatpak remote publishes Spotifast
    /// yet, so `flatpak update` would find nothing.
    FlatpakBundle { download: String, command: String },
}

impl Route {
    /// The command that updates this copy, for the Copy button.
    pub fn command(&self) -> Option<&str> {
        match self {
            Self::Managed { command, .. } | Self::Copr { command } => command.as_deref(),
            Self::Deb { command, .. }
            | Self::Rpm { command }
            | Self::FlatpakBundle { command, .. } => Some(command),
        }
    }

    /// The file to download before running the command.
    pub fn download(&self) -> Option<&str> {
        match self {
            Self::Deb { download, .. } | Self::FlatpakBundle { download, .. } => Some(download),
            Self::Managed { .. } | Self::Copr { .. } | Self::Rpm { .. } => None,
        }
    }
}

/// Whether this copy can install `release`. Only a release whose
/// `checksums.txt` verifies with the key compiled in, and lists a download
/// of that version for this computer, is announced at all: a failure here
/// is the check's failure.
pub fn support(updater: &Updater, release: &Release) -> anyhow::Result<Support> {
    let verified = updater.verify(release)?;
    Ok(updater
        .installable(&verified)
        .map_err(|reason| notice(updater, &reason, &release.version)))
}

/// The notice for a copy that can't install `version`, for `reason`.
pub fn notice(updater: &Updater, reason: &Unsupported, version: &str) -> Notice {
    log::info!("update {version} available; this copy can't install it: {reason:?}");
    let managed = updater.managed_update().map(Managed::from);
    let user = directories::UserDirs::new();
    let saved = |name: &str| {
        let home = user.as_ref().map(directories::UserDirs::home_dir);
        match user.as_ref().and_then(directories::UserDirs::download_dir) {
            Some(folder) => shell_path(&folder.join(name), home),
            None => format!("~/Downloads/{name}"),
        }
    };
    Notice {
        reason: reason.to_string(),
        route: route(managed, version, std::env::consts::ARCH, rpm_vendor, saved),
    }
}

/// fastframe-update's answer, kept as the parts the notice reads.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Managed {
    manager: Manager,
    command: Option<String>,
}

impl From<ManagedUpdate> for Managed {
    fn from(update: ManagedUpdate) -> Self {
        Self {
            manager: update.manager,
            command: update.command,
        }
    }
}

/// How a copy `managed` reports gets `version` on processor `arch`.
/// `vendor` reads the installed RPM's vendor, `saved` names where the
/// browser saves a download.
fn route(
    managed: Option<Managed>,
    version: &str,
    arch: &str,
    vendor: impl FnOnce() -> Option<String>,
    saved: impl Fn(&str) -> String,
) -> Option<Route> {
    let managed = managed?;
    let file = |name: &str| format!("{DOWNLOADS}/v{version}/{name}");
    // The release's own files carry the version into a path and a command.
    let release_file = plain_version(version) && matches!(arch, "x86_64" | "aarch64");
    match managed.manager {
        Manager::Apt if release_file => {
            let arch = if arch == "aarch64" { "arm64" } else { "amd64" };
            let name = format!("spotifast_{version}_{arch}.deb");
            Some(Route::Deb {
                command: format!("sudo apt install {}", saved(&name)),
                download: file(&name),
            })
        }
        Manager::Flatpak if release_file && arch == "x86_64" => {
            let name = format!("spotifast-v{version}-x86_64.flatpak");
            Some(Route::FlatpakBundle {
                command: format!("flatpak install --user {}", saved(&name)),
                download: file(&name),
            })
        }
        // COPR stamps its own vendor ("Fedora Copr - user paolino"); the
        // release's RPM, made by nFPM, has none.
        Manager::Dnf => {
            let vendor = vendor().unwrap_or_default();
            Some(if vendor.to_ascii_lowercase().contains("copr") {
                Route::Copr {
                    command: managed.command,
                }
            } else if release_file && (vendor.is_empty() || vendor == "(none)") {
                Route::Rpm {
                    command: format!(
                        "sudo dnf install {}",
                        file(&format!("spotifast-{version}-1.{arch}.rpm"))
                    ),
                }
            } else {
                Route::Managed {
                    manager: Manager::Dnf,
                    command: managed.command,
                }
            })
        }
        // apt and Flatpak would only find the installed version again.
        Manager::Apt | Manager::Flatpak => None,
        manager => Some(Route::Managed {
            manager,
            command: managed.command,
        }),
    }
}

/// The vendor of the RPM that owns the running executable.
fn rpm_vendor() -> Option<String> {
    let executable = std::env::current_exe().ok()?.canonicalize().ok()?;
    let output = std::process::Command::new("rpm")
        .args(["-qf", "--qf", "%{VENDOR}"])
        .arg(executable)
        .env("LC_ALL", "C")
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// Only `X.Y.Z` reaches a file name or a command.
fn plain_version(version: &str) -> bool {
    let parts: Vec<&str> = version.split('.').collect();
    parts.len() == 3
        && parts.iter().all(|part| {
            !part.is_empty() && part.len() <= 9 && part.bytes().all(|b| b.is_ascii_digit())
        })
}

/// `path` as a shell reads it: `~/...` under the home folder when nothing
/// in it needs quoting, else single-quoted.
fn shell_path(path: &Path, home: Option<&Path>) -> String {
    let plain = |text: &str| {
        text.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"/-_.+@,:".contains(&b))
    };
    if let Some(rest) = home.and_then(|home| path.strip_prefix(home).ok())
        && let Some(rest) = rest.to_str()
        && plain(rest)
    {
        return format!("~/{rest}");
    }
    let text = path.to_string_lossy();
    if plain(&text) {
        return text.into_owned();
    }
    format!("'{}'", text.replace('\'', r"'\''"))
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
    fn reapplying_system_proxy_rebuilds_the_cached_client() {
        let cache = Mutex::new(None);
        let proxy = crate::settings::ProxyConfig::System;
        cached_updater(&cache, &proxy, || Ok(Updater::new(CONFIG, NoNetwork))).unwrap();
        forget_cached_updater(&cache);
        assert!(cache.lock().unwrap().is_none());
        let mut rebuilt = false;
        cached_updater(&cache, &proxy, || {
            rebuilt = true;
            Ok(Updater::new(CONFIG, NoNetwork))
        })
        .unwrap();
        assert!(
            rebuilt,
            "the same policy must resolve the system proxy again"
        );
        cached_updater(&cache, &proxy, || panic!("replacement was not cached")).unwrap();
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

    /// A start with nothing downloaded starts as usual.
    #[test]
    fn a_start_with_nothing_downloaded_installs_nothing() {
        assert!(!install_kept(true, Some(VERSION), Vec::new()));
    }

    /// Spotifast's releases stay on major version 0, so every release
    /// installs on its own until the person turns it off or goes back.
    #[test]
    fn a_newer_release_installs_on_its_own_unless_the_person_went_back() {
        let major: u64 = VERSION.split('.').next().unwrap().parse().unwrap();
        let next = format!("{major}.999.0");
        assert_eq!(offer(&next, true, Some(VERSION)), Offer::Automatic);
        assert_ne!(offer(&next, false, Some(VERSION)), Offer::Automatic);
        // Went back from `next`: offered, never installed on its own, and
        // its download is not installed at the next start either.
        assert_ne!(offer(&next, true, Some(&next)), Offer::Automatic);
        assert!(!policy::installs_at_start(
            VERSION,
            &next,
            true,
            Some(&next),
            true
        ));
        assert!(policy::installs_at_start(
            VERSION,
            &next,
            true,
            Some(VERSION),
            true
        ));
    }

    fn managed(manager: Manager, command: &str) -> Option<Managed> {
        Some(Managed {
            manager,
            command: Some(command.into()),
        })
    }

    fn saved(name: &str) -> String {
        format!("~/Downloads/{name}")
    }

    fn no_vendor() -> Option<String> {
        panic!("only an RPM install asks for its vendor")
    }

    #[test]
    fn a_package_manager_with_a_repository_gets_its_own_command() {
        for (manager, command) in [
            (Manager::Aur, "yay -Syu spotifast-bin"),
            (Manager::Pacman, "sudo pacman -Syu spotifast"),
            (Manager::HomebrewCask, "brew upgrade --cask spotifast"),
            (Manager::Nix, "nix profile upgrade spotifast"),
            (Manager::Cargo, "cargo install spotifast"),
        ] {
            let route = route(
                managed(manager, command),
                "0.18.0",
                "x86_64",
                no_vendor,
                saved,
            )
            .unwrap();
            assert_eq!(route.command(), Some(command), "{manager:?}");
            assert_eq!(route.download(), None, "{manager:?}");
        }
        // NixOS and Home Manager have no one command.
        let nixos = route(
            Some(Managed {
                manager: Manager::Nix,
                command: None,
            }),
            "0.18.0",
            "x86_64",
            no_vendor,
            saved,
        );
        assert_eq!(
            nixos,
            Some(Route::Managed {
                manager: Manager::Nix,
                command: None
            })
        );
    }

    #[test]
    fn copr_upgrades_through_dnf_and_the_release_rpm_installs_the_new_file() {
        let copr = || Some("Fedora Copr - user paolino".to_owned());
        let dnf = "sudo dnf upgrade --refresh spotifast";
        assert_eq!(
            route(managed(Manager::Dnf, dnf), "0.18.0", "x86_64", copr, saved),
            Some(Route::Copr {
                command: Some(dnf.into())
            })
        );
        // Another vendor's package (Fedora's own, say) is plain dnf.
        let fedora = || Some("Fedora Project".to_owned());
        assert_eq!(
            route(
                managed(Manager::Dnf, dnf),
                "0.18.0",
                "x86_64",
                fedora,
                saved
            ),
            Some(Route::Managed {
                manager: Manager::Dnf,
                command: Some(dnf.into())
            })
        );
        for vendor in [None, Some(String::new()), Some("(none)".to_owned())] {
            assert_eq!(
                route(
                    managed(Manager::Dnf, dnf),
                    "0.18.0",
                    "aarch64",
                    || vendor.clone(),
                    saved
                ),
                Some(Route::Rpm {
                    command: "sudo dnf install https://github.com/crmne/spotifast/releases/download/v0.18.0/spotifast-0.18.0-1.aarch64.rpm".into()
                }),
                "{vendor:?}"
            );
        }
    }

    #[test]
    fn the_release_deb_and_flatpak_are_downloaded_then_installed() {
        let deb = route(
            managed(
                Manager::Apt,
                "sudo apt update && sudo apt install --only-upgrade spotifast",
            ),
            "0.18.0",
            "aarch64",
            no_vendor,
            saved,
        )
        .unwrap();
        assert_eq!(
            deb,
            Route::Deb {
                download:
                    "https://github.com/crmne/spotifast/releases/download/v0.18.0/spotifast_0.18.0_arm64.deb"
                        .into(),
                command: "sudo apt install ~/Downloads/spotifast_0.18.0_arm64.deb".into(),
            }
        );
        let flatpak = route(
            managed(Manager::Flatpak, "flatpak update rocks.spotifast.Spotifast"),
            "0.18.0",
            "x86_64",
            no_vendor,
            saved,
        )
        .unwrap();
        assert_eq!(
            flatpak.download(),
            Some(
                "https://github.com/crmne/spotifast/releases/download/v0.18.0/spotifast-v0.18.0-x86_64.flatpak"
            )
        );
        assert_eq!(
            flatpak.command(),
            Some("flatpak install --user ~/Downloads/spotifast-v0.18.0-x86_64.flatpak")
        );
        // The release has no ARM Flatpak.
        assert_eq!(
            route(
                managed(Manager::Flatpak, "flatpak update rocks.spotifast.Spotifast"),
                "0.18.0",
                "aarch64",
                no_vendor,
                saved
            ),
            None
        );
    }

    #[test]
    fn a_copy_no_package_manager_owns_keeps_the_reason() {
        assert_eq!(route(None, "0.18.0", "x86_64", no_vendor, saved), None);
    }

    #[test]
    fn only_plain_versions_reach_a_release_file_command() {
        assert!(plain_version("0.18.0"));
        assert!(plain_version("10.0.123"));
        for bad in [
            "0.18",
            "0.18.0-rc.1",
            "0.18.0;rm",
            "v0.18.0",
            "",
            "0..1",
            "1.2.3.4",
        ] {
            assert!(!plain_version(bad), "{bad}");
            let deb = route(
                managed(Manager::Apt, "sudo apt install --only-upgrade spotifast"),
                bad,
                "x86_64",
                no_vendor,
                saved,
            );
            assert_eq!(deb, None, "{bad}");
        }
    }

    #[test]
    fn download_paths_are_pasted_as_one_word() {
        let home = Path::new("/home/ana");
        assert_eq!(
            shell_path(Path::new("/home/ana/Downloads/a.deb"), Some(home)),
            "~/Downloads/a.deb"
        );
        assert_eq!(
            shell_path(Path::new("/home/ana/Scaricati/a.deb"), None),
            "/home/ana/Scaricati/a.deb"
        );
        assert_eq!(
            shell_path(Path::new("/home/ana/My Downloads/a.deb"), Some(home)),
            "'/home/ana/My Downloads/a.deb'"
        );
        assert_eq!(
            shell_path(Path::new("/home/an'a/D/a.deb"), None),
            r"'/home/an'\''a/D/a.deb'"
        );
    }
}
