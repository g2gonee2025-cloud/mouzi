use std::sync::mpsc::{self, Sender};
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager};
use crate::db::get_settings;
use crate::i18n::TrayI18n;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum WindowAction {
    Popup,
    Settings,
    WatchedFolders,
    Dashboard,
    Suggestions,
    Cleanup,
    Review(Vec<String>),
}

impl WindowAction {
    fn show(self, app: &AppHandle) -> Result<(), String> {
        match self {
            Self::Popup => show_popup_window(app),
            Self::Settings => show_settings_window(app),
            Self::WatchedFolders => show_watched_folders_window(app),
            Self::Dashboard => show_dashboard_window(app),
            Self::Suggestions => show_app_window(app, "/#/suggestions", "suggestions_title"),
            Self::Cleanup => show_app_window(app, "/#/cleanup", "cleanup_title"),
            Self::Review(paths) => return show_review_window(app, paths),
        }.map_err(|error| error.to_string())
    }
}

type WindowCompletion = tauri::async_runtime::Sender<Result<(), String>>;

struct WindowRequest {
    app: AppHandle,
    action: WindowAction,
    completion: Option<WindowCompletion>,
}

struct WindowDispatch(Sender<WindowRequest>);

fn dispatch_in_order<Request: Send + 'static>(
    mut handle: impl FnMut(Request) + Send + 'static,
) -> std::io::Result<Sender<Request>> {
    let (sender, requests) = mpsc::channel();
    // Building a WebView2 window can wait on the Windows event loop. Queue
    // from GUI callbacks and let this worker complete one request at a time.
    std::thread::Builder::new().name("mouzi-window-dispatch".into()).spawn(move || {
        for request in requests { handle(request); }
    })?;
    Ok(sender)
}

fn queue_window(app: &AppHandle, action: WindowAction, completion: Option<WindowCompletion>) -> Result<(), String> {
    let dispatch = app.try_state::<WindowDispatch>().ok_or("Window dispatch is unavailable")?;
    dispatch.inner().0.send(WindowRequest { app: app.clone(), action, completion })
        .map_err(|_| "Window dispatch has stopped".into())
}

fn queue_tray_window(app: &AppHandle, action: WindowAction) {
    if let Err(error) = queue_window(app, action, None) {
        eprintln!("[tray] Could not queue window request: {error}");
    }
}

pub(crate) async fn open_window(app: &AppHandle, action: WindowAction) -> Result<(), String> {
    let (complete, mut result) = tauri::async_runtime::channel(1);
    queue_window(app, action, Some(complete))?;
    result.recv().await.ok_or_else(|| "Window dispatch ended before reporting a result".to_string())?
}

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

    app.manage(WindowDispatch(dispatch_in_order(|request: WindowRequest| {
        let result = request.action.show(&request.app);
        if let Some(complete) = request.completion {
            let _ = complete.try_send(result);
        } else if let Err(error) = result {
            eprintln!("[tray] Could not open window: {error}");
        }
    })?));

    let mut builder = TrayIconBuilder::with_id("tray")
        .tooltip(i18n.get("tooltip"))
        .menu(&menu)
        .on_menu_event(|app, event| {
            let action = match event.id.as_ref() {
                "quit" => { app.exit(0); return; }
                "settings" => WindowAction::Settings,
                "dashboard" => WindowAction::Dashboard,
                "suggestions" => WindowAction::Suggestions,
                "cleanup" => WindowAction::Cleanup,
                "clean" => WindowAction::Review(Vec::new()),
                _ => return,
            };
            queue_tray_window(app, action);
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
                queue_tray_window(tray.app_handle(), WindowAction::Popup);
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

pub fn show_popup_window(app: &AppHandle) -> tauri::Result<()> {
    let i18n = TrayI18n::new(&tray_lang(app));
    if let Some(window) = app.get_webview_window("popup") {
        window.show()?;
        return window.set_focus();
    }
    let builder = tauri::WebviewWindowBuilder::new(app, "popup", tauri::WebviewUrl::App("/#/popup".into()))
        .title(i18n.get("popup_title"))
        .inner_size(300.0, 420.0)
        .decorations(false)
        .always_on_top(true)
        .shadow(false);
    #[cfg(not(target_os = "macos"))]
    let builder = builder.skip_taskbar(true);
    let window = builder.build()?;
    window.show()?;
    window.set_focus()
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
    if let Some(count) = app.try_state::<crate::AppState>()
        .and_then(|state| state.watcher.lock().ok().map(|watcher| watcher.pending_review_count()))
    {
        set_tray_tooltip(app, count, &translated);
    }
}

pub fn update_tray_tooltip(app: &AppHandle, count: usize) {
    let i18n = TrayI18n::new(&tray_lang(app));
    set_tray_tooltip(app, count, &i18n);
}

fn set_tray_tooltip(app: &AppHandle, count: usize, i18n: &TrayI18n) {
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
fn show_app_window(app: &AppHandle, url: &str, title_key: &str) -> tauri::Result<()> {
    let i18n = TrayI18n::new(&tray_lang(app));
    let title = i18n.get(title_key);
    if let Some(window) = app.get_webview_window("app") {
        window.eval(&location_hash_script(url))?;
        window.set_title(title)?;
        window.show()?;
        return window.set_focus();
    }
    let window = tauri::WebviewWindowBuilder::new(
        app,
        "app",
        tauri::WebviewUrl::App(url.into()),
    )
    .title(title)
    .inner_size(1100.0, 820.0)
    .min_inner_size(800.0, 600.0)
    .build()?;
    window.show()?;
    window.set_focus()
}

pub fn hide_app_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("app") {
        let _ = window.hide();
    }
}

pub fn show_settings_window(app: &AppHandle) -> tauri::Result<()> {
    show_app_window(app, "/#/settings", "settings_title")
}

fn show_watched_folders_window(app: &AppHandle) -> tauri::Result<()> {
    show_settings_window(app)?;
    app.emit_to("app", "open-watched-folders", ())?;
    if let Some(popup) = app.get_webview_window("popup") {
        popup.hide()?;
    }
    Ok(())
}

pub fn show_dashboard_window(app: &AppHandle) -> tauri::Result<()> {
    show_app_window(app, "/#/dashboard", "dashboard_title")
}

fn show_review_window(app: &AppHandle, paths: Vec<String>) -> Result<(), String> {
    let state = app.try_state::<crate::AppState>().ok_or("Application coordinator is unavailable")?;
    *state.review_request.lock().map_err(|_| "Organization review request is unavailable")? = Some(paths);
    if let Err(error) = show_settings_window(app) {
        // The serial dispatcher is the only writer. The renderer may have
        // already consumed the payload if showing the window partly succeeded.
        if let Ok(mut pending) = state.review_request.lock() { *pending = None; }
        return Err(error.to_string());
    }
    app.emit("open-review", ()).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::{dispatch_in_order, location_hash_script, workspace_fragment, WindowAction};
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn queued_window_actions_wait_for_the_first_and_keep_their_order() {
        let (observed, events) = mpsc::channel();
        let (release, blocked) = mpsc::channel();
        let queue = dispatch_in_order(move |action| {
            let first = action == WindowAction::Popup;
            observed.send(action).unwrap();
            if first { blocked.recv().unwrap(); }
        }).unwrap();

        queue.send(WindowAction::Popup).unwrap();
        let timeout = Duration::from_secs(2);
        assert_eq!(events.recv_timeout(timeout).unwrap(), WindowAction::Popup);
        queue.send(WindowAction::Settings).unwrap();
        queue.send(WindowAction::Dashboard).unwrap();
        assert_eq!(events.try_recv(), Err(mpsc::TryRecvError::Empty));
        release.send(()).unwrap();
        assert_eq!(events.recv_timeout(timeout).unwrap(), WindowAction::Settings);
        assert_eq!(events.recv_timeout(timeout).unwrap(), WindowAction::Dashboard);
    }

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
