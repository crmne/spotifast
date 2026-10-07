//! Where Spotizgeg keeps its files.
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

/// The fork's name before it became Spotizgeg; its files are taken over.
const PREVIOUS_NAME: &str = "spotifast";

impl AppDirs {
    pub fn discover() -> Self {
        let dirs = Self::for_name("spotizgeg");
        dirs.adopt(&Self::for_name(PREVIOUS_NAME));
        dirs
    }

    /// Moves each of `previous`'s directories here while this one does not
    /// exist yet, so settings, history and caches survive the rename. A
    /// directory that cannot be moved is left where it was.
    fn adopt(&self, previous: &Self) {
        for (old, new) in [
            (&previous.config, &self.config),
            (&previous.state, &self.state),
            (&previous.cache, &self.cache),
        ] {
            if old == new || new.exists() || !old.is_dir() {
                continue;
            }
            let moved = new
                .parent()
                .map_or(Ok(()), std::fs::create_dir_all)
                .and_then(|()| std::fs::rename(old, new));
            if let Err(error) = moved {
                log::warn!(
                    "could not move {} to {}: {error}",
                    old.display(),
                    new.display()
                );
            }
        }
    }

    fn for_name(name: &str) -> Self {
        let project = ProjectDirs::from("me", "paolino", name);
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
                    config: fallback.join(format!("{name}-config")),
                    state: fallback.join(format!("{name}-state")),
                    cache: fallback.join(format!("{name}-cache")),
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
        self.state.join("spotizgeg.log")
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

#[cfg(test)]
mod tests {
    use super::*;

    fn dirs_in(root: &std::path::Path, name: &str) -> AppDirs {
        AppDirs {
            config: root.join("config").join(name),
            state: root.join("state").join(name),
            cache: root.join("cache").join(name),
        }
    }

    /// Settings and history kept under the fork's previous name move to the
    /// new one on first start, and a directory already there is kept as is.
    #[test]
    fn the_previous_names_files_move_to_the_new_one_once() {
        let root = std::env::temp_dir().join(format!(
            "spotizgeg-adopt-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let previous = dirs_in(&root, "old");
        let current = dirs_in(&root, "new");
        std::fs::create_dir_all(&previous.config).unwrap();
        std::fs::write(previous.settings_file(), "{\"volume\":37}").unwrap();
        std::fs::create_dir_all(&previous.state).unwrap();
        std::fs::write(previous.history_file(), "[]").unwrap();
        // The cache was made fresh already: the old one stays put.
        std::fs::create_dir_all(&previous.cache).unwrap();
        std::fs::create_dir_all(&current.cache).unwrap();

        current.adopt(&previous);
        assert_eq!(
            std::fs::read_to_string(current.settings_file()).unwrap(),
            "{\"volume\":37}"
        );
        assert!(current.history_file().is_file());
        assert!(!previous.config.exists() && !previous.state.exists());
        assert!(previous.cache.is_dir());

        // Nothing left to move, and what is here is not touched again.
        std::fs::write(current.settings_file(), "{}").unwrap();
        current.adopt(&previous);
        assert_eq!(
            std::fs::read_to_string(current.settings_file()).unwrap(),
            "{}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
