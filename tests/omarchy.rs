//! The optional desktop hook must copy palettes without changing preferences
//! or launching an app. All paths and commands here belong to the test.
#![cfg(target_os = "linux")]

use std::{os::unix::fs::PermissionsExt, path::PathBuf, process::Command};

struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn omarchy_hook_replaces_only_its_palette_and_tolerates_a_stopped_app() {
    let scratch =
        Scratch(std::env::temp_dir().join(format!("spotizgeg omarchy {}", rand::random::<u64>())));
    let source = scratch.0.join("current theme");
    let config = scratch.0.join("config");
    let target = config.join("spotizgeg/themes");
    let bin = scratch.0.join("bin");
    let log = scratch.0.join("commands");
    for dir in [&source, &target, &bin] {
        std::fs::create_dir_all(dir).unwrap();
    }
    // Nix provides tools through PATH without /bin or /usr/bin. Keep that
    // environment and use its actual interpreter for the generated fixture.
    let search_path = std::env::var_os("PATH").expect("test tools on PATH");
    let shell = std::env::split_paths(&search_path)
        .map(|directory| directory.join("bash"))
        .find(|path| path.is_file())
        .expect("bash for the Omarchy hook");
    let command_path = std::env::join_paths(
        std::iter::once(bin.clone()).chain(std::env::split_paths(&search_path)),
    )
    .unwrap();
    std::fs::write(
        bin.join("spotizgeg"),
        format!(
            "#!{}\nprintf '%s\\n' \"$*\" >> \"$SPOTIZGEG_HOOK_TEST_LOG\"\nexit 1\n",
            shell.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(
        bin.join("spotizgeg"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let run = || {
        let output = Command::new(&shell)
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/contrib/omarchy/spotizgeg-theme"
            ))
            .env("PATH", &command_path)
            .env("XDG_CONFIG_HOME", &config)
            .env("SPOTIZGEG_OMARCHY_THEME_DIR", &source)
            .env_remove("SPOTIZGEG_THEMES_DIR")
            .env("SPOTIZGEG_HOOK_TEST_LOG", &log)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    };

    run();
    assert!(!log.exists(), "a missing palette cannot request a reload");
    let outside = scratch.0.join("unrelated.json");
    std::fs::write(&outside, "preserve me").unwrap();
    std::os::unix::fs::symlink(&outside, target.join("omarchy.json")).unwrap();
    std::fs::write(config.join("spotizgeg/settings.json"), "unchanged settings").unwrap();
    for text in [r#"{"base":"dark"}"#, r#"{"base":"light"}"#] {
        std::fs::write(source.join("spotizgeg.json"), text).unwrap();
        run();
        assert_eq!(
            std::fs::read_to_string(target.join("omarchy.json")).unwrap(),
            text
        );
        let metadata = std::fs::symlink_metadata(target.join("omarchy.json")).unwrap();
        assert!(metadata.is_file());
        assert_eq!(metadata.permissions().mode() & 0o777, 0o644);
    }
    assert_eq!(std::fs::read_to_string(&outside).unwrap(), "preserve me");
    assert_eq!(
        std::fs::read_to_string(config.join("spotizgeg/settings.json")).unwrap(),
        "unchanged settings"
    );
    assert_eq!(
        std::fs::read_to_string(&log).unwrap(),
        "reload-themes\nreload-themes\n"
    );
    assert_eq!(
        std::fs::read_dir(&target).unwrap().count(),
        1,
        "no temporary file remains"
    );
    std::fs::remove_file(source.join("spotizgeg.json")).unwrap();
    std::os::unix::fs::symlink(&outside, source.join("spotizgeg.json")).unwrap();
    run();
    assert_eq!(
        std::fs::read_to_string(&log).unwrap(),
        "reload-themes\nreload-themes\n"
    );
    assert_eq!(
        std::fs::read_to_string(target.join("omarchy.json")).unwrap(),
        r#"{"base":"light"}"#
    );
}
