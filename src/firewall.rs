//! Windows Defender Firewall integration: startup check and explicit rule request.

#[cfg(windows)]
use std::ffi::OsStr;
#[cfg(windows)]
use std::os::windows::ffi::OsStrExt;

/// Whether an inbound rule allowing this executable exists in Windows Firewall.
#[cfg(windows)]
pub fn is_allowed() -> bool {
    let Ok(exe_path) = std::env::current_exe() else {
        return true;
    };
    let exe_str = exe_path.to_string_lossy();
    let clean_exe = exe_str.strip_prefix(r"\\?\").unwrap_or(&exe_str);
    let exe_name = exe_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "spotifast.exe".to_string());

    for name in [
        "Spotifast",
        "A native Spotify client",
        "spotifast.exe",
        "fastpotify.exe",
        &exe_name,
    ] {
        let mut cmd = std::process::Command::new("netsh");
        cmd.args([
            "advfirewall",
            "firewall",
            "show",
            "rule",
            &format!("name={name}"),
            "verbose",
        ]);
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
        }
        if let Ok(output) = cmd.output() {
            if output.status.success() {
                let text = String::from_utf8_lossy(&output.stdout);
                if rule_matches_exe(&text, clean_exe) {
                    return true;
                }
            }
        }
    }
    false
}

/// Request UAC elevation to add an inbound Windows Defender Firewall rule for this executable.
#[cfg(windows)]
pub fn request_access() -> Result<(), String> {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::WaitForSingleObject;
    use windows_sys::Win32::UI::Shell::{
        SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_HIDE;

    let exe_path = std::env::current_exe().map_err(|e| e.to_string())?;
    let exe_str = exe_path.to_string_lossy();
    let clean_exe = exe_str.strip_prefix(r"\\?\").unwrap_or(&exe_str);

    let verb: Vec<u16> = OsStr::new("runas").encode_wide().chain(Some(0)).collect();
    let file: Vec<u16> = OsStr::new("netsh.exe")
        .encode_wide()
        .chain(Some(0))
        .collect();
    let params: Vec<u16> = OsStr::new(&format!(
        "advfirewall firewall add rule name=\"Spotifast\" dir=in action=allow program=\"{clean_exe}\" enable=yes"
    ))
    .encode_wide()
    .chain(Some(0))
    .collect();

    let mut info: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
    info.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
    info.fMask = SEE_MASK_NOCLOSEPROCESS;
    info.lpVerb = verb.as_ptr();
    info.lpFile = file.as_ptr();
    info.lpParameters = params.as_ptr();
    info.nShow = SW_HIDE as i32;

    let success = unsafe { ShellExecuteExW(&mut info) };
    if success == 0 {
        return Err("elevation request was cancelled or failed".to_string());
    }

    if !info.hProcess.is_null() {
        unsafe {
            WaitForSingleObject(info.hProcess, 10_000);
            CloseHandle(info.hProcess);
        }
    }

    if is_allowed() {
        Ok(())
    } else {
        Err("firewall rule was not created".to_string())
    }
}

/// Fallback for non-Windows platforms.
#[cfg(not(windows))]
pub fn is_allowed() -> bool {
    true
}

/// Fallback for non-Windows platforms.
#[cfg(not(windows))]
pub fn request_access() -> Result<(), String> {
    Ok(())
}

/// Parses the output of `netsh advfirewall firewall show rule` to check if
/// an inbound allow rule exists for `target_exe`.
pub fn rule_matches_exe(output: &str, target_exe: &str) -> bool {
    let target_norm = target_exe
        .strip_prefix(r"\\?\")
        .unwrap_or(target_exe)
        .trim()
        .to_lowercase()
        .replace('/', "\\");
    for block in output.split("Rule Name:") {
        if block.trim().is_empty() {
            continue;
        }
        let mut enabled = false;
        let mut direction_in = false;
        let mut action_allow = false;
        let mut program_match = false;

        for line in block.lines() {
            let line = line.trim();
            if let Some((key, val)) = line.split_once(':') {
                let key = key.trim();
                let val = val.trim();
                if key.eq_ignore_ascii_case("Enabled") && val.eq_ignore_ascii_case("Yes") {
                    enabled = true;
                } else if key.eq_ignore_ascii_case("Direction") && val.eq_ignore_ascii_case("In") {
                    direction_in = true;
                } else if key.eq_ignore_ascii_case("Action") && val.eq_ignore_ascii_case("Allow") {
                    action_allow = true;
                } else if key.eq_ignore_ascii_case("Program") {
                    let clean_val = val
                        .strip_prefix(r"\\?\")
                        .unwrap_or(val)
                        .trim()
                        .to_lowercase()
                        .replace('/', "\\");
                    if clean_val == target_norm {
                        program_match = true;
                    }
                }
            }
        }

        if enabled && direction_in && action_allow && program_match {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::rule_matches_exe;

    #[test]
    fn parses_matching_allow_rule() {
        let output = r#"
Rule Name:                            Spotifast
----------------------------------------------------------------------
Description:                          Spotifast
Enabled:                              Yes
Direction:                            In
Profiles:                             Private,Public
Grouping:                             
LocalIP:                              Any
RemoteIP:                             Any
Protocol:                             TCP
LocalPort:                            Any
RemotePort:                           Any
Edge traversal:                       Defer to user
Program:                              C:\Programs\Spotifast\spotifast.exe
InterfaceTypes:                       Any
Security:                             NotRequired
Rule source:                          Local Setting
Action:                               Allow
"#;
        assert!(rule_matches_exe(
            output,
            r"C:\Programs\Spotifast\spotifast.exe"
        ));
        assert!(rule_matches_exe(
            output,
            r"c:\programs\spotifast\spotifast.exe"
        ));
        assert!(!rule_matches_exe(output, r"C:\Other\app.exe"));
    }

    #[test]
    fn ignores_disabled_or_blocked_rules() {
        let disabled = r#"
Rule Name:                            Spotifast
Enabled:                              No
Direction:                            In
Program:                              C:\app.exe
Action:                               Allow
"#;
        assert!(!rule_matches_exe(disabled, r"C:\app.exe"));

        let blocked = r#"
Rule Name:                            Spotifast
Enabled:                              Yes
Direction:                            In
Program:                              C:\app.exe
Action:                               Block
"#;
        assert!(!rule_matches_exe(blocked, r"C:\app.exe"));

        let outbound = r#"
Rule Name:                            Spotifast
Enabled:                              Yes
Direction:                            Out
Program:                              C:\app.exe
Action:                               Allow
"#;
        assert!(!rule_matches_exe(outbound, r"C:\app.exe"));
    }

    #[test]
    fn parses_rule_with_forward_slashes_and_client_name() {
        let output = r#"
Rule Name:                            A native Spotify client
----------------------------------------------------------------------
Description:                          A native Spotify client
Enabled:                              Yes
Direction:                            In
Program:                              C:/Programs/Spotifast/spotifast.exe
Action:                               Allow
"#;
        assert!(rule_matches_exe(
            output,
            r"C:\Programs\Spotifast\spotifast.exe"
        ));
        assert!(rule_matches_exe(
            output,
            "C:/Programs/Spotifast/spotifast.exe"
        ));
    }
}
