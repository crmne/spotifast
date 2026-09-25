//! The tray on a private bus shaped like Flatpak's: the item must register
//! without owning a name, and a click must reach the app.
#![cfg(target_os = "linux")]

use std::io::BufRead;
use std::process::{Command, Stdio};
use std::sync::mpsc::Sender;
use std::time::Duration;

struct Watcher(Sender<String>);

#[zbus::interface(name = "org.kde.StatusNotifierWatcher")]
impl Watcher {
    fn register_status_notifier_item(&self, service: &str) {
        self.0.send(service.to_string()).expect("registration");
    }

    #[zbus(property)]
    fn is_status_notifier_host_registered(&self) -> bool {
        true
    }
}

fn config() -> fastframe_tray::Config {
    fastframe_tray::Config {
        id: "spotifast",
        title: "Spotifast".into(),
        icon: spotifast::util::app_icon_rgba,
        template_icon: None,
        menu: vec![fastframe_tray::MenuItem::action(
            "show",
            "Show or hide Spotifast",
        )],
    }
}

/// Exercise the actual tray on a private bus that permits the watcher
/// name but denies every application-owned name, like Flatpak's proxy.
/// A subprocess keeps the test's bus and sandbox environment isolated.
#[test]
fn flatpak_tray_registers_without_owning_a_name() {
    const CHILD: &str = "SPOTIFAST_TRAY_TEST_CHILD";
    if std::env::var_os(CHILD).is_some() {
        let (registered, registrations) = std::sync::mpsc::channel();
        let server = zbus::blocking::connection::Builder::session()
            .unwrap()
            .name("org.kde.StatusNotifierWatcher")
            .unwrap()
            .serve_at("/StatusNotifierWatcher", Watcher(registered))
            .unwrap()
            .build()
            .unwrap();
        let tray =
            fastframe_tray::Tray::spawn(config(), || {}).expect("sandbox registration succeeds");
        let name = registrations.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(name.starts_with(':'), "register the unique connection name");
        server
            .call_method(
                Some(name.as_str()),
                "/StatusNotifierItem",
                Some("org.kde.StatusNotifierItem"),
                "Activate",
                &(0_i32, 0_i32),
            )
            .unwrap();
        assert_eq!(tray.events(), vec![fastframe_tray::Event::Toggle]);
        server.close().unwrap();
        assert!(
            fastframe_tray::Tray::spawn(config(), || {}).is_none(),
            "no watcher still means no tray"
        );
        return;
    }

    let root = std::env::temp_dir().join(format!("spotifast-tray-bus-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let config = root.join("bus.conf");
    std::fs::write(&config, r#"<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN" "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig><type>session</type><listen>unix:tmpdir=/tmp</listen>
<policy context="default"><allow user="*"/><allow send_destination="*"/>
<allow receive_sender="*"/><deny own="*"/>
<allow own="org.kde.StatusNotifierWatcher"/></policy></busconfig>"#).unwrap();
    let mut bus = match Command::new("dbus-daemon")
        .arg(format!("--config-file={}", config.display()))
        .args(["--nofork", "--print-address"])
        .stdout(Stdio::piped())
        .spawn()
    {
        Ok(bus) => bus,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("dbus-daemon is unavailable; private-bus tray test skipped");
            let _ = std::fs::remove_dir_all(root);
            return;
        }
        Err(error) => panic!("private bus: {error}"),
    };
    let mut address = String::new();
    std::io::BufReader::new(bus.stdout.take().unwrap())
        .read_line(&mut address)
        .unwrap();
    let result = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "flatpak_tray_registers_without_owning_a_name",
            "--nocapture",
        ])
        .env(CHILD, "1")
        .env("FLATPAK_ID", "rocks.spotifast.Spotifast")
        .env("DBUS_SESSION_BUS_ADDRESS", address.trim())
        .status();
    let _ = bus.kill();
    let _ = bus.wait();
    let _ = std::fs::remove_dir_all(root);
    assert!(
        result.unwrap().success(),
        "tray registration and activation must work"
    );
}
