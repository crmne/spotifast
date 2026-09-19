use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, ensure};

use super::install::{self, Installation, Prepared};

const IDENTIFIER: &str = "me.paolino.fastpotify";

// The bundled executable is still "fastpotify", left over from before the
// app itself was renamed to Spotifast. A future release means to rename it
// to "Spotifast" too; this lands first so every current install already
// accepts that name (as opposed to the app bundle, already handled by
// `image_bundle`) once it does, rather than rejecting the download that
// renames it.
const EXECUTABLE_NAMES: [&str; 2] = ["fastpotify", "Spotifast"];

pub(super) fn bundle_root(executable: &Path) -> Result<&Path> {
    let root = executable
        .ancestors()
        .nth(3)
        .context("Missing app bundle")?;
    ensure!(
        root.extension().is_some_and(|extension| extension == "app")
            && EXECUTABLE_NAMES
                .iter()
                .any(|name| root.join("Contents/MacOS").join(name) == executable),
        "Move the app to Applications, then open it to update."
    );
    Ok(root)
}

/// Where a bundle's own `CFBundleExecutable` says its executable lives.
/// Never a literal name: a download can rename it (see `EXECUTABLE_NAMES`).
fn executable_path(bundle: &Path) -> Result<PathBuf> {
    let executable = plist(bundle, "CFBundleExecutable")?;
    Ok(bundle.join("Contents/MacOS").join(executable))
}

fn plist(bundle: &Path, key: &str) -> Result<String> {
    let output = Command::new("/usr/libexec/PlistBuddy")
        .args(["-c", &format!("Print :{key}")])
        .arg(bundle.join("Contents/Info.plist"))
        .output()?;
    ensure!(output.status.success(), "The app bundle is missing {key}");
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

fn identity(bundle: &Path) -> Result<()> {
    ensure!(
        plist(bundle, "CFBundleIdentifier")? == IDENTIFIER
            && EXECUTABLE_NAMES.contains(&plist(bundle, "CFBundleExecutable")?.as_str())
            && plist(bundle, "CFBundlePackageType")? == "APPL",
        "The download is not a Spotifast app bundle"
    );
    Ok(())
}

pub(super) fn detect(executable: &Path) -> Result<()> {
    ensure!(
        !executable.starts_with("/Volumes")
            && !executable
                .components()
                .any(|part| part.as_os_str() == "AppTranslocation"),
        "Move the app to Applications, then open it to update."
    );
    let bundle = bundle_root(executable)?;
    let prefixes = [
        Some(PathBuf::from("/opt/homebrew")),
        Some(PathBuf::from("/usr/local")),
        std::env::var_os("HOMEBREW_PREFIX").map(PathBuf::from),
    ];
    for prefix in prefixes.into_iter().flatten() {
        ensure!(
            !["fastpotify", "spotifast"]
                .iter()
                .any(|name| cask_owns(&prefix.join("Caskroom").join(name), bundle)),
            "Update this installation with Homebrew."
        );
    }
    identity(bundle)
}

fn cask_owns(cask: &Path, bundle: &Path) -> bool {
    let Ok(bundle) = bundle.canonicalize() else {
        return false;
    };
    fs::read_dir(cask).is_ok_and(|versions| {
        versions.flatten().any(|version| {
            ["Spotifast.app", "Fastpotify.app"].iter().any(|name| {
                version
                    .path()
                    .join(name)
                    .canonicalize()
                    .is_ok_and(|installed| installed == bundle)
            })
        })
    })
}

fn team(bundle: &Path) -> Result<Option<String>> {
    let verify = Command::new("/usr/bin/codesign")
        .args(["--verify", "--deep", "--strict"])
        .arg(bundle)
        .output()?;
    ensure!(
        verify.status.success(),
        "The app signature could not be verified"
    );
    let output = Command::new("/usr/bin/codesign")
        .args(["--display", "--verbose=4"])
        .arg(bundle)
        .output()?;
    ensure!(
        output.status.success(),
        "Cannot read the app signing identity"
    );
    Ok(String::from_utf8(output.stderr)?
        .lines()
        .find_map(|line| line.strip_prefix("TeamIdentifier="))
        .filter(|value| *value != "not set")
        .map(str::to_owned))
}

fn validate(bundle: &Path, installation: &Installation, version: &str) -> Result<()> {
    identity(bundle)?;
    ensure!(
        plist(bundle, "CFBundleShortVersionString")? == version,
        "The app bundle has the wrong version"
    );
    let incoming = team(bundle)?;
    if let Some(current) = team(bundle_root(&installation.executable)?)? {
        ensure!(
            incoming.as_deref() == Some(current.as_str()),
            "The update was signed by a different publisher"
        );
        let assessment = Command::new("/usr/sbin/spctl")
            .args(["--assess", "--type", "execute"])
            .arg(bundle)
            .output()?;
        ensure!(
            assessment.status.success(),
            "macOS could not approve this update for launch"
        );
    }
    install::verify_version(&executable_path(bundle)?, version)
}

struct Mounted(PathBuf);

fn mountpoint(archive: &Path) -> Result<PathBuf> {
    let mount = archive
        .parent()
        .context("Missing update directory")?
        .join(format!("mounted-{:016x}", rand::random::<u64>()));
    fs::create_dir(&mount)?;
    Ok(mount)
}

impl Mounted {
    fn open(archive: &Path) -> Result<Self> {
        // A failed detach can leave the previous attempt mounted. Never
        // traverse that volume or reuse its directory on a later attempt.
        let mount = mountpoint(archive)?;
        let output = Command::new("/usr/bin/hdiutil")
            .args(["attach", "-readonly", "-nobrowse", "-mountpoint"])
            .arg(&mount)
            .arg(archive)
            .output()?;
        if !output.status.success() {
            let _ = fs::remove_dir(&mount);
            anyhow::bail!("macOS could not open the downloaded disk image");
        }
        Ok(Self(mount))
    }

    fn bundle(&self) -> Result<PathBuf> {
        image_bundle(&self.0)
    }
}

fn image_bundle(root: &Path) -> Result<PathBuf> {
    for name in ["Spotifast.app", "Fastpotify.app"] {
        let bundle = root.join(name);
        match fs::symlink_metadata(&bundle) {
            Ok(metadata) => {
                ensure!(
                    metadata.is_dir(),
                    "The disk image has an invalid app bundle"
                );
                return Ok(bundle);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    anyhow::bail!("The disk image has no Spotifast app bundle")
}

impl Drop for Mounted {
    fn drop(&mut self) {
        let detached = Command::new("/usr/bin/hdiutil")
            .arg("detach")
            .arg(&self.0)
            .output()
            .is_ok_and(|output| output.status.success());
        if detached {
            let _ = fs::remove_dir(&self.0);
        }
    }
}

pub(super) fn validate_download(
    archive: &Path,
    installation: &Installation,
    version: &str,
) -> Result<()> {
    let mounted = Mounted::open(archive)?;
    validate(&mounted.bundle()?, installation, version)
}

/// Replaces the app bundle and returns the path of the executable now
/// installed there. The download's `CFBundleExecutable` decides that name,
/// which won't always match `prepared.installation.executable`: that field
/// is the pre-update process's own path, and stays the right one to use
/// for a rollback, but the wrong one to relaunch after a rename.
pub(super) fn replace(prepared: &Prepared) -> Result<PathBuf> {
    let target = bundle_root(&prepared.installation.executable)?;
    let backup = prepared.directory.join("previous");
    let candidate = prepared.directory.join("Spotifast.app");
    ensure!(
        !backup.exists() && !candidate.exists(),
        "This update was already applied"
    );
    let mounted = Mounted::open(&prepared.payload)?;
    let source = mounted.bundle()?;
    validate(&source, &prepared.installation, &prepared.version)?;
    let copy = Command::new("/usr/bin/ditto")
        .arg(&source)
        .arg(&candidate)
        .output()?;
    ensure!(
        copy.status.success(),
        "Could not copy the downloaded app bundle"
    );
    validate(&candidate, &prepared.installation, &prepared.version)?;
    drop(mounted);
    fs::rename(target, &backup).context("Cannot back up the current app bundle")?;
    if let Err(error) = fs::rename(&candidate, target) {
        fs::rename(&backup, target).context("Could not restore the previous app bundle")?;
        return Err(error).context("Could not replace the app bundle");
    }
    executable_path(target)
}

pub(super) fn restore(prepared: &Prepared) -> Result<()> {
    let backup = prepared.directory.join("previous");
    if backup.is_dir() {
        let target = bundle_root(&prepared.installation.executable)?;
        if target.exists() {
            fs::rename(target, prepared.directory.join("failed.app"))
                .context("Could not move the failed update aside")?;
        }
        fs::rename(backup, target).context("Could not restore the previous app bundle")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renamed_images_prefer_the_new_bundle_and_still_accept_old_images() {
        let root =
            std::env::temp_dir().join(format!("spotifast-image-test-{}", rand::random::<u64>()));
        fs::create_dir(&root).unwrap();
        assert!(image_bundle(&root).is_err());
        let legacy = root.join("Fastpotify.app");
        fs::create_dir(&legacy).unwrap();
        assert_eq!(image_bundle(&root).unwrap(), legacy);
        let app = root.join("Spotifast.app");
        fs::create_dir(&app).unwrap();
        assert_eq!(image_bundle(&root).unwrap(), app);
        fs::remove_dir(&app).unwrap();
        std::os::unix::fs::symlink(&legacy, &app).unwrap();
        assert!(
            image_bundle(&root).is_err(),
            "never follow an app-bundle symlink"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn homebrew_ownership_survives_the_bundle_rename() {
        let root =
            std::env::temp_dir().join(format!("spotifast-cask-test-{}", rand::random::<u64>()));
        for name in ["Fastpotify.app", "Spotifast.app"] {
            let installed = root.join("Applications").join(name);
            let version = root.join("Caskroom/fastpotify/0.8.0");
            fs::create_dir_all(&installed).unwrap();
            fs::create_dir_all(&version).unwrap();
            std::os::unix::fs::symlink(&installed, version.join(name)).unwrap();
            assert!(cask_owns(&root.join("Caskroom/fastpotify"), &installed));
            assert!(!cask_owns(&root.join("Caskroom/unrelated"), &installed));
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn another_mount_attempt_leaves_the_previous_volume_alone() {
        let directory =
            std::env::temp_dir().join(format!("fastpotify-mount-test-{}", rand::random::<u64>()));
        fs::create_dir(&directory).unwrap();
        let archive = directory.join("update.dmg");
        let first = mountpoint(&archive).unwrap();
        fs::write(first.join("still-mounted"), b"existing volume").unwrap();
        let second = mountpoint(&archive).unwrap();
        assert_ne!(first, second);
        assert_eq!(
            fs::read(first.join("still-mounted")).unwrap(),
            b"existing volume"
        );
        assert!(second.is_dir());
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn homebrew_app_symlink_does_not_claim_other_copies() {
        let directory =
            std::env::temp_dir().join(format!("fastpotify-cask-test-{}", rand::random::<u64>()));
        let installed = directory.join("Applications/Fastpotify.app");
        let cask = directory.join("Caskroom/fastpotify");
        let copy = directory.join("dev/Fastpotify.app");
        for path in [&installed, &copy, &cask.join("0.7.1")] {
            fs::create_dir_all(path).unwrap();
        }
        std::os::unix::fs::symlink(&installed, cask.join("0.7.1/Fastpotify.app")).unwrap();
        assert!(cask_owns(&cask, &installed));
        assert!(!cask_owns(&cask, &copy));
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn rollback_restores_resources_and_executable_together() {
        let directory =
            std::env::temp_dir().join(format!("fastpotify-mac-test-{}", rand::random::<u64>()));
        let app = directory.join("Fastpotify.app");
        fs::create_dir_all(app.join("Contents/MacOS")).unwrap();
        fs::write(app.join("Contents/MacOS/fastpotify"), b"new executable").unwrap();
        fs::write(app.join("Contents/Info.plist"), b"new metadata").unwrap();
        let installation = Installation {
            executable: app.join("Contents/MacOS/fastpotify"),
            kind: install::Kind::MacBundle,
        };
        let stage = install::staging(&installation).unwrap();
        assert_eq!(stage.parent(), Some(directory.as_path()));
        let backup = stage.join("previous");
        fs::create_dir_all(backup.join("Contents/MacOS")).unwrap();
        fs::write(backup.join("Contents/MacOS/fastpotify"), b"old executable").unwrap();
        fs::write(backup.join("Contents/Info.plist"), b"old metadata").unwrap();
        let prepared = Prepared {
            installation,
            directory: stage.clone(),
            payload: stage.join("update.dmg"),
            sha256: String::new(),
            version: "0.7.2".into(),
        };
        restore(&prepared).unwrap();
        assert_eq!(
            fs::read(app.join("Contents/MacOS/fastpotify")).unwrap(),
            b"old executable"
        );
        assert_eq!(
            fs::read(app.join("Contents/Info.plist")).unwrap(),
            b"old metadata"
        );
        assert_eq!(
            fs::read(stage.join("failed.app/Contents/Info.plist")).unwrap(),
            b"new metadata"
        );
        assert!(!backup.exists());
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn only_expected_bundle_layout_is_accepted() {
        assert_eq!(
            bundle_root(Path::new(
                "/Applications/Fastpotify.app/Contents/MacOS/fastpotify"
            ))
            .unwrap(),
            Path::new("/Applications/Fastpotify.app")
        );
        for path in [
            "/Applications/Fastpotify.app/fastpotify",
            "/tmp/Contents/MacOS/fastpotify",
            "/usr/local/bin/fastpotify",
        ] {
            assert!(bundle_root(Path::new(path)).is_err());
        }
        assert!(
            detect(Path::new(
                "/Volumes/Spotifast/Fastpotify.app/Contents/MacOS/fastpotify"
            ))
            .is_err()
        );
        assert!(detect(Path::new("/private/var/folders/test/AppTranslocation/test/Fastpotify.app/Contents/MacOS/fastpotify")).is_err());
    }

    #[test]
    fn the_renamed_executable_updates_alongside_the_old_one() {
        // Today's installs run "fastpotify"; a later release means to
        // rename it to "Spotifast". Both must resolve to their bundle so
        // today's installs keep auto-updating once that happens.
        assert_eq!(
            bundle_root(Path::new(
                "/Applications/Spotifast.app/Contents/MacOS/Spotifast"
            ))
            .unwrap(),
            Path::new("/Applications/Spotifast.app")
        );
        assert_eq!(
            bundle_root(Path::new(
                "/Applications/Spotifast.app/Contents/MacOS/fastpotify"
            ))
            .unwrap(),
            Path::new("/Applications/Spotifast.app")
        );
        assert!(
            bundle_root(Path::new(
                "/Applications/Spotifast.app/Contents/MacOS/spotifast"
            ))
            .is_err(),
            "the Linux/Windows binary name is not the macOS bundle's executable name"
        );
    }

    fn write_info_plist(bundle: &Path, executable: &str) {
        fs::create_dir_all(bundle.join("Contents")).unwrap();
        fs::write(
            bundle.join("Contents/Info.plist"),
            format!(
                r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleIdentifier</key><string>{IDENTIFIER}</string>
    <key>CFBundleExecutable</key><string>{executable}</string>
    <key>CFBundlePackageType</key><string>APPL</string>
</dict>
</plist>
"#
            ),
        )
        .unwrap();
    }

    #[test]
    fn identity_accepts_either_executable_name_but_nothing_else() {
        let bundle = std::env::temp_dir()
            .join(format!(
                "fastpotify-identity-test-{}",
                rand::random::<u64>()
            ))
            .join("Spotifast.app");
        for executable in ["fastpotify", "Spotifast"] {
            write_info_plist(&bundle, executable);
            identity(&bundle)
                .unwrap_or_else(|error| panic!("{executable} should be accepted: {error}"));
        }
        write_info_plist(&bundle, "SomeOtherName");
        assert!(identity(&bundle).is_err());
        fs::remove_dir_all(bundle.parent().unwrap()).unwrap();
    }

    #[test]
    fn executable_path_resolves_a_renamed_bundle() {
        // `validate` and `replace` both used to hardcode "fastpotify" here;
        // they now resolve whichever name the bundle actually declares, so
        // a download that renames the executable still gets found.
        let bundle = std::env::temp_dir()
            .join(format!(
                "fastpotify-executable-path-test-{}",
                rand::random::<u64>()
            ))
            .join("Spotifast.app");
        write_info_plist(&bundle, "Spotifast");
        assert_eq!(
            executable_path(&bundle).unwrap(),
            bundle.join("Contents/MacOS/Spotifast")
        );
        write_info_plist(&bundle, "fastpotify");
        assert_eq!(
            executable_path(&bundle).unwrap(),
            bundle.join("Contents/MacOS/fastpotify")
        );
        fs::remove_dir_all(bundle.parent().unwrap()).unwrap();
    }
}
