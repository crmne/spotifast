//! Durable Spotify grants and proxy passwords in the platform credential store.
//!
//! All native calls run on one dedicated thread. A locked store cannot hold
//! the UI, command loop, or runtime shutdown hostage. Generation checks reject
//! work from before sign-out, including a write that returns after sign-out.
//! Non-secret revocation markers prevent restoration after a failed deletion.

use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
    mpsc,
};
use std::time::Duration;

use librespot_core::authentication::Credentials;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{auth::StoredToken, paths::AppDirs};

const SERVICE: &str = "rocks.spotizgeg.Spotizgeg";
/// Where grants were kept before the fork was renamed: read once, moved to
/// [`SERVICE`], and removed on sign-out like the new ones.
const PREVIOUS_SERVICE: &str = "rocks.spotifast.Spotifast";
const TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Slot {
    Shared,
    Personal,
    Playback,
    Proxy,
}

impl Slot {
    pub const SPOTIFY: [Self; 3] = [Self::Shared, Self::Personal, Self::Playback];
    pub const ALL: [Self; 4] = [Self::Shared, Self::Personal, Self::Playback, Self::Proxy];
    pub(crate) fn index(self) -> usize {
        self as usize
    }
    fn name(self) -> &'static str {
        match self {
            Self::Shared => "shared-web",
            Self::Personal => "personal-web",
            Self::Playback => "playback",
            Self::Proxy => "proxy-password",
        }
    }
}

/// Deliberately has no Debug implementation: it contains usable secrets.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub enum Grant {
    Web(StoredToken),
    Playback(Credentials),
    Proxy(ProxyPassword),
}

/// No Debug implementation: the password is usable, and the username is private.
/// A saved password belongs to one network endpoint and username.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct ProxyPassword {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: String,
}

impl ProxyPassword {
    fn valid(&self) -> bool {
        let settings = crate::settings::Settings {
            proxy_host: self.host.clone(),
            proxy_port: self.port.to_string(),
            proxy_username: self.username.clone(),
            proxy_password: self.password.clone(),
            ..Default::default()
        };
        settings.proxy_password_record().ok().flatten().as_ref() == Some(self)
    }
}

impl Grant {
    fn valid_for(&self, slot: Slot) -> bool {
        match (slot, self) {
            (Slot::Shared, Self::Web(token)) => {
                token.client_id == crate::auth::DEFAULT_WEB_CLIENT_ID
                    && !token.refresh_token.is_empty()
            }
            (Slot::Personal, Self::Web(token)) => {
                !token.client_id.is_empty() && !token.refresh_token.is_empty()
            }
            (Slot::Playback, Self::Playback(credentials)) => {
                credentials
                    .username
                    .as_deref()
                    .is_some_and(|name| !name.is_empty())
                    && !credentials.auth_data.is_empty()
            }
            (Slot::Proxy, Self::Proxy(password)) => password.valid(),
            _ => false,
        }
    }
}

#[derive(Serialize, Deserialize)]
struct Record {
    version: u32,
    slot: Slot,
    grant: Grant,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error(
        "The system credential store is unavailable. Unlock or enable it to remember this sign-in."
    )]
    Unavailable,
    #[error(
        "The system credential store is locked or access was denied. Unlock it to remember this sign-in."
    )]
    Locked,
    #[error("The system credential store did not respond. This sign-in cannot be remembered yet.")]
    Timeout,
    #[error("The stored Spotify grant is invalid. Sign in again.")]
    Invalid,
    #[error(
        "Unable to update credential-storage state. Check the application state directory's permissions."
    )]
    Filesystem,
    #[error(
        "The system credential store did not retain the grant. This sign-in cannot be remembered."
    )]
    Verification,
    #[error("The sign-in changed while credential storage was in progress.")]
    Stale,
}

impl Error {
    pub(crate) fn proxy_message(self) -> &'static str {
        match self {
            Self::Unavailable | Self::Locked => {
                "Unlock or enable the system credential store to remember the proxy password."
            }
            Self::Timeout => {
                "The credential store did not respond. The proxy password is available only for this session."
            }
            Self::Invalid => {
                "The stored proxy password or its endpoint is invalid. Enter the proxy settings again."
            }
            Self::Filesystem => {
                "Unable to update proxy-password storage. Check the application state directory permissions."
            }
            Self::Verification => {
                "The credential store did not retain the proxy password. It is available only for this session."
            }
            Self::Stale => "The proxy settings changed while the password was being stored.",
        }
    }
}

fn native_error(error: keyring_core::Error) -> Error {
    // Some provider errors contain the secret or arbitrary platform data.
    // Never propagate their Debug/Display text into application diagnostics.
    match error {
        keyring_core::Error::NoStorageAccess(_) => Error::Locked,
        _ => Error::Unavailable,
    }
}

trait ProtectedStore: Send {
    fn read(&mut self, key: &str) -> Result<Option<Vec<u8>>, Error>;
    fn write(&mut self, key: &str, secret: &[u8]) -> Result<(), Error>;
    fn delete(&mut self, key: &str) -> Result<(), Error>;
}

#[derive(Default)]
struct NativeStore {
    store: Option<Arc<keyring_core::api::CredentialStore>>,
}

impl NativeStore {
    fn entry(&mut self, key: &str) -> Result<keyring_core::Entry, Error> {
        self.entry_in(SERVICE, key)
    }

    fn entry_in(&mut self, service: &str, key: &str) -> Result<keyring_core::Entry, Error> {
        if self.store.is_none() {
            #[cfg(target_os = "linux")]
            let store = zbus_secret_service_keyring_store::Store::new();
            #[cfg(target_os = "macos")]
            let store = apple_native_keyring_store::keychain::Store::new();
            #[cfg(windows)]
            let store = windows_native_keyring_store::Store::new();
            self.store = Some(store.map_err(native_error)?);
        }
        self.store
            .as_ref()
            .ok_or(Error::Unavailable)?
            .build(service, key, None)
            .map_err(native_error)
    }

    fn read_in(&mut self, service: &str, key: &str) -> Result<Option<Vec<u8>>, Error> {
        match self.entry_in(service, key)?.get_secret() {
            Ok(secret) => Ok(Some(secret)),
            Err(keyring_core::Error::NoEntry) => Ok(None),
            Err(error) => Err(native_error(error)),
        }
    }

    fn delete_in(&mut self, service: &str, key: &str) -> Result<(), Error> {
        match self.entry_in(service, key)?.delete_credential() {
            Ok(()) | Err(keyring_core::Error::NoEntry) => Ok(()),
            Err(error) => Err(native_error(error)),
        }
    }
}

impl ProtectedStore for NativeStore {
    fn read(&mut self, key: &str) -> Result<Option<Vec<u8>>, Error> {
        if let Some(secret) = self.read_in(SERVICE, key)? {
            return Ok(Some(secret));
        }
        // A grant from before the rename moves over the first time it is
        // read; should the move fail, it is still used from where it is.
        let Some(secret) = self.read_in(PREVIOUS_SERVICE, key)? else {
            return Ok(None);
        };
        if self.write(key, &secret).is_ok() {
            let _ = self.delete_in(PREVIOUS_SERVICE, key);
        }
        Ok(Some(secret))
    }
    fn write(&mut self, key: &str, secret: &[u8]) -> Result<(), Error> {
        self.entry(key)?.set_secret(secret).map_err(native_error)
    }
    fn delete(&mut self, key: &str) -> Result<(), Error> {
        self.delete_in(PREVIOUS_SERVICE, key)?;
        self.delete_in(SERVICE, key)
    }
}

type Job = Box<dyn FnOnce(&mut dyn ProtectedStore) + Send>;

struct Inner {
    dirs: AppDirs,
    profile: String,
    generations: [AtomicU64; 4],
    // Held only around the tiny local marker files, never a native store call.
    markers: Mutex<()>,
    jobs: mpsc::SyncSender<Job>,
}

#[derive(Clone)]
pub struct Store {
    inner: Arc<Inner>,
}

/// Permission to read/write one grant for one authorization generation.
#[derive(Clone)]
pub struct Lease {
    store: Store,
    slot: Slot,
    generation: u64,
}

#[derive(Serialize, Deserialize)]
struct Marker {
    version: u32,
    revoked: bool,
}

pub struct Loaded {
    pub grant: Option<Grant>,
    pub warning: Option<Error>,
}

impl Store {
    pub fn new(dirs: AppDirs) -> Self {
        Self::with_backend(dirs, Box::new(NativeStore::default()))
    }

    #[cfg(test)]
    pub(crate) fn in_memory(dirs: AppDirs) -> Self {
        tests::memory_store(dirs)
    }

    fn with_backend(dirs: AppDirs, mut backend: Box<dyn ProtectedStore>) -> Self {
        let (jobs, receiver) = mpsc::sync_channel::<Job>(16);
        let runtime = tokio::runtime::Handle::try_current().ok();
        let _ = std::thread::Builder::new()
            .name("spotify-credentials".into())
            .spawn(move || {
                let _entered = runtime.as_ref().map(tokio::runtime::Handle::enter);
                while let Ok(job) = receiver.recv() {
                    job(backend.as_mut());
                }
            });
        let profile = std::fs::read_to_string(dirs.state.join("credential-profile"))
            .ok()
            .filter(|value| value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
            .unwrap_or_else(|| {
                format!(
                    "{:x}",
                    Sha256::digest(dirs.state.to_string_lossy().as_bytes())
                )
            });
        Self {
            inner: Arc::new(Inner {
                dirs,
                profile,
                generations: std::array::from_fn(|_| AtomicU64::new(0)),
                markers: Mutex::new(()),
                jobs,
            }),
        }
    }

    pub fn lease(&self, slot: Slot) -> Lease {
        Lease {
            store: self.clone(),
            slot,
            generation: self.inner.generations[slot.index()].load(Ordering::SeqCst),
        }
    }

    /// Invalidate in-flight work before canceling workers or deleting grants.
    pub fn invalidate(&self, slot: Slot) {
        self.inner.generations[slot.index()].fetch_add(1, Ordering::SeqCst);
    }

    /// Persist revocation before contacting the native store. Even a locked
    /// keychain or an interrupted deletion must not sign the user back in.
    pub fn revoke(&self, slot: Slot) -> Result<(), Error> {
        self.invalidate(slot);
        let _guard = self.inner.markers.lock().unwrap_or_else(|p| p.into_inner());
        let marker = self.write_marker(slot, true);
        let legacy = self.remove_legacy(slot);
        marker.and(legacy)
    }

    pub fn revoke_spotify(&self) -> Result<(), Error> {
        let mut error = None;
        for slot in Slot::SPOTIFY {
            if let Err(failure) = self.revoke(slot) {
                error = Some(failure);
            }
        }
        // Sign-out also removes corrupt/partial old combined records, whose
        // client identity cannot be established by a migration reader.
        let legacy = self.inner.dirs.legacy_web_token_file();
        for path in [legacy.clone(), legacy.with_extension("json.tmp")] {
            if let Err(failure) = remove_file(&path) {
                error = Some(failure);
            }
        }
        error.map_or(Ok(()), Err)
    }

    fn marker_path(&self, slot: Slot) -> PathBuf {
        self.inner
            .dirs
            .state
            .join("credential-storage")
            .join(format!("{}.json", slot.name()))
    }

    fn marker(&self, slot: Slot) -> Result<Option<Marker>, Error> {
        match std::fs::read(self.marker_path(slot)) {
            Ok(bytes) => {
                let marker: Marker = serde_json::from_slice(&bytes).map_err(|_| Error::Invalid)?;
                if marker.version != 1 {
                    return Err(Error::Invalid);
                }
                Ok(Some(marker))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(_) => Err(Error::Filesystem),
        }
    }

    fn write_marker(&self, slot: Slot, revoked: bool) -> Result<(), Error> {
        let path = self.marker_path(slot);
        let parent = path.parent().ok_or(Error::Filesystem)?;
        std::fs::create_dir_all(parent).map_err(|_| Error::Filesystem)?;
        let bytes = serde_json::to_vec(&Marker {
            version: 1,
            revoked,
        })
        .map_err(|_| Error::Invalid)?;
        let temporary = path.with_extension("json.tmp");
        std::fs::write(&temporary, bytes).map_err(|_| Error::Filesystem)?;
        crate::util::replace_file(&temporary, &path).map_err(|_| Error::Filesystem)
    }

    fn legacy_paths(&self, slot: Slot) -> Vec<PathBuf> {
        if slot == Slot::Proxy {
            let path = self.inner.dirs.proxy_secret_file();
            return vec![path.clone(), path.with_extension("tmp")];
        }
        let dirs = &self.inner.dirs;
        let path = match slot {
            Slot::Shared => dirs.shared_web_token_file(),
            Slot::Personal => dirs.personal_web_token_file(),
            Slot::Playback => dirs.credentials_dir().join("credentials.json"),
            Slot::Proxy => unreachable!("proxy legacy paths handled above"),
        };
        let mut paths = vec![path.clone(), path.with_extension("json.tmp")];
        if slot != Slot::Playback {
            // The old combined path belongs to whichever client identity it contains.
            let legacy = dirs.legacy_web_token_file();
            if StoredToken::load(&legacy).is_some_and(|token| {
                (slot == Slot::Shared) == (token.client_id == crate::auth::DEFAULT_WEB_CLIENT_ID)
            }) {
                paths.push(legacy.clone());
                paths.push(legacy.with_extension("json.tmp"));
            }
        }
        paths
    }

    fn remove_legacy(&self, slot: Slot) -> Result<(), Error> {
        let mut result = Ok(());
        for path in self.legacy_paths(slot) {
            if let Err(error) = remove_file(&path) {
                result = Err(error);
            }
        }
        result
    }

    fn legacy_proxy(&self) -> Result<Option<Grant>, Error> {
        let text = match std::fs::read_to_string(self.inner.dirs.settings_file()) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return match self.inner.dirs.proxy_secret_file().try_exists() {
                    Ok(false) => Ok(None),
                    Ok(true) => Err(Error::Invalid),
                    Err(_) => Err(Error::Filesystem),
                };
            }
            Err(_) => return Err(Error::Filesystem),
        };
        let mut settings: crate::settings::Settings =
            serde_json::from_str(&text).map_err(|_| Error::Invalid)?;
        settings.migrate_proxy(&text);
        match std::fs::read_to_string(self.inner.dirs.proxy_secret_file()) {
            Ok(password) => settings.proxy_password = password,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(Error::Filesystem),
        }
        settings
            .proxy_password_record()
            .map(|password| password.map(Grant::Proxy))
            .map_err(|_| Error::Invalid)
    }

    fn legacy(&self, slot: Slot) -> Result<Option<Grant>, Error> {
        if slot == Slot::Proxy {
            return self.legacy_proxy();
        }
        for path in self
            .legacy_paths(slot)
            .into_iter()
            .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        {
            let bytes = match std::fs::read(path) {
                Ok(bytes) => bytes,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(_) => return Err(Error::Filesystem),
            };
            let grant = if slot == Slot::Playback {
                Grant::Playback(serde_json::from_slice(&bytes).map_err(|_| Error::Invalid)?)
            } else {
                Grant::Web(serde_json::from_slice(&bytes).map_err(|_| Error::Invalid)?)
            };
            if !grant.valid_for(slot) {
                return Err(Error::Invalid);
            }
            return Ok(Some(grant));
        }
        Ok(None)
    }
}

fn remove_file(path: &Path) -> Result<(), Error> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(Error::Filesystem),
    }
}

impl Lease {
    pub fn current(&self) -> bool {
        self.store.inner.generations[self.slot.index()].load(Ordering::SeqCst) == self.generation
    }
    pub fn generation(&self) -> u64 {
        self.generation
    }
    fn check(&self) -> Result<(), Error> {
        if self.current() {
            Ok(())
        } else {
            Err(Error::Stale)
        }
    }
    fn key(&self) -> String {
        format!("{}:{}", self.store.inner.profile, self.slot.name())
    }

    fn request<T: Send + 'static>(
        &self,
        guard_generation: bool,
        operation: impl FnOnce(Self, &mut dyn ProtectedStore) -> Result<T, Error> + Send + 'static,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<T, Error>> + Send>> {
        let pending = (|| {
            if guard_generation {
                self.check()?;
            }
            let (sender, receiver) = tokio::sync::oneshot::channel();
            let lease = self.clone();
            self.store
                .inner
                .jobs
                .try_send(Box::new(move |backend| {
                    let result = (if guard_generation {
                        lease.check()
                    } else {
                        Ok(())
                    })
                    .and_then(|()| operation(lease.clone(), backend));
                    let result = result.and_then(|value| {
                        if guard_generation {
                            lease.check()?;
                        }
                        Ok(value)
                    });
                    let _ = sender.send(result);
                }))
                .map_err(|_| Error::Unavailable)?;
            Ok::<_, Error>(receiver)
        })();
        Box::pin(async move {
            tokio::time::timeout(TIMEOUT, pending?)
                .await
                .map_err(|_| Error::Timeout)?
                .map_err(|_| Error::Unavailable)?
        })
    }

    pub async fn load(&self) -> Result<Loaded, Error> {
        self.request(true, |lease, backend| {
            let marker = lease.store.marker(lease.slot)?;
            if marker.as_ref().is_some_and(|marker| marker.revoked) {
                return Ok(Loaded {
                    grant: None,
                    warning: None,
                });
            }
            let existing = backend.read(&lease.key());
            if let Ok(Some(bytes)) = &existing {
                let record: Record = serde_json::from_slice(bytes).map_err(|_| Error::Invalid)?;
                if record.version != 1
                    || record.slot != lease.slot
                    || !record.grant.valid_for(lease.slot)
                {
                    return Err(Error::Invalid);
                }
                let _guard = lease
                    .store
                    .inner
                    .markers
                    .lock()
                    .unwrap_or_else(|p| p.into_inner());
                lease.check()?;
                lease.store.write_marker(lease.slot, false)?;
                let warning = lease.store.remove_legacy(lease.slot).err();
                return Ok(Loaded {
                    grant: Some(record.grant),
                    warning,
                });
            }
            if marker.is_some() {
                return existing.map(|_| Loaded {
                    grant: None,
                    warning: None,
                });
            }
            let Some(grant) = lease.store.legacy(lease.slot)? else {
                return existing.map(|_| Loaded {
                    grant: None,
                    warning: None,
                });
            };
            // Keep a failed migration recoverable. No new plaintext is written,
            // and the caller must show the warning about persistence.
            let warning = match existing {
                Err(error) => Some(error),
                Ok(_) => lease.save_inner(backend, grant.clone()).err(),
            };
            Ok(Loaded {
                grant: Some(grant),
                warning,
            })
        })
        .await
    }

    fn save_inner(&self, backend: &mut dyn ProtectedStore, grant: Grant) -> Result<(), Error> {
        self.check()?;
        if !grant.valid_for(self.slot) {
            return Err(Error::Invalid);
        }
        let bytes = serde_json::to_vec(&Record {
            version: 1,
            slot: self.slot,
            grant,
        })
        .map_err(|_| Error::Invalid)?;
        backend.write(&self.key(), &bytes)?;
        if !self.current() {
            // Operations on this store are serialized. No newer write can be
            // erased here; it is still queued behind this one.
            let _ = backend.delete(&self.key());
            return Err(Error::Stale);
        }
        if backend.read(&self.key())?.as_deref() != Some(bytes.as_slice()) {
            return Err(Error::Verification);
        }
        let _guard = self
            .store
            .inner
            .markers
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        self.check()?;
        self.store.write_marker(self.slot, false)?;
        self.store.remove_legacy(self.slot)
    }

    /// Enqueue immediately so refreshes preserve write order even when callers
    /// await completion outside their token mutex.
    pub fn save(
        &self,
        grant: Grant,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), Error>> + Send>> {
        self.request(true, move |lease, backend| lease.save_inner(backend, grant))
    }

    /// Call `Store::revoke` before this. Deletion failure remains visible while
    /// the local revocation marker prevents restoration on the next launch.
    pub fn delete(
        &self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), Error>> + Send>> {
        // Deletion is enqueued before any replacement write. Run it even if a
        // subsequent authorization increments the generation while it waits.
        self.request(false, |lease, backend| backend.delete(&lease.key()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[derive(Default)]
    struct Fake {
        values: HashMap<String, Vec<u8>>,
        read_error: Option<Error>,
        write_error: Option<Error>,
        delete_error: Option<Error>,
        discard_write: bool,
        entered: Option<tokio::sync::oneshot::Sender<()>>,
        release: Option<mpsc::Receiver<()>>,
    }
    pub(super) fn memory_store(dirs: AppDirs) -> Store {
        Store::with_backend(
            dirs,
            Box::new(Backend(Arc::new(Mutex::new(Fake::default())))),
        )
    }
    struct Backend(Arc<Mutex<Fake>>);
    impl ProtectedStore for Backend {
        fn read(&mut self, key: &str) -> Result<Option<Vec<u8>>, Error> {
            let fake = self.0.lock().unwrap();
            if let Some(error) = fake.read_error {
                return Err(error);
            }
            Ok(fake.values.get(key).cloned())
        }
        fn write(&mut self, key: &str, value: &[u8]) -> Result<(), Error> {
            let (entered, release) = {
                let mut fake = self.0.lock().unwrap();
                if let Some(error) = fake.write_error {
                    return Err(error);
                }
                (fake.entered.take(), fake.release.take())
            };
            if let Some(entered) = entered {
                let _ = entered.send(());
            }
            if let Some(release) = release {
                release.recv().unwrap();
            }
            let mut fake = self.0.lock().unwrap();
            if !fake.discard_write {
                fake.values.insert(key.to_owned(), value.to_vec());
            }
            Ok(())
        }
        fn delete(&mut self, key: &str) -> Result<(), Error> {
            let mut fake = self.0.lock().unwrap();
            if let Some(error) = fake.delete_error {
                return Err(error);
            }
            fake.values.remove(key);
            Ok(())
        }
    }

    struct Fixture {
        dirs: AppDirs,
        fake: Arc<Mutex<Fake>>,
        store: Store,
    }
    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let root = std::env::temp_dir().join(format!(
                "spotizgeg-credential-tests-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let dirs = AppDirs {
                config: root.join("config"),
                state: root.join("state"),
                cache: root.join("cache"),
            };
            dirs.ensure().unwrap();
            let fake = Arc::new(Mutex::new(Fake::default()));
            let store = Store::with_backend(dirs.clone(), Box::new(Backend(fake.clone())));
            Self { dirs, fake, store }
        }
        fn restart(&self) -> Store {
            Store::with_backend(self.dirs.clone(), Box::new(Backend(self.fake.clone())))
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(self.dirs.state.parent().unwrap());
        }
    }

    fn web(client: &str) -> Grant {
        Grant::Web(StoredToken {
            client_id: client.into(),
            access_token: "dummy-access".into(),
            refresh_token: "dummy-refresh".into(),
            expires_at: u64::MAX,
            scope: "dummy-scope".into(),
        })
    }
    fn playback() -> Grant {
        Grant::Playback(Credentials { username: Some("dummy-account".into()), auth_data: b"dummy-reusable-grant".to_vec(), auth_type: librespot_protocol::authentication::AuthenticationType::AUTHENTICATION_STORED_SPOTIFY_CREDENTIALS })
    }

    #[tokio::test]
    async fn a_recorded_credential_profile_keeps_grants_and_revocation_markers() {
        let fixture = Fixture::new();
        let grant = web(crate::auth::DEFAULT_WEB_CLIENT_ID);
        fixture
            .store
            .lease(Slot::Shared)
            .save(grant.clone())
            .await
            .unwrap();
        fixture.store.revoke(Slot::Playback).unwrap();
        let parent = fixture.dirs.state.parent().unwrap();
        let renamed = AppDirs {
            config: parent.join("new-config"),
            state: parent.join("new-state"),
            cache: parent.join("new-cache"),
        };
        // Profiles moved by the rename record the account identity of their
        // original state folder; grants saved under it must stay reachable.
        std::fs::rename(&fixture.dirs.state, &renamed.state).unwrap();
        std::fs::write(
            renamed.state.join("credential-profile"),
            &fixture.store.inner.profile,
        )
        .unwrap();
        let store = Store::with_backend(renamed, Box::new(Backend(fixture.fake.clone())));
        assert_eq!(store.inner.profile, fixture.store.inner.profile);
        assert!(store.lease(Slot::Shared).load().await.unwrap().grant == Some(grant));
        assert!(
            store
                .lease(Slot::Playback)
                .load()
                .await
                .unwrap()
                .grant
                .is_none()
        );
    }
    fn proxy_password() -> Grant {
        Grant::Proxy(ProxyPassword {
            host: "127.0.0.1".into(),
            port: 8080,
            username: "dummy-user".into(),
            password: " dummy-private-password\n".into(),
        })
    }

    fn write_legacy(path: &Path, grant: &Grant) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let bytes = match grant {
            Grant::Web(token) => serde_json::to_vec(token).unwrap(),
            Grant::Playback(credentials) => serde_json::to_vec(credentials).unwrap(),
            Grant::Proxy(password) => password.password.as_bytes().to_vec(),
        };
        std::fs::write(path, bytes).unwrap();
    }

    fn write_proxy_legacy(f: &Fixture, combined: bool) -> Grant {
        let Grant::Proxy(password) = proxy_password() else {
            unreachable!()
        };
        let settings = crate::settings::Settings {
            proxy_mode: crate::settings::ProxyMode::Http,
            proxy_host: password.host.clone(),
            proxy_port: password.port.to_string(),
            proxy_username: password.username.clone(),
            ..Default::default()
        };
        let mut json = serde_json::to_value(settings).unwrap();
        if combined {
            json["proxy_password"] = password.password.clone().into();
        }
        std::fs::write(f.dirs.settings_file(), serde_json::to_vec(&json).unwrap()).unwrap();
        if !combined {
            std::fs::write(f.dirs.proxy_secret_file(), password.password.as_bytes()).unwrap();
        }
        Grant::Proxy(password)
    }

    #[tokio::test]
    async fn proxy_password_migration_verifies_before_removing_legacy_data() {
        for combined in [false, true] {
            let f = Fixture::new();
            let expected = write_proxy_legacy(&f, combined);
            let settings_before = std::fs::read(f.dirs.settings_file()).unwrap();
            let loaded = f.store.lease(Slot::Proxy).load().await.unwrap();
            assert!(loaded.grant == Some(expected.clone()));
            assert_eq!(loaded.warning, None);
            assert!(!f.dirs.proxy_secret_file().exists());
            // Settings belong to the UI thread. A verified event permits its
            // next atomic save to remove the legacy JSON password.
            assert_eq!(
                std::fs::read(f.dirs.settings_file()).unwrap(),
                settings_before
            );
            assert!(f.restart().lease(Slot::Proxy).load().await.unwrap().grant == Some(expected));
        }
    }

    #[tokio::test]
    async fn a_failed_proxy_migration_retains_the_exact_original_and_can_retry() {
        for combined in [false, true] {
            let f = Fixture::new();
            let expected = write_proxy_legacy(&f, combined);
            let path = if combined {
                f.dirs.settings_file()
            } else {
                f.dirs.proxy_secret_file()
            };
            let before = std::fs::read(&path).unwrap();
            f.fake.lock().unwrap().write_error = Some(Error::Locked);
            let loaded = f.store.lease(Slot::Proxy).load().await.unwrap();
            assert!(
                loaded.grant == Some(expected.clone()),
                "legacy password still serves this session"
            );
            assert_eq!(loaded.warning, Some(Error::Locked));
            assert_eq!(std::fs::read(&path).unwrap(), before);
            f.fake.lock().unwrap().write_error = None;
            let loaded = f.restart().lease(Slot::Proxy).load().await.unwrap();
            assert!(loaded.grant == Some(expected));
            assert_eq!(loaded.warning, None);
        }
    }

    #[tokio::test]
    async fn spotify_sign_out_retains_the_network_password() {
        let f = Fixture::new();
        let password = proxy_password();
        f.store
            .lease(Slot::Proxy)
            .save(password.clone())
            .await
            .unwrap();
        f.store
            .lease(Slot::Shared)
            .save(web(crate::auth::DEFAULT_WEB_CLIENT_ID))
            .await
            .unwrap();
        f.store.revoke_spotify().unwrap();
        let restarted = f.restart();
        assert!(restarted.lease(Slot::Proxy).load().await.unwrap().grant == Some(password));
        assert!(
            restarted
                .lease(Slot::Shared)
                .load()
                .await
                .unwrap()
                .grant
                .is_none()
        );
    }

    #[tokio::test]
    async fn a_failed_proxy_delete_cannot_restore_the_forgotten_password() {
        let f = Fixture::new();
        let password = write_proxy_legacy(&f, true);
        f.store.lease(Slot::Proxy).save(password).await.unwrap();
        f.fake.lock().unwrap().delete_error = Some(Error::Locked);
        f.store.revoke(Slot::Proxy).unwrap();
        assert_eq!(
            f.store.lease(Slot::Proxy).delete().await,
            Err(Error::Locked)
        );
        assert!(
            f.restart()
                .lease(Slot::Proxy)
                .load()
                .await
                .unwrap()
                .grant
                .is_none()
        );
    }

    #[tokio::test]
    #[ignore = "requires an unlocked platform credential store; uses dummy grants only"]
    async fn native_store_round_trip() {
        let f = Fixture::new();
        let key = f.store.lease(Slot::Shared).key();
        tokio::task::spawn_blocking(move || {
            let mut native = NativeStore::default();
            assert_eq!(native.read(&key).unwrap(), None);
            native.write(&key, b"dummy-probe").unwrap();
            assert_eq!(
                native.read(&key).unwrap().as_deref(),
                Some(b"dummy-probe".as_slice())
            );
            native.delete(&key).unwrap();
            assert_eq!(native.read(&key).unwrap(), None);
        })
        .await
        .unwrap();
        let store = Store::new(f.dirs.clone());
        let grants = [
            web(crate::auth::DEFAULT_WEB_CLIENT_ID),
            web("dummy-personal-client"),
            playback(),
            proxy_password(),
        ];
        for (slot, grant) in Slot::ALL.into_iter().zip(grants.iter()) {
            store.lease(slot).save(grant.clone()).await.unwrap();
        }
        let restarted = Store::new(f.dirs.clone());
        for (slot, grant) in Slot::ALL.into_iter().zip(grants) {
            assert!(restarted.lease(slot).load().await.unwrap().grant == Some(grant));
        }
        store.revoke_spotify().unwrap();
        store.revoke(Slot::Proxy).unwrap();
        for slot in Slot::ALL {
            let lease = store.lease(slot);
            lease.delete().await.unwrap();
            assert!(
                lease
                    .request(false, |lease, backend| backend.read(&lease.key()))
                    .await
                    .unwrap()
                    .is_none()
            );
        }
    }

    #[tokio::test]
    async fn queued_deletion_cannot_erase_a_new_authorization() {
        let f = Fixture::new();
        let old = web("dummy-old-personal-client");
        let new = web("dummy-new-personal-client");
        f.store.lease(Slot::Personal).save(old).await.unwrap();
        f.store.revoke(Slot::Personal).unwrap();
        let deletion = f.store.lease(Slot::Personal).delete();
        f.store.invalidate(Slot::Personal);
        let saving = f.store.lease(Slot::Personal).save(new.clone());
        deletion.await.unwrap();
        saving.await.unwrap();
        assert!(
            f.restart()
                .lease(Slot::Personal)
                .load()
                .await
                .unwrap()
                .grant
                == Some(new)
        );
    }

    #[test]
    fn a_failed_revocation_marker_does_not_prevent_legacy_cleanup() {
        let f = Fixture::new();
        let path = f.dirs.shared_web_token_file();
        write_legacy(&path, &web(crate::auth::DEFAULT_WEB_CLIENT_ID));
        std::fs::write(f.dirs.state.join("credential-storage"), b"not a directory").unwrap();
        assert_eq!(f.store.revoke(Slot::Shared), Err(Error::Filesystem));
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn all_grants_migrate_and_survive_restart_without_plaintext_files() {
        let f = Fixture::new();
        let grants = [
            web(crate::auth::DEFAULT_WEB_CLIENT_ID),
            web("dummy-personal-client"),
            playback(),
        ];
        let paths = [
            f.dirs.shared_web_token_file(),
            f.dirs.personal_web_token_file(),
            f.dirs.credentials_dir().join("credentials.json"),
        ];
        for ((slot, grant), path) in Slot::SPOTIFY
            .into_iter()
            .zip(grants.iter())
            .zip(paths.iter())
        {
            write_legacy(path, grant);
            let loaded = f.store.lease(slot).load().await.unwrap();
            assert!(loaded.grant.as_ref() == Some(grant));
            assert_eq!(loaded.warning, None);
            assert!(!path.exists());
            let marker = std::fs::read_to_string(f.store.marker_path(slot)).unwrap();
            assert!(!marker.contains("dummy"));
        }
        let restarted = f.restart();
        for (slot, grant) in Slot::SPOTIFY.into_iter().zip(grants) {
            assert!(restarted.lease(slot).load().await.unwrap().grant == Some(grant));
        }
        assert_eq!(f.fake.lock().unwrap().values.len(), 3);
    }

    #[tokio::test]
    async fn combined_legacy_path_migrates_to_its_client_identity() {
        for (slot, client) in [
            (Slot::Shared, crate::auth::DEFAULT_WEB_CLIENT_ID),
            (Slot::Personal, "dummy-personal-client"),
        ] {
            let f = Fixture::new();
            let grant = web(client);
            write_legacy(&f.dirs.legacy_web_token_file(), &grant);
            assert!(f.store.lease(slot).load().await.unwrap().grant == Some(grant));
            assert!(!f.dirs.legacy_web_token_file().exists());
        }
    }

    #[tokio::test]
    async fn failed_migration_keeps_original_and_reports_warning_then_can_retry() {
        let f = Fixture::new();
        let grant = web(crate::auth::DEFAULT_WEB_CLIENT_ID);
        let path = f.dirs.shared_web_token_file();
        write_legacy(&path, &grant);
        f.fake.lock().unwrap().write_error = Some(Error::Locked);
        let loaded = f.store.lease(Slot::Shared).load().await.unwrap();
        assert!(loaded.grant == Some(grant.clone()));
        assert_eq!(loaded.warning, Some(Error::Locked));
        assert!(path.exists());
        assert!(!f.store.marker_path(Slot::Shared).exists());
        f.fake.lock().unwrap().write_error = None;
        let loaded = f.store.lease(Slot::Shared).load().await.unwrap();
        assert!(loaded.grant == Some(grant));
        assert_eq!(loaded.warning, None);
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn migration_requires_read_back_before_removing_original() {
        let f = Fixture::new();
        write_legacy(
            &f.dirs.shared_web_token_file(),
            &web(crate::auth::DEFAULT_WEB_CLIENT_ID),
        );
        f.fake.lock().unwrap().discard_write = true;
        let loaded = f.store.lease(Slot::Shared).load().await.unwrap();
        assert_eq!(loaded.warning, Some(Error::Verification));
        assert!(f.dirs.shared_web_token_file().exists());
        assert!(!f.store.marker_path(Slot::Shared).exists());
    }

    #[tokio::test]
    async fn late_write_after_signout_cannot_recreate_grant() {
        let f = Fixture::new();
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = mpsc::channel();
        {
            let mut fake = f.fake.lock().unwrap();
            fake.entered = Some(entered_tx);
            fake.release = Some(release_rx);
        }
        let lease = f.store.lease(Slot::Shared);
        let pending_lease = lease.clone();
        let pending = tokio::spawn(async move {
            pending_lease
                .save(web(crate::auth::DEFAULT_WEB_CLIENT_ID))
                .await
        });
        entered_rx.await.unwrap();
        f.store.revoke_spotify().unwrap();
        assert!(!lease.current());
        release_tx.send(()).unwrap();
        assert_eq!(pending.await.unwrap(), Err(Error::Stale));
        assert!(f.fake.lock().unwrap().values.is_empty());
        assert!(
            f.restart()
                .lease(Slot::Shared)
                .load()
                .await
                .unwrap()
                .grant
                .is_none()
        );
    }

    #[tokio::test]
    async fn failed_native_deletion_remains_revoked_across_restart() {
        let f = Fixture::new();
        f.store
            .lease(Slot::Shared)
            .save(web(crate::auth::DEFAULT_WEB_CLIENT_ID))
            .await
            .unwrap();
        f.fake.lock().unwrap().delete_error = Some(Error::Locked);
        f.store.revoke_spotify().unwrap();
        assert_eq!(
            f.store.lease(Slot::Shared).delete().await,
            Err(Error::Locked)
        );
        assert_eq!(f.fake.lock().unwrap().values.len(), 1);
        // A revoked load must not even require access to the locked service.
        f.fake.lock().unwrap().read_error = Some(Error::Locked);
        assert!(
            f.restart()
                .lease(Slot::Shared)
                .load()
                .await
                .unwrap()
                .grant
                .is_none()
        );
    }

    #[tokio::test]
    async fn revoked_or_protected_slots_never_revive_stale_legacy_files() {
        let f = Fixture::new();
        let grant = web(crate::auth::DEFAULT_WEB_CLIENT_ID);
        f.store
            .lease(Slot::Shared)
            .save(grant.clone())
            .await
            .unwrap();
        f.fake.lock().unwrap().values.clear();
        write_legacy(&f.dirs.shared_web_token_file(), &grant);
        assert!(
            f.restart()
                .lease(Slot::Shared)
                .load()
                .await
                .unwrap()
                .grant
                .is_none()
        );
        f.store.revoke_spotify().unwrap();
        write_legacy(&f.dirs.shared_web_token_file(), &grant);
        assert!(
            f.restart()
                .lease(Slot::Shared)
                .load()
                .await
                .unwrap()
                .grant
                .is_none()
        );
    }

    #[tokio::test]
    async fn signout_removes_partial_and_corrupt_legacy_files_and_can_repeat() {
        let f = Fixture::new();
        let paths = [
            f.dirs.shared_web_token_file(),
            f.dirs.personal_web_token_file(),
            f.dirs.legacy_web_token_file(),
            f.dirs.credentials_dir().join("credentials.json"),
        ];
        for path in &paths {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, b"corrupt-dummy-grant").unwrap();
            std::fs::write(path.with_extension("json.tmp"), b"partial-dummy-grant").unwrap();
        }
        f.store.revoke_spotify().unwrap();
        f.store.revoke_spotify().unwrap();
        for path in paths {
            assert!(!path.exists());
            assert!(!path.with_extension("json.tmp").exists());
        }
    }

    #[tokio::test]
    async fn profiles_are_isolated_and_invalid_records_are_rejected() {
        let f = Fixture::new();
        let other = Fixture::new();
        let isolated = Store::with_backend(other.dirs.clone(), Box::new(Backend(f.fake.clone())));
        f.store
            .lease(Slot::Shared)
            .save(web(crate::auth::DEFAULT_WEB_CLIENT_ID))
            .await
            .unwrap();
        assert!(
            isolated
                .lease(Slot::Shared)
                .load()
                .await
                .unwrap()
                .grant
                .is_none()
        );
        for bytes in [
            b"not-json".to_vec(),
            serde_json::to_vec(&Record {
                version: 2,
                slot: Slot::Shared,
                grant: web(crate::auth::DEFAULT_WEB_CLIENT_ID),
            })
            .unwrap(),
            serde_json::to_vec(&Record {
                version: 1,
                slot: Slot::Playback,
                grant: playback(),
            })
            .unwrap(),
        ] {
            f.fake
                .lock()
                .unwrap()
                .values
                .insert(f.store.lease(Slot::Shared).key(), bytes);
            assert!(matches!(
                f.store.lease(Slot::Shared).load().await,
                Err(Error::Invalid)
            ));
        }
    }
}
