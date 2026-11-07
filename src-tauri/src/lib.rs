pub mod archive;
pub mod classify;
pub mod categories;
pub mod cleanup;
pub mod commands;
pub mod db;
pub mod i18n;
pub mod ignore;
pub mod integration;
pub mod operations;
pub mod rules;
pub mod safe_fs;
pub mod scan;
pub mod scheduler;
pub mod suppress;
pub mod tray;
pub mod watcher;

use commands::*;
use db::init_db;
use directories::ProjectDirs;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use tauri::Manager;
use tauri_plugin_autostart::ManagerExt;
use suppress::SuppressionSet;
use watcher::FolderWatcher;
#[cfg(target_os = "macos")]
use tauri::ActivationPolicy;

pub struct AppState {
    pub watcher: Arc<Mutex<FolderWatcher>>,
    pub ignored_files: Arc<SuppressionSet>,
    pub scheduler: scheduler::Scheduler,
    pub is_scanning: Arc<AtomicBool>,
    pub review_request: Mutex<Option<Vec<String>>>,
    pub add_folder_request: Mutex<Option<String>>,
}

/// Debug builds use a distinct profile and never register OS autostart.
/// An explicit override is useful for isolated manual validation.
fn application_data_dir() -> Result<std::path::PathBuf, Box<dyn std::error::Error>> {
    let application = if cfg!(debug_assertions) { "mouzi-dev" } else { "mouzi" };
    let default = ProjectDirs::from("cc", "mouzi", application)
        .ok_or("Could not resolve the application data directory")?.data_dir().to_path_buf();
    let path = if cfg!(debug_assertions) {
        std::env::var_os("MOUZI_DATA_DIR").map(std::path::PathBuf::from).unwrap_or(default)
    } else { default };
    if !path.is_absolute() {
        return Err("MOUZI_DATA_DIR must be absolute".into());
    }
    if cfg!(debug_assertions) {
        if let Some(prod) = ProjectDirs::from("cc", "mouzi", "mouzi") {
            validate_development_profile(&path, prod.data_dir())?;
        }
    }
    Ok(path)
}

fn validate_development_profile(path: &std::path::Path, production: &std::path::Path) -> Result<(), String> {
    let path = crate::safe_fs::resolve_future_directory(path)?;
    let production = crate::safe_fs::resolve_future_directory(production)?;
    if crate::safe_fs::is_within_any_root(&path, &[production]) {
        return Err("Development data must be outside the production profile".into());
    }
    Ok(())
}

#[cfg(test)]
mod profile_tests {
    use super::*;
    #[test]
    fn development_profile_rejects_production_and_descendants() {
        let root = std::env::temp_dir().join(format!("mouzi-profile-test-{}",std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let production=root.join("production");std::fs::create_dir_all(&production).unwrap();
        assert!(validate_development_profile(&production,&production).is_err());
        assert!(validate_development_profile(&production.join("nested/new"),&production).is_err());
        assert!(validate_development_profile(&root.join("development"),&production).is_ok());
        assert!(validate_development_profile(&root.join("production-sibling"),&production).is_ok());
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let mut context = tauri::generate_context!();
    if cfg!(debug_assertions) {
        context.config_mut().identifier = "cc.mouzi.dev".into();
        context.config_mut().product_name = Some("Mouzi Development".into());
    }
    let ignored_files = Arc::new(SuppressionSet::new());
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec!["--autostart"]),
        ))
        .plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            if integration::handle_folder_args(app, &args) {
                return;
            }
            if let Some(window) = app.get_webview_window("app") {
                let _ = window.show();
                let _ = window.set_focus();
            } else if let Some(window) = app.get_webview_window("popup") {
                let _ = window.show();
                let _ = window.set_focus();
            }
        }))
        .manage(AppState {
            watcher: Arc::new(Mutex::new(FolderWatcher::new(ignored_files.clone()))),
            ignored_files,
            scheduler: scheduler::Scheduler::new(),
            is_scanning: Arc::new(AtomicBool::new(false)),
            review_request: Mutex::new(None),
            add_folder_request: Mutex::new(None),
        })
        .setup(|app| {
            // Hid app from dock on macOS
            #[cfg(target_os = "macos")]
            app.set_activation_policy(ActivationPolicy::Accessory);

            let app_handle = app.handle().clone();

            // Initialize database
            let data_dir = application_data_dir()?;
            std::fs::create_dir_all(&data_dir)?;
            init_db(data_dir).map_err(|e| format!("Failed to initialize database: {e}"))?;

            // Initialize default rules on first run
            let is_first_run = if let Ok(settings) = db::get_settings() {
                let first = settings.first_run;
                if first {
                    db::complete_first_run(&commands::get_system_language())
                        .map_err(|e| format!("First-run initialization failed: {e}"))?;
                }
                first
            } else {
                false
            };

            // Setup system tray
            let tray_lang = db::get_settings()
                .map(|s| s.language)
                .unwrap_or_else(|_| "en".to_string());
            tray::setup_tray(&app_handle, &tray_lang)?;

            // On first launch, show the popup so the user knows the app is running
            if is_first_run {
                tray::show_popup_window(&app_handle);
            }

            // Dev: open the dashboard so `tauri dev` lands on the main surface.
            #[cfg(debug_assertions)]
            {
                if std::env::var("MOUZI_OPEN").unwrap_or_else(|_| "dashboard".into()) != "none" {
                    tray::show_dashboard_window(&app_handle);
                }
            }

            // Sync autostart with user settings
            if !cfg!(debug_assertions) {
              if let Ok(settings) = db::get_settings() {
                let auto_manager = app.autolaunch();
                if settings.autostart {
                    let _ = auto_manager.enable();
                } else {
                    let _ = auto_manager.disable();
                }
              }
            }

            // Start folder watcher
            let state = app.state::<AppState>();
            {
                let mut watcher = state.watcher.lock().unwrap();
                let _ = watcher.watch_folders(app_handle.clone());
            }

            // Start scheduled-clean background thread
            state.scheduler.start(app_handle.clone(), state.ignored_files.clone());
            integration::handle_folder_args(&app_handle, &std::env::args().collect::<Vec<_>>());

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            preview_cmd,
            apply_preview_cmd,
            discard_preview_cmd,
            history_cmd,
            undo_selected_cmd,
            set_only_new_cmd,
            show_review_cmd,
            take_review_request_cmd,
            take_add_folder_request_cmd,
            explorer_integration_cmd,
            explorer_integration_status_cmd,
            get_system_language,
            get_rules_cmd,
            add_rule_cmd,
            update_rule_cmd,
            delete_rule_cmd,
            get_folders_cmd,
            add_folder_cmd,
            remove_folder_cmd,
            update_folder_mode_cmd,
            get_logs_cmd,
            get_history_counts_cmd,
            get_cleanup_logs_cmd,
            get_stats_cmd,
            undo_action_cmd,
            undo_all_cmd,
            get_settings_cmd,
            update_settings_cmd,
            enable_autostart_cmd,
            disable_autostart_cmd,
            is_autostart_enabled_cmd,
            clear_logs_cmd,
            scan_folder_cmd,
            clean_now_cmd,
            import_archive_cmd,
            cancel_archive_import_cmd,
            open_folder_cmd,
            get_downloads_folder,
            initialize_defaults_cmd,
            close_popup,
            close_settings,
            show_notification,
            load_mouziignore_cmd,
            save_mouziignore_cmd,
            show_popup_cmd,
            get_pending_files_cmd,
            refresh_watcher_cmd,
            get_schedule_cmd,
            update_schedule_cmd,
            get_recent_scheduler_runs_cmd,
            get_version_cmd,
            export_rules_cmd,
            import_rules_cmd,
            start_scan_cmd,
            is_scanning_cmd,
            get_dashboard_stats_cmd,
            get_inventory_files_cmd,
            find_duplicates_cmd,
            find_large_files_cmd,
            find_stale_files_cmd,
            find_empty_dirs_cmd,
            prepare_cleanup_cmd,
            execute_cleanup_cmd,
            get_suggestions_cmd,
            prepare_suggestion_cmd,
            dismiss_suggestion_cmd,
            reset_dismissed_suggestions_cmd,
            accept_suggestion_cmd,
        ])
        .run(context)
        .expect("error while running tauri application");
}
