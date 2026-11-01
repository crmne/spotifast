//! Direct, bounded downloads from Spotify. Installed builds are immutable;
//! replacing a small pointer never overwrites a running Windows executable.
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    io::Read,
    path::{Component, Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const CDN: &str = "soloist-builds.spotifycdn.com";
const ARCHIVE_LIMIT: usize = 80 * 1024 * 1024;
const EXPANDED_LIMIT: u64 = 160 * 1024 * 1024;
pub const DAY: u64 = 86_400;

pub fn artifact(os: &str, arch: &str) -> Option<&'static str> {
    match (os, arch) {
        ("linux", "x86_64") => Some("soloist_release_x86_64.tar.gz"),
        ("linux", "aarch64") => Some("soloist_release_arm64.tar.gz"),
        ("linux", "arm") => Some("soloist_release_arm32.tar.gz"),
        ("macos", "aarch64") => Some("soloist_release_osx_arm64.tar.gz"),
        ("windows", "x86_64") => Some("soloist_release_windows_x86_64.zip"),
        _ => None,
    }
}

pub fn supported() -> bool {
    artifact(std::env::consts::OS, std::env::consts::ARCH).is_some()
}

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Build {
    pub version: String,
    pub built: u64,
    pub checked: u64,
    pub digest: String,
}
impl Build {
    pub fn path(&self, root: &Path) -> PathBuf {
        root.join(&self.digest).join(executable())
    }
    pub fn usable(&self, at: u64) -> bool {
        self.built <= at.saturating_add(DAY)
            && at < self.built.saturating_add(90 * DAY)
            && self.digest.len() == 64
            && self.digest.bytes().all(|b| b.is_ascii_hexdigit())
    }
    pub fn due(&self, at: u64) -> bool {
        at.saturating_sub(self.built) >= 60 * DAY && at.saturating_sub(self.checked) >= DAY
    }
}
fn executable() -> &'static str {
    if cfg!(windows) {
        "soloist.exe"
    } else {
        "soloist"
    }
}

pub fn private_dir(path: &Path) -> Result<()> {
    std::fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// A path alone is not an installation identity: two computers commonly have
/// the same username and state directory. Keep a random identity across starts.
pub async fn instance_id(root: &Path) -> Result<String> {
    let path = root.join("instance-id");
    match tokio::fs::read_to_string(&path).await {
        Ok(id) => {
            ensure!(
                id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit()),
                "The saved Soloist installation identity is invalid"
            );
            Ok(id)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let id = format!("{:032x}", rand::random::<u128>());
            let bytes = id.as_bytes().to_vec();
            tokio::task::spawn_blocking(move || fastframe_file::write_private(&path, bytes))
                .await??;
            Ok(id)
        }
        Err(error) => Err(error.into()),
    }
}

pub async fn saved(root: &Path, name: &str) -> Option<Build> {
    let build: Build =
        serde_json::from_slice(&tokio::fs::read(root.join(name)).await.ok()?).ok()?;
    if !build.usable(now()) {
        return None;
    }
    let bytes = tokio::fs::read(build.path(root)).await.ok()?;
    (format!("{:x}", Sha256::digest(&bytes)) == build.digest).then_some(build)
}

pub async fn remember(root: &Path, name: &str, build: &Build) -> Result<()> {
    let path = root.join(name);
    let bytes = serde_json::to_vec(build)?;
    tokio::task::spawn_blocking(move || fastframe_file::write_private(&path, bytes)).await??;
    Ok(())
}

/// Keep the current build, its pending replacement and any rollback build
/// selected by the caller. Abandoned staging folders are collected next day.
pub async fn collect(root: &Path, keep: &[String]) -> Result<()> {
    let mut entries = tokio::fs::read_dir(root).await?;
    while let Some(entry) = entries.next_entry().await? {
        let name = entry.file_name().to_string_lossy().into_owned();
        let old_build = name.len() == 64
            && name.bytes().all(|b| b.is_ascii_hexdigit())
            && !keep.contains(&name);
        let old_stage = name.starts_with("staging-")
            && entry
                .metadata()
                .await?
                .modified()?
                .elapsed()
                .is_ok_and(|age| age > Duration::from_secs(DAY));
        if (old_build || old_stage) && entry.file_type().await?.is_dir() {
            let _ = tokio::fs::remove_dir_all(entry.path()).await;
        }
    }
    Ok(())
}

pub fn parse_version(text: &str, at: u64) -> Result<(String, u64)> {
    let words: Vec<_> = text.split_whitespace().collect();
    ensure!(
        words.first() == Some(&"soloist"),
        "Unexpected Soloist version response"
    );
    let version = words.get(1).context("Missing Soloist version")?.to_string();
    let index = words
        .iter()
        .position(|word| *word == "build")
        .context("Missing Soloist build date")?;
    let built: u64 = words
        .get(index + 1)
        .context("Missing Soloist build date")?
        .parse()?;
    ensure!(
        built <= at.saturating_add(DAY) && at < built.saturating_add(90 * DAY),
        "Spotify's downloaded Soloist build has expired. Check the computer's date and try again later."
    );
    Ok((version, built))
}

fn accepted(path: &Path, exe: &str) -> Result<Option<String>> {
    ensure!(
        path.components()
            .all(|p| matches!(p, Component::Normal(_) | Component::CurDir)),
        "Unsafe path in Soloist archive"
    );
    let name = path
        .file_name()
        .and_then(|p| p.to_str())
        .context("Invalid archive entry")?;
    ensure!(!name.contains('\\'), "Unsafe path in Soloist archive");
    Ok([exe, "THIRD_PARTY_LICENSES.txt", "CHANGELOG.md"]
        .contains(&name)
        .then(|| name.to_string()))
}

fn extract(bytes: &[u8], zipped: bool, destination: &Path, exe: &str) -> Result<()> {
    private_dir(destination)?;
    let mut seen = std::collections::HashSet::new();
    let mut total = 0u64;
    let mut save = |name: &str, size: u64, reader: &mut dyn Read| -> Result<()> {
        total = total.checked_add(size).context("Archive too large")?;
        ensure!(
            total <= EXPANDED_LIMIT && seen.insert(name.to_owned()),
            "Invalid Soloist archive size or duplicate entry"
        );
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(destination.join(name))?;
        let written = std::io::copy(&mut reader.take(size + 1), &mut file)?;
        ensure!(written == size, "Truncated Soloist archive");
        file.sync_all()?;
        Ok(())
    };
    if zipped {
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))?;
        ensure!(archive.len() <= 64, "Too many Soloist archive entries");
        // Reject archives whose local headers tell a different story from
        // their central directory, including aliases and overlapping entries.
        archive.validate_header_consistency()?;
        for i in 0..archive.len() {
            let mut entry = archive.by_index(i)?;
            ensure!(!entry.is_symlink(), "Link in Soloist archive");
            let name = accepted(
                Path::new(
                    entry
                        .name()
                        .context("Invalid Soloist archive filename")?
                        .as_ref(),
                ),
                exe,
            )?;
            if entry.is_file()
                && let Some(name) = name
            {
                save(&name, entry.size(), &mut entry)?;
            }
        }
    } else {
        let decoded = flate2::read::GzDecoder::new(bytes).take(EXPANDED_LIMIT + 1);
        let mut archive = tar::Archive::new(decoded);
        for (i, entry) in archive.entries()?.enumerate() {
            ensure!(i < 64, "Too many Soloist archive entries");
            let mut entry = entry?;
            let kind = entry.header().entry_type();
            ensure!(
                kind.is_file() || kind.is_dir(),
                "Link or special file in Soloist archive"
            );
            let name = accepted(&entry.path()?, exe)?;
            if kind.is_file()
                && let Some(name) = name
            {
                save(&name, entry.size(), &mut entry)?;
            }
        }
    }
    ensure!(
        seen.contains(exe),
        "Soloist executable missing from archive"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            destination.join(exe),
            std::fs::Permissions::from_mode(0o700),
        )?;
    }
    Ok(())
}

pub async fn download(
    root: &Path,
    http: &crate::http::Http,
    progress: &(dyn Fn(u64, u64) + Send + Sync),
) -> Result<Build> {
    let artifact = artifact(std::env::consts::OS, std::env::consts::ARCH)
        .context("Spotify does not publish Soloist for this platform")?;
    let mut response = http
        .client()
        .map_err(anyhow::Error::msg)?
        .get(format!("https://{CDN}/{artifact}"))
        .timeout(Duration::from_secs(180))
        .send()
        .await?
        .error_for_status()?;
    ensure!(
        response.url().scheme() == "https" && response.url().host_str() == Some(CDN),
        "Soloist download left Spotify's download server"
    );
    let total = response.content_length().unwrap_or(0);
    ensure!(
        total <= ARCHIVE_LIMIT as u64,
        "Soloist download is too large"
    );
    let mut bytes = Vec::new();
    let mut reported = std::time::Instant::now();
    progress(0, total);
    while let Some(chunk) = response.chunk().await? {
        ensure!(
            bytes.len().saturating_add(chunk.len()) <= ARCHIVE_LIMIT,
            "Soloist download is too large"
        );
        bytes.extend_from_slice(&chunk);
        if reported.elapsed() >= Duration::from_millis(150) {
            progress(bytes.len() as u64, total);
            reported = std::time::Instant::now();
        }
    }
    let stage = root.join(format!("staging-{:016x}", rand::random::<u64>()));
    let extracting = stage.clone();
    let result = tokio::task::spawn_blocking(move || {
        extract(
            &bytes,
            artifact.ends_with(".zip"),
            &extracting,
            executable(),
        )
    })
    .await?;
    if let Err(error) = result {
        let _ = tokio::fs::remove_dir_all(&stage).await;
        return Err(error);
    }
    let result = async {
        let mut command = tokio::process::Command::new(stage.join(executable()));
        command.arg("--version").kill_on_drop(true);
        #[cfg(windows)] command.creation_flags(0x08000000);
        let output = tokio::time::timeout(Duration::from_secs(10), command.output()).await??;
        ensure!(output.status.success(), "Downloaded Soloist could not run on this computer");
        let (version, built) = parse_version(std::str::from_utf8(&output.stdout)?, now())?;
        let digest = format!("{:x}", Sha256::digest(tokio::fs::read(stage.join(executable())).await?));
        let build = Build { version, built, checked: now(), digest };
        let destination = root.join(&build.digest);
        if !destination.exists() { tokio::fs::rename(&stage, &destination).await?; }
        else if saved_build_matches(&build, root).await { /* The same official build is already present. */ }
        else { bail!("The installed Soloist build failed its integrity check. Remove it and try again."); }
        Ok(build)
    }.await;
    let _ = tokio::fs::remove_dir_all(&stage).await;
    result
}
async fn saved_build_matches(build: &Build, root: &Path) -> bool {
    tokio::fs::read(build.path(root))
        .await
        .is_ok_and(|bytes| format!("{:x}", Sha256::digest(bytes)) == build.digest)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn platform_matrix_and_expiry_use_build_date() {
        assert!(artifact("macos", "x86_64").is_none());
        assert!(artifact("windows", "aarch64").is_none());
        assert!(artifact("windows", "x86_64").unwrap().ends_with(".zip"));
        for arch in ["arm", "aarch64", "x86_64"] {
            assert!(artifact("linux", arch).is_some());
        }
        let at = 1_800_000_000;
        assert!(parse_version(&format!("soloist 1.3 build {} (date)", at - 89 * DAY), at).is_ok());
        assert!(parse_version(&format!("soloist 1.3 build {} (date)", at - 90 * DAY), at).is_err());
        assert!(parse_version("soloist 1.3 build nonsense", at).is_err());
        assert!(parse_version(&format!("soloist 1.3 build {}", at + 2 * DAY), at).is_err());
    }
    #[test]
    fn rejects_traversal_and_windows_paths() {
        for path in ["../soloist", "/soloist", "C:\\soloist", "foo/../../soloist"] {
            assert!(accepted(Path::new(path), "soloist").is_err());
        }
        assert_eq!(
            accepted(Path::new("./soloist"), "soloist")
                .unwrap()
                .as_deref(),
            Some("soloist")
        );
    }
    fn scratch() -> PathBuf {
        std::env::temp_dir().join(format!("soloist-archive-test-{}", rand::random::<u64>()))
    }
    #[tokio::test]
    async fn installation_identity_survives_restart_and_differs_between_computers() {
        let first = scratch();
        let second = scratch();
        private_dir(&first).unwrap();
        private_dir(&second).unwrap();
        let id = instance_id(&first).await.unwrap();
        assert_eq!(id, instance_id(&first).await.unwrap());
        assert_ne!(id, instance_id(&second).await.unwrap());
        std::fs::write(first.join("instance-id"), b"broken").unwrap();
        assert!(instance_id(&first).await.is_err());
        std::fs::remove_dir_all(first).unwrap();
        std::fs::remove_dir_all(second).unwrap();
    }
    fn tarball(entries: &[(&str, &[u8], tar::EntryType)]) -> Vec<u8> {
        let mut tar = tar::Builder::new(Vec::new());
        for (name, bytes, kind) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(bytes.len() as u64);
            header.set_mode(0o700);
            header.set_entry_type(*kind);
            if kind.is_symlink() {
                header.set_link_name("/outside").unwrap();
            }
            header.set_cksum();
            tar.append_data(&mut header, name, *bytes).unwrap();
        }
        let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        std::io::Write::write_all(&mut gzip, &tar.into_inner().unwrap()).unwrap();
        gzip.finish().unwrap()
    }
    #[test]
    fn archive_extracts_only_the_player_and_notices() {
        let dir = scratch();
        let bytes = tarball(&[
            ("soloist", b"executable", tar::EntryType::Regular),
            (
                "THIRD_PARTY_LICENSES.txt",
                b"licenses",
                tar::EntryType::Regular,
            ),
            ("surprise.sh", b"other", tar::EntryType::Regular),
        ]);
        extract(&bytes, false, &dir, "soloist").unwrap();
        assert_eq!(std::fs::read(dir.join("soloist")).unwrap(), b"executable");
        assert!(dir.join("THIRD_PARTY_LICENSES.txt").exists());
        assert!(!dir.join("surprise.sh").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn rejects_links_duplicates_and_missing_executables() {
        for entries in [
            vec![("soloist", b"".as_slice(), tar::EntryType::Symlink)],
            vec![
                ("soloist", b"a".as_slice(), tar::EntryType::Regular),
                ("soloist", b"b".as_slice(), tar::EntryType::Regular),
            ],
            vec![("README", b"x".as_slice(), tar::EntryType::Regular)],
        ] {
            let dir = scratch();
            assert!(extract(&tarball(&entries), false, &dir, "soloist").is_err());
            let _ = std::fs::remove_dir_all(dir);
        }
    }
    #[test]
    fn extracts_windows_zip_and_checks_crc() {
        use std::io::Write;
        let mut archive = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        archive
            .start_file(
                "soloist.exe",
                zip::write::SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Stored),
            )
            .unwrap();
        archive.write_all(b"executable").unwrap();
        let mut bytes = archive.finish().unwrap().into_inner();
        let dir = scratch();
        extract(&bytes, true, &dir, "soloist.exe").unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        let start = bytes.windows(10).position(|w| w == b"executable").unwrap();
        bytes[start] ^= 1;
        assert!(extract(&bytes, true, &dir, "soloist.exe").is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn zip_header_disagreements_and_traversal_never_overwrite_outside_files() {
        use std::io::Write;
        let mut archive = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        archive
            .start_file("soloist.exe", zip::write::SimpleFileOptions::default())
            .unwrap();
        archive.write_all(b"executable").unwrap();
        let bytes = archive.finish().unwrap().into_inner();
        let local = bytes.windows(4).position(|w| w == b"PK\x03\x04").unwrap() + 30;
        let central = bytes.windows(4).position(|w| w == b"PK\x01\x02").unwrap() + 46;
        for offsets in [vec![local], vec![central], vec![local, central]] {
            let root = scratch();
            private_dir(&root).unwrap();
            let outside = root.join("evil.exe");
            std::fs::write(&outside, b"leave alone").unwrap();
            let mut forged = bytes.clone();
            for offset in offsets {
                forged[offset..offset + 11].copy_from_slice(b"../evil.exe");
            }
            assert!(extract(&forged, true, &root.join("build"), "soloist.exe").is_err());
            assert_eq!(std::fs::read(outside).unwrap(), b"leave alone");
            std::fs::remove_dir_all(root).unwrap();
        }
    }
    #[test]
    #[ignore = "requires SOLOIST_QA_ZIP pointing to Spotify's current official Windows archive"]
    fn official_windows_archive_extracts() {
        let bytes = std::fs::read(std::env::var("SOLOIST_QA_ZIP").unwrap()).unwrap();
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!(
                "soloist-windows-archive-qa-{}",
                rand::random::<u64>()
            ));
        let result = extract(&bytes, true, &root, "soloist.exe");
        let _ = std::fs::remove_dir_all(root);
        result.unwrap();
    }
    #[tokio::test]
    async fn corrupted_build_is_never_restored_and_updates_are_due_before_expiry() {
        let root = scratch();
        let bytes = b"executable";
        let build = Build {
            version: "test".into(),
            built: now() - 65 * DAY,
            checked: now() - 2 * DAY,
            digest: format!("{:x}", Sha256::digest(bytes)),
        };
        assert!(build.due(now()));
        assert!(build.usable(now()));
        private_dir(build.path(&root).parent().unwrap()).unwrap();
        std::fs::write(build.path(&root), bytes).unwrap();
        remember(&root, "current.json", &build).await.unwrap();
        assert!(saved(&root, "current.json").await.is_some());
        std::fs::write(build.path(&root), b"modified").unwrap();
        assert!(saved(&root, "current.json").await.is_none());
        std::fs::remove_dir_all(root).unwrap();
    }
}
