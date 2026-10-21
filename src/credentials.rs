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

const SERVICE: &str = "rocks.spotifast.Spotifast";
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

fn native_error(operation: &str, error: keyring_core::Error) -> Error {
    log::warn!(
        "credential store {operation} failed: {}",
        describe_native_error(&error)
    );
    match error {
        keyring_core::Error::NoStorageAccess(_) => Error::Locked,
        _ => Error::Unavailable,
    }
}

/// What a provider error may say in the log: its kind, and on Windows the
/// error code. Some provider errors contain the secret or arbitrary platform
/// data, so their own Debug/Display text is never used, except the Windows
/// store's fixed "Windows ERROR_NAME" or "Windows error code N" (#815).
fn describe_native_error(error: &keyring_core::Error) -> String {
    use keyring_core::Error as E;
    let (kind, platform) = match error {
        E::PlatformFailure(platform) => ("platform failure", Some(platform)),
        E::NoStorageAccess(platform) => ("no storage access", Some(platform)),
        E::NoEntry => ("no entry", None),
        E::BadEncoding(_) => ("bad encoding", None),
        E::BadDataFormat(..) => ("bad data format", None),
        E::BadStoreFormat(_) => ("bad store format", None),
        E::TooLong(..) => ("attribute too long", None),
        E::Invalid(..) => ("invalid parameter", None),
        E::Ambiguous(_) => ("ambiguous entry", None),
        E::NoDefaultStore => ("no default store", None),
        E::NotSupportedByStore(_) => ("not supported by store", None),
        _ => ("other", None),
    };
    let code = platform
        .map(ToString::to_string)
        .filter(|text| windows_error_code(text));
    match code {
        Some(code) => format!("{kind} ({code})"),
        None => kind.to_owned(),
    }
}

fn windows_error_code(text: &str) -> bool {
    if let Some(name) = text.strip_prefix("Windows ERROR_") {
        !name.is_empty()
            && name
                .bytes()
                .all(|byte| byte.is_ascii_uppercase() || byte == b'_')
    } else if let Some(number) = text.strip_prefix("Windows error code ") {
        !number.is_empty() && number.bytes().all(|byte| byte.is_ascii_digit())
    } else {
        false
    }
}

/// Windows writes with Enterprise persistence, which survives a restart and
/// roams with the profile. Some machines refuse it, or keep the credential
/// for the logon session only, so the grant is gone after a restart (#815).
/// Either way, write it again with Local persistence, which also survives a
/// restart. `enterprise` returns the persistence the store reports after the
/// write, when it can tell.
#[cfg(any(windows, test))]
fn write_preferring_enterprise(
    enterprise: impl FnOnce() -> Result<Option<String>, keyring_core::Error>,
    local: impl FnOnce() -> Result<(), keyring_core::Error>,
) -> Result<(), Error> {
    let kept = match enterprise() {
        Ok(persistence) if persistence.as_deref().is_none_or(|kind| kind != "Session") => {
            return Ok(());
        }
        Ok(_) => {
            log::warn!(
                "credential store kept the grant for this logon session only; writing it with Local persistence"
            );
            true
        }
        Err(error) => {
            native_error("write with Enterprise persistence", error);
            false
        }
    };
    match local() {
        Ok(()) => {
            log::info!("credential store write with Local persistence succeeded");
            Ok(())
        }
        // The session-only copy still serves until the restart.
        Err(error) if kept => {
            native_error("write with Local persistence", error);
            Ok(())
        }
        Err(error) => Err(native_error("write with Local persistence", error)),
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
        self.entry_with(key, None)
    }

    fn entry_with(
        &mut self,
        key: &str,
        modifiers: Option<&std::collections::HashMap<&str, &str>>,
    ) -> Result<keyring_core::Entry, Error> {
        if self.store.is_none() {
            #[cfg(target_os = "linux")]
            let store = zbus_secret_service_keyring_store::Store::new();
            #[cfg(target_os = "macos")]
            let store = apple_native_keyring_store::keychain::Store::new();
            #[cfg(windows)]
            let store = windows_native_keyring_store::Store::new();
            self.store = Some(store.map_err(|error| native_error("open", error))?);
        }
        self.store
            .as_ref()
            .ok_or(Error::Unavailable)?
            .build(SERVICE, key, modifiers)
            .map_err(|error| native_error("entry", error))
    }
}

impl ProtectedStore for NativeStore {
    fn read(&mut self, key: &str) -> Result<Option<Vec<u8>>, Error> {
        match self.entry(key)?.get_secret() {
            Ok(secret) => Ok(Some(secret)),
            Err(keyring_core::Error::NoEntry) => Ok(None),
            Err(error) => Err(native_error("read", error)),
        }
    }
    #[cfg(not(windows))]
    fn write(&mut self, key: &str, secret: &[u8]) -> Result<(), Error> {
        self.entry(key)?
            .set_secret(secret)
            .map_err(|error| native_error("write", error))
    }
    #[cfg(windows)]
    fn write(&mut self, key: &str, secret: &[u8]) -> Result<(), Error> {
        let enterprise = self.entry(key)?;
        let local = self.entry_with(
            key,
            Some(&std::collections::HashMap::from([("persistence", "Local")])),
        )?;
        write_preferring_enterprise(
            || {
                enterprise.set_secret(secret)?;
                Ok(enterprise
                    .get_attributes()
                    .ok()
                    .and_then(|mut attributes| attributes.remove("persistence")))
            },
            || local.set_secret(secret),
        )
    }
    fn delete(&mut self, key: &str) -> Result<(), Error> {
        match self.entry(key)?.delete_credential() {
            Ok(()) | Err(keyring_core::Error::NoEntry) => Ok(()),
            Err(error) => Err(native_error("delete", error)),
        }
    }
}

/// Credential Manager first, and a copy sealed for this Windows user in the
/// state directory. On some machines Credential Manager refuses the write
/// (#748) or drops the entries during a restart (#815); the sealed copy then
/// serves instead. Every save writes both, and sign-out deletes both.
#[cfg(any(windows, test))]
struct WithSealedCopy<P, C> {
    primary: P,
    copy: C,
}

#[cfg(any(windows, test))]
impl<P: ProtectedStore, C: ProtectedStore> ProtectedStore for WithSealedCopy<P, C> {
    fn read(&mut self, key: &str) -> Result<Option<Vec<u8>>, Error> {
        let primary = self.primary.read(key);
        if let Ok(Some(secret)) = &primary {
            // A grant saved before the copy existed gains one here, so it
            // survives the next time the native store loses it.
            if !matches!(self.copy.read(key), Ok(Some(copy)) if copy == *secret)
                && let Err(error) = self.copy.write(key, secret)
            {
                log::warn!("could not keep a sealed copy of the grant: {error:?}");
            }
            return primary;
        }
        match self.copy.read(key) {
            Ok(Some(secret)) => {
                log::info!(
                    "credential store {}; using the sealed copy",
                    if primary.is_ok() {
                        "no longer has the grant"
                    } else {
                        "is unavailable"
                    }
                );
                Ok(Some(secret))
            }
            Ok(None) => primary,
            // The native store's own error says more, when it has one.
            Err(error) => primary.and(Err(error)),
        }
    }

    fn write(&mut self, key: &str, secret: &[u8]) -> Result<(), Error> {
        let primary = self.primary.write(key, secret);
        if primary.is_err() {
            // An older grant left there must not shadow the new copy.
            let _ = self.primary.delete(key);
        }
        let copy = self.copy.write(key, secret);
        if let Err(error) = copy {
            log::warn!("could not write the sealed copy of the grant: {error:?}");
            // Nor may an older copy outlive the grant that replaced it.
            let _ = self.copy.delete(key);
        }
        match (primary, copy) {
            (Err(error), Err(_)) => Err(error),
            _ => Ok(()),
        }
    }

    fn delete(&mut self, key: &str) -> Result<(), Error> {
        let primary = self.primary.delete(key);
        let copy = self.copy.delete(key);
        primary.and(copy)
    }
}

/// Encrypts a secret so only the current user can decrypt it. `context` is
/// bound to the result: a sealed grant opens only under its own key.
#[cfg(any(windows, test))]
trait Sealer: Send {
    fn seal(&self, secret: &[u8], context: &[u8]) -> Result<Vec<u8>, Error>;
    fn open(&self, sealed: &[u8], context: &[u8]) -> Result<Vec<u8>, Error>;
}

/// Sealed grants, one file per key, replaced whole on every write.
#[cfg(any(windows, test))]
struct SealedFiles<S> {
    dir: PathBuf,
    sealer: S,
}

#[cfg(any(windows, test))]
impl<S> SealedFiles<S> {
    const MAGIC: &'static [u8] = b"spotifast-sealed-grant-1\n";

    fn path(&self, key: &str) -> PathBuf {
        // Keys are "<hex profile>:<slot name>"; ':' cannot name a Windows file.
        self.dir.join(format!("{}.sealed", key.replace(':', ".")))
    }

    fn context(key: &str) -> Vec<u8> {
        format!("{SERVICE}\n{key}").into_bytes()
    }
}

#[cfg(any(windows, test))]
impl<S: Sealer> ProtectedStore for SealedFiles<S> {
    fn read(&mut self, key: &str) -> Result<Option<Vec<u8>>, Error> {
        let bytes = match std::fs::read(self.path(key)) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(Error::Filesystem),
        };
        let sealed = bytes.strip_prefix(Self::MAGIC).ok_or(Error::Invalid)?;
        self.sealer.open(sealed, &Self::context(key)).map(Some)
    }

    fn write(&mut self, key: &str, secret: &[u8]) -> Result<(), Error> {
        let mut bytes = Self::MAGIC.to_vec();
        bytes.extend(self.sealer.seal(secret, &Self::context(key))?);
        fastframe_file::write_private(&self.path(key), bytes).map_err(|_| Error::Filesystem)
    }

    fn delete(&mut self, key: &str) -> Result<(), Error> {
        remove_file(&self.path(key))
    }
}

/// DPAPI with the user's own key: the copy opens only for this Windows user.
#[cfg(windows)]
struct Dpapi;

#[cfg(windows)]
impl Dpapi {
    fn transform(input: &[u8], context: &[u8], seal: bool) -> Result<Vec<u8>, Error> {
        use windows_sys::Win32::Foundation::{GetLastError, LocalFree};
        use windows_sys::Win32::Security::Cryptography::{
            CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
        };
        let blob = |bytes: &[u8]| -> Result<CRYPT_INTEGER_BLOB, Error> {
            Ok(CRYPT_INTEGER_BLOB {
                cbData: u32::try_from(bytes.len()).map_err(|_| Error::Invalid)?,
                pbData: bytes.as_ptr().cast_mut(),
            })
        };
        let input = blob(input)?;
        let entropy = blob(context)?;
        let mut output = CRYPT_INTEGER_BLOB {
            cbData: 0,
            pbData: std::ptr::null_mut(),
        };
        // SAFETY: the input blobs borrow live slices that DPAPI only reads,
        // and on success `output` is a LocalAlloc buffer of `cbData` bytes
        // that is copied and then freed exactly once.
        unsafe {
            let ok = if seal {
                CryptProtectData(
                    &input,
                    std::ptr::null(),
                    &entropy,
                    std::ptr::null(),
                    std::ptr::null(),
                    CRYPTPROTECT_UI_FORBIDDEN,
                    &mut output,
                )
            } else {
                CryptUnprotectData(
                    &input,
                    std::ptr::null_mut(),
                    &entropy,
                    std::ptr::null(),
                    std::ptr::null(),
                    CRYPTPROTECT_UI_FORBIDDEN,
                    &mut output,
                )
            };
            if ok == 0 || output.pbData.is_null() {
                let code = GetLastError();
                log::warn!(
                    "sealed grant {} failed (Windows error code {code})",
                    if seal { "encryption" } else { "decryption" }
                );
                return Err(if seal {
                    Error::Unavailable
                } else {
                    Error::Invalid
                });
            }
            let length = output.cbData as usize;
            let bytes = std::slice::from_raw_parts(output.pbData, length).to_vec();
            // Do not leave a decrypted grant behind in freed memory.
            std::ptr::write_bytes(output.pbData, 0, length);
            LocalFree(output.pbData.cast());
            Ok(bytes)
        }
    }
}

#[cfg(windows)]
impl Sealer for Dpapi {
    fn seal(&self, secret: &[u8], context: &[u8]) -> Result<Vec<u8>, Error> {
        Self::transform(secret, context, true)
    }
    fn open(&self, sealed: &[u8], context: &[u8]) -> Result<Vec<u8>, Error> {
        Self::transform(sealed, context, false)
    }
}

/// The profile a state folder's grants are saved under, when it records one.
const PROFILE_FILE: &str = "credential-profile";
/// The profile a portable folder takes its first grants from (#703).
const ADOPT_FILE: &str = "credential-profile-source";

/// A profile a state folder recorded in `file`. Folders moved by the rename,
/// and portable folders, record one; any other folder's profile is
/// [`profile_of`] its path.
fn recorded_profile(state: &Path, file: &str) -> Option<String> {
    std::fs::read_to_string(state.join(file))
        .ok()
        .filter(|value| value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

/// The profile of a state folder that recorded none: a digest of its path.
fn profile_of(state: &Path) -> String {
    format!("{:x}", Sha256::digest(state.to_string_lossy().as_bytes()))
}

/// Makes `state` record its profile, unless it records one already, so its
/// grants stay reachable when the folder moves to another path: a portable
/// folder on a drive that changes letter, say. Runs before the window opens.
pub fn keep_profile(state: &Path) -> std::io::Result<()> {
    if recorded_profile(state, PROFILE_FILE).is_some() {
        return Ok(());
    }
    fastframe_file::write(&state.join(PROFILE_FILE), profile_of(state))
}

/// Gives `copy`, a portable folder's state copied from `source`, a profile
/// of its own, and records `source`'s, whose grants the store saves under
/// the new profile when it first opens. The copy then starts signed in, and
/// the two keep separate entries in the platform store: signing out of one
/// leaves the other signed in.
pub fn adopt_profile(copy: &Path, source: &Path) -> std::io::Result<()> {
    let from = recorded_profile(source, PROFILE_FILE).unwrap_or_else(|| profile_of(source));
    let own: String = (0..4)
        .map(|_| format!("{:016x}", rand::random::<u64>()))
        .collect();
    fastframe_file::write(&copy.join(ADOPT_FILE), from)?;
    fastframe_file::write(&copy.join(PROFILE_FILE), own)
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
        #[cfg(windows)]
        let backend = Box::new(WithSealedCopy {
            primary: NativeStore::default(),
            copy: SealedFiles {
                dir: dirs.state.join("credential-storage"),
                sealer: Dpapi,
            },
        });
        #[cfg(not(windows))]
        let backend = Box::new(NativeStore::default());
        Self::with_backend(dirs, backend)
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
        let profile =
            recorded_profile(&dirs.state, PROFILE_FILE).unwrap_or_else(|| profile_of(&dirs.state));
        let source = recorded_profile(&dirs.state, ADOPT_FILE).filter(|source| *source != profile);
        let store = Self {
            inner: Arc::new(Inner {
                dirs,
                profile,
                generations: std::array::from_fn(|_| AtomicU64::new(0)),
                markers: Mutex::new(()),
                jobs,
            }),
        };
        // First in the queue, so every load sees the adopted grants.
        if let Some(source) = source {
            let adopting = store.clone();
            let _ = store.inner.jobs.try_send(Box::new(move |backend| {
                adopting.adopt(&source, backend);
            }));
        }
        store
    }

    /// Saves the grants of the profile a portable folder was copied from
    /// under its own profile, once (see [`adopt_profile`]). A grant this
    /// folder already has, or revoked, stays as it is. Tried again at the
    /// next start if any grant could not be read or saved.
    fn adopt(&self, source: &str, backend: &mut dyn ProtectedStore) {
        let mut done = true;
        for slot in Slot::ALL {
            let own = format!("{}:{}", self.inner.profile, slot.name());
            let from = format!("{source}:{}", slot.name());
            let adopted = (|| {
                if self.marker(slot)?.is_some_and(|marker| marker.revoked)
                    || backend.read(&own)?.is_some()
                {
                    return Ok(());
                }
                let Some(bytes) = backend.read(&from)? else {
                    return Ok(());
                };
                backend.write(&own, &bytes)?;
                if backend.read(&own)?.as_deref() != Some(bytes.as_slice()) {
                    return Err(Error::Verification);
                }
                Ok(())
            })();
            match adopted {
                // The copied sealed copy of the source's grant has done its
                // job; the source's own entries stay with the source.
                Ok(()) => {
                    let _ = remove_file(
                        &self
                            .inner
                            .dirs
                            .state
                            .join("credential-storage")
                            .join(format!("{source}.{}.sealed", slot.name())),
                    );
                }
                Err(error) => {
                    log::warn!(
                        "could not take over the {} grant of the folder this one was copied from: {error:?}",
                        slot.name()
                    );
                    done = false;
                }
            }
        }
        if done {
            let _ = remove_file(&self.inner.dirs.state.join(ADOPT_FILE));
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
        let bytes = serde_json::to_vec(&Marker {
            version: 1,
            revoked,
        })
        .map_err(|_| Error::Invalid)?;
        // It holds no secret, but it lives with the credentials, which only
        // their owner reads.
        fastframe_file::write_private(&self.marker_path(slot), bytes).map_err(|_| Error::Filesystem)
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

    #[derive(Debug)]
    struct Platform(&'static str);
    impl std::fmt::Display for Platform {
        fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str(self.0)
        }
    }
    impl std::error::Error for Platform {}
    fn platform(text: &'static str) -> keyring_core::Error {
        keyring_core::Error::PlatformFailure(Box::new(Platform(text)))
    }

    /// Windows error codes reach the log; no other provider text does,
    /// since it may carry the secret (#815).
    #[test]
    fn native_errors_log_windows_codes_and_never_provider_text() {
        assert_eq!(
            describe_native_error(&platform("Windows error code 1312")),
            "platform failure (Windows error code 1312)"
        );
        assert_eq!(
            describe_native_error(&keyring_core::Error::NoStorageAccess(Box::new(Platform(
                "Windows ERROR_NO_SUCH_LOGON_SESSION"
            )))),
            "no storage access (Windows ERROR_NO_SUCH_LOGON_SESSION)"
        );
        for leaky in [
            platform("Windows error code 5 dummy-secret"),
            platform("dummy-secret"),
            keyring_core::Error::BadEncoding(b"dummy-secret".to_vec()),
            keyring_core::Error::Invalid("secret".into(), "dummy-secret".into()),
        ] {
            assert!(!describe_native_error(&leaky).contains("dummy-secret"));
        }
        assert_eq!(native_error("write", platform("x")), Error::Unavailable);
        assert_eq!(
            native_error(
                "write",
                keyring_core::Error::NoStorageAccess(Box::new(Platform("x")))
            ),
            Error::Locked
        );
    }

    /// A refused or session-only Enterprise write falls back to Local
    /// persistence, which survives a restart (#815, #748, #803).
    #[test]
    fn windows_write_falls_back_to_local_persistence() {
        let local_called = std::cell::Cell::new(0);
        let local_ok = || {
            local_called.set(local_called.get() + 1);
            Ok(())
        };
        assert_eq!(
            write_preferring_enterprise(|| Ok(Some("Enterprise".into())), local_ok),
            Ok(())
        );
        assert_eq!(write_preferring_enterprise(|| Ok(None), local_ok), Ok(()));
        assert_eq!(local_called.get(), 0);

        assert_eq!(
            write_preferring_enterprise(|| Err(platform("Windows error code 1312")), local_ok),
            Ok(())
        );
        assert_eq!(
            write_preferring_enterprise(|| Ok(Some("Session".into())), local_ok),
            Ok(())
        );
        assert_eq!(local_called.get(), 2);

        assert_eq!(
            write_preferring_enterprise(
                || Err(platform("Windows error code 1312")),
                || Err(keyring_core::Error::NoStorageAccess(Box::new(Platform(
                    "x"
                ))))
            ),
            Err(Error::Locked)
        );
        // The session copy still serves this run when Local is refused too.
        assert_eq!(
            write_preferring_enterprise(
                || Ok(Some("Session".into())),
                || Err(platform("Windows error code 1312"))
            ),
            Ok(())
        );
    }

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
                "spotifast-credential-tests-{}-{}",
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

    fn copy_tree(from: &Path, to: &Path) {
        std::fs::create_dir_all(to).unwrap();
        for entry in std::fs::read_dir(from).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                copy_tree(&entry.path(), &to.join(entry.file_name()));
            } else {
                std::fs::copy(entry.path(), to.join(entry.file_name())).unwrap();
            }
        }
    }

    /// #703: a portable folder made from the per-user state keeps the
    /// sign-in saved there, and the installed copy keeps it too.
    #[tokio::test]
    async fn a_portable_copy_of_the_state_keeps_its_sign_in() {
        let fixture = Fixture::new();
        let grant = web(crate::auth::DEFAULT_WEB_CLIENT_ID);
        let lease = fixture.store.lease(Slot::Shared);
        lease.save(grant.clone()).await.unwrap();
        let root = fixture.dirs.state.parent().unwrap();
        let portable = AppDirs {
            config: root.join("portable").join("config"),
            state: root.join("portable").join("state"),
            cache: root.join("portable").join("cache"),
        };
        copy_tree(&fixture.dirs.state, &portable.state);
        adopt_profile(&portable.state, &fixture.dirs.state).unwrap();
        let store = Store::with_backend(portable.clone(), Box::new(Backend(fixture.fake.clone())));
        assert_ne!(store.inner.profile, fixture.store.inner.profile);
        assert!(store.lease(Slot::Shared).load().await.unwrap().grant == Some(grant.clone()));
        // Its own entry now, and the adoption is done.
        let own = format!("{}:shared-web", store.inner.profile);
        assert!(fixture.fake.lock().unwrap().values.contains_key(&own));
        assert!(!portable.state.join(ADOPT_FILE).exists());
        // Signing out of the portable copy leaves the installed one signed in.
        store.revoke_spotify().unwrap();
        store.lease(Slot::Shared).delete().await.unwrap();
        let reopened = Store::with_backend(portable, Box::new(Backend(fixture.fake.clone())));
        assert!(
            reopened
                .lease(Slot::Shared)
                .load()
                .await
                .unwrap()
                .grant
                .is_none()
        );
        let installed = fixture.restart().lease(Slot::Shared).load().await.unwrap();
        assert!(installed.grant == Some(grant));
        assert!(
            !fixture.dirs.state.join(PROFILE_FILE).exists(),
            "the installed state is left as it was"
        );
    }

    /// An adoption that could not save is tried again at the next start.
    #[tokio::test]
    async fn a_failed_adoption_is_tried_again() {
        let fixture = Fixture::new();
        let grant = web(crate::auth::DEFAULT_WEB_CLIENT_ID);
        fixture
            .store
            .lease(Slot::Shared)
            .save(grant.clone())
            .await
            .unwrap();
        let root = fixture.dirs.state.parent().unwrap();
        let portable = AppDirs {
            config: root.join("portable-config"),
            state: root.join("portable-state"),
            cache: root.join("portable-cache"),
        };
        copy_tree(&fixture.dirs.state, &portable.state);
        adopt_profile(&portable.state, &fixture.dirs.state).unwrap();
        fixture.fake.lock().unwrap().write_error = Some(Error::Unavailable);
        let store = Store::with_backend(portable.clone(), Box::new(Backend(fixture.fake.clone())));
        assert!(
            store
                .lease(Slot::Shared)
                .load()
                .await
                .unwrap()
                .grant
                .is_none()
        );
        assert!(portable.state.join(ADOPT_FILE).exists());
        fixture.fake.lock().unwrap().write_error = None;
        let store = Store::with_backend(portable.clone(), Box::new(Backend(fixture.fake.clone())));
        assert!(store.lease(Slot::Shared).load().await.unwrap().grant == Some(grant));
        assert!(!portable.state.join(ADOPT_FILE).exists());
    }

    /// A portable folder records its own profile, so its sign-in survives
    /// the folder moving to another drive, and a recorded one is kept.
    #[tokio::test]
    async fn a_portable_folder_keeps_its_sign_in_when_it_moves() {
        let fixture = Fixture::new();
        keep_profile(&fixture.dirs.state).unwrap();
        let recorded = std::fs::read_to_string(fixture.dirs.state.join(PROFILE_FILE)).unwrap();
        assert_eq!(recorded, fixture.store.inner.profile);
        let grant = web(crate::auth::DEFAULT_WEB_CLIENT_ID);
        fixture
            .store
            .lease(Slot::Shared)
            .save(grant.clone())
            .await
            .unwrap();
        let root = fixture.dirs.state.parent().unwrap();
        let moved = AppDirs {
            config: root.join("moved-config"),
            state: root.join("moved-state"),
            cache: root.join("moved-cache"),
        };
        std::fs::rename(&fixture.dirs.state, &moved.state).unwrap();
        keep_profile(&moved.state).unwrap();
        assert_eq!(
            std::fs::read_to_string(moved.state.join(PROFILE_FILE)).unwrap(),
            recorded
        );
        let store = Store::with_backend(moved, Box::new(Backend(fixture.fake.clone())));
        assert!(store.lease(Slot::Shared).load().await.unwrap().grant == Some(grant));
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

    /// Stands in for DPAPI: reversible, and opens only under its own context.
    struct TestSealer;
    impl Sealer for TestSealer {
        fn seal(&self, secret: &[u8], context: &[u8]) -> Result<Vec<u8>, Error> {
            let mut sealed = Sha256::digest(context).to_vec();
            sealed.extend(secret.iter().map(|byte| byte ^ 0x5a));
            Ok(sealed)
        }
        fn open(&self, sealed: &[u8], context: &[u8]) -> Result<Vec<u8>, Error> {
            let secret = sealed
                .strip_prefix(Sha256::digest(context).as_slice())
                .ok_or(Error::Invalid)?;
            Ok(secret.iter().map(|byte| byte ^ 0x5a).collect())
        }
    }

    /// The Windows arrangement: a native store, here the fake, and sealed
    /// files in the fixture's state directory.
    fn sealed_backend(f: &Fixture) -> Box<dyn ProtectedStore> {
        Box::new(WithSealedCopy {
            primary: Backend(f.fake.clone()),
            copy: sealed_files(f),
        })
    }
    fn sealed_files(f: &Fixture) -> SealedFiles<TestSealer> {
        SealedFiles {
            dir: f.dirs.state.join("credential-storage"),
            sealer: TestSealer,
        }
    }
    fn with_sealed_copy(f: &Fixture) -> Store {
        Store::with_backend(f.dirs.clone(), sealed_backend(f))
    }

    #[test]
    fn sealed_files_round_trip_without_plaintext_on_disk() {
        let f = Fixture::new();
        let mut files = sealed_files(&f);
        let key = f.store.lease(Slot::Shared).key();
        assert_eq!(files.read(&key), Ok(None));
        files.write(&key, b"dummy-sealed-grant").unwrap();
        assert_eq!(
            files.read(&key).unwrap().as_deref(),
            Some(b"dummy-sealed-grant".as_slice())
        );
        let path = files.path(&key);
        let on_disk = std::fs::read(&path).unwrap();
        assert!(!on_disk.windows(18).any(|w| w == b"dummy-sealed-grant"));
        assert!(!path.file_name().unwrap().to_string_lossy().contains(':'));
        // A copy moved to another key does not open there.
        let other = f.store.lease(Slot::Personal).key();
        std::fs::write(files.path(&other), &on_disk).unwrap();
        assert_eq!(files.read(&other), Err(Error::Invalid));
        std::fs::write(&path, b"not a sealed grant").unwrap();
        assert_eq!(files.read(&key), Err(Error::Invalid));
        files.delete(&key).unwrap();
        files.delete(&key).unwrap();
        assert_eq!(files.read(&key), Ok(None));
    }

    /// Credential Manager refuses every write on some machines (#748).
    #[tokio::test]
    async fn a_refused_native_write_is_remembered_by_the_sealed_copy() {
        let f = Fixture::new();
        f.fake.lock().unwrap().write_error = Some(Error::Unavailable);
        let store = with_sealed_copy(&f);
        let grants = [
            web(crate::auth::DEFAULT_WEB_CLIENT_ID),
            web("dummy-personal-client"),
            playback(),
            proxy_password(),
        ];
        for (slot, grant) in Slot::ALL.into_iter().zip(grants.iter()) {
            store.lease(slot).save(grant.clone()).await.unwrap();
        }
        assert!(f.fake.lock().unwrap().values.is_empty());
        let restarted = with_sealed_copy(&f);
        for (slot, grant) in Slot::ALL.into_iter().zip(grants) {
            let loaded = restarted.lease(slot).load().await.unwrap();
            assert!(loaded.grant == Some(grant));
            assert_eq!(loaded.warning, None);
        }
    }

    /// Credential Manager keeps the entries until Windows restarts, then
    /// drops them (#815).
    #[tokio::test]
    async fn a_grant_the_native_store_loses_comes_back_from_the_sealed_copy() {
        let f = Fixture::new();
        let grant = web(crate::auth::DEFAULT_WEB_CLIENT_ID);
        let store = with_sealed_copy(&f);
        store.lease(Slot::Shared).save(grant.clone()).await.unwrap();
        assert_eq!(f.fake.lock().unwrap().values.len(), 1);
        f.fake.lock().unwrap().values.clear();
        assert!(
            with_sealed_copy(&f)
                .lease(Slot::Shared)
                .load()
                .await
                .unwrap()
                .grant
                == Some(grant.clone())
        );
        // An unavailable native store does not hide the copy either.
        f.fake.lock().unwrap().read_error = Some(Error::Locked);
        assert!(
            with_sealed_copy(&f)
                .lease(Slot::Shared)
                .load()
                .await
                .unwrap()
                .grant
                == Some(grant)
        );
    }

    /// Grants saved before the sealed copy existed gain one on load, and
    /// then survive the native store losing them.
    #[tokio::test]
    async fn native_only_grants_gain_a_sealed_copy_on_load() {
        let f = Fixture::new();
        let grant = playback();
        f.store
            .lease(Slot::Playback)
            .save(grant.clone())
            .await
            .unwrap();
        let key = f.store.lease(Slot::Playback).key();
        assert!(!sealed_files(&f).path(&key).exists());
        assert!(
            with_sealed_copy(&f)
                .lease(Slot::Playback)
                .load()
                .await
                .unwrap()
                .grant
                == Some(grant.clone())
        );
        assert!(sealed_files(&f).path(&key).exists());
        f.fake.lock().unwrap().values.clear();
        assert!(
            with_sealed_copy(&f)
                .lease(Slot::Playback)
                .load()
                .await
                .unwrap()
                .grant
                == Some(grant)
        );
    }

    /// A refused write must not leave the older native entry to win the
    /// next read over the newer copy.
    #[tokio::test]
    async fn a_refused_native_write_drops_the_older_native_entry() {
        let f = Fixture::new();
        let store = with_sealed_copy(&f);
        let old = web("dummy-old-personal-client");
        let new = web("dummy-new-personal-client");
        store.lease(Slot::Personal).save(old).await.unwrap();
        f.fake.lock().unwrap().write_error = Some(Error::Unavailable);
        store.lease(Slot::Personal).save(new.clone()).await.unwrap();
        assert!(f.fake.lock().unwrap().values.is_empty());
        assert!(
            with_sealed_copy(&f)
                .lease(Slot::Personal)
                .load()
                .await
                .unwrap()
                .grant
                == Some(new)
        );
    }

    #[tokio::test]
    async fn the_native_error_stands_when_neither_store_keeps_the_grant() {
        let f = Fixture::new();
        f.fake.lock().unwrap().write_error = Some(Error::Locked);
        // A file where the copies' directory should be makes their writes fail.
        std::fs::write(f.dirs.state.join("credential-storage"), b"not a directory").unwrap();
        let store = with_sealed_copy(&f);
        assert_eq!(
            store
                .lease(Slot::Shared)
                .save(web(crate::auth::DEFAULT_WEB_CLIENT_ID))
                .await,
            Err(Error::Locked)
        );
    }

    #[tokio::test]
    async fn sign_out_deletes_the_sealed_copy_too() {
        let f = Fixture::new();
        let store = with_sealed_copy(&f);
        let lease = store.lease(Slot::Shared);
        lease
            .save(web(crate::auth::DEFAULT_WEB_CLIENT_ID))
            .await
            .unwrap();
        let path = sealed_files(&f).path(&lease.key());
        assert!(path.exists());
        store.revoke_spotify().unwrap();
        lease.delete().await.unwrap();
        assert!(!path.exists());
        assert!(f.fake.lock().unwrap().values.is_empty());
        // A grant signed in again after the revocation is restored as usual.
        let grant = web(crate::auth::DEFAULT_WEB_CLIENT_ID);
        store.lease(Slot::Shared).save(grant.clone()).await.unwrap();
        f.fake.lock().unwrap().values.clear();
        assert!(
            with_sealed_copy(&f)
                .lease(Slot::Shared)
                .load()
                .await
                .unwrap()
                .grant
                == Some(grant)
        );
    }

    #[cfg(windows)]
    #[test]
    fn dpapi_seals_for_this_user_and_this_key_only() {
        let sealed = Dpapi.seal(b"dummy-dpapi-grant", b"context-a").unwrap();
        assert!(!sealed.windows(17).any(|w| w == b"dummy-dpapi-grant"));
        assert_eq!(
            Dpapi.open(&sealed, b"context-a").unwrap(),
            b"dummy-dpapi-grant"
        );
        assert_eq!(Dpapi.open(&sealed, b"context-b"), Err(Error::Invalid));
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
