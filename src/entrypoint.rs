// Shared by the Spotifast command and its Fastpotify compatibility command.

use spotifast::{app, backend, paths, settings, single_instance, util};

use clap::{CommandFactory, FromArgMatches, Parser};

/// A fast, native Spotify client.
#[derive(Debug, Parser)]
#[command(name = env!("CARGO_BIN_NAME"), version, about)]
struct Cli {
    /// A command for the running instance; without one, the app starts.
    #[command(subcommand)]
    control: Option<Control>,

    /// A Spotify link to open: spotify:track:…, or an open.spotify.com
    /// address. The running Spotifast opens it when there is one, which
    /// is how the desktop hands links over.
    #[arg(value_name = "LINK")]
    link: Option<String>,

    /// Spotify Connect device name for this session.
    #[arg(long)]
    device_name: Option<String>,

    /// Log more from librespot and the Web API client.
    #[arg(short, long)]
    verbose: bool,

    /// Start with sample data and no Spotify connection (for screenshots).
    #[cfg(feature = "demo")]
    #[arg(long)]
    demo: bool,

    #[cfg(feature = "demo")]
    #[arg(long, requires = "demo")]
    demo_update_feed: Option<String>,

    #[cfg(feature = "demo")]
    #[arg(long, requires = "demo")]
    demo_data: Option<std::path::PathBuf>,

    /// Page to open in demo mode, e.g. `home`, `playlist:pl1`, `artist:art0`.
    #[cfg(feature = "demo")]
    #[arg(long)]
    demo_page: Option<String>,

    /// Extra demo surfaces: a comma-separated list of `queue`, `playing-next`,
    /// `devices`, `shortcuts`, `create`, `light`, `focus`, `update`, `personal-app`,
    /// `windows-taskbar`, `german`, `lyrics`, `lyrics-fullscreen`, `collection-loading`,
    /// `shuffle-selected`, `shuffle-started`, `library-list`, `library-list-narrow`,
    /// `library-list-wide`, `library-grid`, `library-grid-narrow`, or `library-grid-wide`.
    #[cfg(feature = "demo")]
    #[arg(long)]
    demo_show: Option<String>,

    /// Interface language, in place of the saved setting and the system's.
    /// Requires demo mode.
    #[cfg(feature = "demo")]
    #[arg(long, value_enum)]
    demo_language: Option<spotifast::i18n::Locale>,

    /// Write a PNG of the demo window to this path and exit. Implies
    /// `--demo`. Without `--demo-size`, the shot is the window's own frame
    /// buffer: full screen where that request is honoured, and the size of
    /// the tile under a tiling window manager, which decides for itself.
    #[cfg(feature = "demo")]
    #[arg(long, value_name = "PATH")]
    demo_shot: Option<std::path::PathBuf>,

    /// How long to let cover art download before the shot is taken.
    #[cfg(feature = "demo")]
    #[arg(long, value_name = "MS", default_value_t = 6000)]
    demo_shot_delay: u64,

    /// Inner window size for `--demo-shot`, as `WIDTHxHEIGHT`.
    #[cfg(feature = "demo")]
    #[arg(long, value_name = "WIDTHxHEIGHT", value_parser = parse_demo_size)]
    demo_size: Option<[f32; 2]>,

    /// Hold a drag for `--demo-shot`: press the primary button at the first
    /// point, move to the second and keep it held, as `X,Y:X,Y` in points.
    #[cfg(feature = "demo")]
    #[arg(long, value_name = "X,Y:X,Y", value_parser = parse_demo_drag)]
    demo_drag: Option<[egui::Pos2; 2]>,
}

/// Remote control of the running instance, for Raycast scripts, launchers,
/// and hands on keyboards.
#[derive(Debug, clap::Subcommand)]
enum Control {
    /// Toggle play/pause
    PlayPause,
    /// Start playback if paused
    Play,
    /// Pause playback if playing
    Pause,
    /// Skip to the next track
    Next,
    /// Return to the previous track
    Previous,
    /// Seek by this many seconds; negative seeks backwards
    Seek {
        #[arg(allow_negative_numbers = true)]
        seconds: i64,
    },
    /// Seek to a position, in seconds from the start
    SeekTo { seconds: u32 },
    /// Set the volume to a percentage
    Volume {
        #[arg(value_parser = clap::value_parser!(u8).range(0..=100))]
        percent: u8,
    },
    /// Raise the volume
    VolumeUp {
        #[arg(default_value_t = 10, value_parser = clap::value_parser!(u8).range(1..=100))]
        percent: u8,
    },
    /// Lower the volume
    VolumeDown {
        #[arg(default_value_t = 10, value_parser = clap::value_parser!(u8).range(1..=100))]
        percent: u8,
    },
    /// Toggle mute
    Mute,
    /// Toggle shuffle, or set it outright
    Shuffle { state: Option<OnOff> },
    /// Cycle the repeat mode, or set it outright
    Repeat { mode: Option<Repeat> },
    /// Save the playing track to your library, or take it back out
    Like,
    /// Play a Spotify URI: a track, album, playlist, artist, or show
    PlayUri { uri: String },
    /// List the Spotify Connect devices
    Devices {
        /// Print the JSON the running instance sent instead.
        #[arg(long)]
        raw: bool,
    },
    /// Move playback to a device, by the id `devices` prints
    Transfer { device_id: String },
    /// Print the playing track
    NowPlaying {
        /// Print the fields tab-separated instead: state, title, artists,
        /// album, position_ms, duration_ms, volume, shuffle, repeat,
        /// art_url, saved, device.
        #[arg(long)]
        raw: bool,
    },
    /// Bring the window of the running instance forward
    Show,
    /// Reload local palette files without starting the app or interrupting playback
    ReloadThemes,
}

#[derive(Clone, Copy, Debug, clap::ValueEnum)]
enum OnOff {
    On,
    Off,
}

#[derive(Clone, Copy, Debug, clap::ValueEnum)]
enum Repeat {
    /// Play through and stop
    Off,
    /// Repeat the album, playlist, or queue
    Context,
    /// Repeat this track
    Track,
}

/// Sends one control verb to the running instance. Speaks over the
/// single-instance loopback socket, which Linux does not have.
#[cfg(not(target_os = "linux"))]
fn run_control(control: Control) -> i32 {
    let raw = matches!(
        control,
        Control::NowPlaying { raw: true } | Control::Devices { raw: true }
    );
    let verb = match control {
        Control::PlayPause => "playpause".to_owned(),
        Control::Play => "play".to_owned(),
        Control::Pause => "pause".to_owned(),
        Control::Next => "next".to_owned(),
        Control::Previous => "previous".to_owned(),
        Control::Seek { seconds } => format!("seek-by {}", seconds.saturating_mul(1000)),
        Control::SeekTo { seconds } => format!("seek-to {}", u64::from(seconds) * 1000),
        Control::Volume { percent } => format!("volume-set {percent}"),
        Control::VolumeUp { percent } => format!("volume-by {percent}"),
        Control::VolumeDown { percent } => format!("volume-by -{percent}"),
        Control::Mute => "mute".to_owned(),
        Control::Shuffle { state: None } => "shuffle".to_owned(),
        Control::Shuffle { state: Some(state) } => {
            let state = match state {
                OnOff::On => "on",
                OnOff::Off => "off",
            };
            format!("shuffle-set {state}")
        }
        Control::Repeat { mode: None } => "repeat".to_owned(),
        Control::Repeat { mode: Some(mode) } => {
            let mode = match mode {
                Repeat::Off => "off",
                Repeat::Context => "context",
                Repeat::Track => "track",
            };
            format!("repeat-set {mode}")
        }
        Control::Like => "save-toggle".to_owned(),
        Control::PlayUri { uri } => format!("play-uri {uri}"),
        Control::Devices { .. } => "devices".to_owned(),
        Control::Transfer { device_id } => format!("transfer {device_id}"),
        Control::NowPlaying { .. } => "nowplaying".to_owned(),
        Control::Show => "show".to_owned(),
        Control::ReloadThemes => "reload-themes".to_owned(),
    };
    match single_instance::send(&verb) {
        Ok(single_instance::Reply::Ok) => 0,
        Ok(single_instance::Reply::NowPlaying(snapshot)) => {
            if raw {
                println!("{snapshot}");
            } else {
                println!("{}", format_now_playing(&snapshot));
            }
            0
        }
        Ok(single_instance::Reply::Devices(snapshot)) => {
            if raw {
                println!("{snapshot}");
            } else {
                print!("{}", format_devices(&snapshot));
            }
            0
        }
        Err(error) => {
            eprintln!("Spotifast is not running or does not support remote control: {error}");
            1
        }
    }
}

#[cfg(target_os = "linux")]
fn run_control(control: Control) -> i32 {
    let result = match control {
        Control::Like => single_instance::toggle_saved(),
        Control::ReloadThemes => single_instance::reload_themes(),
        _ => {
            eprintln!(
                "On Linux the running instance speaks MPRIS instead; use e.g. \
                 `playerctl --player=spotifast play-pause`."
            );
            return 2;
        }
    };
    match result {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("Spotifast is not running or does not support remote control: {error}");
            1
        }
    }
}

#[cfg(target_os = "linux")]
const PULSEAUDIO_PROPERTIES: [(&str, &str); 2] = [
    ("PULSE_PROP_application.name", "Spotifast"),
    ("PULSE_PROP_stream.description", "Spotify playback"),
];

#[cfg(target_os = "linux")]
fn missing_pulseaudio_properties(
    mut is_set: impl FnMut(&str) -> bool,
) -> impl Iterator<Item = (&'static str, &'static str)> {
    PULSEAUDIO_PROPERTIES
        .into_iter()
        .filter(move |(key, _)| !is_set(key))
}

/// Gives the librespot PulseAudio backend useful stream metadata.
///
/// That backend leaves both names empty unless its caller provides them. An
/// unnamed stream cannot be identified reliably by mixers and audio processors.
#[cfg(target_os = "linux")]
fn configure_pulseaudio_properties() {
    for (key, value) in missing_pulseaudio_properties(|key| std::env::var_os(key).is_some()) {
        // SAFETY: run calls this at process startup, before it creates any
        // threads. Keep explicit values supplied by the launcher or user.
        unsafe { std::env::set_var(key, value) };
    }
}

/// The `nowplaying` snapshot as one human-readable line.
#[cfg(not(target_os = "linux"))]
fn format_now_playing(snapshot: &str) -> String {
    let mut fields = snapshot.split('\t');
    let state = fields.next().unwrap_or_default();
    let title = fields.next().unwrap_or_default();
    let artists = fields.next().unwrap_or_default();
    let _album = fields.next();
    let position_ms: u32 = fields.next().and_then(|ms| ms.parse().ok()).unwrap_or(0);
    let duration_ms: u32 = fields.next().and_then(|ms| ms.parse().ok()).unwrap_or(0);
    let clock = |ms: u32| format!("{}:{:02}", ms / 60_000, ms % 60_000 / 1000);
    match state {
        "playing" | "paused" => {
            let mark = if state == "playing" { "▶" } else { "⏸" };
            format!(
                "{mark} {title} — {artists}  [{} / {}]",
                clock(position_ms),
                clock(duration_ms)
            )
        }
        _ => "Nothing playing".to_owned(),
    }
}

/// The `devices` snapshot as one line per device, the active one marked.
/// The id comes first because `spotifast transfer` is what it is for.
#[cfg(not(target_os = "linux"))]
fn format_devices(snapshot: &str) -> String {
    let Ok(devices) = serde_json::from_str::<Vec<serde_json::Value>>(snapshot) else {
        return String::new();
    };
    let field =
        |device: &serde_json::Value, key: &str| device[key].as_str().unwrap_or_default().to_owned();
    devices
        .iter()
        .map(|device| {
            format!(
                "{}{}\t{}\t{}\n",
                if device["active"].as_bool().unwrap_or(false) {
                    "* "
                } else {
                    "  "
                },
                field(device, "id"),
                field(device, "name"),
                field(device, "kind"),
            )
        })
        .collect()
}

pub(crate) fn run() -> eframe::Result<()> {
    #[cfg(target_os = "linux")]
    configure_pulseaudio_properties();
    // First of all: `--apply-update <job>` makes this process the update
    // helper, which installs and exits; otherwise the receipt and error an
    // update relaunch carries are taken out of the arguments.
    let launch = fastframe_update::intercept(&spotifast::updates::CONFIG);
    // A MilkDrop child launch is a bare visualiser window, not the app: it has
    // its own event loop and OpenGL context, reads the sound from a shared
    // buffer, and never touches the app's state. Handle it before anything
    // else, including the argument parser, which does not know its flags.
    #[cfg(feature = "milkdrop")]
    if let Some(args) = spotifast::milkdrop::child::Args::parse() {
        std::process::exit(spotifast::milkdrop::child::run(args));
    }

    // Follow the invoked command, including the Linux package's spotifast
    // symlink. Old updaters execute a file named fastpotify and require its
    // original --version output; both commands otherwise start the same app.
    let name = match launch
        .arguments
        .first()
        .and_then(|arg| std::path::Path::new(arg).file_stem())
        .and_then(|arg| arg.to_str())
    {
        Some(name) if name.eq_ignore_ascii_case("fastpotify") => "fastpotify",
        _ => "spotifast",
    };
    let cli = Cli::from_arg_matches(
        &Cli::command()
            .name(name)
            .get_matches_from(&launch.arguments),
    )
    .unwrap_or_else(|error| error.exit());
    // Demo mode invents plays, settings, and a signed-in account. Without a
    // folder of its own it would write them into the real profile, where
    // they would pass for the user's history and stop the next real launch
    // from moving an older profile across.
    #[cfg(feature = "demo")]
    let cli = if (cli.demo || cli.demo_shot.is_some()) && cli.demo_data.is_none() {
        Cli {
            demo_data: Some(
                std::env::temp_dir().join(format!("spotifast-demo-{}", std::process::id())),
            ),
            ..cli
        }
    } else {
        cli
    };
    // A control launch is a client, not a second app: talk to the running
    // instance and exit before touching the log file it is writing to.
    if let Some(control) = cli.control {
        std::process::exit(run_control(control));
    }
    // A link is read before anything starts: one that is not a Spotify
    // link ends the launch here rather than reaching the running instance.
    let link = cli
        .link
        .as_deref()
        .map(|text| match spotifast::link::parse(text) {
            Some(uri) => uri,
            None => {
                eprintln!("not a Spotify link: {text}");
                std::process::exit(2);
            }
        });
    // The application (audio engine, Web API, MPRIS, tray) outlives any
    // window. Closing to the tray destroys the window and the shell below
    // creates a new one when the tray or MPRIS asks for it. Plain window
    // lifecycle, portable across desktops.
    let waker = backend::Waker::default();

    // A second launch surfaces the instance already running instead of
    // starting a rival one. Held for the lifetime of the process.
    #[cfg(feature = "demo")]
    let demo = cli.demo || cli.demo_shot.is_some();
    #[cfg(feature = "demo")]
    let guarded = !demo;
    #[cfg(not(feature = "demo"))]
    let guarded = true;
    let instance = if guarded {
        match single_instance::acquire(&waker, link.as_deref()) {
            single_instance::Outcome::Only(guard) => Some(guard),
            single_instance::Outcome::Surfaced => {
                log::info!("Spotifast is already running; asked it to show its window");
                return Ok(());
            }
        }
    } else {
        None
    };
    #[cfg(feature = "demo")]
    let migrate = guarded && cli.demo_data.is_none() && launch.receipt.is_none();
    #[cfg(not(feature = "demo"))]
    let migrate = guarded && launch.receipt.is_none();
    if migrate {
        paths::AppDirs::discover()
            .migrate_legacy()
            .map_err(|error| eframe::Error::AppCreation(Box::new(error)))?;
        if let (Some(old), Some(new)) = (
            eframe::storage_dir("fastpotify"),
            eframe::storage_dir("spotifast"),
        ) {
            paths::migrate_directory(&old, &new)
                .map_err(|error| eframe::Error::AppCreation(Box::new(error)))?;
        }
    }
    let default_filter = if cli.verbose {
        "info,librespot=info,spotifast=debug"
    } else {
        // Which system face draws each script the interface font lacks,
        // one line per script at startup, for reports of odd-looking text.
        "warn,spotifast=info,fastframe_fonts=info"
    };
    let dirs = paths::AppDirs::for_launch(launch.receipt.is_some());
    #[cfg(feature = "demo")]
    let dirs = cli
        .demo_data
        .as_ref()
        .map(|base| paths::AppDirs {
            config: base.join("config"),
            state: base.join("state"),
            cache: base.join("cache"),
        })
        .unwrap_or(dirs);
    let dirs_ready = dirs.ensure();
    // Launched from a desktop, stderr goes nowhere; keep the run's log where
    // a bug report can find it, and a line per panic in the panic log (with
    // any link in its message removed: a URL can carry a token).
    if let Err(error) = fastframe_log::Logging::new("spotifast", env!("CARGO_PKG_VERSION"))
        .filter(default_filter)
        .file(dirs.log_file())
        .panic_log(dirs.panic_log())
        .panic_message(fastframe_log::PanicMessage::Redacted(
            fastframe_log::redact::links,
        ))
        .init()
    {
        eprintln!("not logging: {error}");
    }
    if let Err(error) = dirs_ready {
        log::warn!("unable to create the application directories: {error}");
    }
    let mut settings = settings::Settings::load(&dirs.settings_file());
    if let Some(name) = cli.device_name {
        settings.device_name = name;
    }
    spotifast::window::set_custom_titlebar(settings.custom_titlebar);
    // macOS delivers links as Apple Events; install before the event loop.
    #[cfg(target_os = "macos")]
    if let Some(guard) = &instance {
        spotifast::mac_links::install(guard.commands(), waker.clone());
    }

    // A capture run is a throwaway process next to the real one: no tray
    // icon of its own, and no second MPRIS service to fight over media keys.
    #[allow(unused_mut)]
    let mut options = app::AppOptions::default();
    #[cfg(feature = "demo")]
    if demo {
        options.restore_sign_in = false;
    }
    #[cfg(feature = "demo")]
    if cli.demo_shot.is_some() {
        options = app::AppOptions {
            media_controls: false,
            restore_sign_in: false,
            tray: false,
        };
    }
    #[cfg(windows)]
    let desktop_surfaces = options.media_controls;
    #[allow(unused_mut)]
    let mut app = app::App::new(&waker, dirs, settings, options);
    if guarded {
        app.enable_desktop_themes();
    }
    #[cfg(feature = "demo")]
    let load_themes = guarded || cli.demo_data.is_some();
    #[cfg(not(feature = "demo"))]
    let load_themes = true;
    if load_themes {
        app.load_custom_themes(&waker);
    }
    app.update_receipt = launch.receipt;
    if let Some(error) = launch.error {
        app.report_update_failure(error);
    }
    if let Some(guard) = &instance {
        app.set_remote_control(guard);
    }
    if let Some(uri) = link {
        app.open_link(uri);
    }
    #[cfg(feature = "demo")]
    if demo {
        spotifast::demo::populate(&mut app);
        spotifast::demo::apply_flags(&mut app, cli.demo_page.as_deref(), cli.demo_show.as_deref());
        if let Some(feed) = &cli.demo_update_feed {
            match fastframe_update::Source::local(feed) {
                Ok(source) => app.update_source = source,
                Err(error) => {
                    eprintln!("{error:#}");
                    std::process::exit(2);
                }
            }
            app.update_restart_arguments =
                vec!["--demo".into(), "--demo-page".into(), "settings".into()];
            if let Some(base) = &cli.demo_data {
                app.update_restart_arguments
                    .extend(["--demo-data".into(), base.to_string_lossy().into_owned()]);
            }
            app.actions.push(spotifast::model::Action::CheckForUpdates);
        }
        if let Some(locale) = cli.demo_language {
            app.settings.language = spotifast::settings::LanguageChoice::Locale(locale);
            app.locale = locale;
        }
    }
    #[cfg(feature = "demo")]
    let shot = cli.demo_shot.clone().map(|path| Shot {
        path,
        due: std::time::Instant::now() + std::time::Duration::from_millis(cli.demo_shot_delay),
        asked: false,
    });
    #[cfg(feature = "demo")]
    let demo_drag = cli
        .demo_drag
        .map(|[from, to]| DemoDrag { from, to, frame: 0 });
    #[cfg(feature = "demo")]
    let demo_inner = cli.demo_size;
    #[cfg(feature = "demo")]
    let demo_storage = app.dirs.cache.join("demo-window.ron");
    let window_profile = app.dirs.window_profile();
    fastframe_shell::Shell::new(app, &waker)
        .idle(fastframe_tray::idle)
        .run(|lease| {
            #[cfg(windows)]
            let creator_waker = waker.clone();
            #[cfg(feature = "demo")]
            let creator_shot = shot.clone();
            let mini = lease.peek(MiniWindow::wanted);
            #[cfg(feature = "demo")]
            let options = {
                let options = native_options(
                    shot.is_some() && mini.is_none() && demo_inner.is_none(),
                    mini,
                    demo_inner,
                );
                if demo {
                    demo_native_options(options, demo_storage.clone())
                } else {
                    options
                }
            };
            #[cfg(not(feature = "demo"))]
            let options = native_options(false, mini, None);
            let options = profile_options(options, window_profile);
            let persist_memory = options.persist_window;
            #[cfg(windows)]
            let thumbbar_enabled = desktop_surfaces && options.viewport.taskbar != Some(false);
            #[cfg(target_os = "linux")]
            let hide_from_taskbar = options.viewport.taskbar == Some(false);
            eframe::run_native(
                "Spotifast",
                options,
                Box::new(move |cc| {
                    if let Some(gl) = &cc.gl {
                        use eframe::glow::HasContext;
                        // eframe has made this window's GL context current
                        // before calling the app creator. These identify the
                        // renderer actually selected, which may differ from
                        // the listed GPU.
                        unsafe {
                            log::info!(
                                "OpenGL renderer: {}; vendor: {}; version: {}",
                                gl.get_parameter_string(eframe::glow::RENDERER),
                                gl.get_parameter_string(eframe::glow::VENDOR),
                                gl.get_parameter_string(eframe::glow::VERSION)
                            );
                        }
                    }
                    let mut app = lease.take(&cc.egui_ctx);
                    // Built once per window, before the first frame; the
                    // handler wakes the loop so a menu pick is not held until
                    // the next repaint.
                    #[cfg(target_os = "macos")]
                    {
                        spotifast::mac_touchbar_crash_guard::install();
                        spotifast::mac_menu::init();
                        let ctx = cc.egui_ctx.clone();
                        spotifast::mac_menu::set_waker(move || ctx.request_repaint());
                    }
                    {
                        use raw_window_handle::HasDisplayHandle;
                        if let Ok(display) = cc.display_handle() {
                            app.window_level_supported =
                                spotifast::window::supports_window_level(display.as_raw());
                            app.taskbar_hiding_supported =
                                spotifast::window::supports_hiding_from_taskbar(display.as_raw());
                        }
                    }
                    // winit hides a taskbar button on Windows only; X11 is
                    // asked here, while the window is still unmapped.
                    #[cfg(target_os = "linux")]
                    if hide_from_taskbar {
                        use raw_window_handle::HasWindowHandle;
                        if let Ok(handle) = cc.window_handle() {
                            spotifast::window::skip_x11_taskbar(handle.as_raw());
                        }
                    }
                    app.attach(&cc.egui_ctx);
                    #[cfg(windows)]
                    let thumbbar = {
                        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
                        let mut toolbar = spotifast::thumbbar::ThumbBar::new();
                        if thumbbar_enabled
                            && let Ok(handle) = cc.window_handle()
                            && let RawWindowHandle::Win32(window) = handle.as_raw()
                        {
                            let wake = creator_waker.clone();
                            // The shell and toolbar share this window's thread
                            // and lifetime; the toolbar is detached on shell
                            // drop.
                            unsafe { toolbar.attach(window.hwnd.get(), move || wake.wake()) };
                        }
                        toolbar
                    };
                    Ok(Box::new(Shell {
                        app,
                        persist_memory,
                        #[cfg(windows)]
                        thumbbar,
                        #[cfg(feature = "demo")]
                        shot: creator_shot.clone(),
                        #[cfg(feature = "demo")]
                        drag: demo_drag,
                    }))
                }),
            )
            .inspect_err(|error| log::error!("Native window failed: {error}"))
        })
}

/// The Winamp mini player's window, when that is the window to open.
struct MiniWindow {
    /// A first size; the window corrects it once it knows the display.
    size: egui::Vec2,
    position: Option<[f32; 2]>,
    on_top: bool,
    taskbar: bool,
    storage_path: std::path::PathBuf,
}

impl MiniWindow {
    fn wanted(app: &app::App) -> Option<Self> {
        app.settings.winamp_window.then(|| Self {
            size: spotifast::ui::winamp::initial_size(&app.settings),
            position: app.winamp.restore_pos,
            on_top: app.settings.winamp_on_top,
            taskbar: app.settings.winamp_show_taskbar,
            storage_path: app.dirs.cache.join("winamp.ron"),
        })
    }
}

const fn main_window_decorated(custom_titlebar: bool) -> bool {
    !custom_titlebar
}

#[cfg(any(test, feature = "demo"))]
fn parse_demo_size(spec: &str) -> Result<[f32; 2], String> {
    let (width, height) = spec
        .split_once(['x', 'X'])
        .ok_or_else(|| format!("expected WIDTHxHEIGHT, got {spec}"))?;
    let width: f32 = width
        .trim()
        .parse()
        .map_err(|_| format!("invalid width in {spec}"))?;
    let height: f32 = height
        .trim()
        .parse()
        .map_err(|_| format!("invalid height in {spec}"))?;
    if width < 1.0 || height < 1.0 {
        return Err(format!("size must be at least 1x1, got {spec}"));
    }
    Ok([width, height])
}

#[cfg(feature = "demo")]
fn parse_demo_drag(spec: &str) -> Result<[egui::Pos2; 2], String> {
    let point = |point: &str| -> Result<egui::Pos2, String> {
        let (x, y) = point
            .split_once(',')
            .ok_or_else(|| format!("expected X,Y, got {point}"))?;
        let x: f32 = x
            .trim()
            .parse()
            .map_err(|_| format!("invalid x in {point}"))?;
        let y: f32 = y
            .trim()
            .parse()
            .map_err(|_| format!("invalid y in {point}"))?;
        Ok(egui::pos2(x, y))
    };
    let (from, to) = spec
        .split_once(':')
        .ok_or_else(|| format!("expected X,Y:X,Y, got {spec}"))?;
    Ok([point(from)?, point(to)?])
}

// Windows can stop drawing a window created outside the current monitors.
// App::attach restores the saved position only once the native scale is known
// and window::can_restore has checked its title bar against a live work area.
fn mini_creation_position(position: Option<[f32; 2]>, on_windows: bool) -> Option<[f32; 2]> {
    if on_windows { None } else { position }
}

fn native_options(
    fullscreen: bool,
    mini: Option<MiniWindow>,
    inner_size: Option<[f32; 2]>,
) -> eframe::NativeOptions {
    // The app keeps the mini player's position and shaded size separately.
    // Its closing window must not replace the main window's eframe geometry.
    let persist_window = mini.is_none();
    // Disabling saving does not disable eframe's startup restore. Give the
    // mini player its own path, and Shell disables its egui-memory saving too,
    // so it neither reads the main window's geometry nor creates a state file.
    let persistence_path = mini.as_ref().map(|mini| mini.storage_path.clone());
    #[cfg(target_os = "linux")]
    let persistence_path = persistence_path.or_else(|| {
        // Keep the native profile path even when Flatpak supplies its app ID.
        eframe::storage_dir("spotifast").map(|dir| dir.join("app.ron"))
    });
    #[cfg(target_os = "linux")]
    let app_id = spotifast::media_controls::desktop_entry();
    #[cfg(not(target_os = "linux"))]
    let app_id = "spotifast";
    let icon = if cfg!(target_os = "macos") {
        // macOS takes the dock icon from the bundle's .icns, which is the
        // 1024px drawing with the platform's rounding. Setting a window
        // icon there replaces it with this flat 128px square.
        egui::IconData::default()
    } else {
        app_icon()
    };
    let viewport = egui::ViewportBuilder::default()
        .with_title("Spotifast")
        .with_app_id(app_id)
        .with_taskbar(true)
        .with_icon(icon);
    let viewport = match mini {
        Some(mini) => {
            let level = app::on_top_window_level(mini.on_top);
            // See-through, for skins that are not rectangles; the skin
            // paints every pixel that is the window. MilkDrop runs in its own
            // process, so nothing else shares this window's surface.
            let viewport = viewport
                .with_decorations(false)
                .with_transparent(true)
                .with_resizable(false)
                .with_maximize_button(false)
                .with_inner_size(mini.size)
                .with_min_inner_size(mini.size)
                .with_max_inner_size(mini.size)
                .with_window_level(level);
            // egui applies this native attribute on Windows only; the app
            // creator asks X11 itself (window::skip_x11_taskbar).
            let viewport = viewport.with_taskbar(mini.taskbar);
            match mini_creation_position(mini.position, cfg!(windows)) {
                Some([x, y]) => viewport.with_position([x, y]),
                None => viewport,
            }
        }
        None => {
            let size = inner_size.unwrap_or([1240.0, 800.0]);
            let mut viewport = viewport
                // macOS: no title bar strip above the app. The content runs to
                // the top edge and the traffic lights float over it, the way
                // every other music player on the platform looks; the interface
                // leaves room for them with `theme::titlebar_inset`.
                .with_fullsize_content_view(true)
                .with_titlebar_shown(false)
                .with_title_shown(false)
                // Windows has no equivalent to macOS's floating traffic lights.
                // Removing its decorations lets the app surface fill the window.
                .with_decorations(main_window_decorated(spotifast::window::custom_titlebar()))
                .with_inner_size(size)
                .with_min_inner_size(inner_size.unwrap_or([760.0, 520.0]))
                .with_fullscreen(fullscreen);
            if inner_size.is_some() {
                viewport = viewport.with_max_inner_size(size);
            }
            viewport
        }
    };
    eframe::NativeOptions {
        viewport,
        persist_window,
        persistence_path,
        ..Default::default()
    }
}

fn profile_options(mut options: eframe::NativeOptions, profile: &str) -> eframe::NativeOptions {
    if options.persist_window {
        options.persistence_path = eframe::storage_dir(profile).map(|dir| dir.join("app.ron"));
    }
    options
}

#[cfg(any(test, feature = "demo"))]
fn demo_native_options(
    mut options: eframe::NativeOptions,
    storage: std::path::PathBuf,
) -> eframe::NativeOptions {
    // Saving and loading are separate in eframe. An unused, unsaved path
    // prevents the normal profile's geometry and zoom from entering a demo.
    options.persistence_path = Some(storage);
    options.persist_window = false;
    options
}

#[cfg(test)]
mod native_window_tests {
    use super::*;

    #[test]
    fn updater_trial_keeps_previous_geometry_without_touching_demo_storage() {
        for profile in ["fastpotify", "spotifast"] {
            let main = profile_options(native_options(false, None, None), profile);
            assert_eq!(
                main.persistence_path,
                eframe::storage_dir(profile).map(|dir| dir.join("app.ron"))
            );
            let demo_path = std::path::PathBuf::from("temporary/demo.ron");
            let demo = profile_options(
                demo_native_options(native_options(false, None, None), demo_path.clone()),
                profile,
            );
            assert_eq!(demo.persistence_path, Some(demo_path));
        }
    }

    #[test]
    fn launcher_identity_preserves_main_and_mini_storage() {
        let main = native_options(false, None, None);
        let mini_path = std::path::PathBuf::from("cache/winamp.ron");
        let mini = native_options(
            false,
            Some(MiniWindow {
                size: egui::vec2(550.0, 232.0),
                position: None,
                on_top: false,
                taskbar: true,
                storage_path: mini_path.clone(),
            }),
            None,
        );
        #[cfg(target_os = "linux")]
        {
            let id = spotifast::media_controls::desktop_entry();
            assert_eq!(main.viewport.app_id.as_deref(), Some(id.as_str()));
            assert_eq!(mini.viewport.app_id, main.viewport.app_id);
            assert_eq!(
                main.persistence_path,
                eframe::storage_dir("spotifast").map(|dir| dir.join("app.ron"))
            );
        }
        #[cfg(not(target_os = "linux"))]
        {
            assert_eq!(main.viewport.app_id.as_deref(), Some("spotifast"));
            assert_eq!(mini.viewport.app_id, main.viewport.app_id);
            assert_eq!(main.persistence_path, None);
        }
        assert_eq!(mini.persistence_path, Some(mini_path));
        assert!(main.persist_window);
        assert!(!mini.persist_window);
    }

    #[test]
    fn windows_never_creates_the_mini_player_at_an_unchecked_saved_position() {
        for position in [Some([3560.0, 908.0]), Some([-1920.0, 100.0]), None] {
            assert_eq!(mini_creation_position(position, true), None);
            assert_eq!(mini_creation_position(position, false), position);
        }
    }

    #[test]
    fn only_the_main_window_persists_framework_geometry() {
        assert!(native_options(false, None, None).persist_window);
        for shaded in [false, true] {
            let settings = settings::Settings {
                winamp_shaded: shaded,
                skin_scale: Some(2),
                ..Default::default()
            };
            let size = spotifast::ui::winamp::initial_size(&settings);
            let options = native_options(
                false,
                Some(MiniWindow {
                    size,
                    position: Some([300.0, 200.0]),
                    on_top: false,
                    taskbar: true,
                    storage_path: std::path::PathBuf::from("cache/winamp.ron"),
                }),
                None,
            );
            assert!(
                !options.persist_window,
                "mini geometry must not overwrite main"
            );
            assert_eq!(options.viewport.inner_size, Some(size));
            assert_eq!(
                options.viewport.position,
                mini_creation_position(Some([300.0, 200.0]), cfg!(windows)).map(egui::Pos2::from)
            );
            assert!(options.persistence_path.is_some());
        }
    }

    #[test]
    fn main_window_uses_the_platform_decoration_policy() {
        let options = native_options(false, None, None);
        assert_eq!(
            options.viewport.decorations,
            Some(!spotifast::window::custom_titlebar())
        );
        assert_eq!(options.viewport.fullsize_content_view, Some(true));
        assert_eq!(options.viewport.titlebar_shown, Some(false));
        assert_eq!(options.viewport.title_shown, Some(false));
    }

    /// Both windows ask for vsync: AppKit resize animations need it, and on
    /// Wayland eframe paces frames by the compositor's frame callbacks
    /// instead of waiting in the swap, so a hidden window cannot block (#266).
    #[test]
    fn windows_wait_for_vsync() {
        assert!(native_options(false, None, None).glow_options.vsync);
        let mini = MiniWindow {
            size: egui::vec2(550.0, 232.0),
            position: None,
            on_top: false,
            taskbar: true,
            storage_path: "cache/winamp.ron".into(),
        };
        assert!(native_options(false, Some(mini), None).glow_options.vsync);
    }

    #[test]
    fn hiding_the_mini_taskbar_button_never_hides_the_main_window_button() {
        for taskbar in [false, true] {
            let mini = MiniWindow {
                size: egui::vec2(550.0, 232.0),
                position: Some([123.0, 456.0]),
                on_top: true,
                taskbar,
                storage_path: "cache/winamp.ron".into(),
            };
            let options = native_options(false, Some(mini), None);
            assert_eq!(options.viewport.taskbar, Some(taskbar));
            assert_eq!(
                options.viewport.position,
                mini_creation_position(Some([123.0, 456.0]), cfg!(windows)).map(egui::Pos2::from)
            );
            assert_eq!(options.viewport.inner_size, Some(egui::vec2(550.0, 232.0)));
            assert_eq!(
                options.viewport.window_level,
                Some(egui::WindowLevel::AlwaysOnTop)
            );
            assert_eq!(
                native_options(false, None, None).viewport.taskbar,
                Some(true)
            );
        }
    }

    #[cfg(feature = "demo")]
    #[test]
    fn demo_drag_presses_at_the_first_point_and_holds_at_the_second() {
        let [from, to] = parse_demo_drag("10,20:110, 220").unwrap();
        assert_eq!(
            (from, to),
            (egui::pos2(10.0, 20.0), egui::pos2(110.0, 220.0))
        );
        assert!(parse_demo_drag("10,20").is_err());
        assert!(parse_demo_drag("10:20").is_err());
        let mut drag = DemoDrag { from, to, frame: 0 };
        let frames: Vec<_> = (0..=DemoDrag::REST + DemoDrag::GLIDE + 5)
            .map(|_| drag.events())
            .collect();
        let presses = frames
            .iter()
            .flatten()
            .filter(|event| matches!(event, egui::Event::PointerButton { pressed: true, .. }))
            .count();
        assert_eq!(presses, 1, "one press, never a release");
        assert!(
            !frames
                .iter()
                .flatten()
                .any(|event| matches!(event, egui::Event::PointerButton { pressed: false, .. }))
        );
        assert_eq!(frames[0][0], egui::Event::PointerMoved(from));
        assert_eq!(frames.last().unwrap()[0], egui::Event::PointerMoved(to));
    }

    #[test]
    fn demo_size_parses_width_by_height() {
        assert_eq!(parse_demo_size("760x800").unwrap(), [760.0, 800.0]);
        assert!(parse_demo_size("wide").is_err());
        let options = native_options(false, None, Some([760.0, 800.0]));
        assert_eq!(options.viewport.inner_size, Some(egui::vec2(760.0, 800.0)));
        assert_eq!(
            options.viewport.min_inner_size,
            Some(egui::vec2(760.0, 800.0))
        );
        assert_eq!(
            options.viewport.max_inner_size,
            Some(egui::vec2(760.0, 800.0))
        );
    }

    #[test]
    fn demo_window_storage_is_separate_and_never_saved() {
        let cache = std::path::PathBuf::from("isolated-demo/cache");
        for mini in [
            None,
            Some(MiniWindow {
                size: egui::vec2(550.0, 232.0),
                position: None,
                on_top: false,
                taskbar: true,
                storage_path: cache.join("winamp.ron"),
            }),
        ] {
            let options = demo_native_options(
                native_options(false, mini, Some([760.0, 520.0])),
                cache.join("demo-window.ron"),
            );
            assert_eq!(
                options.persistence_path,
                Some(cache.join("demo-window.ron"))
            );
            assert!(!options.persist_window);
            // Shell keeps this policy in its own field, so it holds even
            // after on_exit has handed the App back.
        }
    }

    #[test]
    fn only_windows_removes_the_native_frame() {
        assert!(!main_window_decorated(true));
        assert!(main_window_decorated(false));
    }
}

/// The eframe adapter around the long-lived [`app::App`]: delegates frames
/// and, when the window goes away, hands the state back for the next window.
struct Shell {
    app: fastframe_shell::Held<app::App>,
    /// Keep demo and mini-window memory out of the normal profile, including
    /// after on_exit has returned the App to the event loop.
    persist_memory: bool,
    #[cfg(windows)]
    thumbbar: spotifast::thumbbar::ThumbBar,
    /// A pending `--demo-shot` capture, if this is a screenshot run.
    #[cfg(feature = "demo")]
    shot: Option<Shot>,
    /// A drag held for `--demo-drag`, if this run shows one.
    #[cfg(feature = "demo")]
    drag: Option<DemoDrag>,
}

/// A scripted pointer for `--demo-drag`: rest on `from`, press there, glide
/// to `to` and hold, so the shot shows a drag in progress through the same
/// code a real pointer runs.
#[cfg(feature = "demo")]
#[derive(Clone, Copy)]
struct DemoDrag {
    from: egui::Pos2,
    to: egui::Pos2,
    frame: u32,
}

#[cfg(feature = "demo")]
impl DemoDrag {
    /// Frames to rest before pressing, so the rows under `from` are laid out.
    const REST: u32 = 20;
    /// Frames the glide from `from` to `to` takes.
    const GLIDE: u32 = 20;

    fn events(&mut self) -> Vec<egui::Event> {
        let frame = self.frame;
        self.frame = self.frame.saturating_add(1);
        let pos = if frame <= Self::REST {
            self.from
        } else {
            let t = ((frame - Self::REST) as f32 / Self::GLIDE as f32).min(1.0);
            self.from.lerp(self.to, t)
        };
        let mut events = vec![egui::Event::PointerMoved(pos)];
        if frame == Self::REST {
            events.push(egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            });
        }
        events
    }
}

/// A screenshot the window still owes us.
///
/// Cover art arrives over the network, so the capture waits for `due` before
/// asking egui for the frame buffer. The image comes back as an input event
/// on a later frame, which is where it gets written and the window closed.
#[cfg(feature = "demo")]
#[derive(Clone)]
struct Shot {
    path: std::path::PathBuf,
    due: std::time::Instant,
    asked: bool,
}

#[cfg(feature = "demo")]
impl Shell {
    fn drive_shot(&mut self, ctx: &egui::Context) {
        let Some(shot) = self.shot.as_mut() else {
            return;
        };

        // Nothing here is driven by user input, so the frames have to be
        // asked for: art still has to load and the request has to be issued.
        ctx.request_repaint();

        if !shot.asked && std::time::Instant::now() >= shot.due {
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
            shot.asked = true;
        }

        let image = ctx.input(|input| {
            input.events.iter().find_map(|event| match event {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        let Some(image) = image else {
            return;
        };

        let [width, height] = [image.size[0] as u32, image.size[1] as u32];
        let pixels: Vec<u8> = image
            .pixels
            .iter()
            .flat_map(|pixel| pixel.to_srgba_unmultiplied())
            .collect();
        match image::RgbaImage::from_raw(width, height, pixels) {
            Some(buffer) => match buffer.save(&shot.path) {
                Ok(()) => log::info!("wrote {}x{} to {}", width, height, shot.path.display()),
                Err(error) => log::error!("could not write {}: {error}", shot.path.display()),
            },
            None => log::error!("the frame buffer did not match {width}x{height}"),
        }
        self.shot = None;
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }
}

impl eframe::App for Shell {
    fn persist_egui_memory(&self) -> bool {
        self.persist_memory
    }

    #[cfg(feature = "demo")]
    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        if let Some(drag) = self.drag.as_mut() {
            // The script owns the pointer: drop whatever the real one did.
            raw_input.events.retain(|event| {
                !matches!(
                    event,
                    egui::Event::PointerMoved(_)
                        | egui::Event::PointerButton { .. }
                        | egui::Event::PointerGone
                )
            });
            raw_input.events.extend(drag.events());
        }
    }

    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let app = &mut *self.app;
        #[cfg(target_os = "macos")]
        for command in spotifast::mac_menu::drain_commands() {
            use spotifast::mac_menu::MenuCommand;
            use spotifast::model::{Action, Dialog, Page};
            let action = match command {
                MenuCommand::PlayPause => Action::TogglePlay,
                MenuCommand::Next => Action::Next,
                MenuCommand::Previous => Action::Previous,
                MenuCommand::SeekForward => Action::SeekBy(10_000),
                MenuCommand::SeekBackward => Action::SeekBy(-10_000),
                MenuCommand::ToggleShuffle => Action::ToggleShuffle,
                MenuCommand::CycleRepeat => Action::CycleRepeat,
                MenuCommand::VolumeUp => Action::VolumeBy(5),
                MenuCommand::VolumeDown => Action::VolumeBy(-5),
                MenuCommand::ToggleMute => Action::ToggleMute,
                MenuCommand::Home => Action::Open(Page::Home),
                MenuCommand::Search => Action::FocusSearch,
                MenuCommand::LikedSongs => Action::Open(Page::LikedSongs),
                MenuCommand::Sidebar => Action::ToggleSidebar,
                MenuCommand::Queue => Action::ToggleQueuePanel,
                MenuCommand::Settings => Action::Open(Page::Settings),
                MenuCommand::CheckForUpdates => Action::CheckForUpdates,
                MenuCommand::Shortcuts => Action::ShowDialog(Dialog::Shortcuts),
                MenuCommand::Back => Action::Back,
                MenuCommand::Forward => Action::Forward,
                MenuCommand::OpenRepo => {
                    ctx.open_url(egui::OpenUrl::new_tab("https://github.com/crmne/spotifast"));
                    continue;
                }
                // Editing goes through egui, which owns the text field
                // and the clipboard.
                MenuCommand::Cut => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::RequestCut);
                    continue;
                }
                MenuCommand::Copy => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::RequestCopy);
                    continue;
                }
                MenuCommand::Paste => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::RequestPaste);
                    continue;
                }
                MenuCommand::SelectAll => {
                    ctx.input_mut(|input| {
                        input.events.push(egui::Event::Key {
                            key: egui::Key::A,
                            physical_key: None,
                            pressed: true,
                            repeat: false,
                            modifiers: egui::Modifiers::COMMAND,
                        });
                    });
                    continue;
                }
            };
            app.actions.push(action);
        }
        #[cfg(windows)]
        for command in self.thumbbar.drain_commands() {
            if let Some(action) = command.action(&app.thumb_state(false)) {
                app.actions.push(action);
            }
        }
        app.background_frame(ctx);
        #[cfg(windows)]
        self.thumbbar
            .sync(app.thumb_state(ctx.system_theme() != Some(egui::Theme::Light)));
        #[cfg(feature = "demo")]
        self.drive_shot(ctx);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let app = &mut *self.app;
        app.frame_ui(ui);
        if let Some(receipt) = app.update_receipt.take() {
            std::thread::spawn(move || {
                if let Err(error) = receipt.acknowledge() {
                    log::error!("Could not confirm the update: {error:#}");
                }
            });
        }
        #[cfg(windows)]
        self.thumbbar
            .sync(app.thumb_state(ui.ctx().system_theme() != Some(egui::Theme::Light)));
    }

    /// The mini player's window is see-through where the skin leaves it
    /// out; the big window paints itself over eframe's own ground.
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        if self.app.settings.winamp_window {
            [0.0; 4]
        } else {
            egui::Color32::from_rgba_unmultiplied(12, 12, 12, 180).to_normalized_gamma_f32()
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        let app = &mut *self.app;
        app.save_state();
    }
}

/// The toolbar goes before the window; the app then returns to the shell
/// with the `Held` field.
#[cfg(windows)]
impl Drop for Shell {
    fn drop(&mut self) {
        self.thumbbar.detach();
    }
}

/// The window icon, from the shared runtime drawing.
fn app_icon() -> egui::IconData {
    const SIZE: usize = 128;
    egui::IconData {
        rgba: util::app_icon_rgba(SIZE),
        width: SIZE as u32,
        height: SIZE as u32,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    fn pulse_metadata_defaults_preserve_existing_properties() {
        let defaults = missing_pulseaudio_properties(|_| false).collect::<Vec<_>>();
        assert_eq!(defaults, PULSEAUDIO_PROPERTIES);

        let defaults = missing_pulseaudio_properties(|key| key == "PULSE_PROP_application.name")
            .collect::<Vec<_>>();
        assert_eq!(
            defaults,
            [("PULSE_PROP_stream.description", "Spotify playback")]
        );

        assert_eq!(missing_pulseaudio_properties(|_| true).count(), 0);
    }

    /// A link on the command line is a link, and a control verb is still a
    /// verb: the two do not get in each other's way.
    #[test]
    fn a_link_and_a_verb_are_told_apart() {
        // #given / #when / #then
        let launch = Cli::try_parse_from(["fastpotify", "spotify:track:4uLU6hMCjMI75M1A2tKUQC"])
            .expect("a link parses");
        assert_eq!(
            launch.link.as_deref(),
            Some("spotify:track:4uLU6hMCjMI75M1A2tKUQC")
        );
        assert!(launch.control.is_none());

        let launch = Cli::try_parse_from([
            "fastpotify",
            "https://open.spotify.com/album/1DFixLWuPkv3KT3TnV35m3?si=x",
            "--verbose",
        ])
        .expect("a web address parses");
        assert!(launch.link.is_some());
        assert!(launch.verbose);

        let verb = Cli::try_parse_from(["fastpotify", "next"]).expect("a verb parses");
        assert!(matches!(verb.control, Some(Control::Next)));
        assert!(verb.link.is_none());

        let bare = Cli::try_parse_from(["fastpotify"]).expect("a plain launch parses");
        assert!(bare.link.is_none() && bare.control.is_none());
    }
}
