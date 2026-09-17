//! User preferences, stored as one readable JSON file.

use std::path::Path;

use serde::{Deserialize, Serialize};

/// Local Library identity only. Never sent to Spotify as a context URI.
pub const LIKED_SONGS_KEY: &str = "spotifast:liked-songs";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LibraryShelf {
    #[default]
    Playlists,
    Albums,
    Artists,
    Podcasts,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LibrarySort {
    Library,
    RecentlyPlayed,
    Name,
    RecentlyAdded,
    Local,
    Spotify,
}

impl LibrarySort {
    pub fn supports(self, shelf: LibraryShelf) -> bool {
        match self {
            Self::RecentlyPlayed | Self::Name | Self::Library => true,
            Self::RecentlyAdded => matches!(shelf, LibraryShelf::Albums | LibraryShelf::Podcasts),
            Self::Local | Self::Spotify => shelf == LibraryShelf::Playlists,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeChoice {
    Dark,
    Light,
    #[default]
    System,
}

/// Whether a Home shelf is drawn. Hidden shelves still refresh normally.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HomeShelfSettings {
    pub visible: bool,
}

impl Default for HomeShelfSettings {
    fn default() -> Self {
        Self { visible: true }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HomeSettings {
    pub made_for_you: HomeShelfSettings,
    pub recommendations: HomeShelfSettings,
}

/// Mini-player visualizer mode.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VisMode {
    #[default]
    Bars,
    Scope,
    Off,
}

impl VisMode {
    /// Next mode in the display's click cycle.
    pub fn next(self) -> Self {
        match self {
            Self::Bars => Self::Scope,
            Self::Scope => Self::Off,
            Self::Off => Self::Bars,
        }
    }
}

impl ThemeChoice {
    pub const ALL: [ThemeChoice; 3] = [Self::System, Self::Light, Self::Dark];

    pub fn label(self) -> &'static str {
        match self {
            Self::Dark => "Dark",
            Self::Light => "Light",
            Self::System => "Follow system",
        }
    }
}

/// How outbound HTTP traffic reaches the network.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProxyMode {
    /// No proxy, ignoring environment and OS proxy settings.
    Off,
    /// Environment variables and, on macOS and Windows, the OS proxy.
    #[default]
    System,
    /// A configured HTTP proxy.
    Http,
    /// A configured SOCKS5 proxy.
    Socks,
}

impl ProxyMode {
    pub const ALL: [ProxyMode; 4] = [Self::Off, Self::System, Self::Http, Self::Socks];

    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "Off",
            Self::System => "System",
            Self::Http => "HTTP",
            Self::Socks => "SOCKS5",
        }
    }

    pub fn is_manual(self) -> bool {
        matches!(self, Self::Http | Self::Socks)
    }
}

/// Only confirmed proxy preferences are written with other settings. The
/// password stays in memory and the protected store, never in this snapshot.
#[derive(Clone, PartialEq, Eq)]
pub struct ProxyPreferences {
    mode: ProxyMode,
    host: String,
    port: String,
    username: String,
}

impl ProxyPreferences {
    pub fn apply_to(&self, settings: &mut Settings) {
        settings.proxy_mode = self.mode;
        settings.proxy_host.clone_from(&self.host);
        settings.proxy_port.clone_from(&self.port);
        settings.proxy_username.clone_from(&self.username);
    }
}

fn proxy_mode_is_system(mode: &ProxyMode) -> bool {
    *mode == ProxyMode::System
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// The Spotify Connect name other devices see.
    pub device_name: String,
    /// 96, 160, or 320 kbps.
    pub bitrate: u16,
    pub normalisation: bool,
    pub autoplay: bool,
    pub gapless: bool,
    /// librespot backend name; `None` picks the platform default.
    pub audio_backend: Option<String>,
    pub audio_device: Option<String>,
    /// Windows output buffer in milliseconds. Smaller values may click under
    /// load; larger values delay playback controls.
    /// See [`crate::sink::DEFAULT_BUFFER_MS`].
    #[serde(default = "default_buffer_ms")]
    pub audio_buffer_ms: u32,
    pub audio_cache: bool,
    pub audio_cache_mb: u64,
    pub theme: ThemeChoice,
    /// Filename selected from the local themes directory.
    pub custom_theme: Option<String>,
    /// Last accepted appearance, retained if its source file becomes unavailable.
    #[serde(
        default,
        deserialize_with = "crate::theme::custom::read_cached_theme",
        skip_serializing_if = "Option::is_none"
    )]
    pub custom_theme_cache: Option<crate::theme::custom::CustomTheme>,
    /// Last detected system palette, so following Omarchy survives a restart.
    #[serde(
        default,
        deserialize_with = "crate::theme::custom::read_cached_theme",
        skip_serializing_if = "Option::is_none"
    )]
    pub system_theme_cache: Option<crate::theme::custom::CustomTheme>,
    pub home: HomeSettings,
    /// Tint the interface with the colour of the playing album's art.
    pub accent_from_art: bool,
    /// Last local volume, 0..=65535.
    pub volume: u16,
    /// Whether the library sidebar is visible.
    pub sidebar_visible: bool,
    /// The playing album's art docked large at the sidebar's bottom.
    pub art_expanded: bool,
    /// Use compact single-line rows without cover art in the sidebar.
    pub sidebar_compact: bool,
    /// Show the Library as responsive cover cards instead of rows.
    pub sidebar_grid: bool,
    pub sidebar_width: f32,
    pub lyrics_width: f32,
    pub queue_width: f32,
    /// Use compact single-line rows without cover art in track lists.
    pub tracklist_compact: bool,
    /// Linux: middle-click a list to autoscroll it. Off by default, because
    /// Linux desktops usually paste the primary selection on middle click.
    /// Windows always autoscrolls and macOS never does.
    pub middle_click_autoscroll: bool,
    pub search_history: Vec<String>,
    pub show_shortcut_hints: bool,
    /// An optional personal Spotify Web API application id. The shared
    /// application remains active for coverage when this is present.
    pub web_client_id: Option<String>,
    /// Legacy reminder time, retained for older Spotifast versions.
    pub personal_app_nudge_at: Option<String>,
    /// The listener has dismissed or followed the personal-app introduction.
    pub personal_app_intro_seen: bool,
    /// Local playback has been authorized at least once on this machine, so
    /// the app can resume it silently instead of prompting.
    pub playback_authorized: bool,
    /// Closing the window hides to the tray and keeps the music playing.
    pub keep_playing_in_background: bool,
    /// Ask GitHub once a day whether a newer release exists.
    pub check_for_updates: bool,
    pub download_updates_automatically: bool,
    /// Context URIs and the local Liked Songs key, in pin order.
    pub pinned_contexts: Vec<String>,
    /// Older settings keep Liked Songs first until it is moved or unpinned.
    pub liked_songs_pinned: bool,
    /// The sidebar's own playlist order, set by dragging rows. Kept while
    /// another sort is selected; empty means no saved local arrangement.
    pub sidebar_order: Vec<String>,
    /// Explicit order per Library shelf. Missing shelves keep their previous
    /// behaviour; selecting another order never deletes the local arrangement.
    pub library_sort: std::collections::BTreeMap<LibraryShelf, LibrarySort>,
    /// Interface zoom, egui's zoom factor; Ctrl+plus/minus changes it.
    pub zoom: f32,
    /// The Winamp window is open.
    pub winamp_window: bool,
    /// Windows and X11: keep a taskbar button while the Winamp window is visible.
    pub winamp_show_taskbar: bool,
    /// Windows: draw Spotifast's own title bar and window buttons instead of
    /// the standard Windows frame.
    pub custom_titlebar: bool,
    /// Skin file or folder name. `None` selects the built-in skin.
    pub skin: Option<String>,
    /// Screen pixels per skin pixel; `None` picks double size for the
    /// display.
    pub skin_scale: Option<u8>,
    /// The Winamp window stays above other windows.
    pub winamp_on_top: bool,
    /// The mini player's visualiser: bars, scope, or off.
    pub vis: VisMode,
    /// The playlist window is open under the mini player.
    pub playlist_open: bool,
    /// How tall the playlist window is, in skin pixels.
    pub playlist_height: u32,
    /// The equalizer window is open under the mini player.
    pub eq_open: bool,
    /// The equalizer shapes local playback.
    pub eq_on: bool,
    /// The preamp, in decibels, never above zero.
    pub eq_preamp_db: f32,
    /// The ten bands, in decibels, 60 Hz to 16 kHz.
    pub eq_bands_db: [f32; 10],
    /// The balance, -1 all left to 1 all right.
    pub balance: f32,
    /// Play both channels the same.
    pub mono: bool,
    /// The playlist window is rolled up to its title bar.
    pub playlist_shaded: bool,
    /// The equalizer window is rolled up to its title bar.
    pub eq_shaded: bool,
    /// The main window is rolled up to its title bar.
    pub winamp_shaded: bool,
    /// The MilkDrop window is open (its own window, not part of the skin).
    pub milkdrop_open: bool,
    /// How long each preset plays before the next, in seconds.
    pub milkdrop_seconds: u32,
    /// How many frames a second the MilkDrop window draws; 0 is uncapped.
    pub milkdrop_fps: u32,
    /// Last reported MilkDrop screen refresh rate. The first value sets the
    /// default frame rate; this field is not directly configurable.
    pub milkdrop_screen_hz: u32,
    /// The picture's inner resolution: 1 full, 2 half, 4 quarter.
    pub milkdrop_scale: u32,
    /// The MilkDrop window fills the screen.
    pub milkdrop_fullscreen: bool,
    /// The MilkDrop window's size in logical points, when not full-screen.
    pub milkdrop_size: [f32; 2],
    /// Which proxy to use. Older files without this field stay on `system`.
    #[serde(default, skip_serializing_if = "proxy_mode_is_system")]
    pub proxy_mode: ProxyMode,
    /// Combined address from older settings files. Split into host and port
    /// on load; not written again.
    #[serde(default, skip_serializing)]
    pub proxy: String,
    /// Host of a manual HTTP or SOCKS5 proxy. Ignored when the mode is Off
    /// or System.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub proxy_host: String,
    /// Port of a manual HTTP or SOCKS5 proxy.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub proxy_port: String,
    /// Optional proxy login for Web requests.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub proxy_username: String,
    /// Optional proxy password, in memory and the platform credential store.
    /// Legacy JSON copies remain until protected migration succeeds.
    #[serde(default, skip_serializing)]
    pub proxy_password: String,
    /// Legacy settings and their password must survive until protected migration
    /// succeeds. This flag is only in memory and is never persisted.
    #[serde(skip)]
    pub proxy_password_legacy: bool,
}

impl std::fmt::Debug for Settings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Serialization already omits the password and old combined URL.
        // Keep the proxy username out of diagnostics as well.
        let mut preferences = serde_json::to_value(self).map_err(|_| std::fmt::Error)?;
        if let Some(fields) = preferences.as_object_mut() {
            fields.remove("proxy_username");
        }
        f.debug_struct("Settings")
            .field("preferences", &preferences)
            .finish()
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            device_name: "Spotifast".to_string(),
            bitrate: 320,
            normalisation: false,
            autoplay: true,
            gapless: true,
            audio_backend: None,
            audio_device: None,
            audio_buffer_ms: default_buffer_ms(),
            audio_cache: true,
            audio_cache_mb: 1024,
            theme: ThemeChoice::System,
            custom_theme: None,
            custom_theme_cache: None,
            system_theme_cache: None,
            home: HomeSettings::default(),
            accent_from_art: true,
            volume: (u16::MAX as u32 * 70 / 100) as u16,
            sidebar_visible: true,
            art_expanded: false,
            sidebar_compact: false,
            sidebar_grid: false,
            sidebar_width: 250.0,
            lyrics_width: 360.0,
            queue_width: 360.0,
            tracklist_compact: false,
            middle_click_autoscroll: false,
            search_history: Vec::new(),
            show_shortcut_hints: true,
            web_client_id: None,
            personal_app_nudge_at: None,
            personal_app_intro_seen: false,
            playback_authorized: false,
            keep_playing_in_background: true,
            check_for_updates: true,
            download_updates_automatically: false,
            pinned_contexts: Vec::new(),
            liked_songs_pinned: true,
            sidebar_order: Vec::new(),
            library_sort: std::collections::BTreeMap::new(),
            zoom: 1.0,
            winamp_window: false,
            winamp_show_taskbar: true,
            custom_titlebar: false,
            skin: None,
            skin_scale: None,
            winamp_on_top: false,
            vis: VisMode::default(),
            playlist_open: false,
            playlist_height: 174,
            eq_open: false,
            eq_on: false,
            eq_preamp_db: 0.0,
            eq_bands_db: [0.0; 10],
            balance: 0.0,
            mono: false,
            playlist_shaded: false,
            eq_shaded: false,
            winamp_shaded: false,
            milkdrop_open: false,
            milkdrop_seconds: crate::milkdrop::DEFAULT_SECONDS,
            milkdrop_fps: crate::milkdrop::DEFAULT_FPS,
            milkdrop_screen_hz: 0,
            milkdrop_scale: 1,
            milkdrop_fullscreen: false,
            milkdrop_size: crate::milkdrop::DEFAULT_SIZE,
            proxy_mode: ProxyMode::System,
            proxy: String::new(),
            proxy_host: String::new(),
            proxy_port: String::new(),
            proxy_username: String::new(),
            proxy_password: String::new(),
            proxy_password_legacy: false,
        }
    }
}

fn default_buffer_ms() -> u32 {
    crate::sink::DEFAULT_BUFFER_MS
}

impl Settings {
    pub(crate) fn cached_palette(&self) -> Option<crate::theme::Palette> {
        let theme = if self.custom_theme.is_some() {
            self.custom_theme_cache.as_ref()
        } else if self.theme == ThemeChoice::System {
            self.system_theme_cache.as_ref()
        } else {
            None
        };
        theme.map(|theme| theme.palette)
    }

    pub(crate) fn proxy_password_record(
        &self,
    ) -> Result<Option<crate::credentials::ProxyPassword>, String> {
        if self.proxy_password.is_empty() {
            return Ok(None);
        }
        let manual = ManualProxy::parse(
            ManualKind::Http,
            &self.proxy_host,
            &self.proxy_port,
            &self.proxy_username,
            &self.proxy_password,
        )?;
        Ok(manual.password_record())
    }

    pub(crate) fn restore_proxy_password(
        &mut self,
        saved: &crate::credentials::ProxyPassword,
    ) -> bool {
        let Ok(mut manual) = ManualProxy::parse(
            ManualKind::Http,
            &self.proxy_host,
            &self.proxy_port,
            &self.proxy_username,
            "",
        ) else {
            return false;
        };
        if !manual.restore_password(saved) {
            return false;
        }
        self.proxy_password = manual.password;
        true
    }

    pub fn proxy_preferences(&self) -> ProxyPreferences {
        ProxyPreferences {
            mode: self.proxy_mode,
            host: self.proxy_host.clone(),
            port: self.proxy_port.clone(),
            username: self.proxy_username.clone(),
        }
    }

    pub fn library_pins(&self) -> Vec<String> {
        let mut pins = self.pinned_contexts.clone();
        if !self.liked_songs_pinned {
            pins.retain(|key| key != LIKED_SONGS_KEY);
        } else if !pins.iter().any(|key| key == LIKED_SONGS_KEY) {
            pins.insert(0, LIKED_SONGS_KEY.into());
        }
        pins
    }

    pub fn load(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(text) => {
                let mut settings = serde_json::from_str(&text).unwrap_or_else(|error| {
                    log::warn!("settings at {} are unreadable: {error}", path.display());
                    Self::default()
                });
                settings.migrate_proxy(&text);
                if settings.device_name == "Fastpotify" {
                    settings.device_name = "Spotifast".into();
                }
                for key in settings
                    .pinned_contexts
                    .iter_mut()
                    .chain(settings.sidebar_order.iter_mut())
                {
                    if key == "fastpotify:liked-songs" {
                        *key = LIKED_SONGS_KEY.into();
                    }
                }
                settings.proxy_password_legacy = !settings.proxy_password.is_empty();
                settings
            }
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self, path: &Path) {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let text = match self
            .encode_for_profile(path == crate::paths::AppDirs::legacy().settings_file())
        {
            Ok(text) => text,
            Err(error) => {
                log::warn!("unable to encode settings: {error}");
                return;
            }
        };
        let temporary = path.with_extension("json.tmp");
        let written = std::fs::write(&temporary, text)
            .and_then(|()| crate::util::replace_file(&temporary, path));
        if let Err(error) = written {
            log::warn!("unable to save settings to {}: {error}", path.display());
        }
    }

    fn encode_for_profile(&self, legacy: bool) -> serde_json::Result<String> {
        if !legacy {
            return serde_json::to_string_pretty(self);
        }
        // A trial launch can still roll back to a client that only recognizes
        // the previous local key. Keep its on-disk spelling until migration.
        let mut saved = self.clone();
        for key in saved
            .pinned_contexts
            .iter_mut()
            .chain(saved.sidebar_order.iter_mut())
        {
            if key == LIKED_SONGS_KEY {
                *key = "fastpotify:liked-songs".into();
            }
        }
        serde_json::to_string_pretty(&saved)
    }

    pub fn platform_backend(&self) -> Option<String> {
        self.audio_backend.clone().or_else(|| {
            if cfg!(target_os = "linux") {
                Some("pulseaudio".to_string())
            } else {
                None
            }
        })
    }

    pub fn remember_search(&mut self, query: &str) {
        let query = query.trim();
        if query.is_empty() {
            return;
        }
        self.search_history.retain(|entry| entry != query);
        self.search_history.insert(0, query.to_string());
        self.search_history.truncate(12);
    }

    pub(crate) fn migrate_proxy(&mut self, text: &str) {
        if !text.contains("\"proxy_mode\"") {
            self.proxy_mode = infer_legacy_proxy_mode(&self.proxy);
        }
        if self.proxy_host.is_empty() && !self.proxy.trim().is_empty() {
            let (host, port) = split_legacy_address(&self.proxy);
            self.proxy_host = host;
            if self.proxy_port.is_empty() {
                self.proxy_port = port;
            }
        }
    }

    /// The proxy the HTTP client should use. Off and System always succeed.
    /// HTTP and SOCKS5 need a host and port.
    pub fn proxy_config(&self) -> Result<ProxyConfig, String> {
        match self.proxy_mode {
            ProxyMode::Off => Ok(ProxyConfig::Off),
            ProxyMode::System => Ok(ProxyConfig::System),
            ProxyMode::Http => Ok(ProxyConfig::Http(ManualProxy::parse(
                ManualKind::Http,
                &self.proxy_host,
                &self.proxy_port,
                &self.proxy_username,
                &self.proxy_password,
            )?)),
            ProxyMode::Socks => Ok(ProxyConfig::Socks(ManualProxy::parse(
                ManualKind::Socks,
                &self.proxy_host,
                &self.proxy_port,
                &self.proxy_username,
                &self.proxy_password,
            )?)),
        }
    }
}

fn infer_legacy_proxy_mode(proxy: &str) -> ProxyMode {
    let raw = proxy.trim();
    if raw.is_empty() {
        return ProxyMode::System;
    }
    let scheme = raw.split("://").next().unwrap_or("").to_ascii_lowercase();
    match scheme.as_str() {
        "socks" | "socks5" | "socks5h" => ProxyMode::Socks,
        _ => ProxyMode::Http,
    }
}

fn validate_host(host: &str) -> Result<String, String> {
    let host = host.trim();
    if host.is_empty() {
        return Err("enter a host".into());
    }
    let host = host
        .split_once("://")
        .map_or(host, |(_, rest)| rest)
        .trim_start_matches('[')
        .trim_end_matches(']');
    if host.is_empty() {
        return Err("enter a host".into());
    }
    if host.parse::<std::net::IpAddr>().is_ok() {
        return Ok(host.to_string());
    }
    if is_hostname(host) {
        return Ok(host.to_string());
    }
    Err("the host must be a hostname or IP address".into())
}

fn is_hostname(host: &str) -> bool {
    if host.len() > 253 || host.starts_with('.') || host.ends_with('.') {
        return false;
    }
    host.split('.').all(|label| {
        (1..=63).contains(&label.len())
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    })
}

fn validate_port(port: &str) -> Result<u16, String> {
    let port = port.trim();
    if port.is_empty() {
        return Err("enter a port".into());
    }
    if !port.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err("the port must be a number between 1 and 65535".into());
    }
    match port.parse::<u16>() {
        Ok(port) if port > 0 => Ok(port),
        _ => Err("the port must be a number between 1 and 65535".into()),
    }
}

fn split_legacy_address(raw: &str) -> (String, String) {
    let raw = raw.trim();
    let rest = raw.split_once("://").map_or(raw, |(_, rest)| rest);
    let rest = rest.rsplit_once('@').map_or(rest, |(_, rest)| rest);
    if let Some(inner) = rest.strip_prefix('[')
        && let Some((host, port)) = inner.split_once("]:")
    {
        return (host.to_string(), port.trim().to_string());
    }
    if let Some((host, port)) = rest.rsplit_once(':')
        && !host.contains(':')
    {
        return (host.to_string(), port.trim().to_string());
    }
    (rest.to_string(), String::new())
}

/// The resolved proxy policy used by the HTTP client and local playback.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum ProxyConfig {
    /// Invalid saved settings block requests until corrected in the interface.
    Invalid(String),
    Off,
    #[default]
    System,
    Http(ManualProxy),
    Socks(ManualProxy),
}

impl ProxyConfig {
    pub(crate) fn password_record(&self) -> Option<crate::credentials::ProxyPassword> {
        match self {
            Self::Http(manual) | Self::Socks(manual) => manual.password_record(),
            _ => None,
        }
    }

    pub(crate) fn restore_password(&mut self, saved: &crate::credentials::ProxyPassword) -> bool {
        match self {
            Self::Http(manual) | Self::Socks(manual) => manual.restore_password(saved),
            _ => false,
        }
    }

    /// Librespot's CONNECT client only understands unauthenticated,
    /// plaintext HTTP proxies. Never give it a credential-bearing URL: the
    /// upstream client logs that URL at info level and does not send proxy
    /// authentication in either of its HTTP paths.
    pub fn librespot_url(&self) -> Option<reqwest::Url> {
        match self {
            Self::Http(manual) => manual.librespot_url(),
            Self::System => system_http_proxy(),
            Self::Invalid(_) | Self::Off | Self::Socks(_) => None,
        }
    }
}

/// The HTTP proxy System mode would hand librespot, if it can resolve one.
/// SOCKS system proxies are ignored: the engine cannot use them.
fn system_http_proxy() -> Option<reqwest::Url> {
    let matcher = hyper_util::client::proxy::matcher::Matcher::from_system();
    let dest = http::Uri::from_static("https://apresolve.spotify.com");
    let intercept = matcher.intercept(&dest)?;
    librespot_system_proxy(&intercept)
}

fn librespot_system_proxy(
    intercept: &hyper_util::client::proxy::matcher::Intercept,
) -> Option<reqwest::Url> {
    if intercept.basic_auth().is_some() || intercept.raw_auth().is_some() {
        return None;
    }
    http_proxy_from_uri(intercept.uri())
}

fn http_proxy_from_uri(uri: &http::Uri) -> Option<reqwest::Url> {
    match uri.scheme_str() {
        Some("http") => reqwest::Url::parse(&uri.to_string()).ok(),
        _ => None,
    }
}

#[derive(Clone, Copy)]
enum ManualKind {
    Http,
    Socks,
}

/// A configured HTTP or SOCKS5 endpoint.
#[derive(Clone, PartialEq, Eq)]
pub struct ManualProxy {
    endpoint: reqwest::Url,
    username: String,
    password: String,
}

impl std::fmt::Debug for ManualProxy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ManualProxy")
            .field("url", &self.redacted())
            .finish()
    }
}

impl ManualProxy {
    fn password_record(&self) -> Option<crate::credentials::ProxyPassword> {
        if self.password.is_empty() {
            return None;
        }
        Some(crate::credentials::ProxyPassword {
            host: self.endpoint.host_str()?.to_string(),
            port: self.endpoint.port()?,
            username: self.username.clone(),
            password: self.password.clone(),
        })
    }

    fn restore_password(&mut self, saved: &crate::credentials::ProxyPassword) -> bool {
        if self.endpoint.host_str() != Some(saved.host.as_str())
            || self.endpoint.port() != Some(saved.port)
            || self.username != saved.username
        {
            return false;
        }
        self.password.clone_from(&saved.password);
        true
    }

    fn parse(
        kind: ManualKind,
        host: &str,
        port: &str,
        username: &str,
        password: &str,
    ) -> Result<Self, String> {
        let host = validate_host(host)?;
        let port = validate_port(port)?;
        let host = if host.contains(':') {
            format!("[{host}]")
        } else {
            host
        };
        let scheme = match kind {
            ManualKind::Http => "http",
            // Resolve Spotify hostnames at the proxy, so SOCKS mode does not
            // leak DNS queries or fail behind a proxy-only network.
            ManualKind::Socks => "socks5h",
        };
        let endpoint = reqwest::Url::parse(&format!("{scheme}://{host}:{port}"))
            .map_err(|error| format!("not a proxy address: {error}"))?;
        Ok(Self {
            endpoint,
            username: username.to_string(),
            password: password.to_string(),
        })
    }

    /// A reqwest proxy with credentials applied separately so build errors
    /// cannot echo a password.
    pub fn reqwest_proxy(&self) -> Result<reqwest::Proxy, reqwest::Error> {
        let mut proxy = reqwest::Proxy::all(self.endpoint.clone())?;
        if !self.username.is_empty() || !self.password.is_empty() {
            proxy = proxy.basic_auth(&self.username, &self.password);
        }
        Ok(proxy)
    }

    fn librespot_url(&self) -> Option<reqwest::Url> {
        (self.username.is_empty() && self.password.is_empty()).then(|| self.endpoint.clone())
    }

    /// The credential-free endpoint, for logs and the debugger.
    pub fn redacted(&self) -> reqwest::Url {
        self.endpoint.clone()
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn trial_launch_writes_keys_the_previous_release_can_still_read() {
        let settings = super::Settings {
            pinned_contexts: vec![super::LIKED_SONGS_KEY.into(), "spotify:playlist:one".into()],
            sidebar_order: vec![super::LIKED_SONGS_KEY.into()],
            ..Default::default()
        };
        for (legacy, key) in [
            (true, "fastpotify:liked-songs"),
            (false, super::LIKED_SONGS_KEY),
        ] {
            let saved: super::Settings =
                serde_json::from_str(&settings.encode_for_profile(legacy).unwrap()).unwrap();
            assert_eq!(saved.pinned_contexts, [key, "spotify:playlist:one"]);
            assert_eq!(saved.sidebar_order, [key]);
        }
        assert_eq!(settings.pinned_contexts[0], super::LIKED_SONGS_KEY);
    }
    use super::Settings;

    #[test]
    fn new_profiles_follow_the_system_and_saved_choices_are_preserved() {
        use super::ThemeChoice;
        assert_eq!(Settings::default().theme, ThemeChoice::System);
        assert_eq!(ThemeChoice::default(), ThemeChoice::System);
        let empty: Settings = serde_json::from_str("{}").unwrap();
        assert_eq!(empty.theme, ThemeChoice::System);
        for (json, choice) in [("dark", ThemeChoice::Dark), ("light", ThemeChoice::Light)] {
            let settings: Settings =
                serde_json::from_value(serde_json::json!({"theme": json, "volume": 37})).unwrap();
            assert_eq!(settings.theme, choice);
            assert_eq!(settings.volume, 37);
        }
        let settings: Settings = serde_json::from_value(serde_json::json!({
            "theme": "dark", "system_theme_cache": {"broken": true}, "volume": 37
        }))
        .unwrap();
        assert!(settings.system_theme_cache.is_none());
        assert_eq!(settings.theme, ThemeChoice::Dark);
        assert_eq!(settings.volume, 37);
    }

    #[test]
    fn custom_theme_cache_round_trips_and_a_bad_cache_keeps_other_settings() {
        let mut settings: Settings = serde_json::from_str("{}").unwrap();
        assert!(settings.custom_theme.is_none() && settings.custom_theme_cache.is_none());
        settings.custom_theme = Some("gruvbox.json".into());
        let mut palette = crate::theme::Palette::light();
        palette.shadow = egui::Color32::from_rgba_unmultiplied(37, 128, 249, 117);
        settings.custom_theme_cache = Some(crate::theme::custom::CustomTheme {
            filename: "gruvbox.json".into(),
            palette,
        });
        let encoded = serde_json::to_string(&settings).unwrap();
        assert_eq!(
            serde_json::from_str::<Settings>(&encoded).unwrap(),
            settings
        );
        let mut damaged = serde_json::to_value(&settings).unwrap();
        damaged["custom_theme_cache"] = serde_json::json!({"palette": "broken"});
        damaged["audio_cache_mb"] = 777.into();
        let recovered: Settings = serde_json::from_value(damaged).unwrap();
        assert_eq!(recovered.custom_theme.as_deref(), Some("gruvbox.json"));
        assert_eq!(recovered.audio_cache_mb, 777);
        assert!(recovered.custom_theme_cache.is_none());
    }

    #[test]
    fn partial_home_preferences_keep_defaults_and_survive_a_settings_round_trip() {
        for (home, made_for_you, recommendations) in [
            ("{}", true, true),
            (r#"{"made_for_you":{"visible":false}}"#, false, true),
            (r#"{"recommendations":{"visible":false}}"#, true, false),
            (
                r#"{"made_for_you":{"visible":false},"recommendations":{"visible":false}}"#,
                false,
                false,
            ),
        ] {
            let settings: Settings =
                serde_json::from_str(&format!(r#"{{"volume":12345,"home":{home}}}"#)).unwrap();
            assert_eq!(settings.home.made_for_you.visible, made_for_you);
            assert_eq!(settings.home.recommendations.visible, recommendations);
            assert_eq!(settings.volume, 12345);
            let restored: Settings =
                serde_json::from_str(&serde_json::to_string(&settings).unwrap()).unwrap();
            assert_eq!(restored, settings);
        }
        let old: Settings = serde_json::from_str(r#"{"volume":12345}"#).unwrap();
        assert!(old.home.made_for_you.visible);
        assert!(old.home.recommendations.visible);
        assert_eq!(old.volume, 12345);
    }

    #[test]
    fn older_settings_keep_the_sidebar_visible() {
        let settings: Settings = serde_json::from_str("{}").unwrap();
        assert!(settings.sidebar_visible);
    }

    #[test]
    fn older_library_settings_keep_liked_songs_ahead_of_existing_pins() {
        let settings: Settings = serde_json::from_str(
            r#"{"pinned_contexts":["spotify:playlist:one"],"sidebar_order":["spotify:playlist:two"]}"#,
        ).unwrap();
        assert!(settings.liked_songs_pinned);
        assert_eq!(
            settings.library_pins(),
            [super::LIKED_SONGS_KEY, "spotify:playlist:one"]
        );
        assert_eq!(settings.sidebar_order, ["spotify:playlist:two"]);
    }

    #[test]
    fn older_settings_keep_the_winamp_window_closed_and_the_built_in_skin() {
        let settings: Settings = serde_json::from_str(r#"{"zoom": 1.2}"#).unwrap();
        assert!(!settings.winamp_window);
        assert!(settings.winamp_show_taskbar);
        assert_eq!(settings.skin, None);
        assert_eq!(settings.skin_scale, None);
        assert!(!settings.winamp_on_top);
        assert_eq!(settings.vis, super::VisMode::Bars);
        assert!(!settings.playlist_open);
        assert_eq!(settings.playlist_height, 174);
        assert!(!settings.eq_on);
        assert_eq!(settings.eq_bands_db, [0.0; 10]);
        assert_eq!(settings.balance, 0.0);
        assert!(!settings.mono);
        assert!(!settings.playlist_shaded);
        assert!(!settings.eq_shaded);
        assert!(!settings.winamp_shaded);
    }

    #[test]
    fn the_visualiser_cycles_bars_scope_off() {
        use super::VisMode;
        assert_eq!(VisMode::Bars.next(), VisMode::Scope);
        assert_eq!(VisMode::Scope.next(), VisMode::Off);
        assert_eq!(VisMode::Off.next(), VisMode::Bars);
        let settings: Settings = serde_json::from_str(r#"{"vis": "scope"}"#).unwrap();
        assert_eq!(settings.vis, VisMode::Scope);
    }

    #[test]
    fn a_chosen_skin_round_trips() {
        let settings = Settings {
            winamp_window: true,
            skin: Some("Zaxon.wsz".into()),
            skin_scale: Some(3),
            ..Settings::default()
        };
        let json = serde_json::to_string(&settings).unwrap();
        let restored: Settings = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, settings);
    }

    #[test]
    fn hidden_sidebar_round_trips() {
        let settings = Settings {
            sidebar_visible: false,
            ..Settings::default()
        };
        let json = serde_json::to_string(&settings).unwrap();
        let restored: Settings = serde_json::from_str(&json).unwrap();
        assert!(!restored.sidebar_visible);
    }

    #[test]
    fn older_settings_default_to_standard_sidebar() {
        let settings: Settings = serde_json::from_str("{}").unwrap();
        assert!(!settings.sidebar_compact);
    }

    #[test]
    fn compact_sidebar_round_trips() {
        let settings = Settings {
            sidebar_compact: true,
            ..Settings::default()
        };
        let json = serde_json::to_string(&settings).unwrap();
        let restored: Settings = serde_json::from_str(&json).unwrap();
        assert!(restored.sidebar_compact);
    }

    #[test]
    fn library_grid_round_trips_and_older_settings_keep_the_list() {
        let old: Settings = serde_json::from_str("{}").unwrap();
        assert!(!old.sidebar_grid);

        let settings = Settings {
            sidebar_grid: true,
            ..Settings::default()
        };
        let json = serde_json::to_string(&settings).unwrap();
        let restored: Settings = serde_json::from_str(&json).unwrap();
        assert!(restored.sidebar_grid);
    }

    #[test]
    fn older_settings_default_to_standard_tracklist() {
        let settings: Settings = serde_json::from_str("{}").unwrap();
        assert!(!settings.tracklist_compact);
    }

    #[test]
    fn compact_tracklist_round_trips() {
        let settings = Settings {
            tracklist_compact: true,
            ..Settings::default()
        };
        let json = serde_json::to_string(&settings).unwrap();
        let restored: Settings = serde_json::from_str(&json).unwrap();
        assert!(restored.tracklist_compact);
    }

    #[test]
    fn middle_click_autoscroll_is_opt_in_and_round_trips() {
        let settings: Settings = serde_json::from_str("{}").unwrap();
        assert!(!settings.middle_click_autoscroll);
        let settings = Settings {
            middle_click_autoscroll: true,
            ..Settings::default()
        };
        let json = serde_json::to_string(&settings).unwrap();
        let restored: Settings = serde_json::from_str(&json).unwrap();
        assert!(restored.middle_click_autoscroll);
    }

    #[test]
    fn older_settings_use_the_system_proxy() {
        let settings: Settings = serde_json::from_str("{}").unwrap();
        assert_eq!(settings.proxy_mode, super::ProxyMode::System);
        assert!(settings.proxy_host.is_empty());
        assert!(settings.proxy_port.is_empty());
        assert!(settings.proxy_username.is_empty());
        assert!(settings.proxy_password.is_empty());
        assert_eq!(settings.proxy_config().unwrap(), super::ProxyConfig::System);
    }

    #[test]
    fn a_proxy_round_trips_through_settings() {
        let settings = Settings {
            proxy_mode: super::ProxyMode::Socks,
            proxy_host: "127.0.0.1".into(),
            proxy_port: "1080".into(),
            proxy_username: "alice".into(),
            proxy_password: "secret".into(),
            ..Settings::default()
        };
        let json = serde_json::to_string(&settings).unwrap();
        let restored: Settings = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.proxy_mode, settings.proxy_mode);
        assert_eq!(restored.proxy_host, settings.proxy_host);
        assert_eq!(restored.proxy_port, settings.proxy_port);
        assert_eq!(restored.proxy_username, settings.proxy_username);
        assert!(restored.proxy_password.is_empty());
        assert!(json.contains("socks"));
        assert!(json.contains("127.0.0.1"));
        assert!(json.contains("1080"));
        assert!(json.contains("alice"));
        assert!(!json.contains("secret"));
        assert!(!json.contains("proxy_password"));
        assert!(!json.contains("\"proxy\":"));
    }

    #[test]
    fn settings_debug_keeps_proxy_credentials_private() {
        let settings = Settings {
            proxy_username: "private-proxy-user".into(),
            proxy_password: "private-proxy-password".into(),
            ..Default::default()
        };
        let debug = format!("{settings:?}");
        assert!(!debug.contains("private-proxy-user"));
        assert!(!debug.contains("private-proxy-password"));
    }

    #[test]
    fn a_saved_proxy_password_is_bound_to_its_endpoint_and_username() {
        let original = Settings {
            proxy_mode: super::ProxyMode::Http,
            proxy_host: "127.0.0.1".into(),
            proxy_port: "8080".into(),
            proxy_username: "dummy-user".into(),
            proxy_password: " dummy-private-password\n".into(),
            ..Default::default()
        };
        let password = original.proxy_password_record().unwrap().unwrap();
        let mut restored = original.clone();
        restored.proxy_password.clear();
        assert!(restored.restore_proxy_password(&password));
        assert_eq!(restored.proxy_password, original.proxy_password);
        for (host, port, username) in [
            ("other.invalid", "8080", "dummy-user"),
            ("127.0.0.1", "8081", "dummy-user"),
            ("127.0.0.1", "8080", "other-user"),
        ] {
            let mut changed = Settings {
                proxy_host: host.into(),
                proxy_port: port.into(),
                proxy_username: username.into(),
                ..restored.clone()
            };
            changed.proxy_password.clear();
            assert!(!changed.restore_proxy_password(&password));
            assert!(changed.proxy_password.is_empty());
        }
        assert!(
            !serde_json::to_string(&restored)
                .unwrap()
                .contains("dummy-private-password")
        );
    }

    #[test]
    fn an_empty_proxy_is_omitted_from_the_file() {
        let json = serde_json::to_string(&Settings::default()).unwrap();
        assert!(!json.contains("proxy"));
    }

    #[test]
    fn off_is_written_and_round_trips() {
        let settings = Settings {
            proxy_mode: super::ProxyMode::Off,
            ..Settings::default()
        };
        let json = serde_json::to_string(&settings).unwrap();
        assert!(json.contains("\"off\""));
        let restored: Settings = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.proxy_mode, super::ProxyMode::Off);
        assert_eq!(restored.proxy_config().unwrap(), super::ProxyConfig::Off);
    }

    #[test]
    fn a_legacy_socks_url_becomes_socks_mode() {
        let dir = std::env::temp_dir().join(format!("spotifast-proxy-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("settings.json");
        std::fs::write(
            &path,
            r#"{"proxy":"socks5://127.0.0.1:1080","proxy_username":"alice"}"#,
        )
        .unwrap();
        let settings = Settings::load(&path);
        assert_eq!(settings.proxy_mode, super::ProxyMode::Socks);
        assert_eq!(settings.proxy_host, "127.0.0.1");
        assert_eq!(settings.proxy_port, "1080");
        let super::ProxyConfig::Socks(manual) = settings.proxy_config().unwrap() else {
            panic!("expected a SOCKS proxy");
        };
        assert_eq!(manual.redacted().scheme(), "socks5h");
        assert_eq!(manual.redacted().host_str(), Some("127.0.0.1"));
        assert_eq!(manual.redacted().port(), Some(1080));
        assert!(manual.redacted().username().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn http_and_socks_parse_a_host_and_port() {
        let http = super::ManualProxy::parse(super::ManualKind::Http, "127.0.0.1", "7890", "", "")
            .unwrap();
        assert_eq!(http.redacted().as_str(), "http://127.0.0.1:7890/");
        assert!(
            super::ProxyConfig::Http(http.clone())
                .librespot_url()
                .is_some()
        );

        let socks =
            super::ManualProxy::parse(super::ManualKind::Socks, "127.0.0.1", "1080", "", "")
                .unwrap();
        assert_eq!(socks.redacted().scheme(), "socks5h");
        assert_eq!(socks.redacted().port(), Some(1080));
        assert_eq!(super::ProxyConfig::Socks(socks).librespot_url(), None);
    }

    #[test]
    fn engine_only_accepts_plain_http_proxy_urls() {
        assert_eq!(super::ProxyConfig::Off.librespot_url(), None);
        let http_uri: http::Uri = "http://127.0.0.1:8080".parse().unwrap();
        let https_uri: http::Uri = "https://127.0.0.1:8080".parse().unwrap();
        let socks_uri: http::Uri = "socks5://127.0.0.1:1080".parse().unwrap();
        assert!(super::http_proxy_from_uri(&http_uri).is_some());
        assert!(super::http_proxy_from_uri(&https_uri).is_none());
        assert!(super::http_proxy_from_uri(&socks_uri).is_none());
    }

    #[test]
    fn proxy_credentials_stay_opaque_and_out_of_urls() {
        let proxy = super::ManualProxy::parse(
            super::ManualKind::Http,
            "127.0.0.1",
            "7890",
            "alice/name",
            " secret/with spaces ",
        )
        .unwrap();
        let redacted = proxy.redacted();
        assert!(redacted.username().is_empty());
        assert_eq!(redacted.password(), None);
        assert_eq!(proxy.username, "alice/name");
        assert_eq!(proxy.password, " secret/with spaces ");
        assert_eq!(
            super::ProxyConfig::Http(proxy.clone()).librespot_url(),
            None
        );
        let debug = format!("{proxy:?}");
        assert!(!debug.contains("alice"));
        assert!(!debug.contains("secret"));
    }

    #[test]
    fn system_proxy_authentication_and_tls_endpoints_stay_out_of_librespot() {
        let authenticated = hyper_util::client::proxy::matcher::Matcher::builder()
            .all("http://alice:secret@127.0.0.1:8080")
            .build();
        let destination = http::Uri::from_static("https://apresolve.spotify.com");
        let intercept = authenticated.intercept(&destination).unwrap();
        assert!(super::librespot_system_proxy(&intercept).is_none());

        let tls = hyper_util::client::proxy::matcher::Matcher::builder()
            .all("https://127.0.0.1:8080")
            .build();
        let intercept = tls.intercept(&destination).unwrap();
        assert!(super::librespot_system_proxy(&intercept).is_none());
    }

    #[test]
    fn proxy_parse_rejects_a_missing_host_or_port() {
        assert!(
            super::ManualProxy::parse(super::ManualKind::Http, "", "8080", "", "")
                .unwrap_err()
                .contains("host")
        );
        assert!(
            super::ManualProxy::parse(super::ManualKind::Http, "127.0.0.1", "", "", "")
                .unwrap_err()
                .contains("port")
        );
        assert!(
            super::ManualProxy::parse(super::ManualKind::Http, "127.0.0.1", "abc", "", "")
                .unwrap_err()
                .contains("number")
        );
        assert!(
            super::ManualProxy::parse(super::ManualKind::Http, "not a host", "1080", "", "")
                .unwrap_err()
                .contains("hostname")
        );
        assert!(
            super::ManualProxy::parse(super::ManualKind::Http, "127.0.0.1", "0", "", "")
                .unwrap_err()
                .contains("65535")
        );
        assert!(
            super::ManualProxy::parse(super::ManualKind::Http, "127.0.0.1", "65536", "", "")
                .unwrap_err()
                .contains("65535")
        );
        super::ManualProxy::parse(super::ManualKind::Http, "localhost", "8080", "", "").unwrap();
        super::ManualProxy::parse(super::ManualKind::Http, "::1", "8080", "", "").unwrap();
    }

    #[test]
    fn personal_app_nudge_time_is_backward_compatible_and_round_trips() {
        let older: Settings = serde_json::from_str("{}").unwrap();
        assert_eq!(older.personal_app_nudge_at, None);
        assert!(!older.personal_app_intro_seen);

        let settings = Settings {
            personal_app_nudge_at: Some("2026-09-03T15:00:00Z".into()),
            personal_app_intro_seen: true,
            ..Settings::default()
        };
        let json = serde_json::to_string(&settings).unwrap();
        let restored: Settings = serde_json::from_str(&json).unwrap();
        assert_eq!(
            restored.personal_app_nudge_at,
            settings.personal_app_nudge_at
        );
        assert!(restored.personal_app_intro_seen);
    }
}

/// The last playlist tree received for one account.
///
/// Only folder order is cached. Edit grants must always come from the live
/// session because they can be revoked.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CachedRootlist {
    pub account_id: String,
    pub entries: Vec<crate::player::RootlistEntry>,
}

/// Restorable UI session: what was open when the app last closed.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SessionState {
    pub last_page: Option<String>,
    /// Context URIs most recently played, newest first.
    pub recent_contexts: Vec<String>,
    /// What was playing when the app closed, to resume from a cold start.
    pub last_context: Option<String>,
    pub last_track: Option<String>,
    pub last_position_ms: u32,
    /// Manually queued songs to restore with the remembered track.
    ///
    /// Context rows are excluded to prevent duplicates. This replaced the old
    /// `last_queue` field, so sessions using that field restore no added rows.
    pub last_added_queue: Vec<String>,
    /// Queue rows displayed on the next start. Playback restores manual rows
    /// from `last_added_queue`; it does not enqueue this list.
    pub last_queue_rows: Vec<crate::api::models::PlayableItem>,
    /// Sidebar folders rolled up, by their rootlist ids.
    pub collapsed_folders: Vec<String>,
    /// Last good playlist tree, scoped to the account that supplied it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rootlist: Option<CachedRootlist>,
    /// Shuffle mode saved across contexts and restarts.
    pub shuffle_on: bool,
    /// Each table's chosen sort, by encoded page, restored at start.
    pub sorts: Vec<(String, crate::model::TableSort)>,
    /// Last window inner size, to restore on next launch.
    pub window_size: Option<[f32; 2]>,
    /// Last window outer position, to restore on next launch.
    pub window_pos: Option<[f32; 2]>,
    /// Whether the queue panel was open.
    pub queue_open: Option<bool>,
    /// Which tab the queue panel showed: `queue` or `recents`.
    pub queue_tab: Option<String>,
    /// Last outer position of the Winamp window.
    pub winamp_pos: Option<[f32; 2]>,
    /// Last outer position of the MilkDrop window.
    pub milkdrop_pos: Option<[f32; 2]>,
    /// The window mode fullscreen lyrics left, when the app closed while
    /// showing them. eframe restores the window full screen, so the next
    /// start returns it to this mode instead.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lyrics_fullscreen_from: Option<WindowMode>,
}

/// Whether a window was full screen, and whether it was maximized.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct WindowMode {
    pub fullscreen: bool,
    pub maximized: bool,
}

impl SessionState {
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let text = match serde_json::to_string(self) {
            Ok(text) => text,
            Err(error) => {
                log::warn!("unable to encode session: {error}");
                return;
            }
        };
        let temporary = path.with_extension("json.tmp");
        let written = std::fs::write(&temporary, text)
            .and_then(|()| crate::util::replace_file(&temporary, path));
        if let Err(error) = written {
            log::warn!("unable to save session to {}: {error}", path.display());
        }
    }
}

#[cfg(test)]
mod session_tests {
    use super::{CachedRootlist, SessionState};
    use crate::player::RootlistEntry;

    #[test]
    fn old_sessions_without_a_playlist_tree_remain_readable() {
        let state: SessionState = serde_json::from_str(r#"{"last_page":"home"}"#).unwrap();
        assert_eq!(state.last_page.as_deref(), Some("home"));
        assert_eq!(state.rootlist, None);
    }

    #[test]
    fn the_playlist_tree_round_trips_with_its_account() {
        let state = SessionState {
            rootlist: Some(CachedRootlist {
                account_id: "listener".into(),
                entries: vec![
                    RootlistEntry::FolderStart {
                        id: "folder".into(),
                        name: "Favorites".into(),
                    },
                    RootlistEntry::Playlist("spotify:playlist:one".into()),
                    RootlistEntry::FolderEnd,
                ],
            }),
            ..SessionState::default()
        };

        let json = serde_json::to_string(&state).unwrap();
        assert_eq!(serde_json::from_str::<SessionState>(&json).unwrap(), state);
    }

    #[test]
    fn a_new_session_atomically_replaces_the_previous_one() {
        let root = std::env::temp_dir().join(format!(
            "spotifast-session-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let path = root.join("session.json");
        let state = |page: &str| SessionState {
            last_page: Some(page.into()),
            ..SessionState::default()
        };

        state("home").save(&path);
        state("liked").save(&path);

        assert_eq!(
            SessionState::load(&path).last_page.as_deref(),
            Some("liked")
        );
        assert!(!path.with_extension("json.tmp").exists());
        let _ = std::fs::remove_dir_all(root);
    }
}
