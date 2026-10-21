//! The macOS bundle carries the launch agent that starts Spotifast in the
//! tray at login (#122), exactly as fastframe-autostart registers it.
use std::fs;
use std::path::PathBuf;

/// A packaging file as committed. A Windows checkout may turn its line
/// endings into CRLF; the bundle is built on macOS, from LF.
fn packaging(file: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("packaging/macos")
        .join(file);
    fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()))
        .replace("\r\n", "\n")
}

#[test]
fn the_bundle_carries_the_agent_that_starts_in_the_tray() {
    let id = "rocks.spotifast.Spotifast";
    assert_eq!(
        packaging("autostart.plist"),
        fastframe_autostart::macos_agent_plist(id, fastframe_autostart::START_HIDDEN),
        "the committed agent is the one the crate expects"
    );
    let bundle = packaging("bundle.sh");
    assert!(bundle.contains(&format!("identifier={id}\n")));
    assert!(
        bundle.contains(
            r#""$here/autostart.plist" "$app/Contents/Library/LaunchAgents/$identifier.autostart.plist""#
        ),
        "bundle.sh copies it where SMAppService looks: {}",
        fastframe_autostart::macos_agent_plist_name(id)
    );
}
