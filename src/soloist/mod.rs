//! Optional, user-installed Spotify Soloist. Spotifast owns only the child it
//! started, and keeps its files and credentials separate from librespot.
mod audio;
pub mod install;
mod protocol;
mod volume;
use crate::{
    credentials::{Grant, Lease},
    http::Http,
    paths::AppDirs,
};
use anyhow::{Context, Result, bail, ensure};
pub use audio::OutputVolume;
pub use protocol::Client;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::watch;

/// No Debug: a personal Soloist key is a usable secret.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct Key {
    pub account: String,
    pub value: String,
}
impl Key {
    pub fn valid(&self) -> bool {
        !self.account.is_empty()
            && !self.value.is_empty()
            && self.value.len() <= 4096
            && self.value.bytes().all(|b| b.is_ascii_graphic())
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub enum Status {
    #[default]
    Off,
    Starting,
    NeedsKey,
    Downloading {
        received: u64,
        total: u64,
    },
    Pairing {
        name: String,
        version: String,
    },
    Ready {
        name: String,
        version: String,
        expires: u64,
    },
    Failed(String),
}
impl Status {
    pub fn name(&self) -> Option<&str> {
        match self {
            Self::Pairing { name, .. } | Self::Ready { name, .. } => Some(name),
            _ => None,
        }
    }
}
#[derive(Clone, Debug)]
pub enum Update {
    Status(Status),
    Audio(bool),
    OutputVolume,
    Changed { queue: bool },
    UpdateReady(String),
    UpdateFailed,
}
pub type Notify = Arc<dyn Fn(Update) + Send + Sync>;

pub struct Handle {
    pub account: String,
    stop: watch::Sender<bool>,
    task: Option<tokio::task::JoinHandle<()>>,
    pub client: Client,
}
impl Handle {
    pub fn start(config: Config) -> Self {
        let (stop, stopped) = watch::channel(false);
        let client = Client::default();
        let running = client.clone();
        let account = config.account.clone();
        let notify = config.notify.clone();
        let task = tokio::spawn(async move {
            let result = run(config, running.clone(), stopped).await;
            running.clear();
            if let Err(error) = result {
                notify(Update::Status(Status::Failed(error.to_string())));
            }
        });
        Self {
            account,
            stop,
            task: Some(task),
            client,
        }
    }
    pub fn is_running(&self) -> bool {
        self.task.as_ref().is_some_and(|task| !task.is_finished())
    }
    pub async fn stop(mut self) {
        self.client.clear();
        let _ = self.stop.send(true);
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}
impl Drop for Handle {
    fn drop(&mut self) {
        self.client.clear();
        let _ = self.stop.send(true);
    }
}

fn account_dir(dirs: &AppDirs, account: &str) -> PathBuf {
    dirs.state
        .join("soloist")
        .join("accounts")
        .join(format!("{:x}", Sha256::digest(account.as_bytes())))
}
pub async fn forget(dirs: &AppDirs) -> Result<()> {
    let path = dirs.state.join("soloist").join("accounts");
    match tokio::fs::remove_dir_all(path).await {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

pub struct Config {
    pub dirs: AppDirs,
    pub account: String,
    pub key: Option<String>,
    pub lease: Lease,
    pub http: Http,
    pub notify: Notify,
    pub audio_config: crate::player::EngineConfig,
    pub output_volume: OutputVolume,
}

async fn run(args: Config, client: Client, mut stopped: watch::Receiver<bool>) -> Result<()> {
    let Config {
        dirs,
        account,
        key,
        lease,
        http,
        notify,
        audio_config,
        output_volume,
    } = args;
    ensure!(
        install::supported(),
        "Spotify does not publish Soloist for this platform yet."
    );
    notify(Update::Status(Status::Starting));
    let prepare = async {
        let key = match key {
            Some(value) => {
                let key = Key {
                    account: account.clone(),
                    value: value.trim().into(),
                };
                ensure!(
                    key.valid(),
                    "Enter a valid personal Soloist key from Spotify."
                );
                lease.save(Grant::Soloist(key.clone())).await.context(
                "Could not save the Soloist key. Unlock the system credential store and try again.",
            )?;
                key
            }
            None => match lease.load().await?.grant {
                Some(Grant::Soloist(key)) if key.account == account => key,
                _ => {
                    notify(Update::Status(Status::NeedsKey));
                    return Ok(None);
                }
            },
        };
        let root = dirs.state.join("soloist").join("builds");
        let data = account_dir(&dirs, &account);
        let prepare_root = root.clone();
        let prepare_data = data.clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            if let Some(parent) = prepare_root.parent() {
                install::private_dir(parent)?;
            }
            install::private_dir(&prepare_root)?;
            install::private_dir(&prepare_data)?;
            Ok(())
        })
        .await??;
        let instance =
            install::instance_id(root.parent().expect("Soloist state directory")).await?;
        // Include the installation identity as well as the account/profile:
        // different computers can have identical state paths and usernames.
        let suffix = format!(
            "{:x}",
            Sha256::digest(format!("{instance}:{}", data.to_string_lossy()).as_bytes())
        );
        let name = format!("Spotifast Soloist ({})", &suffix[..8]);
        let build = if let Some(build) = install::saved(&root, "pending.json").await {
            build
        } else if let Some(build) = install::saved(&root, "current.json").await {
            build
        } else {
            install::download(&root, &http, &|received, total| {
                notify(Update::Status(Status::Downloading { received, total }))
            })
            .await?
        };
        Ok::<_, anyhow::Error>(Some((key, root, data, name, build)))
    };
    let prepared = tokio::select! {
        _ = stopped.changed() => return Ok(()),
        result = prepare => result?,
    };
    let Some((key, root, data, name, mut build)) = prepared else {
        return Ok(());
    };
    let mut keep = vec![build.digest.clone()];
    if let Some(previous) = install::saved(&root, "current.json").await {
        keep.push(previous.digest);
    }
    let _ = install::collect(&root, &keep).await;
    let mut expired_retries = 0;
    loop {
        if *stopped.borrow() {
            return Ok(());
        }
        let previous = install::saved(&root, "current.json").await;
        let outcome = process(
            Player {
                root: &root,
                data: &data,
                name: &name,
                key: &key,
                build: &build,
                http: &http,
                notify: &notify,
                client: &client,
                audio_config: &audio_config,
                output_volume: &output_volume,
            },
            stopped.clone(),
        )
        .await;
        let outcome = match outcome {
            Ok(outcome) => outcome,
            Err(_)
                if previous
                    .as_ref()
                    .is_some_and(|old| old.digest != build.digest) =>
            {
                build = previous.expect("checked previous build");
                // Restore the pointer before retrying. If the old player also
                // fails, it must report the error instead of bouncing forever
                // between two installed builds.
                install::remember(&root, "current.json", &build).await?;
                let _ = tokio::fs::remove_file(root.join("pending.json")).await;
                notify(Update::UpdateFailed);
                continue;
            }
            Err(error) => return Err(error),
        };
        match outcome {
            Outcome::Stopped => return Ok(()),
            Outcome::Update(next) => build = next,
            Outcome::Expired if expired_retries == 0 => {
                expired_retries += 1;
                let progress = |received, total| {
                    notify(Update::Status(Status::Downloading { received, total }))
                };
                build = tokio::select! {
                    _ = stopped.changed() => return Ok(()),
                    result = install::download(&root, &http, &progress) => result?,
                };
            }
            Outcome::Expired => bail!(
                "Spotify's current Soloist build has expired. Try again when Spotify publishes its replacement."
            ),
        }
    }
}

enum Outcome {
    Stopped,
    Update(install::Build),
    Expired,
}
struct Player<'a> {
    root: &'a std::path::Path,
    data: &'a std::path::Path,
    name: &'a str,
    key: &'a Key,
    build: &'a install::Build,
    http: &'a Http,
    notify: &'a Notify,
    client: &'a Client,
    audio_config: &'a crate::player::EngineConfig,
    output_volume: &'a OutputVolume,
}
struct CancelDownload(tokio::task::AbortHandle);
impl Drop for CancelDownload {
    fn drop(&mut self) {
        self.0.abort();
    }
}
async fn process(player: Player<'_>, mut stopped: watch::Receiver<bool>) -> Result<Outcome> {
    let Player {
        root,
        data,
        name,
        key,
        build,
        http,
        notify,
        client,
        audio_config,
        output_volume,
    } = player;
    for file in ["ws.addr", "ws.port"] {
        let _ = tokio::fs::remove_file(data.join(file)).await;
    }
    let bridge = audio::Bridge::open(audio_config.clone(), output_volume.clone()).await;
    let mut command = tokio::process::Command::new(build.path(root));
    #[cfg(target_os = "linux")]
    if let Some(bridge) = &bridge {
        command.args(["--pipewire-device", &bridge.name]);
        let props = std::env::var("PIPEWIRE_PROPS").unwrap_or_default();
        let props = props.trim();
        let props = props
            .strip_prefix('{')
            .and_then(|s| s.strip_suffix('}'))
            .unwrap_or(props);
        // Only this child's stream stays at unity. Its documented volume
        // events drive our final gain, after the equalizer and visualizers.
        command.env(
            "PIPEWIRE_PROPS",
            format!("{{ {props} channelmix.lock-volumes = true node.dont-fallback = true node.dont-move = true }}"),
        );
    }
    command
        .args([
            "--device-name",
            name,
            "--api-key",
            &key.value,
            "--ws",
            "127.0.0.1:0",
            "--cache-size",
            "512",
        ])
        .arg("--data-dir")
        .arg(data)
        .arg("--cache-dir")
        .arg(data.join("cache"))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x08000000);
    // Soloist currently requires the key in argv. Never log this command or
    // the child's output: it stays in the private data directory, without the
    // key. See the documented upstream limitation.
    let mut child = command.spawn().context(
        "Soloist could not start. Check that this operating system supports Spotify's build.",
    )?;
    keep_output(&data.join("soloist-output.log"), &key.value, &mut child).await;
    if let Some(bridge) = &bridge {
        bridge.child(child.id().context("Missing Soloist process")?);
    }
    let mut cancellation = stopped.clone();
    let work = async {
        let port = tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                if let Some(exit) = child.try_wait()? {
                    log::warn!("Soloist exited before it was ready: {exit}");
                    if exit.code() == Some(10) { return Ok(None); }
                    bail!("Soloist stopped before it was ready. Check your key and this computer's audio service, then retry.");
                }
                if let Ok(text) = tokio::fs::read_to_string(data.join("ws.port")).await
                    && let Ok(port) = text.trim().parse::<u16>() && port != 0 {
                    let address = tokio::fs::read_to_string(data.join("ws.addr")).await?;
                    ensure!(address.trim() == "127.0.0.1", "Soloist did not bind its API to this computer");
                    break Ok(Some(port));
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }).await.context("Soloist took too long to start. Retry after checking the audio service.")??;
        let Some(port) = port else {
            return Ok(Outcome::Expired);
        };
        let mut socket =
            tokio::time::timeout(Duration::from_secs(5), protocol::connect(port)).await??;
        install::remember(root, "current.json", build).await?;
        let _ = tokio::fs::remove_file(root.join("pending.json")).await;
        client.listening(Some(port));
        notify(Update::Status(Status::Pairing {
            name: name.into(),
            version: build.version.clone(),
        }));
        let mut audio_tick = tokio::time::interval(Duration::from_millis(250));
        let mut audio_routed = false;
        let mut source_volume = volume::SourceVolume::default();
        let mut idle = false;
        let mut active = true;
        let mut player_idle = false;
        let mut logged_in = false;
        let mut pending = None;
        let mut downloading: Option<tokio::task::JoinHandle<Result<install::Build>>> = None;
        let mut cancel_download = None;
        let mut checked = build.checked;
        let mut tick = tokio::time::interval(Duration::from_secs(30));
        let mut changed = Instant::now() - Duration::from_secs(10);
        let mut changes = tokio::time::interval(Duration::from_millis(400));
        changes.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut pending_change = false;
        let mut pending_queue = false;
        loop {
            tokio::select! {
                _ = stopped.changed() => return Ok(Outcome::Stopped),
                _ = audio_tick.tick(), if bridge.is_some() => {
                    let bridge = bridge.as_ref().expect("audio bridge");
                    if bridge.failed() { bail!("Soloist's audio output stopped. Retry in Settings to reconnect it."); }
                    if source_volume.maintain(logged_in && bridge.owns_output_volume(), Instant::now())? {
                        protocol::send(&mut socket, json!({"command":"set_volume", "volume":100})).await?;
                        protocol::send(&mut socket, json!({"command":"get_state"})).await?;
                    }
                    if bridge.routed() != audio_routed { audio_routed = bridge.routed(); notify(Update::Audio(audio_routed)); }
                }
                exit = child.wait() => {
                    let exit = exit?;
                    log::warn!("Soloist exited: {exit}");
                    if exit.code() == Some(10) { return Ok(Outcome::Expired); }
                    bail!("Soloist stopped. Retry in Settings to start it again.");
                }
                _ = changes.tick(), if pending_change => {
                    changed = Instant::now();
                    pending_change = false;
                    notify(Update::Changed { queue: std::mem::take(&mut pending_queue) });
                }
                _ = tick.tick() => {
                    protocol::send(&mut socket, json!({"command":"get_auth_state"})).await?;
                    if logged_in { protocol::send(&mut socket, json!({"command":"get_state"})).await?; }
                    let mut current = build.clone();
                    current.checked = checked;
                    if current.due(install::now()) && downloading.is_none() && pending.is_none() {
                        checked = install::now();
                        let root = root.to_owned();
                        let http = http.clone();
                        let task = tokio::spawn(async move { install::download(&root, &http, &|_, _| {}).await });
                        cancel_download = Some(CancelDownload(task.abort_handle()));
                        downloading = Some(task);
                    }
                    if downloading.as_ref().is_some_and(|task| task.is_finished()) {
                        let result = downloading.take().expect("finished download").await?;
                        drop(cancel_download.take());
                        match result {
                            Ok(mut next) => {
                                next.checked = checked;
                                if next.digest != build.digest && next.built > build.built {
                                    install::remember(root, "pending.json", &next).await?;
                                    notify(Update::UpdateReady(next.version.clone()));
                                    pending = Some(next);
                                } else {
                                    let mut same = build.clone(); same.checked = checked;
                                    install::remember(root, "current.json", &same).await?;
                                }
                            }
                            Err(_) => notify(Update::UpdateFailed),
                        }
                    }
                }
                message = protocol::receive(&mut socket) => {
                    let message = message?;
                    if let Some(bridge) = &bridge { bridge.update(&message); }
                    if source_volume.observe(&message, output_volume) { notify(Update::OutputVolume); }
                    if source_volume.maintain(logged_in && message["logged_in"] != false && bridge.as_ref().is_some_and(|bridge| bridge.owns_output_volume()), Instant::now())? {
                        protocol::send(&mut socket, json!({"command":"set_volume", "volume":100})).await?;
                        protocol::send(&mut socket, json!({"command":"get_state"})).await?;
                    }
                    let kind = message["type"].as_str().unwrap_or_default();
                    if let Some(value) = message["is_active"].as_bool() { active = value; client.active(active); }
                    if kind == "auth_state" {
                        let authenticated = message["logged_in"].as_bool().unwrap_or(false);
                        if authenticated != logged_in {
                            logged_in = authenticated;
                            if logged_in {
                                notify(Update::Status(Status::Ready { name: name.into(), version: build.version.clone(), expires: build.built + 90 * install::DAY }));
                                if let Some(bridge) = &bridge { notify(Update::Audio(bridge.routed())); }
                                protocol::send(&mut socket, json!({"command":"get_state"})).await?;
                            }
                            else { client.bind(None); notify(Update::Status(Status::Pairing { name: name.into(), version: build.version.clone() })); }
                        }
                    }
                    // Pausing retains the queue and context in the live child.
                    // Only an inactive device or an idle player may restart.
                    if matches!(kind, "playback_state" | "playback_changed") {
                        if message["status"] == "idle" { player_idle = true; }
                        else if matches!(message["status"].as_str(), Some("playing" | "buffering" | "paused")) { player_idle = false; }
                    }
                    idle = !active || player_idle;
                    if matches!(kind, "track_changed" | "playback_changed" | "device_changed" | "context_changed" | "options_changed" | "queue_changed" | "position_sync" | "volume_changed") {
                        pending_queue |= matches!(kind, "queue_changed" | "track_changed" | "options_changed");
                        if changed.elapsed() >= Duration::from_millis(400) {
                            changed = Instant::now();
                            pending_change = false;
                            notify(Update::Changed { queue: std::mem::take(&mut pending_queue) });
                        } else { pending_change = true; }
                    }
                }
            }
            if idle && let Some(next) = pending.take() {
                return Ok(Outcome::Update(next));
            }
        }
    };
    let result = tokio::select! {
        _ = cancellation.changed() => Ok(Outcome::Stopped),
        result = work => result,
    };
    client.listening(None);
    // Always reap our own child before another launch or sign-out cleanup.
    let _ = child.kill().await;
    let _ = child.wait().await;
    drop(bridge);
    notify(Update::Audio(false));
    result
}

/// Soloist's own account of why it stopped goes to a file beside its data.
/// Each start replaces the file, and a long run stops at `OUTPUT_LIMIT`, so
/// what led to an early exit is what stays.
async fn keep_output(path: &std::path::Path, key: &str, child: &mut tokio::process::Child) {
    let Ok(file) = tokio::fs::File::create(path).await else {
        return;
    };
    let file = Arc::new(tokio::sync::Mutex::new((file, 0)));
    if let Some(stdout) = child.stdout.take() {
        tokio::spawn(copy_output(stdout, key.to_owned(), file.clone()));
    }
    if let Some(stderr) = child.stderr.take() {
        tokio::spawn(copy_output(stderr, key.to_owned(), file));
    }
}

const OUTPUT_LIMIT: usize = 1 << 20;

/// Drains one of the child's streams to the end, as a full pipe would stall it.
async fn copy_output(
    stream: impl tokio::io::AsyncRead + Unpin,
    key: String,
    file: Arc<tokio::sync::Mutex<(tokio::fs::File, usize)>>,
) {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
    let mut lines = tokio::io::BufReader::new(stream).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let line = format!("{}\n", line.replace(&key, "<key>"));
        let mut file = file.lock().await;
        if file.1 + line.len() <= OUTPUT_LIMIT {
            file.1 += line.len();
            let _ = file.0.write_all(line.as_bytes()).await;
            let _ = file.0.flush().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn soloist_output_is_kept_without_the_key() {
        use tokio::io::AsyncWriteExt;
        let path = std::env::current_dir()
            .unwrap()
            .join("target")
            .join(format!("soloist-output-{}", rand::random::<u64>()));
        let file = tokio::fs::File::create(&path).await.unwrap();
        let (mut child, stream) = tokio::io::duplex(64);
        let copy = tokio::spawn(copy_output(
            stream,
            "secret".into(),
            Arc::new(tokio::sync::Mutex::new((file, 0))),
        ));
        child.write_all(b"started with secret\n").await.unwrap();
        drop(child);
        copy.await.unwrap();
        let kept = tokio::fs::read_to_string(&path).await.unwrap();
        tokio::fs::remove_file(&path).await.unwrap();
        assert_eq!(kept, "started with <key>\n");
    }
    #[test]
    fn keys_are_account_scoped_and_reject_control_characters() {
        assert!(
            Key {
                account: "a".into(),
                value: "dummy-key".into()
            }
            .valid()
        );
        assert!(
            !Key {
                account: "".into(),
                value: "dummy-key".into()
            }
            .valid()
        );
        assert!(
            !Key {
                account: "a".into(),
                value: "key\n--pair".into()
            }
            .valid()
        );
    }
    /// Explicit live smoke test, never run by normal tests or CI. The caller
    /// provides a personal key via a secret manager. No playback is started.
    #[tokio::test]
    #[ignore = "downloads and starts Spotify Soloist; requires SOLOIST_QA_KEY from a secret manager"]
    async fn native_start_and_stop() {
        let value = std::env::var("SOLOIST_QA_KEY").expect("SOLOIST_QA_KEY is required");
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!("soloist-smoke-{}", rand::random::<u64>()));
        let dirs = AppDirs {
            config: root.join("config"),
            state: root.join("state"),
            cache: root.join("cache"),
        };
        let store = crate::credentials::Store::in_memory(dirs.clone());
        let (events, mut receive) = tokio::sync::mpsc::unbounded_channel();
        let audio_config = crate::app::engine_config(
            &dirs,
            &crate::settings::Settings::default(),
            crate::settings::ProxyConfig::Off,
            crate::vis::AudioTap::new(),
            crate::eq::shared(),
        );
        let handle = Handle::start(Config {
            dirs,
            account: "native-smoke-test".into(),
            key: Some(value),
            lease: store.lease(crate::credentials::Slot::Soloist),
            http: Http::default(),
            notify: Arc::new(move |event| {
                let _ = events.send(event);
            }),
            audio_config,
            output_volume: OutputVolume::default(),
        });
        let result = tokio::time::timeout(Duration::from_secs(120), async {
            while let Some(event) = receive.recv().await {
                match event {
                    Update::Status(Status::Pairing { version, .. }) => {
                        println!("Official Soloist {version} started and exposed its loopback API");
                        return Ok(());
                    }
                    Update::Status(Status::Failed(error)) => return Err(error),
                    _ => {}
                }
            }
            Err("Soloist ended without a setup status".into())
        })
        .await;
        handle.stop().await;
        tokio::fs::remove_dir_all(root).await.unwrap();
        assert!(
            matches!(&result, Ok(Ok(()))),
            "Live smoke test failed: {result:?}"
        );
    }
}
