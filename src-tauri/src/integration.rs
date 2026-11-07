use tauri::{Emitter, Manager};

pub fn handle_folder_args(app: &tauri::AppHandle, args: &[String]) -> bool {
    let Some(index) = args.iter().position(|arg| arg == "--add-folder") else {
        return false;
    };
    let Some(path) = args
        .get(index + 1)
        .filter(|p| std::path::Path::new(p).is_dir())
    else {
        return false;
    };
    let Some(state) = app.try_state::<crate::AppState>() else { return false; };
    let Ok(mut request) = state.add_folder_request.lock() else { return false; };
    *request = Some(path.clone());
    drop(request);
    crate::tray::show_settings_window(app);
    let _ = app.emit("add-folder-request", ());
    true
}

#[cfg(target_os = "windows")]
const KEY: &str = "HKCU\\Software\\Classes\\Directory\\shell\\Mouzi";

pub fn enabled() -> bool {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        std::process::Command::new("reg.exe")
            .args(["query", KEY])
            .creation_flags(0x08000000)
            .output()
            .is_ok_and(|o| o.status.success())
    }
    #[cfg(not(target_os = "windows"))]
    {
        false
    }
}

pub fn set_enabled(enabled: bool) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        let run = |args: &[&str]| -> Result<(), String> {
            let output = std::process::Command::new("reg.exe")
                .args(args)
                .creation_flags(0x08000000)
                .output()
                .map_err(|e| e.to_string())?;
            if output.status.success() {
                Ok(())
            } else {
                Err(String::from_utf8_lossy(&output.stderr).into())
            }
        };
        if !enabled {
            return if self::enabled() {
                run(&["delete", KEY, "/f"])
            } else {
                Ok(())
            };
        }
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        run(&["add", KEY, "/ve", "/d", "Add to Mouzi", "/f"])?;
        run(&[
            "add",
            &format!("{KEY}\\command"),
            "/ve",
            "/d",
            &format!("\"{}\" --add-folder \"%1\"", exe.display()),
            "/f",
        ])
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = enabled;
        Err("Explorer integration is available on Windows".into())
    }
}
