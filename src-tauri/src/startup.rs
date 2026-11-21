use serde::Serialize;
use std::sync::Mutex;
use tauri::AppHandle;
#[cfg(not(windows))]
use tauri_plugin_autostart::ManagerExt;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum StartupStatus {
    Enabled,
    Disabled,
    BlockedBySystem,
    DifferentLocation,
    Unavailable,
}

const DEVELOPMENT: &str = "Autostart is unavailable in the isolated development profile";
static STARTUP: Mutex<()> = Mutex::new(());

pub(crate) fn status(app: &AppHandle) -> Result<StartupStatus, String> {
    if cfg!(debug_assertions) {
        return Ok(StartupStatus::Unavailable);
    }
    let _guard = STARTUP.lock().map_err(|_| "Startup settings are unavailable")?;
    #[cfg(windows)]
    return windows_status(&app.package_info().name, &running_executable()?);
    #[cfg(not(windows))]
    registered_status(app)
}

pub(crate) fn set_enabled(app: &AppHandle, enabled: bool) -> Result<StartupStatus, String> {
    if cfg!(debug_assertions) {
        return Err(DEVELOPMENT.into());
    }
    let _guard = STARTUP.lock().map_err(|_| "Startup settings are unavailable")?;
    #[cfg(windows)]
    return set_windows_startup(&app.package_info().name, &running_executable()?, enabled);
    #[cfg(not(windows))]
    let write = |enabled| {
        let manager = app.autolaunch();
        if enabled { manager.enable() } else { manager.disable() }.map_err(|error| error.to_string())
    };
    #[cfg(not(windows))]
    verify_change(enabled, || registered_status(app), write)
}

#[cfg(windows)]
fn running_executable() -> Result<std::path::PathBuf, String> {
    std::env::current_exe().map_err(|error| format!("Cannot locate the running Mouzi executable: {error}"))
}

#[cfg(not(windows))]
fn registered_status(app: &AppHandle) -> Result<StartupStatus, String> {
    // The Linux desktop entry and macOS LaunchAgent backends report registration.
    app.autolaunch().is_enabled().map(|enabled| {
        if enabled { StartupStatus::Enabled } else { StartupStatus::Disabled }
    }).map_err(|error| error.to_string())
}

fn verify_change(
    enabled: bool,
    read: impl Fn() -> Result<StartupStatus, String>,
    write: impl FnOnce(bool) -> Result<(), String>,
) -> Result<StartupStatus, String> {
    let requested = if enabled { StartupStatus::Enabled } else { StartupStatus::Disabled };
    let previous = read()?;
    if previous == requested {
        return Ok(previous);
    }
    if previous == StartupStatus::Unavailable {
        return Err("Startup settings are unavailable".into());
    }

    // A failed native call can have changed part of the registration. The
    // command invalidates status on every outcome so Settings rereads it.
    write(enabled).map_err(|error| format!("Could not change startup settings: {error}"))?;
    let observed = read().map_err(|error| format!("Could not verify startup settings: {error}"))?;
    match observed {
        status if status == requested => Ok(status),
        StartupStatus::BlockedBySystem => Err("Mouzi is still disabled in Windows Startup Apps.".into()),
        _ => Err("The system did not apply the startup change.".into()),
    }
}

#[cfg(windows)]
mod windows {
    use super::{verify_change, StartupStatus};
    use std::ffi::{OsStr, OsString};
    use std::io::{self, ErrorKind};
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use std::path::{Component, Path, Prefix};
    use winreg::enums::{HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_BINARY, REG_EXPAND_SZ, REG_SZ};
    use winreg::{RegKey, RegValue};

    // Keep compatibility with the previous auto-launch registration.
    const RUN: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Run";
    const APPROVAL: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run";
    const STARTUP_ARGUMENT: &str = "--autostart";
    const INVALID_REGISTRATION: &str = "The Windows startup registration is invalid.";
    const CSTR_EQUAL: i32 = 2;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn ExpandEnvironmentStringsW(source: *const u16, output: *mut u16, size: u32) -> u32;
        fn CompareStringOrdinal(left: *const u16, left_len: i32, right: *const u16, right_len: i32, ignore_case: i32) -> i32;
    }

    fn read_value(key: &str, app_name: &str) -> io::Result<Option<RegValue>> {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        match hkcu.open_subkey_with_flags(key, KEY_QUERY_VALUE).and_then(|key| key.get_raw_value(app_name)) {
            Ok(value) => Ok(Some(value)),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    pub(crate) fn windows_status(app_name: &str, executable: &Path) -> Result<StartupStatus, String> {
        let Some(run) = read_value(RUN, app_name).map_err(|error| format!("Cannot read Windows startup registration: {error}"))? else {
            return Ok(StartupStatus::Disabled);
        };
        let current = registration_matches(&run, executable)?;

        if let Some(approval) = read_value(APPROVAL, app_name).map_err(|error| format!("Cannot read Windows startup approval: {error}"))? {
            if approval_status(&approval)? == StartupStatus::BlockedBySystem {
                return Ok(StartupStatus::BlockedBySystem);
            }
        }
        Ok(if current { StartupStatus::Enabled } else { StartupStatus::DifferentLocation })
    }

    fn registration_matches(run: &RegValue, executable: &Path) -> Result<bool, String> {
        if !matches!(run.vtype, REG_SZ | REG_EXPAND_SZ) || run.bytes.len() % 2 != 0 {
            return Err(INVALID_REGISTRATION.into());
        }
        let mut command: Vec<u16> = run.bytes.chunks_exact(2).map(|bytes| u16::from_le_bytes([bytes[0], bytes[1]])).collect();
        while command.last() == Some(&0) { command.pop(); }
        if command.contains(&0) { return Err(INVALID_REGISTRATION.into()); }
        if run.vtype == REG_EXPAND_SZ {
            command = expand_environment_command(command)?;
        }

        let command = trim_argument_spaces(&command);
        let argument = STARTUP_ARGUMENT.encode_utf16().collect::<Vec<_>>();
        let quoted_argument = format!("\"{STARTUP_ARGUMENT}\"").encode_utf16().collect::<Vec<_>>();
        let target = command.strip_suffix(argument.as_slice()).or_else(|| command.strip_suffix(quoted_argument.as_slice()))
            .filter(|prefix| prefix.last().is_some_and(|unit| argument_space(*unit)))
            .ok_or(INVALID_REGISTRATION)?;
        let target = trim_argument_spaces(target);
        let quote = [u16::from(b'"')];
        let quoted = target.starts_with(&quote);
        // Old Mouzi entries left the path unquoted. Separate only the exact
        // final argument so spaces in both the directory and filename survive.
        let target = if let Some(path) = target.strip_prefix(&quote) {
            path.strip_suffix(&quote).ok_or(INVALID_REGISTRATION)?
        } else {
            target
        };
        let stored_path = OsString::from_wide(target);
        let stored_path = Path::new(&stored_path);
        let stored = executable_spelling(stored_path).ok_or(INVALID_REGISTRATION)?;
        let current = executable_spelling(executable).ok_or("The running Mouzi executable has an invalid startup path.")?;
        let stored_len = i32::try_from(stored.len()).map_err(|_| INVALID_REGISTRATION)?;
        let current_len = i32::try_from(current.len()).map_err(|_| INVALID_REGISTRATION)?;
        // Compare counted UTF-16 as data using Windows ordinal case rules.
        let order = unsafe { CompareStringOrdinal(stored.as_ptr(), stored_len, current.as_ptr(), current_len, 1) };
        if order == 0 {
            return Err(format!("Cannot compare Windows startup paths: {}", io::Error::last_os_error()));
        }
        if order != CSTR_EQUAL && !quoted && stored_path.file_name().is_some_and(|name| {
            // A previous executable token in a bare filename could be the real
            // command followed by arguments. Only accept that ambiguous legacy
            // spelling when the full path is known to be this executable.
            name.as_encoded_bytes().split(|&byte| byte == b' ').rev().skip(1)
                .any(|part| part.last_chunk::<4>().is_some_and(|suffix| suffix.eq_ignore_ascii_case(b".exe")))
        }) {
            return Err(INVALID_REGISTRATION.into());
        }
        Ok(order == CSTR_EQUAL)
    }

    fn expand_environment_command(mut command: Vec<u16>) -> Result<Vec<u16>, String> {
        command.push(0);
        let required = unsafe { ExpandEnvironmentStringsW(command.as_ptr(), std::ptr::null_mut(), 0) };
        if required == 0 {
            return Err(format!("Cannot expand Windows startup registration: {}", io::Error::last_os_error()));
        }
        let mut expanded = vec![0; required as usize];
        let written = unsafe { ExpandEnvironmentStringsW(command.as_ptr(), expanded.as_mut_ptr(), required) };
        if written == 0 || written > required {
            return Err("Cannot expand the Windows startup registration.".into());
        }
        expanded.truncate(written as usize - 1);
        Ok(expanded)
    }

    fn argument_space(unit: u16) -> bool { matches!(unit, 0x09 | 0x20) }

    fn trim_argument_spaces(mut units: &[u16]) -> &[u16] {
        while units.first().is_some_and(|unit| argument_space(*unit)) { units = &units[1..]; }
        while units.last().is_some_and(|unit| argument_space(*unit)) { units = &units[..units.len() - 1]; }
        units
    }

    fn append_path_component(output: &mut Vec<u16>, component: &OsStr) -> Option<()> {
        let units = component.encode_wide().collect::<Vec<_>>();
        if units.is_empty() || matches!(units.last(), Some(0x20 | 0x2e))
            || units.iter().any(|unit| matches!(unit, 0..=31 | 0x22 | 0x2a | 0x2f | 0x3a | 0x3c | 0x3e | 0x3f | 0x5c | 0x7c)) {
            return None;
        }
        output.push(u16::from(b'\\'));
        output.extend(units);
        Some(())
    }

    /// Normalize conventional disk, UNC and verbatim spellings lexically.
    /// Paths from startup values are never opened, even if the old copy exists.
    fn executable_spelling(path: &Path) -> Option<Vec<u16>> {
        // Check the original suffix as well as components so a directory-style
        // trailing separator or dot cannot be mistaken for an executable.
        path.file_name()?;
        if !path.as_os_str().as_encoded_bytes().last_chunk::<4>()?.eq_ignore_ascii_case(b".exe") {
            return None;
        }
        let mut components = path.components();
        let Component::Prefix(prefix) = components.next()? else { return None; };
        let mut normalized = Vec::new();
        match prefix.kind() {
            Prefix::Disk(letter) | Prefix::VerbatimDisk(letter) => normalized.extend([u16::from(letter), u16::from(b':')]),
            Prefix::UNC(server, share) | Prefix::VerbatimUNC(server, share) => {
                normalized.push(u16::from(b'\\'));
                append_path_component(&mut normalized, server)?;
                append_path_component(&mut normalized, share)?;
            }
            _ => return None,
        }
        if !matches!(components.next(), Some(Component::RootDir)) { return None; }
        for component in components {
            let Component::Normal(name) = component else { return None; };
            append_path_component(&mut normalized, name)?;
        }
        Some(normalized)
    }

    fn approval_status(approval: &RegValue) -> Result<StartupStatus, String> {
        if approval.vtype != REG_BINARY || approval.bytes.len() != 12 {
            return Err("The Windows startup approval state is invalid.".into());
        }
        // The first four bytes identify the decision even without a timestamp.
        match approval.bytes.as_slice() {
            [0x00 | 0x02 | 0x06, 0, 0, 0, timestamp @ ..] if timestamp.iter().all(|&byte| byte == 0) => {
                Ok(StartupStatus::Enabled)
            }
            [0x03 | 0x07, 0, 0, 0, ..] => Ok(StartupStatus::BlockedBySystem),
            _ => Err("The Windows startup approval state is unrecognized.".into()),
        }
    }

    pub(crate) fn set_windows_startup(
        app_name: &str,
        executable: &Path,
        enabled: bool,
    ) -> Result<StartupStatus, String> {
        verify_change(enabled, || windows_status(app_name, executable), |enabled| {
            write_windows_startup(app_name, executable, enabled)
        })
    }

    fn write_windows_startup(app_name: &str, executable: &Path, enabled: bool) -> Result<(), String> {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        if !enabled {
            return hkcu.open_subkey_with_flags(RUN, KEY_SET_VALUE).and_then(|key| key.delete_value(app_name))
                .map_err(|error| error.to_string());
        }

        executable_spelling(executable).ok_or("The running Mouzi executable has an invalid startup path.")?;
        let approval = read_value(APPROVAL, app_name).map_err(|error| format!("Cannot read Windows startup approval: {error}"))?;
        let blocked = approval.as_ref().map(approval_status).transpose()? == Some(StartupStatus::BlockedBySystem);
        let mut command = OsString::from("\"");
        command.push(executable.as_os_str());
        command.push("\" --autostart");
        let run = match hkcu.open_subkey_with_flags(RUN, KEY_SET_VALUE) {
            Err(error) if error.kind() == ErrorKind::NotFound => hkcu.create_subkey_with_flags(RUN, KEY_SET_VALUE).map(|(key, _)| key),
            other => other,
        }.map_err(|error| error.to_string())?;
        run.set_value(app_name, &command).map_err(|error| error.to_string())?;
        if blocked {
            // A user-requested enable can lift a recognized Windows denial.
            // Permitted and absent approval records need no write.
            let mut allowed = RegValue { vtype: REG_BINARY, bytes: vec![0; 12] };
            allowed.bytes[0] = 2;
            hkcu.open_subkey_with_flags(APPROVAL, KEY_SET_VALUE)
                .and_then(|key| key.set_raw_value(app_name, &allowed))
                .map_err(|error| format!("Cannot update Windows startup approval: {error}"))?;
        }
        Ok(())
    }

    #[cfg(test)]
    fn run_value(command: &str) -> RegValue {
        RegValue {
            vtype: REG_SZ,
            bytes: command.encode_utf16().chain([0]).flat_map(u16::to_le_bytes).collect(),
        }
    }

    #[cfg(test)]
    #[test]
    fn windows_startup_recognizes_only_the_intended_current_path_and_argument() {
        let current = Path::new(r"C:\Fixture Space\Étage\Mouzi Portable.exe");
        for command in [
            r#""C:\Fixture Space\Étage\Mouzi Portable.exe" --autostart"#,
            r"C:\Fixture Space\Étage\Mouzi Portable.exe --autostart",
            r#""c:/fixture space/étage/MOUZI PORTABLE.EXE" "--autostart""#,
            r#""\\?\C:\Fixture Space\Étage\Mouzi Portable.exe" --autostart"#,
        ] {
            assert_eq!(registration_matches(&run_value(command), current), Ok(true), "{command}");
        }
        assert_eq!(
            registration_matches(&run_value(r"C:\Former Copy\Étage\Mouzi Portable.exe --autostart"), current),
            Ok(false),
        );
        assert_eq!(
            registration_matches(
                &run_value(r#""\\?\UNC\server\share space\Mouzi.exe" --autostart"#),
                Path::new(r"\\SERVER\Share Space\Mouzi.exe"),
            ),
            Ok(true),
        );
    }

    #[cfg(test)]
    #[test]
    fn windows_startup_rejects_unrecognized_commands_and_raw_string_shapes() {
        let current = Path::new(r"C:\Fixture Space\Mouzi.exe");
        for command in [
            "",
            "not a startup command",
            r"C:relative\Mouzi.exe --autostart",
            r"C:\Fixture Space\Mouzi.exe",
            r#""C:\Fixture Space\Mouzi.exe" --autostart --extra"#,
            r"C:\Fixture Space\Mouzi.exe extra.exe --autostart",
            r#""C:\Fixture Space\Mouzi.exe --autostart"#,
            r#"C:\Fixture Space\Mouzi.exe" --autostart"#,
            r"C:\Fixture Space\Mouzi.exe\ --autostart",
            r"C:\Fixture Space\..\Mouzi.exe --autostart",
            r"\\.\C:\Fixture Space\Mouzi.exe --autostart",
            r"\\server\share.exe --autostart",
            "C:\\Fixture Space\\Mouzi.exe\0 --autostart",
        ] {
            assert!(registration_matches(&run_value(command), current).is_err(), "{command:?}");
        }
        let mut odd = run_value(r"C:\Fixture Space\Mouzi.exe --autostart");
        odd.bytes.push(0);
        assert!(registration_matches(&odd, current).is_err());
        odd.bytes.pop();
        odd.vtype = REG_BINARY;
        assert!(registration_matches(&odd, current).is_err());
    }

    #[cfg(test)]
    #[test]
    fn windows_startup_uses_literal_sz_and_expands_expand_sz() {
        let root = std::env::var_os("SystemRoot").expect("Windows SystemRoot");
        let executable = Path::new(&root).join("Fixture Mouzi.exe");
        let mut run = run_value(r#""%SystemRoot%\Fixture Mouzi.exe" --autostart"#);
        assert!(registration_matches(&run, &executable).is_err());
        run.vtype = REG_EXPAND_SZ;
        assert_eq!(registration_matches(&run, &executable), Ok(true));
    }

    #[cfg(test)]
    #[test]
    fn windows_startup_compares_path_units_without_replacement_characters() {
        let prefix = r"C:\Fixture ".encode_utf16();
        let suffix = r"\Mouzi.exe".encode_utf16();
        let unusual_path = prefix.clone().chain([0xd800]).chain(suffix.clone()).collect::<Vec<_>>();
        let expected = std::path::PathBuf::from(OsString::from_wide(&unusual_path));
        let different = prefix.chain([0xd801]).chain(suffix).collect::<Vec<_>>();
        let as_run = |units: Vec<u16>| RegValue {
            vtype: REG_SZ,
            bytes: [u16::from(b'"')].into_iter().chain(units).chain("\" --autostart".encode_utf16())
                .chain([0]).flat_map(u16::to_le_bytes).collect(),
        };
        assert_eq!(registration_matches(&as_run(unusual_path), &expected), Ok(true));
        assert_eq!(registration_matches(&as_run(different), &expected), Ok(false));
    }

    #[cfg(test)]
    #[test]
    fn windows_approval_honors_denial_even_without_a_timestamp() {
        let mut approval = RegValue { vtype: REG_BINARY, bytes: vec![0; 12] };
        for permitted in [0, 2, 6] {
            approval.bytes[0] = permitted;
            assert_eq!(approval_status(&approval), Ok(StartupStatus::Enabled));
        }
        for denied in [3, 7] {
            approval.bytes[0] = denied;
            approval.bytes[9] = 0;
            assert_eq!(approval_status(&approval), Ok(StartupStatus::BlockedBySystem));
            approval.bytes[9] = 1;
            assert_eq!(approval_status(&approval), Ok(StartupStatus::BlockedBySystem));
        }
    }

    #[cfg(test)]
    #[test]
    fn windows_approval_rejects_unrecognized_or_conflicting_state() {
        let mut approval = RegValue { vtype: REG_BINARY, bytes: vec![0; 12] };
        approval.bytes[0] = 0xA2;
        assert!(approval_status(&approval).is_err());
        approval.bytes[0] = 2;
        approval.bytes[1] = 1;
        assert!(approval_status(&approval).is_err());
        approval.bytes[1] = 0;
        approval.bytes[4] = 1;
        assert!(approval_status(&approval).is_err());
        approval.bytes[4] = 0;
        approval.bytes.push(0);
        assert!(approval_status(&approval).is_err());
        approval.bytes.truncate(8);
        assert!(approval_status(&approval).is_err());
        approval.bytes.resize(12, 0);
        approval.vtype = REG_SZ;
        assert!(approval_status(&approval).is_err());
    }
}

#[cfg(windows)]
pub(crate) use windows::{set_windows_startup, windows_status};

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn explicit_startup_changes_require_verified_native_state() {
        let state = Cell::new(StartupStatus::BlockedBySystem);
        let read = || Ok(state.get());
        assert!(verify_change(true, read, |_| Ok(())).unwrap_err().contains("still disabled"));
        assert_eq!(verify_change(true, read, |_| {
            state.set(StartupStatus::Enabled);
            Ok(())
        }), Ok(StartupStatus::Enabled));
        assert_eq!(verify_change(false, read, |_| {
            state.set(StartupStatus::Disabled);
            Ok(())
        }), Ok(StartupStatus::Disabled));
        assert_eq!(verify_change(false, read, |_| panic!("already disabled")), Ok(StartupStatus::Disabled));
    }

    #[test]
    fn a_partial_native_failure_is_reported_even_if_startup_became_enabled() {
        let state = Cell::new(StartupStatus::Disabled);
        let read = || Ok(state.get());
        let error = verify_change(true, read, |_| {
            state.set(StartupStatus::Enabled);
            Err("approval write failed".into())
        }).unwrap_err();
        assert!(error.contains("approval write failed"));
        assert_eq!(read(), Ok(StartupStatus::Enabled));
    }

    #[test]
    fn a_read_failure_does_not_grant_a_write_or_unverified_success() {
        let denied = || Err("registration unavailable".into());
        assert!(verify_change(true, denied, |_| panic!("unreadable registration")).is_err());
        let observed = Cell::new(false);
        let read = || if observed.replace(true) { denied() } else { Ok(StartupStatus::Disabled) };
        assert!(verify_change(true, read, |_| Ok(())).unwrap_err().contains("Could not verify"));
    }
}
