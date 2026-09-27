use crate::db::*;
use crate::ignore::{load_mouziignore, save_mouziignore};
use crate::rules::manual_scan_folder;
use crate::safe_fs::{move_file, MoveOutcome};
use crate::scan::{self, ScanEvent};
use crate::AppState;
use serde::Serialize;
use std::path::Path;
use std::sync::atomic::Ordering;
use std::time::Instant;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_autostart::ManagerExt;
use tauri_plugin_notification::NotificationExt;

#[tauri::command]
pub fn get_system_language() -> String {
    let locale = sys_locale::get_locale().unwrap_or_else(|| "en".to_string());
    let lang = locale.split('-').next().unwrap_or("en").to_lowercase();
    match lang.as_str() {
        "pl" => "pl".to_string(),
        "it" => "it".to_string(),
        "de" => "de".to_string(),
        "fr" => "fr".to_string(),
        "ru" => "ru".to_string(),
        "ja" => "ja".to_string(),
        "es" => "es".to_string(),
        "uk" => "uk".to_string(),
        _ => "en".to_string(),
    }
}

#[tauri::command]
pub fn get_rules_cmd() -> Result<Vec<Rule>, String> {
    get_rules().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn add_rule_cmd(rule: Rule) -> Result<i64, String> {
    add_rule(&rule).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn update_rule_cmd(rule: Rule) -> Result<(), String> {
    update_rule(&rule).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn delete_rule_cmd(id: i64) -> Result<(), String> {
    delete_rule(id).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn get_folders_cmd() -> Result<Vec<WatchedFolder>, String> {
    get_watched_folders().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn add_folder_cmd(app: tauri::AppHandle, path: String, mode: String) -> Result<i64, String> {
    if !is_valid_folder_mode(&mode) {
        return Err(format!("Invalid folder mode: {}", mode));
    }
    let folder = std::path::Path::new(&path);
    if !folder.is_absolute() {
        return Err(format!("Path must be absolute: {}", path));
    }
    // A drive or filesystem root has no parent. Watching one would make the
    // scanner walk the entire volume, which the design rules out.
    if folder.parent().is_none() {
        return Err(format!("Refusing to watch a whole drive: {}", path));
    }
    if !folder.is_dir() {
        return Err(format!("Not an existing folder: {}", path));
    }
    let id = add_watched_folder(&path, &mode).map_err(|e| e.to_string())?;
    if let Some(state) = app.try_state::<crate::AppState>() {
        let mut watcher = state.watcher.lock().unwrap();
        let _ = watcher.refresh(app.clone());
    }
    Ok(id)
}

#[tauri::command]
pub fn remove_folder_cmd(app: tauri::AppHandle, id: i64) -> Result<(), String> {
    remove_watched_folder(id).map_err(|e| e.to_string())?;
    if let Some(state) = app.try_state::<crate::AppState>() {
        let mut watcher = state.watcher.lock().unwrap();
        let _ = watcher.refresh(app.clone());
    }
    Ok(())
}

#[tauri::command]
pub fn update_folder_mode_cmd(app: tauri::AppHandle, id: i64, mode: String) -> Result<(), String> {
    if !is_valid_folder_mode(&mode) {
        return Err(format!("Invalid folder mode: {}", mode));
    }

    let old_mode = get_watched_folders()
        .ok()
        .and_then(|folders| folders.into_iter().find(|f| f.id == Some(id)))
        .map(|f| f.mode);

    update_folder_mode(id, &mode).map_err(|e| e.to_string())?;

    if let Some(state) = app.try_state::<crate::AppState>() {
        let mut watcher = state.watcher.lock().unwrap();

        // If switching from manual to silent, flush collected files into the auto queue.
        if is_folder_auto_mode(&mode) {
            if let Some(ref old) = old_mode {
                if is_folder_manual_mode(old) {
                    if let Some(folder) = get_watched_folders()
                        .ok()
                        .and_then(|folders| folders.into_iter().find(|f| f.id == Some(id)))
                    {
                        watcher.flush_manual_to_pending(&folder.path);
                    }
                }
            }
        }

        let _ = watcher.refresh(app.clone());
    }
    Ok(())
}

#[tauri::command]
pub fn get_logs_cmd(limit: i64) -> Result<Vec<ActionLog>, String> {
    get_recent_logs(limit).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn get_stats_cmd() -> Result<Vec<(String, i64)>, String> {
    get_weekly_stats().map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// Undo helpers
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UndoResult {
    pub status: String, // "ok" | "collision" | "missing" | "failed"
    pub message: Option<String>,
    pub restored_to: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UndoAllResult {
    pub count: usize,
    pub results: Vec<UndoResult>,
}

fn perform_undo(
    conn: &rusqlite::Connection,
    ignored: &mut std::collections::HashMap<String, Instant>,
    id: i64,
    source: String,
    dest: Option<String>,
) -> Result<UndoResult, String> {
    let dest_path = match dest {
        Some(ref d) if !d.is_empty() => std::path::Path::new(d),
        _ => {
            conn.execute("UPDATE action_logs SET undone=1 WHERE id=?1", [id])
                .map_err(|e| e.to_string())?;
            return Ok(UndoResult {
                status: "missing".to_string(),
                message: None,
                restored_to: None,
            });
        }
    };

    if !dest_path.exists() {
        conn.execute("UPDATE action_logs SET undone=1 WHERE id=?1", [id])
            .map_err(|e| e.to_string())?;
        return Ok(UndoResult {
            status: "missing".to_string(),
            message: None,
            restored_to: None,
        });
    }

    let src_path = std::path::Path::new(&source);

    match move_file(dest_path, src_path) {
        Ok(MoveOutcome::Moved) => {
            // The watcher must ignore both the path the file left and the path it landed on.
            ignored.insert(dest_path.to_string_lossy().to_string(), Instant::now());
            ignored.insert(source.clone(), Instant::now());
            conn.execute("UPDATE action_logs SET undone=1 WHERE id=?1", [id])
                .map_err(|e| e.to_string())?;
            Ok(UndoResult {
                status: "ok".to_string(),
                message: None,
                restored_to: Some(source),
            })
        }
        Ok(MoveOutcome::MovedWithNewName(name)) => {
            let restored = src_path.with_file_name(&name).to_string_lossy().to_string();
            ignored.insert(dest_path.to_string_lossy().to_string(), Instant::now());
            ignored.insert(restored.clone(), Instant::now());
            conn.execute("UPDATE action_logs SET undone=1 WHERE id=?1", [id])
                .map_err(|e| e.to_string())?;
            Ok(UndoResult {
                status: "collision".to_string(),
                message: None,
                restored_to: Some(restored),
            })
        }
        Err(e) => Ok(UndoResult {
            status: "failed".to_string(),
            message: Some(e),
            restored_to: None,
        }),
    }
}

#[tauri::command(async)]
pub fn undo_action_cmd(id: i64, state: tauri::State<AppState>) -> Result<UndoResult, String> {
    let db = get_db();
    let conn = db.lock().unwrap();
    let (source, dest): (String, Option<String>) = conn
        .query_row(
            "SELECT source_path, destination_path FROM action_logs WHERE id=?1 AND undone=0",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(|e| format!("No undoable action found for id={}: {}", id, e))?;

    let mut ignored = state.ignored_files.lock().unwrap();
    perform_undo(&conn, &mut ignored, id, source, dest)
}

#[tauri::command(async)]
pub fn undo_all_cmd(state: tauri::State<AppState>) -> Result<UndoAllResult, String> {
    let logs = crate::db::get_undoable_logs().map_err(|e| e.to_string())?;
    let db = get_db();
    let conn = db.lock().unwrap();
    let mut ignored = state.ignored_files.lock().unwrap();

    let mut results = Vec::with_capacity(logs.len());
    for (id, source, dest) in logs {
        match perform_undo(&conn, &mut ignored, id, source, dest) {
            Ok(r) => results.push(r),
            Err(e) => {
                results.push(UndoResult {
                    status: "failed".to_string(),
                    message: Some(e),
                    restored_to: None,
                });
            }
        }
    }

    let count = results.iter().filter(|r| r.status != "failed").count();
    Ok(UndoAllResult { count, results })
}

#[tauri::command]
pub fn get_cleanup_logs_cmd(limit: i64) -> Result<Vec<CleanupAction>, String> {
    get_cleanup_logs(limit).map_err(|e| e.to_string())
}

/// Return the current app version and release date.
#[tauri::command]
pub fn get_version_cmd(app: AppHandle) -> Result<(String, String), String> {
    let version = app.package_info().version.to_string();
    let release_date = "2026-08-06".to_string();
    Ok((version, release_date))
}

#[tauri::command]
pub fn get_settings_cmd() -> Result<AppSettings, String> {
    get_settings().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn update_settings_cmd(settings: AppSettings) -> Result<(), String> {
    update_settings(&settings).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn clear_logs_cmd() -> Result<(), String> {
    clear_logs().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn scan_folder_cmd(path: String) -> Result<Vec<(String, String, String)>, String> {
    let folders = get_watched_folders().map_err(|e| e.to_string())?;
    if folders
        .iter()
        .find(|f| f.path == path)
        .map(|f| is_folder_paused_mode(&f.mode))
        .unwrap_or(false)
    {
        return Ok(vec![]);
    }
    manual_scan_folder(&path)
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchiveImportSummary {
    pub extracted_count: usize,
    pub sorted_count: usize,
    pub staging_path: String,
    pub results: Vec<(String, String, String)>,
}

#[tauri::command(async)]
pub fn import_archive_cmd(path: String) -> Result<ArchiveImportSummary, String> {
    let extraction = crate::archive::extract_archive(Path::new(&path))?;
    let staging_path = extraction.staging_dir.to_string_lossy().to_string();
    let results = manual_scan_folder(&staging_path)?;

    Ok(ArchiveImportSummary {
        extracted_count: extraction.extracted_files,
        sorted_count: results.len(),
        staging_path,
        results,
    })
}

#[tauri::command]
pub fn open_folder_cmd(path: String) -> Result<(), String> {
    if path.is_empty() {
        return Err("Path is empty".to_string());
    }
    eprintln!("[open_folder_cmd] opening: {}", path);
    #[cfg(target_os = "windows")]
    {
        if path.starts_with("http://") || path.starts_with("https://") {
            // No shell: a URL carrying " & " would otherwise run a second command.
            std::process::Command::new("rundll32")
                .args(["url.dll,FileProtocolHandler", &path])
                .spawn()
                .map_err(|e| e.to_string())?;
        } else {
            // Normalize to backslashes - Windows Explorer requires them
            let win_path = path.replace('/', "\\");
            // Pass the path as an argument rather than building a shell command.
            // A path containing a single quote would otherwise close the quoted
            // string and everything after it would execute as PowerShell.
            std::process::Command::new("explorer")
                .arg(&win_path)
                .spawn()
                .map_err(|e| e.to_string())?;
        }
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(&path)
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    #[cfg(target_os = "linux")]
    {
        std::process::Command::new("xdg-open")
            .arg(&path)
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
pub fn get_downloads_folder() -> String {
    directories::UserDirs::new()
        .and_then(|d| d.download_dir().map(|p| p.to_string_lossy().to_string()))
        .unwrap_or_else(|| {
            if cfg!(target_os = "windows") {
                "C:/Users".to_string()
            } else {
                std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string())
            }
        })
}

#[tauri::command]
pub fn initialize_defaults_cmd() -> Result<(), String> {
    let downloads = get_downloads_folder();
    add_watched_folder(&downloads, FOLDER_MODE_SILENT).map_err(|e| e.to_string())?;
    insert_default_rules(&downloads).map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub fn close_popup(app: AppHandle) {
    if let Some(window) = app.get_webview_window("popup") {
        let _ = window.hide();
    }
}

#[tauri::command]
pub fn close_settings(app: AppHandle) {
    crate::tray::hide_app_window(&app);
}

#[tauri::command]
pub fn show_notification(app: AppHandle, title: String, body: String) {
    let _ = app.notification().builder().title(title).body(body).show();
}

#[tauri::command]
pub fn enable_autostart_cmd(app: AppHandle) -> Result<(), String> {
    app.autolaunch().enable().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn disable_autostart_cmd(app: AppHandle) -> Result<(), String> {
    app.autolaunch().disable().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn is_autostart_enabled_cmd(app: AppHandle) -> Result<bool, String> {
    app.autolaunch().is_enabled().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn load_mouziignore_cmd(folder_path: String) -> Result<Vec<String>, String> {
    Ok(load_mouziignore(&folder_path))
}

#[tauri::command]
pub fn save_mouziignore_cmd(folder_path: String, patterns: Vec<String>) -> Result<(), String> {
    save_mouziignore(&folder_path, &patterns)
}

/// Returns and clears the pending folder path that should be opened after a notification click.
/// The frontend calls this when the popup is shown/focused to handle the in-app flow as a backup.
#[tauri::command]
pub fn get_pending_open_folder_cmd(state: tauri::State<AppState>) -> Option<String> {
    state.pending_open_folder.lock().unwrap().take()
}

#[tauri::command]
pub fn show_popup_cmd(app: AppHandle) {
    crate::tray::show_popup_window(&app);
}

/// Return files detected in manual-mode folders that are waiting for Clean Now.
#[tauri::command(async)]
pub fn get_pending_files_cmd(
    state: tauri::State<AppState>,
) -> Result<Vec<(String, String)>, String> {
    let watcher = state.watcher.lock().unwrap();
    Ok(watcher.get_pending_files())
}

/// Refresh all folder watchers. Useful after changing folder modes.
#[tauri::command]
pub fn refresh_watcher_cmd(app: AppHandle) -> Result<(), String> {
    if let Some(state) = app.try_state::<crate::AppState>() {
        let mut watcher = state.watcher.lock().unwrap();
        watcher.refresh(app.clone()).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Get the current scheduled-clean settings.
#[tauri::command]
pub fn get_schedule_cmd() -> Result<ScheduleSettings, String> {
    let settings = get_settings().map_err(|e| e.to_string())?;
    Ok(ScheduleSettings {
        schedule_enabled: settings.schedule_enabled,
        schedule_times_per_day: settings.schedule_times_per_day,
        schedule_time_1: settings.schedule_time_1,
        schedule_time_2: settings.schedule_time_2,
        schedule_time_3: settings.schedule_time_3,
        schedule_time_4: settings.schedule_time_4,
    })
}

/// Update scheduled-clean settings.
#[tauri::command]
pub fn update_schedule_cmd(schedule: ScheduleSettings) -> Result<(), String> {
    let mut settings = get_settings().map_err(|e| e.to_string())?;
    settings.schedule_enabled = schedule.schedule_enabled;
    settings.schedule_times_per_day = schedule.schedule_times_per_day.clamp(1, 4);
    settings.schedule_time_1 = schedule.schedule_time_1;
    settings.schedule_time_2 = schedule.schedule_time_2;
    settings.schedule_time_3 = schedule.schedule_time_3;
    settings.schedule_time_4 = schedule.schedule_time_4;
    update_settings(&settings).map_err(|e| e.to_string())
}

/// Export all rules to a JSON file at the given path.
#[tauri::command]
pub fn export_rules_cmd(path: String) -> Result<(), String> {
    let rules = get_rules().map_err(|e| e.to_string())?;
    let json = serde_json::to_string_pretty(&rules).map_err(|e| e.to_string())?;
    std::fs::write(&path, json).map_err(|e| e.to_string())?;
    Ok(())
}

/// Import rules from a JSON file at the given path.
/// If `replace` is true, all existing rules are removed before inserting.
/// Each imported rule is inserted with `id` set to None to avoid collisions.
#[tauri::command]
pub fn import_rules_cmd(path: String, replace: bool) -> Result<usize, String> {
    let data = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let rules: Vec<Rule> = serde_json::from_str(&data).map_err(|e| e.to_string())?;

    if replace {
        delete_all_rules().map_err(|e| e.to_string())?;
    }

    let mut count = 0;
    for mut rule in rules {
        rule.id = None;
        add_rule(&rule).map_err(|e| e.to_string())?;
        count += 1;
    }
    Ok(count)
}

// ---------------------------------------------------------------------------
// Scan / Dashboard commands
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanStarted {
    pub started: bool,
    pub reason: Option<String>,
}

/// Scan all watched (non-paused) folders in the background.
/// Returns immediately; emits `scan-progress` events during the walk and
/// `scan-complete` per root when done.
#[tauri::command]
pub fn start_scan_cmd(app: AppHandle, state: tauri::State<'_, AppState>) -> Result<ScanStarted, String> {
    let roots: Vec<String> = get_watched_folders()
        .map_err(|e| e.to_string())?
        .into_iter()
        .filter(|f| !is_folder_paused_mode(&f.mode))
        .map(|f| f.path)
        .collect();
    if roots.is_empty() {
        return Ok(ScanStarted {
            started: false,
            reason: Some("no_roots".into()),
        });
    }
    let is_scanning = state.is_scanning.clone();
    if is_scanning.swap(true, Ordering::SeqCst) {
        return Ok(ScanStarted {
            started: false,
            reason: Some("already_running".into()),
        });
    }
    std::thread::spawn(move || {
        scan::scan_roots(&roots, |event| match event {
            ScanEvent::Progress(p) => {
                let _ = app.emit("scan-progress", p);
            }
            ScanEvent::Complete(s) => {
                let _ = app.emit("scan-complete", s);
            }
        });
        is_scanning.store(false, Ordering::SeqCst);
    });
    Ok(ScanStarted {
        started: true,
        reason: None,
    })
}

/// Returns `true` while a scan is in progress.
#[tauri::command]
pub fn is_scanning_cmd(state: tauri::State<'_, AppState>) -> bool {
    state.is_scanning.load(Ordering::SeqCst)
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardStats {
    pub total_files: i64,
    pub total_bytes: i64,
    pub category_breakdown: Vec<CategoryStat>,
    pub largest_files: Vec<InventoryFile>,
    pub recent_files: Vec<InventoryFile>,
    pub watched_roots: Vec<RootStat>,
    pub last_scan_at: Option<i64>,
    pub insights: DashboardInsights,
    pub age_buckets: Vec<AgeBucket>,
}

/// Aggregate stats for the dashboard view.
#[tauri::command(async)]
pub fn get_dashboard_stats_cmd() -> Result<DashboardStats, String> {
    let stats = get_inventory_stats().map_err(|e| e.to_string())?;
    let category_breakdown = get_category_distribution().map_err(|e| e.to_string())?;
    let largest_files = get_inventory_files("size", None, None, None, 50).map_err(|e| e.to_string())?;
    let recent_files = get_inventory_files("mtime", None, None, None, 50).map_err(|e| e.to_string())?;
    let watched_roots = get_root_summaries().map_err(|e| e.to_string())?;
    let insights = get_dashboard_insights().map_err(|e| e.to_string())?;
    let age_buckets = get_age_buckets().map_err(|e| e.to_string())?;
    Ok(DashboardStats {
        total_files: stats.total_files,
        total_bytes: stats.total_bytes,
        category_breakdown,
        largest_files,
        recent_files,
        watched_roots,
        last_scan_at: stats.last_scan_at,
        insights,
        age_buckets,
    })
}

/// Browse inventory files with optional category, root, and name filters.
#[tauri::command(async)]
pub fn get_inventory_files_cmd(
    sort: Option<String>,
    category: Option<String>,
    root: Option<String>,
    query: Option<String>,
    limit: Option<i64>,
) -> Result<Vec<InventoryFile>, String> {
    let sort = sort.unwrap_or_else(|| "size".into());
    let cat = category.as_deref().filter(|s| !s.is_empty());
    let root = root.as_deref().filter(|s| !s.is_empty());
    let query = query.as_deref().filter(|s| !s.is_empty());
    let limit = limit.unwrap_or(50).clamp(1, 200);
    get_inventory_files(&sort, cat, root, query, limit).map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// Cleanup commands
// ---------------------------------------------------------------------------

/// Find duplicate files across the scanned inventory.
#[tauri::command(async)]
pub fn find_duplicates_cmd() -> Result<Vec<crate::cleanup::DuplicateGroup>, String> {
    if !crate::cleanup::has_inventory() {
        return Err("No scan data — run a scan first".to_string());
    }
    crate::cleanup::find_duplicates()
}

/// Find files larger than a given threshold (in bytes).
#[tauri::command]
pub fn find_large_files_cmd(min_bytes: i64) -> Result<Vec<crate::cleanup::CleanupFile>, String> {
    if !crate::cleanup::has_inventory() {
        return Err("No scan data — run a scan first".to_string());
    }
    crate::cleanup::find_large_files(min_bytes)
}

/// Find files not modified in the given number of days.
#[tauri::command]
pub fn find_stale_files_cmd(days: i64) -> Result<Vec<crate::cleanup::CleanupFile>, String> {
    if !crate::cleanup::has_inventory() {
        return Err("No scan data — run a scan first".to_string());
    }
    crate::cleanup::find_stale_files(days)
}

/// Find deepest empty directories under watched roots.
#[tauri::command(async)]
pub fn find_empty_dirs_cmd() -> Result<Vec<String>, String> {
    if !crate::cleanup::has_inventory() {
        return Err("No scan data — run a scan first".to_string());
    }
    crate::cleanup::find_empty_dirs()
}

/// Execute a batch of cleanup actions (trash files / remove empty dirs).
#[tauri::command(async)]
pub fn execute_cleanup_cmd(
    actions: Vec<crate::cleanup::CleanupRequest>,
) -> Result<Vec<crate::cleanup::CleanupOutcome>, String> {
    Ok(crate::cleanup::execute_cleanup(&actions))
}

// ---------------------------------------------------------------------------
// Suggestion commands (AI-assisted organization)
// ---------------------------------------------------------------------------

/// Get category suggestions for unclassified files in the inventory.
#[tauri::command(async)]
pub fn get_suggestions_cmd(limit: i64) -> Result<Vec<crate::classify::Suggestion>, String> {
    let provider = crate::classify::detect_provider();
    let limit = limit.clamp(1, 200) as usize;
    Ok(crate::classify::get_suggestions(limit, provider.as_ref()))
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AcceptOutcome {
    pub path: String,
    pub status: String, // "ok" | "missing" | "failed"
    pub message: Option<String>,
    pub dest: Option<String>,
}

/// Dismiss a suggestion so it no longer appears in the list.
#[tauri::command]
pub fn dismiss_suggestion_cmd(path: String) -> Result<(), String> {
    crate::db::dismiss_suggestion(&path).map_err(|e| e.to_string())
}

/// Move a file into its suggested category folder and optionally create a
/// matching rule. Returns a per-file status; never panics on missing files.
#[tauri::command]
pub fn accept_suggestion_cmd(
    path: String,
    suggested_category: String,
    create_rule: bool,
) -> Result<AcceptOutcome, String> {
    let src = Path::new(&path);
    if !src.exists() {
        return Ok(AcceptOutcome {
            path,
            status: "missing".to_string(),
            message: None,
            dest: None,
        });
    }

    let file_name = src
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let dest = src
        .parent()
        .map(|p| p.join(&suggested_category).join(&file_name))
        .unwrap_or_else(|| Path::new(&suggested_category).join(&file_name));

    let outcome = match move_file(src, &dest) {
        Ok(MoveOutcome::Moved) => AcceptOutcome {
            path: path.clone(),
            status: "ok".to_string(),
            message: None,
            dest: Some(dest.to_string_lossy().into_owned()),
        },
        Ok(MoveOutcome::MovedWithNewName(name)) => {
            let actual = dest.with_file_name(&name);
            AcceptOutcome {
                path: path.clone(),
                status: "ok".to_string(),
                message: None,
                dest: Some(actual.to_string_lossy().into_owned()),
            }
        }
        Err(e) => AcceptOutcome {
            path: path.clone(),
            status: "failed".to_string(),
            message: Some(e),
            dest: None,
        },
    };

    if outcome.status == "ok" {
        let _ = crate::db::log_action(&ActionLog {
            id: None,
            timestamp: chrono::Utc::now(),
            source_path: path.clone(),
            destination_path: outcome.dest.clone(),
            action: "move".to_string(),
            file_name: file_name.clone(),
            file_type: suggested_category.clone(),
            undone: false,
        });

        if create_rule {
            create_suggestion_rule(&file_name, &suggested_category);
        }

        let _ = crate::db::update_inventory_category(&path, &suggested_category);
        let _ = crate::db::clear_dismissed_suggestion(&path);
    }

    Ok(outcome)
}

/// Create a rule from a suggestion acceptance. Skips insertion if an enabled
/// move rule already covers the same extension with the same destination.
fn create_suggestion_rule(file_name: &str, category: &str) {
    let ext = file_name
        .rfind('.')
        .and_then(|dot| file_name.get(dot + 1..))
        .map(str::to_lowercase);
    let ext = match ext {
        Some(e) if !e.is_empty() => e,
        _ => return,
    };

    let already_covered = get_rules()
        .map(|rules| {
            rules.iter().any(|r| {
                r.enabled
                    && r.action == "move"
                    && r.destination == category
                    && (r.extensions.iter().any(|e| e == &ext) || r.extensions.contains(&"*".to_string()))
            })
        })
        .unwrap_or(false);
    if already_covered {
        return;
    }

    let _ = add_rule(&Rule {
        id: None,
        name: category.to_string(),
        priority: 10,
        enabled: true,
        extensions: vec![ext],
        pattern: None,
        destination: category.to_string(),
        action: "move".to_string(),
        folder_id: 0,
    });
}
