//! Packaging icons must load in Qt's SVG renderer (system tray / desktop).
use std::fs;
use std::path::PathBuf;

#[test]
fn linux_tray_svg_avoids_filters_qt_rejects() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("packaging/icons/spotifast.svg");
    let svg = fs::read_to_string(&path).expect("spotifast.svg");
    assert!(
        !svg.contains("feFuncA") && !svg.contains("<filter"),
        "Qt 6 rejects feFuncA inside feComponentTransfer; the tray then shows a checkerboard (#660)"
    );
    assert!(svg.contains("viewBox"), "icon must remain a valid SVG mark");
}
