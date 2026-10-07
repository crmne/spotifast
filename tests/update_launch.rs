//! What the previous release's updater relies on from a new binary: the
//! update flags are taken out before the argument parser sees them, and
//! `--apply-update` makes the process the helper before anything else runs.

use std::process::Command;

/// An update relaunch passes `--update-receipt` (or `--update-error`)
/// alongside the app's own arguments. The command must accept them, which
/// it only does if the flags are intercepted before clap parses the rest;
/// `--version` also answers `<command> <version>`, as old helpers check.
#[test]
fn update_flags_are_intercepted_before_the_arguments_are_parsed() {
    for flags in [
        [
            "--update-receipt",
            "/missing/.spotizgeg-update-0000000000000000/handoff.json",
        ],
        [
            "--update-error",
            "The update could not start. The previous version has been restored.",
        ],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_spotizgeg"))
            .args(flags)
            .arg("--version")
            .output()
            .unwrap();
        assert!(output.status.success(), "{flags:?}: {output:?}");
        assert_eq!(
            String::from_utf8_lossy(&output.stdout).trim(),
            format!("spotizgeg {}", env!("CARGO_PKG_VERSION"))
        );
    }
}

/// `--apply-update <job>` runs the helper and exits before the app starts:
/// a missing job fails with a message on standard error and exit code 1,
/// and no log file or profile is touched (the environment points them at a
/// scratch folder that must stay empty).
#[test]
fn apply_update_runs_the_helper_before_the_app() {
    let scratch = std::env::temp_dir().join(format!(
        "spotizgeg-apply-update-{:016x}",
        rand::random::<u64>()
    ));
    std::fs::create_dir(&scratch).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_spotizgeg"))
        .arg("--apply-update")
        .arg(scratch.join("missing-job.json"))
        .env("HOME", &scratch)
        .env("XDG_CONFIG_HOME", scratch.join("config"))
        .env("XDG_STATE_HOME", scratch.join("state"))
        .env("XDG_CACHE_HOME", scratch.join("cache"))
        .env("XDG_DATA_HOME", scratch.join("data"))
        .env("APPDATA", scratch.join("appdata"))
        .env("LOCALAPPDATA", scratch.join("localappdata"))
        .output()
        .unwrap();
    let left = std::fs::read_dir(&scratch).unwrap().count();
    std::fs::remove_dir_all(&scratch).unwrap();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(!output.stderr.is_empty(), "the helper explains its failure");
    assert_eq!(left, 0, "the helper must not start the app");
}
