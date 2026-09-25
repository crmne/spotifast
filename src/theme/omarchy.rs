//! The part of the Omarchy set-up fastframe-theme leaves to Spotifast:
//! upgrading the theme hook a Fastpotify-era package installed.

use std::{
    fs,
    io::{self, Read, Write},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
};

/// Rewrites the user's Omarchy hook when it is exactly the one a
/// Fastpotify-era package installed, which copied palettes into
/// `fastpotify/themes`. fastframe-theme installs the packaged hook only
/// where none exists, so without this the old one would stay forever. A hook
/// the user edited is left alone.
pub(super) fn upgrade_legacy_hook() {
    let Some(assets) = packaged_assets() else {
        return;
    };
    let Some(home) = directories::BaseDirs::new().map(|dirs| dirs.home_dir().to_path_buf()) else {
        return;
    };
    if let Err(error) = upgrade(&assets, &home) {
        log::warn!("unable to upgrade the Omarchy theme hook: {error}");
    }
}

/// `<prefix>/share/spotifast/omarchy` beside the running `<prefix>/bin`.
fn packaged_assets() -> Option<PathBuf> {
    let executable = std::env::current_exe().ok()?;
    Some(
        executable
            .parent()?
            .parent()?
            .join("share/spotifast/omarchy"),
    )
}

fn upgrade(assets: &Path, home: &Path) -> io::Result<()> {
    // Only an installed package on a configured Omarchy desktop, as when
    // the hook was installed. Cargo builds and other desktops do nothing.
    if !assets.is_dir()
        || !home.join(".config/omarchy").is_dir()
        || !home.join(".local/state/omarchy/current/theme").is_dir()
    {
        return Ok(());
    }
    let hook_path = home.join(".config/omarchy/hooks/theme-set.d/spotifast-theme");
    if !fs::symlink_metadata(&hook_path).is_ok_and(|metadata| metadata.is_file()) {
        return Ok(());
    }
    let hook = read_small(&assets.join("spotifast-theme"))?;
    let previous = hook.replace("/spotifast/themes", "/fastpotify/themes");
    if read_small(&hook_path)? != previous {
        return Ok(());
    }
    let temporary = hook_path.with_extension(format!("migration-{}.tmp", rand::random::<u64>()));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o755)
        .open(&temporary)?;
    file.write_all(hook.as_bytes())?;
    file.sync_all()?;
    fs::rename(temporary, &hook_path)
}

fn read_small(path: &Path) -> io::Result<String> {
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(64 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 64 * 1024 {
        return Err(io::Error::other("Omarchy theme file exceeds 64 KiB"));
    }
    String::from_utf8(bytes).map_err(io::Error::other)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    const HOOK: &str = include_str!("../../contrib/omarchy/spotifast-theme");

    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            let path = std::env::temp_dir()
                .join(format!("spotifast omarchy setup {}", rand::random::<u64>()));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn only_the_exact_fastpotify_hook_is_upgraded() {
        let root = Scratch::new();
        let assets = root.0.join("package/share/spotifast/omarchy");
        let home = root.0.join("user");
        let hooks = home.join(".config/omarchy/hooks/theme-set.d");
        let hook = hooks.join("spotifast-theme");
        let legacy = HOOK.replace("/spotifast/themes", "/fastpotify/themes");
        assert_ne!(legacy, HOOK);

        // No package, no Omarchy: nothing is touched.
        upgrade(&assets, &home).unwrap();
        assert!(!home.exists());

        for path in [
            &assets,
            &hooks,
            &home.join(".local/state/omarchy/current/theme"),
        ] {
            fs::create_dir_all(path).unwrap();
        }
        fs::write(assets.join("spotifast-theme"), HOOK).unwrap();
        // No hook installed: installing one is fastframe-theme's job.
        upgrade(&assets, &home).unwrap();
        assert!(!hook.exists());

        fs::write(&hook, &legacy).unwrap();
        upgrade(&assets, &home).unwrap();
        assert_eq!(fs::read_to_string(&hook).unwrap(), HOOK);
        assert_ne!(fs::metadata(&hook).unwrap().permissions().mode() & 0o100, 0);
        assert_eq!(fs::read_dir(&hooks).unwrap().count(), 1);

        fs::write(&hook, "user hook").unwrap();
        upgrade(&assets, &home).unwrap();
        assert_eq!(fs::read_to_string(&hook).unwrap(), "user hook");
    }
}
