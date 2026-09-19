//! Where Spotifast keeps its files.
//!
//! Configuration, durable non-secret state, and disposable caches live in the
//! platform's conventional directories. Spotify grants use the platform store;
//! the token paths below are retained only for migration and sign-out cleanup.

use std::path::PathBuf;

use directories::ProjectDirs;

#[derive(Clone, Debug)]
pub struct AppDirs {
    pub config: PathBuf,
    pub state: PathBuf,
    pub cache: PathBuf,
}

impl AppDirs {
    pub fn discover() -> Self {
        // Keep the established paths so upgrades reuse settings and credentials.
        let project = ProjectDirs::from("me", "paolino", "fastpotify");
        match project {
            Some(project) => Self {
                config: project.config_dir().to_path_buf(),
                state: project
                    .state_dir()
                    .map(|path| path.to_path_buf())
                    .unwrap_or_else(|| project.data_local_dir().to_path_buf()),
                cache: project.cache_dir().to_path_buf(),
            },
            None => {
                let fallback = std::env::current_dir().unwrap_or_default();
                Self {
                    config: fallback.join("fastpotify-config"),
                    state: fallback.join("fastpotify-state"),
                    cache: fallback.join("fastpotify-cache"),
                }
            }
        }
    }

    pub fn settings_file(&self) -> PathBuf {
        self.config.join("settings.json")
    }

    /// Winamp skins the listener has added, as `.wsz` files or folders.
    pub fn skins_dir(&self) -> PathBuf {
        self.config.join("skins")
    }

    /// MilkDrop presets, as `.milk` files, in folders or not, with any
    /// textures they use in a `textures` folder inside.
    pub fn milkdrop_dir(&self) -> PathBuf {
        self.config.join("milkdrop")
    }

    pub fn session_file(&self) -> PathBuf {
        self.state.join("session.json")
    }

    /// What was played here, which Spotify never hears about and so
    /// cannot tell us later. See [`crate::history`].
    pub fn history_file(&self) -> PathBuf {
        self.state.join("history.json")
    }

    pub fn shared_web_token_file(&self) -> PathBuf {
        self.state.join("shared_web_api_token.json")
    }

    pub fn personal_web_token_file(&self) -> PathBuf {
        self.state.join("personal_web_api_token.json")
    }

    pub fn legacy_web_token_file(&self) -> PathBuf {
        self.state.join("web_api_token.json")
    }

    /// The log of the current run, replaced at every start.
    pub fn log_file(&self) -> PathBuf {
        self.state.join("fastpotify.log")
    }

    /// Where a panic is recorded before the process dies of it.
    pub fn panic_log(&self) -> PathBuf {
        self.state.join("panic.log")
    }

    pub fn credentials_dir(&self) -> PathBuf {
        self.state.join("credentials")
    }

    /// Optional proxy password, owner-only, never written to settings.json.
    pub fn proxy_secret_file(&self) -> PathBuf {
        self.state.join("proxy_password")
    }

    pub fn volume_dir(&self) -> PathBuf {
        self.state.join("volume")
    }

    pub fn audio_cache_dir(&self) -> PathBuf {
        self.cache.join("audio")
    }

    pub fn art_cache_dir(&self) -> PathBuf {
        self.cache.join("art")
    }

    pub fn lyrics_cache_dir(&self) -> PathBuf {
        self.cache.join("lyrics")
    }

    pub fn playlist_cache_dir(&self) -> PathBuf {
        self.cache.join("playlists")
    }

    pub fn account_playlist_cache_dir(&self, account_id: &str) -> PathBuf {
        self.playlist_cache_dir().join(account_id)
    }

    pub fn liked_songs_cache_file(&self, account_id: &str) -> PathBuf {
        // Hex encoding also keeps unusual account IDs within the cache root.
        let account: String = account_id
            .bytes()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        self.cache
            .join("liked-songs")
            .join(format!("{account}.json"))
    }

    pub fn ensure(&self) -> std::io::Result<()> {
        for dir in [&self.config, &self.state, &self.cache] {
            std::fs::create_dir_all(dir)?;
        }
        Ok(())
    }
}

/// Check a cache folder chosen by the listener before anything writes in it.
///
/// `~` and `~/...` mean the home directory. The picker returns an absolute
/// path, so a relative one is refused. The folder must already exist, be a
/// directory, and accept a file: a read-only folder or a disk that is not
/// there is caught now instead of failing on every cache write later.
///
/// The reason is a clause with no final stop, so callers can put it inside a
/// sentence: `Err("it cannot be written to")`.
pub fn check_cache_folder(raw: &str) -> Result<PathBuf, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("its path is empty".into());
    }
    let folder = expand_home(trimmed);
    if !folder.is_absolute() {
        return Err("its path is not absolute".into());
    }
    match std::fs::metadata(&folder) {
        Ok(metadata) if metadata.is_dir() => {}
        Ok(_) => return Err("it is not a folder".into()),
        Err(_) => return Err("it does not exist".into()),
    }
    if folder.to_str().is_none() {
        // A path that cannot be written into settings.json is no cache folder.
        return Err("its path cannot be represented".into());
    }
    // A file of our own, removed at once: the folder has to accept writes
    // before any cache is pointed at it.
    let probe = folder.join(format!(".spotifast-write-{}", std::process::id()));
    let written = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
        .map(|_| ());
    let _ = std::fs::remove_file(&probe);
    written.map_err(|_| "it cannot be written to".to_string())?;
    Ok(folder)
}

/// `~` and `~/rest` as the home directory. Anything else is left alone.
fn expand_home(raw: &str) -> PathBuf {
    if raw == "~" {
        return home_dir().unwrap_or_else(|| PathBuf::from(raw));
    }
    match raw.strip_prefix("~/").or_else(|| raw.strip_prefix("~\\")) {
        Some(rest) => home_dir()
            .map(|home| home.join(rest))
            .unwrap_or_else(|| PathBuf::from(raw)),
        None => PathBuf::from(raw),
    }
}

fn home_dir() -> Option<PathBuf> {
    // The same crate that already answers where the platform keeps things.
    directories::BaseDirs::new().map(|dirs| dirs.home_dir().to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::check_cache_folder;
    use std::path::PathBuf;

    fn scratch(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("fastpotify-{name}-test-{}", std::process::id()))
    }

    #[test]
    fn a_missing_or_relative_or_empty_folder_is_refused() {
        assert_eq!(check_cache_folder("").unwrap_err(), "its path is empty");
        assert_eq!(check_cache_folder("   ").unwrap_err(), "its path is empty");
        assert_eq!(
            check_cache_folder("cache").unwrap_err(),
            "its path is not absolute"
        );
        let missing = scratch("absent-cache");
        let _ = std::fs::remove_dir_all(&missing);
        assert_eq!(
            check_cache_folder(&missing.to_string_lossy()).unwrap_err(),
            "it does not exist"
        );
    }

    #[test]
    fn a_file_is_not_a_cache_folder() {
        let file = scratch("cache-file");
        std::fs::write(&file, b"not a folder").unwrap();
        assert_eq!(
            check_cache_folder(&file.to_string_lossy()).unwrap_err(),
            "it is not a folder"
        );
        let _ = std::fs::remove_file(&file);
    }

    #[test]
    fn an_existing_folder_is_accepted_and_keeps_no_probe_file() {
        let folder = scratch("cache-ok");
        std::fs::create_dir_all(&folder).unwrap();
        let checked = check_cache_folder(&folder.to_string_lossy()).unwrap();
        assert_eq!(checked, folder);
        let leftovers: Vec<_> = std::fs::read_dir(&folder)
            .unwrap()
            .filter_map(|entry| entry.ok())
            .collect();
        assert!(leftovers.is_empty());
        let _ = std::fs::remove_dir_all(&folder);
    }
}
