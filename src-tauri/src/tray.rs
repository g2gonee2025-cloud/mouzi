use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager};
use crate::db::get_settings;
use crate::i18n::TrayI18n;

pub fn setup_tray(app: &AppHandle, lang: &str) -> Result<(), Box<dyn std::error::Error>> {
    let i18n = TrayI18n::new(lang);

    let quit_i = MenuItem::with_id(app, "quit", i18n.get("quit"), true, None::<&str>)?;
    let clean_i = MenuItem::with_id(app, "clean", i18n.get("clean_now"), true, None::<&str>)?;
    let suggestions_i = MenuItem::with_id(app, "suggestions", i18n.get("suggestions"), true, None::<&str>)?;
    let dashboard_i = MenuItem::with_id(app, "dashboard", i18n.get("dashboard"), true, None::<&str>)?;
    let cleanup_i = MenuItem::with_id(app, "cleanup", i18n.get("cleanup"), true, None::<&str>)?;
    let settings_i = MenuItem::with_id(app, "settings", i18n.get("settings"), true, None::<&str>)?;
    let separator = PredefinedMenuItem::separator(app)?;

    let menu = Menu::with_items(app, &[&clean_i, &suggestions_i, &dashboard_i, &cleanup_i, &settings_i, &separator, &quit_i])?;

    let mut builder = TrayIconBuilder::with_id("tray")
        .tooltip(i18n.get("tooltip"))
        .menu(&menu)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "quit" => {
                app.exit(0);
            }
            "settings" => {
                show_settings_window(app);
            }
            "dashboard" => {
                show_dashboard_window(app);
            }
            "suggestions" => {
                show_suggestions_window(app);
            }
            "cleanup" => {
                show_cleanup_window(app);
            }
            "clean" => {
                if let Err(error) = crate::commands::show_review_cmd(app.clone(), Vec::new()) {
                    eprintln!("[tray] Could not open organization review: {error}");
                }
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click { button, .. } = event {
                if button == MouseButton::Left {
                    show_popup_window(tray.app_handle());
                }
            }
        });

    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }

    let _tray = builder.build(app)?;

    Ok(())
}

fn tray_lang(_app: &AppHandle) -> String {
    get_settings()
        .map(|s| s.language)
        .unwrap_or_else(|_| "en".to_string())
}

pub fn show_popup_window(app: &AppHandle) {
    let i18n = TrayI18n::new(&tray_lang(app));
    if let Some(window) = app.get_webview_window("popup") {
        let _ = window.show();
        let _ = window.set_focus();
    } else {
        #[cfg(target_os = "macos")]
        let window = tauri::WebviewWindowBuilder::new(
            app,
            "popup",
            tauri::WebviewUrl::App("/#/popup".into()),
        )
        .title(i18n.get("popup_title"))
        .inner_size(300.0, 420.0)
        .decorations(false)
        .always_on_top(true)
        .shadow(false)
        .build();

        #[cfg(not(target_os = "macos"))]
        let window = tauri::WebviewWindowBuilder::new(
            app,
            "popup",
            tauri::WebviewUrl::App("/#/popup".into()),
        )
        .title(i18n.get("popup_title"))
        .inner_size(300.0, 420.0)
        .decorations(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .shadow(false)
        .build();

        if let Ok(win) = window {
            let _ = win.show();
            let _ = win.set_focus();
        }
    }
}

pub fn update_tray_language(app: &AppHandle, lang: &str) {
    let translated = TrayI18n::new(lang);
    let result = (|| -> Result<Menu<tauri::Wry>, tauri::Error> {
        let clean = MenuItem::with_id(app, "clean", translated.get("clean_now"), true, None::<&str>)?;
        let suggestions = MenuItem::with_id(app, "suggestions", translated.get("suggestions"), true, None::<&str>)?;
        let dashboard = MenuItem::with_id(app, "dashboard", translated.get("dashboard"), true, None::<&str>)?;
        let cleanup = MenuItem::with_id(app, "cleanup", translated.get("cleanup"), true, None::<&str>)?;
        let settings = MenuItem::with_id(app, "settings", translated.get("settings"), true, None::<&str>)?;
        let separator = PredefinedMenuItem::separator(app)?;
        let quit = MenuItem::with_id(app, "quit", translated.get("quit"), true, None::<&str>)?;
        Menu::with_items(app, &[&clean, &suggestions, &dashboard, &cleanup, &settings, &separator, &quit])
    })();
    if let (Some(tray), Ok(menu)) = (app.tray_by_id("tray"), result) {
        let _ = tray.set_menu(Some(menu));
    }
    let pending = app.try_state::<crate::AppState>().and_then(|state| state.watcher.lock().ok().map(|watcher| watcher.get_pending_files().len())).unwrap_or(0);
    update_tray_tooltip(app, pending);
}

pub fn update_tray_tooltip(app: &AppHandle, count: usize) {
    let i18n = TrayI18n::new(&tray_lang(app));
    let tooltip = if count == 0 {
        i18n.get("tooltip").to_string()
    } else if count == 1 {
        i18n.get("tooltip_one_pending").replace("{}", "1")
    } else {
        i18n.get("tooltip_many_pending")
            .replace("{}", &count.to_string())
    };
    if let Some(tray) = app.tray_by_id("tray") {
        let _ = tray.set_tooltip(Some(&tooltip));
    }
}

/// Convert a Tauri app URL (`/#/dashboard`) to a `location.hash` fragment (`#/dashboard`).
/// Assigning `location.hash = '/#/dashboard'` becomes `#/#/dashboard`, which parseHash
/// does not treat as the dashboard route.
pub fn workspace_fragment(url_or_hash: &str) -> String {
    let s = url_or_hash.trim();
    if let Some(rest) = s.strip_prefix("/#") {
        format!("#{rest}")
    } else if s.starts_with('#') {
        s.to_string()
    } else {
        format!("#/{s}")
    }
}

/// Script that sets `location.hash` in the workspace webview.
/// Quotes and line breaks are stripped so this is safe to `eval`.
pub fn location_hash_script(hash: &str) -> String {
    let fragment = workspace_fragment(hash);
    let safe: String = fragment
        .chars()
        .filter(|c| !matches!(*c, '\'' | '\\' | '\n' | '\r'))
        .collect();
    format!("window.location.hash = '{safe}'")
}

/// One workspace window for dashboard, cleanup, suggestions, and settings.
/// The tray popup stays a separate compact flyout.
/// `url` is a Tauri webview path (`/#/dashboard`); eval uses the fragment only.
fn show_app_window(app: &AppHandle, url: &str, title_key: &str) {
    let i18n = TrayI18n::new(&tray_lang(app));
    let title = i18n.get(title_key);
    if let Some(window) = app.get_webview_window("app") {
        let _ = window.eval(&location_hash_script(url));
        let _ = window.set_title(title);
        let _ = window.show();
        let _ = window.set_focus();
        return;
    }
    let window = tauri::WebviewWindowBuilder::new(
        app,
        "app",
        tauri::WebviewUrl::App(url.into()),
    )
    .title(title)
    .inner_size(1100.0, 820.0)
    .min_inner_size(800.0, 600.0)
    .build();

    if let Ok(win) = window {
        let _ = win.show();
        let _ = win.set_focus();
    }
}

pub fn hide_app_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("app") {
        let _ = window.hide();
    }
}

pub fn show_settings_window(app: &AppHandle) {
    show_app_window(app, "/#/settings", "settings_title");
}

pub fn show_dashboard_window(app: &AppHandle) {
    show_app_window(app, "/#/dashboard", "dashboard_title");
}

pub fn show_suggestions_window(app: &AppHandle) {
    show_app_window(app, "/#/suggestions", "suggestions_title");
}

pub fn show_cleanup_window(app: &AppHandle) {
    show_app_window(app, "/#/cleanup", "cleanup_title");
}

#[cfg(test)]
mod tests {
    use super::{location_hash_script, workspace_fragment};

    fn assigned_fragment(script: &str) -> &str {
        script
            .split('\'')
            .nth(1)
            .expect("eval script must quote the hash")
    }

    #[test]
    fn location_hash_script_sets_route() {
        assert_eq!(
            location_hash_script("#/dashboard"),
            "window.location.hash = '#/dashboard'"
        );
    }

    #[test]
    fn location_hash_script_strips_quotes_and_breaks() {
        assert_eq!(
            location_hash_script("#'evil\n"),
            "window.location.hash = '#evil'"
        );
    }

    #[test]
    fn production_webview_url_eval_is_a_dashboard_fragment() {
        let url = "/#/dashboard";
        assert_eq!(workspace_fragment(url), "#/dashboard");
        let script = location_hash_script(url);
        assert_eq!(assigned_fragment(script.as_str()), "#/dashboard");
        assert_ne!(assigned_fragment(script.as_str()), "/#/dashboard");
    }
}
