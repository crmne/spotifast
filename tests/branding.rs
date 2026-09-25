use std::path::PathBuf;
use std::process::Command;

const COMMANDS: [(&str, &str); 2] = [
    ("spotifast", env!("CARGO_BIN_EXE_spotifast")),
    ("fastpotify", env!("CARGO_BIN_EXE_fastpotify")),
];

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("spotifast-rename-{:016x}", rand::random::<u64>()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn both_commands_report_their_name_and_pass_the_update_version_check() {
    for (name, binary) in COMMANDS {
        let output = Command::new(binary).arg("--version").output().unwrap();
        assert!(output.status.success());
        assert_eq!(
            String::from_utf8(output.stdout).unwrap().trim(),
            format!("{name} {}", env!("CARGO_PKG_VERSION"))
        );
        let help = Command::new(binary).arg("--help").output().unwrap();
        assert!(help.status.success());
        assert!(
            String::from_utf8(help.stdout)
                .unwrap()
                .contains(&format!("Usage: {name}"))
        );
        // The updater's version probe accepts the slug or a legacy name.
        let config = spotifast::updates::CONFIG;
        assert!(name == config.slug || config.legacy_names.contains(&name));
    }
}

#[cfg(unix)]
#[test]
fn the_linux_package_alias_reports_spotifast_without_copying_the_app() {
    let scratch = Scratch::new();
    let alias = scratch.0.join("spotifast");
    std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_fastpotify"), &alias).unwrap();
    let output = Command::new(alias).arg("--version").output().unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap().trim(),
        format!("spotifast {}", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn existing_preferences_and_custom_connect_names_survive_the_rename() {
    use spotifast::settings::{Settings, ThemeChoice};

    let scratch = Scratch::new();
    let path = scratch.0.join("settings.json");
    for name in ["Fastpotify", "Living room", "Carmine's laptop"] {
        let saved = Settings {
            device_name: name.into(),
            theme: ThemeChoice::Light,
            volume: 37,
            pinned_contexts: vec![
                "spotify:playlist:123".into(),
                "fastpotify:liked-songs".into(),
            ],
            ..Settings::default()
        };
        saved.save(&path);
        let mut expected = saved;
        if expected.device_name == "Fastpotify" {
            expected.device_name = "Spotifast".into();
        }
        expected.pinned_contexts[1] = spotifast::settings::LIKED_SONGS_KEY.into();
        assert_eq!(Settings::load(&path), expected);
    }
    assert_eq!(Settings::default().device_name, "Spotifast");
}

#[cfg(target_os = "linux")]
#[test]
fn both_commands_forward_links_to_the_existing_instance_on_a_private_bus() {
    use spotifast::single_instance::{ControlCommand, Outcome};

    const CHILD: &str = "SPOTIFAST_RENAME_PRIVATE_BUS";
    if std::env::var_os(CHILD).is_none() {
        // A clean build (including Nix) need not have /etc/dbus-1/session.conf.
        // Own the bus configuration too, without loading desktop services.
        let scratch = Scratch::new();
        let config = scratch.0.join("session.conf");
        std::fs::write(
            &config,
            r#"<busconfig>
  <type>session</type>
  <listen>unix:tmpdir=/tmp</listen>
  <auth>EXTERNAL</auth>
  <policy context="default">
    <allow own="*"/>
    <allow send_destination="*"/>
    <allow receive_sender="*"/>
  </policy>
</busconfig>"#,
        )
        .unwrap();
        let result = Command::new("dbus-run-session")
            .arg("--config-file")
            .arg(config)
            .args(["--"])
            .arg(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "both_commands_forward_links_to_the_existing_instance_on_a_private_bus",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .output()
            .expect("the Linux test environment needs dbus-run-session");
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        return;
    }

    let scratch = Scratch::new();
    for (_, binary) in COMMANDS {
        let result = Command::new(binary).arg("reload-themes").output().unwrap();
        assert_eq!(result.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&result.stderr).contains("not running"));
        let result = Command::new(binary).arg("like").output().unwrap();
        assert_eq!(result.status.code(), Some(1));
        assert!(
            String::from_utf8_lossy(&result.stderr).contains("not running"),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    let Outcome::Only(guard) = spotifast::single_instance::acquire(&Default::default(), None)
    else {
        panic!("the private bus must start without another instance");
    };
    let commands = guard.commands();
    for (_, binary) in COMMANDS {
        for (link, uri) in [
            (
                "spotify:track:4uLU6hMCjMI75M1A2tKUQC",
                "spotify:track:4uLU6hMCjMI75M1A2tKUQC",
            ),
            (
                "https://open.spotify.com/search/here%20comes%20the%20sun",
                "spotify:search:here%20comes%20the%20sun",
            ),
            (
                "spotify://search/%E6%9D%B1%E4%BA%AC",
                "spotify:search:%E6%9D%B1%E4%BA%AC",
            ),
        ] {
            let mut child = Command::new(binary)
                .arg(link)
                .env("XDG_CONFIG_HOME", scratch.0.join("config"))
                .env("XDG_STATE_HOME", scratch.0.join("state"))
                .env("XDG_CACHE_HOME", scratch.0.join("cache"))
                .spawn()
                .unwrap();
            let started = std::time::Instant::now();
            let status = loop {
                if let Some(status) = child.try_wait().unwrap() {
                    break status;
                }
                if started.elapsed() > std::time::Duration::from_secs(10) {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!("a second command did not forward its link and exit");
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            };
            assert!(status.success());
            assert_eq!(
                std::mem::take(&mut *commands.lock().unwrap()),
                vec![ControlCommand::OpenLink(uri.into())]
            );
        }
        let result = Command::new(binary).arg("reload-themes").output().unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert_eq!(
            std::mem::take(&mut *commands.lock().unwrap()),
            vec![ControlCommand::ReloadThemes],
            "a reload only asks for themes, never OpenLink or Show"
        );
        let result = Command::new(binary).arg("like").output().unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert_eq!(
            std::mem::take(&mut *commands.lock().unwrap()),
            vec![ControlCommand::ToggleSaved],
            "like only toggles the saved state, never OpenLink or Show"
        );
    }
}
