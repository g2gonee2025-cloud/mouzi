use serde::Serialize;
use std::sync::Mutex;
use tauri::AppHandle;
use tauri_plugin_autostart::ManagerExt;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum StartupStatus {
    Enabled,
    Disabled,
    BlockedBySystem,
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
    return windows_status(&app.package_info().name);
    #[cfg(not(windows))]
    registered_status(app)
}

pub(crate) fn set_enabled(app: &AppHandle, enabled: bool) -> Result<StartupStatus, String> {
    if cfg!(debug_assertions) {
        return Err(DEVELOPMENT.into());
    }
    let _guard = STARTUP.lock().map_err(|_| "Startup settings are unavailable")?;
    let write = |enabled| {
        let manager = app.autolaunch();
        if enabled { manager.enable() } else { manager.disable() }.map_err(|error| error.to_string())
    };
    #[cfg(windows)]
    return set_windows_startup(&app.package_info().name, enabled, write);
    #[cfg(not(windows))]
    verify_change(enabled, || registered_status(app), write)
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
    use std::io::{self, ErrorKind};
    use winreg::enums::{HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_BINARY, REG_EXPAND_SZ, REG_SZ};
    use winreg::types::FromRegValue;
    use winreg::{RegKey, RegValue};

    // Match the keys and app name used by auto-launch 0.5.0.
    const RUN: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Run";
    const APPROVAL: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run";

    fn read_value(key: &str, app_name: &str) -> io::Result<Option<RegValue>> {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        match hkcu.open_subkey_with_flags(key, KEY_QUERY_VALUE).and_then(|key| key.get_raw_value(app_name)) {
            Ok(value) => Ok(Some(value)),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    pub(crate) fn windows_status(app_name: &str) -> Result<StartupStatus, String> {
        let Some(run) = read_value(RUN, app_name).map_err(|error| format!("Cannot read Windows startup registration: {error}"))? else {
            return Ok(StartupStatus::Disabled);
        };
        if !matches!(run.vtype, REG_SZ | REG_EXPAND_SZ)
            || String::from_reg_value(&run).map_or(true, |command| command.trim().is_empty())
        {
            return Err("The Windows startup registration is invalid.".into());
        }

        let Some(approval) = read_value(APPROVAL, app_name).map_err(|error| format!("Cannot read Windows startup approval: {error}"))? else {
            return Ok(StartupStatus::Enabled);
        };
        approval_status(&approval)
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
        enabled: bool,
        write: impl FnOnce(bool) -> Result<(), String>,
    ) -> Result<StartupStatus, String> {
        verify_change(enabled, || windows_status(app_name), |enabled| {
            if enabled {
                // The locked backend opens the Run key without creating it.
                let hkcu = RegKey::predef(HKEY_CURRENT_USER);
                match hkcu.open_subkey_with_flags(RUN, KEY_SET_VALUE) {
                    Err(error) if error.kind() == ErrorKind::NotFound => {
                        hkcu.create_subkey_with_flags(RUN, KEY_SET_VALUE)
                            .map_err(|error| error.to_string())?;
                    }
                    Err(error) => return Err(error.to_string()),
                    Ok(_) => {}
                }
            }
            write(enabled)
        })
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
