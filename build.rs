//! Compiles translations, embeds Windows resources and links GLEW for libprojectM.

/// Known names for vcpkg's static GLEW library, in preferred order.
/// Names differ across vcpkg versions and triplets, so use the installed one.
#[cfg(windows)]
const GLEW_NAMES: &[&str] = &["glew32s", "libglew32", "glew32"];

/// Returns the vcpkg triplet matching the target architecture and CRT mode.
#[cfg(windows)]
fn vcpkg_triplet() -> Option<String> {
    let arch = match std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
        Ok("x86_64") => "x64",
        Ok("aarch64") => "arm64",
        _ => return None,
    };
    let static_crt = std::env::var("CARGO_CFG_TARGET_FEATURE")
        .unwrap_or_default()
        .split(',')
        .any(|feature| feature == "crt-static");
    let suffix = if static_crt { "static" } else { "static-md" };
    Some(format!("{arch}-windows-{suffix}"))
}

/// Returns the known GLEW library installed in `lib`.
#[cfg(windows)]
fn glew_library(lib: &std::path::Path) -> Option<&'static str> {
    let present: Vec<String> = std::fs::read_dir(lib)
        .ok()?
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension()?.eq_ignore_ascii_case("lib") {
                Some(path.file_stem()?.to_str()?.to_ascii_lowercase())
            } else {
                None
            }
        })
        .collect();
    GLEW_NAMES
        .iter()
        .copied()
        .find(|name| present.iter().any(|found| found == name))
}

fn main() {
    fastframe_i18n::build::compile_catalogs("assets/i18n");
    #[cfg(windows)]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rerun-if-changed=packaging/windows/spotifast.ico");
        let mut resource = winresource::WindowsResource::new();
        resource
            .set_icon("packaging/windows/spotifast.ico")
            .set("ProductName", "Spotifast")
            .set("FileDescription", "A native Spotify client");
        if let Err(error) = resource.compile() {
            println!("cargo:warning=Windows resources not embedded: {error}");
        }
        // Static libprojectM requires the GLEW library installed by vcpkg.
        if std::env::var_os("CARGO_FEATURE_MILKDROP").is_some() {
            println!("cargo:rerun-if-env-changed=VCPKG_INSTALLATION_ROOT");
            if let (Some(root), Some(triplet)) =
                (std::env::var_os("VCPKG_INSTALLATION_ROOT"), vcpkg_triplet())
            {
                let lib = std::path::Path::new(&root)
                    .join("installed")
                    .join(triplet)
                    .join("lib");
                println!("cargo:rustc-link-search=native={}", lib.display());
                match glew_library(&lib) {
                    Some(name) => println!("cargo:rustc-link-lib=static={name}"),
                    None => {
                        // Include the directory listing in the error for diagnosis.
                        let listing = std::fs::read_dir(&lib)
                            .map(|entries| {
                                entries
                                    .flatten()
                                    .map(|entry| entry.file_name().to_string_lossy().into_owned())
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            })
                            .unwrap_or_else(|error| format!("unreadable: {error}"));
                        println!(
                            "cargo:warning=no GLEW library in {}; it holds: {listing}",
                            lib.display()
                        );
                    }
                }
            }
        }
    }
}
