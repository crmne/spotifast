use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const LIMIT: u64 = 2 * 1024 * 1024 * 1024;
#[cfg(not(target_os = "macos"))]
const MARKER: &str = "fastpotify-portable-v1";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind {
    Portable,
    WindowsInstaller,
    #[cfg(target_os = "macos")]
    MacBundle,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Installation {
    pub executable: PathBuf,
    pub kind: Kind,
}

impl Installation {
    fn root(&self) -> Result<&Path> {
        #[cfg(target_os = "macos")]
        if self.kind == Kind::MacBundle {
            return super::macos::bundle_root(&self.executable);
        }
        Ok(&self.executable)
    }
}

pub fn detect() -> Result<Installation> {
    detect_at(&std::env::current_exe()?.canonicalize()?)
}

pub fn detect_at(executable: &Path) -> Result<Installation> {
    let path = executable.to_string_lossy().replace('\\', "/");
    let lower = path.to_lowercase();
    if std::env::var_os("FLATPAK_ID").is_some() || path.starts_with("/app/") {
        bail!("Update this installation through your software center or flatpak update.");
    }
    if std::env::var_os("SNAP").is_some() || path.starts_with("/snap/") {
        bail!("Update this installation with snap refresh.");
    }
    if lower.contains("/.cargo/") {
        bail!("Update this installation with cargo install.");
    }
    if path.starts_with("/nix/") || lower.contains("/cellar/") || lower.contains("/caskroom/") {
        bail!("Update this installation with Nix or Homebrew.");
    }
    #[cfg(target_os = "linux")]
    {
        for (program, arguments, instruction) in [
            ("dpkg-query", vec!["-S"], "apt"),
            ("rpm", vec!["-qf"], "dnf"),
            ("pacman", vec!["-Qo"], "pacman"),
        ] {
            if Command::new(program)
                .args(arguments)
                .arg(executable)
                .output()
                .is_ok_and(|output| output.status.success())
            {
                bail!("Update this installation through {instruction} or your software center.");
            }
        }
        if path.starts_with("/usr/") || path.starts_with("/bin/") || path.starts_with("/sbin/") {
            bail!(
                "This installation is in a system directory. Use your package manager or the download page."
            );
        }
    }
    #[cfg(not(target_os = "macos"))]
    let directory = executable
        .parent()
        .context("The application has no installation directory")?;
    #[cfg(windows)]
    {
        let installed = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .map(|base| base.join("Programs/Fastpotify/fastpotify.exe"));
        if fs::read_to_string(directory.join("fastpotify-installer.txt"))
            .is_ok_and(|value| value.trim() == "fastpotify-installer-v1")
            || (installed
                .and_then(|path| path.canonicalize().ok())
                .as_deref()
                == Some(executable)
                && directory.join("unins000.exe").is_file())
        {
            return Ok(Installation {
                executable: executable.to_owned(),
                kind: Kind::WindowsInstaller,
            });
        }
    }
    #[cfg(target_os = "macos")]
    {
        super::macos::detect(executable)?;
        Ok(Installation {
            executable: executable.to_owned(),
            kind: Kind::MacBundle,
        })
    }
    #[cfg(not(target_os = "macos"))]
    {
        ensure!(
            fs::read_to_string(directory.join("fastpotify-portable.txt"))
                .is_ok_and(|value| value.trim() == MARKER),
            "This installation does not identify itself as a portable download. Use the download page to install an update-enabled build."
        );
        Ok(Installation {
            executable: executable.to_owned(),
            kind: Kind::Portable,
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Prepared {
    pub installation: Installation,
    pub directory: PathBuf,
    pub payload: PathBuf,
    pub sha256: String,
    pub version: String,
}

#[derive(Serialize, Deserialize)]
struct Handoff {
    prepared: Prepared,
    parent: u32,
    arguments: Vec<String>,
}

pub fn hash(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

pub fn staging(installation: &Installation) -> Result<PathBuf> {
    let parent = installation
        .root()?
        .parent()
        .context("Missing installation directory")?;
    let directory = parent.join(format!(".fastpotify-update-{:016x}", rand::random::<u64>()));
    fs::create_dir(&directory).context("Cannot write to the installation directory")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))?;
    }
    Ok(directory)
}

pub fn extract(archive: &Path, entry: &str, destination: &Path) -> Result<()> {
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)?;
    let mut command = Command::new("tar");
    command
        .arg("-xOf")
        .arg(archive)
        .arg(entry)
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    hidden(&mut command);
    let mut child = command
        .spawn()
        .context("Cannot run tar to unpack the update")?;
    let result = (|| -> Result<()> {
        let mut stdout = child
            .stdout
            .take()
            .context("Missing archive stream")?
            .take(LIMIT + 1);
        let mut file = file;
        let count = std::io::copy(&mut stdout, &mut file)?;
        ensure!(
            count > 0 && count <= LIMIT,
            "The update executable has an invalid size"
        );
        ensure!(
            child.wait()?.success(),
            "Cannot unpack the update executable"
        );
        file.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = child.kill();
        let _ = child.wait();
    }
    result?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(destination, fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}

pub fn hidden(command: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    #[cfg(not(windows))]
    let _ = command;
}

pub fn verify_version(executable: &Path, expected: &str) -> Result<()> {
    let mut command = Command::new(executable);
    command
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    hidden(&mut command);
    let mut child = command
        .spawn()
        .context("The downloaded app cannot run on this computer")?;
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            ensure!(
                status.success(),
                "The downloaded app failed its startup check"
            );
            let mut version = String::new();
            child
                .stdout
                .take()
                .context("Missing version output")?
                .take(4096)
                .read_to_string(&mut version)?;
            ensure!(
                version.trim() == format!("fastpotify {expected}")
                    || version.trim() == format!("spotifast {expected}"),
                "The downloaded app has the wrong version"
            );
            return Ok(());
        }
        if start.elapsed() >= Duration::from_secs(10) {
            let _ = child.kill();
            let _ = child.wait();
            bail!("The downloaded app did not answer its startup check");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

pub fn handoff(prepared: &Prepared, arguments: Vec<String>) -> Result<()> {
    ensure!(
        hash(&prepared.payload)? == prepared.sha256,
        "The staged update changed. Download it again."
    );
    let helper = prepared.directory.join(if cfg!(windows) {
        "helper.exe"
    } else {
        "helper"
    });
    fs::copy(std::env::current_exe()?, &helper)?;
    let job = prepared.directory.join("handoff.json");
    let mut file = File::create(&job)?;
    serde_json::to_writer(
        &mut file,
        &Handoff {
            prepared: prepared.clone(),
            parent: std::process::id(),
            arguments,
        },
    )?;
    file.flush()?;
    file.sync_all()?;
    let mut command = Command::new(helper);
    command
        .arg("--apply-update")
        .arg(&job)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    hidden(&mut command);
    let mut child = command.spawn().context("Cannot start the update helper")?;
    let ready = prepared.directory.join("ready");
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(10) {
        if ready.exists() {
            return Ok(());
        }
        ensure!(
            child.try_wait()?.is_none(),
            "The update helper exited before it was ready"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = child.kill();
    let _ = child.wait();
    bail!("The update helper did not start. Try again.")
}

fn wait_for_parent(parent: u32, ready: &Path) -> Result<()> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
        use windows_sys::Win32::System::Threading::{
            OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject,
        };
        let process = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, parent) };
        ensure!(!process.is_null(), "Cannot watch the running app");
        let result = fs::write(ready, b"ready");
        if result.is_err() {
            unsafe {
                CloseHandle(process);
            }
        }
        result?;
        let outcome = unsafe { WaitForSingleObject(process, 60_000) };
        unsafe {
            CloseHandle(process);
        }
        ensure!(
            outcome == WAIT_OBJECT_0,
            "The app did not close within one minute"
        );
    }
    #[cfg(target_os = "linux")]
    {
        let process = PathBuf::from(format!("/proc/{parent}/stat"));
        let original = fs::read_to_string(&process).context("Cannot watch the running app")?;
        let identity = process_identity(&original).context("Cannot identify the running app")?;
        fs::write(ready, b"ready")?;
        let start = Instant::now();
        loop {
            let current = match fs::read_to_string(&process) {
                Ok(current) => current,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
                Err(error) => return Err(error).context("Cannot watch the running app"),
            };
            if process_identity(&current) != Some(identity)
                || current
                    .rsplit_once(')')
                    .is_some_and(|(_, fields)| fields.trim_start().starts_with('Z'))
            {
                break;
            }
            ensure!(
                start.elapsed() < Duration::from_secs(60),
                "The app did not close within one minute"
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        fs::write(ready, b"ready")?;
        let start = Instant::now();
        while Command::new("/bin/kill")
            .args(["-0", &parent.to_string()])
            .stderr(Stdio::null())
            .status()?
            .success()
        {
            ensure!(
                start.elapsed() < Duration::from_secs(60),
                "The app did not close within one minute"
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn process_identity(stat: &str) -> Option<&str> {
    stat.rsplit_once(')')?.1.split_whitespace().nth(19)
}

/// Replaces the installed app and returns the executable to relaunch. That
/// is `prepared.installation.executable` on every platform except macOS,
/// where the download can rename the executable inside the bundle; see
/// `macos::replace`.
pub fn replace(prepared: &Prepared) -> Result<PathBuf> {
    ensure!(
        hash(&prepared.payload)? == prepared.sha256,
        "The staged update checksum changed"
    );
    let target = &prepared.installation.executable;
    let backup = prepared.directory.join("previous");
    match prepared.installation.kind {
        #[cfg(target_os = "macos")]
        Kind::MacBundle => return super::macos::replace(prepared),
        Kind::Portable => {
            ensure!(!backup.exists(), "This update was already applied");
            backup_current(target, &backup).context("Cannot back up the current app")?;
            #[cfg(windows)]
            fs::remove_file(target).context("The app is still running or cannot be replaced")?;
            if let Err(error) = fs::rename(&prepared.payload, target) {
                #[cfg(windows)]
                fs::copy(&backup, target).context("Could not restore the previous app")?;
                return Err(error).context("Could not replace the app");
            }
        }
        Kind::WindowsInstaller => {
            backup_current(target, &backup).context("Cannot back up the current app")?;
            let mut command = Command::new(&prepared.payload);
            command
                .args([
                    "/VERYSILENT",
                    "/SUPPRESSMSGBOXES",
                    "/NORESTART",
                    "/CLOSEAPPLICATIONS",
                    "/NORESTARTAPPLICATIONS",
                ])
                .arg(format!(
                    "/DIR={}",
                    installer_path(target.parent().context("Missing installation directory")?)
                ))
                .arg(format!(
                    "/LOG={}",
                    installer_path(&prepared.directory.join("installer.log"))
                ));
            hidden(&mut command);
            ensure!(
                command.status()?.success(),
                "The installer failed. See the update installer log."
            );
        }
    }
    Ok(target.clone())
}

fn backup_current(target: &Path, backup: &Path) -> Result<()> {
    let mut source = File::open(target)?;
    let permissions = source.metadata()?.permissions();
    write_backup(&mut source, backup, permissions)
}

/// Rollback recognizes only `previous`. Publish that name after the complete
/// copy has been synced, so a failed copy cannot replace a working executable
/// with the partial backup it left behind.
fn write_backup(source: &mut impl Read, backup: &Path, permissions: fs::Permissions) -> Result<()> {
    ensure!(
        fs::symlink_metadata(backup)
            .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound),
        "This update already has a backup"
    );
    let partial = backup.with_extension("partial");
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&partial)?;
    let result = (|| -> Result<()> {
        std::io::copy(source, &mut output)?;
        output.sync_all()?;
        fs::set_permissions(&partial, permissions)?;
        Ok(())
    })();
    drop(output);
    if let Err(error) = result {
        let _ = fs::remove_file(&partial);
        return Err(error);
    }
    if let Err(error) = fs::rename(&partial, backup) {
        let _ = fs::remove_file(&partial);
        return Err(error.into());
    }
    Ok(())
}

fn installer_path(path: &Path) -> String {
    let path = path.to_string_lossy();
    if let Some(unc) = path.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{unc}")
    } else {
        path.strip_prefix(r"\\?\").unwrap_or(&path).to_owned()
    }
}

/// Overwrites the job file `run_helper` and `acknowledge` both read, so a
/// renamed executable is reflected before the new process reads it back.
fn write_receipt(job: &Path, handoff: &Handoff) -> Result<()> {
    fs::write(job, serde_json::to_vec(handoff)?)?;
    Ok(())
}

pub fn run_helper(job: &Path) -> Result<()> {
    let mut handoff: Handoff = serde_json::from_reader(File::open(job)?)?;
    // Kept aside, untouched, for the rollback paths below: replace() can
    // rename the executable inside a macOS bundle, and a rollback restores
    // the previous bundle, which still has the old one.
    let original = handoff.prepared.clone();
    ensure!(
        job.parent() == Some(original.directory.as_path()),
        "Invalid update job directory"
    );
    ensure!(
        original.payload.parent() == Some(original.directory.as_path()),
        "Invalid staged payload"
    );
    ensure!(
        original.directory.parent() == original.installation.root()?.parent(),
        "Invalid installation directory"
    );
    ensure!(
        hash(&original.payload)? == original.sha256,
        "The staged update checksum changed"
    );
    wait_for_parent(handoff.parent, &original.directory.join("ready"))?;
    // Every failure from here on has already changed something on disk (the
    // bundle itself, or the receipt below), so all of them roll back to
    // `original` and restart the previous app the same way, rather than
    // leaving a new install sitting there unlaunched and unrestored.
    let fail = |error: anyhow::Error, message: &str, arguments: &[String]| -> Result<()> {
        restore_and_restart(&original, arguments)?;
        fs::write(
            original.directory.join("result.txt"),
            format!("{message}: {error:#}"),
        )?;
        Err(error)
    };
    let launch_executable = match replace(&original) {
        Ok(executable) => executable,
        Err(error) => return fail(error, "Update failed", &handoff.arguments),
    };
    if launch_executable != original.installation.executable {
        // The receipt this launch takes below is the same job file
        // `acknowledge` re-reads from the new process, so its recorded
        // executable has to match what actually got installed.
        handoff.prepared.installation.executable = launch_executable.clone();
        if let Err(error) = write_receipt(job, &handoff) {
            return fail(error, "Update failed", &handoff.arguments);
        }
    }
    let mut command = Command::new(&launch_executable);
    command
        .args(&handoff.arguments)
        .arg("--update-receipt")
        .arg(job);
    hidden(&mut command);
    let launch = (|| -> Result<()> {
        let mut child = command
            .spawn()
            .context("Could not launch the updated app")?;
        let start = Instant::now();
        loop {
            if original.directory.join("started").is_file() {
                return Ok(());
            }
            ensure!(
                child.try_wait()?.is_none(),
                "The updated app exited before opening its window"
            );
            if start.elapsed() >= Duration::from_secs(60) {
                let _ = child.kill();
                let _ = child.wait();
                bail!("The updated app did not open its window within one minute");
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    })();
    if let Err(error) = launch {
        return fail(
            error,
            "Update failed; restored the previous app",
            &handoff.arguments,
        );
    }
    fs::write(
        original.directory.join("result.txt"),
        format!("Updated to {}", original.version),
    )?;
    Ok(())
}

fn restore_and_restart(prepared: &Prepared, arguments: &[String]) -> Result<()> {
    #[cfg(target_os = "macos")]
    if prepared.installation.kind == Kind::MacBundle {
        super::macos::restore(prepared)?;
    }
    let backup = prepared.directory.join("previous");
    if backup.is_file() {
        fs::copy(&backup, &prepared.installation.executable)
            .context("Could not restore the previous app")?;
    }
    let mut command = Command::new(&prepared.installation.executable);
    command.args(arguments).args([
        "--update-error",
        "The update could not start. The previous version has been restored.",
    ]);
    hidden(&mut command);
    command
        .spawn()
        .context("Could not restart the previous app")?;
    Ok(())
}

pub fn acknowledge(job: &Path) -> Result<()> {
    let handoff: Handoff = serde_json::from_reader(File::open(job)?)?;
    ensure!(
        job.parent() == Some(handoff.prepared.directory.as_path()),
        "Invalid update receipt directory"
    );
    ensure!(
        std::env::current_exe()?.canonicalize()?
            == handoff.prepared.installation.executable.canonicalize()?,
        "The receipt belongs to a different installation"
    );
    ensure!(
        env!("CARGO_PKG_VERSION") == handoff.prepared.version,
        "The updated app reports the wrong version"
    );
    fs::write(
        handoff.prepared.directory.join("started"),
        env!("CARGO_PKG_VERSION"),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_interrupted_backup_is_never_available_to_rollback() {
        struct InterruptedCopy(bool);
        impl Read for InterruptedCopy {
            fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
                if self.0 {
                    return Err(std::io::Error::other("injected copy failure"));
                }
                self.0 = true;
                buffer[0] = b'p';
                Ok(1)
            }
        }
        let directory =
            std::env::temp_dir().join(format!("fastpotify-backup-test-{}", rand::random::<u64>()));
        fs::create_dir(&directory).unwrap();
        let target = directory.join("fastpotify");
        fs::write(&target, b"working executable").unwrap();
        let backup = directory.join("previous");
        let permissions = fs::metadata(&target).unwrap().permissions();
        assert!(write_backup(&mut InterruptedCopy(false), &backup, permissions).is_err());
        assert!(
            !backup.exists(),
            "rollback must not see the incomplete copy"
        );
        assert!(!backup.with_extension("partial").exists());
        assert_eq!(fs::read(&target).unwrap(), b"working executable");
        backup_current(&target, &backup).unwrap();
        assert_eq!(fs::read(&backup).unwrap(), b"working executable");
        fs::write(&target, b"new executable").unwrap();
        assert!(backup_current(&target, &backup).is_err());
        assert_eq!(
            fs::read(&backup).unwrap(),
            b"working executable",
            "a retry keeps the known backup"
        );
        fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn process_identity_handles_spaces_and_parentheses_in_names() {
        let fields = "S 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 98765 20";
        assert_eq!(
            process_identity(&format!("42 (app (test)) {fields}")),
            Some("98765")
        );
        assert_eq!(process_identity("42 (app) S"), None);
        let current = fs::read_to_string(format!("/proc/{}/stat", std::process::id())).unwrap();
        assert!(process_identity(&current).unwrap().parse::<u64>().is_ok());
    }

    #[test]
    fn unknown_and_package_managed_paths_are_not_portable() {
        for path in [
            "/usr/bin/fastpotify",
            "/nix/store/package/bin/fastpotify",
            "/home/test/.cargo/bin/fastpotify",
            "/unknown/fastpotify",
        ] {
            assert!(detect_at(Path::new(path)).is_err());
        }
    }

    #[test]
    fn installer_arguments_use_paths_inno_setup_accepts() {
        assert_eq!(
            installer_path(Path::new(r"\\?\C:\Users\test\Spotifast")),
            r"C:\Users\test\Spotifast"
        );
        assert_eq!(
            installer_path(Path::new(r"\\?\UNC\server\share\Spotifast")),
            r"\\server\share\Spotifast"
        );
    }

    #[test]
    fn replacement_verifies_before_touching_the_current_executable() {
        let directory =
            std::env::temp_dir().join(format!("fastpotify-updater-test-{}", rand::random::<u64>()));
        fs::create_dir(&directory).unwrap();
        let target = directory.join("fastpotify");
        fs::write(&target, b"old").unwrap();
        let installation = Installation {
            executable: target.clone(),
            kind: Kind::Portable,
        };
        let stage = staging(&installation).unwrap();
        let payload = stage.join("next");
        fs::write(&payload, b"new").unwrap();
        let mut prepared = Prepared {
            installation,
            directory: stage.clone(),
            payload: payload.clone(),
            sha256: "wrong".into(),
            version: "1.0.0".into(),
        };
        assert!(replace(&prepared).is_err());
        assert_eq!(fs::read(&target).unwrap(), b"old");
        prepared.sha256 = hash(&payload).unwrap();
        replace(&prepared).unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"new");
        assert_eq!(fs::read(stage.join("previous")).unwrap(), b"old");
        fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_receipt_records_a_renamed_executable_and_reads_back() {
        let directory =
            std::env::temp_dir().join(format!("fastpotify-receipt-test-{}", rand::random::<u64>()));
        fs::create_dir(&directory).unwrap();
        let job = directory.join("handoff.json");
        let mut handoff = Handoff {
            prepared: Prepared {
                installation: Installation {
                    executable: directory.join("Spotifast.app/Contents/MacOS/fastpotify"),
                    kind: Kind::MacBundle,
                },
                directory: directory.clone(),
                payload: directory.join("update.dmg"),
                sha256: String::new(),
                version: "1.0.0".into(),
            },
            parent: std::process::id(),
            arguments: vec!["--minimized".into()],
        };
        let renamed = directory.join("Spotifast.app/Contents/MacOS/Spotifast");
        handoff.prepared.installation.executable = renamed.clone();
        write_receipt(&job, &handoff).unwrap();
        let read_back: Handoff = serde_json::from_reader(File::open(&job).unwrap()).unwrap();
        assert_eq!(read_back.prepared.installation.executable, renamed);
        assert_eq!(read_back.arguments, vec!["--minimized".to_string()]);
        fs::remove_dir_all(directory).unwrap();
    }
}
