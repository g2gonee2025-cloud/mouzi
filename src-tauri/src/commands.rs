use crate::db::*;
use crate::ignore::{load_mouziignore, save_mouziignore};
use crate::rules::manual_scan_folder;
use crate::safe_fs::{is_within_any_root, move_file, MoveOutcome};
use crate::scan::{self, ScanEvent};
use crate::suppress::{SuppressionSet, SUPPRESSION_TTL};
use crate::AppState;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Instant;
// `MOVE_OBSERVER` and the two test modules that read it are the only users.
#[cfg(test)]
use std::sync::{Mutex, MutexGuard};
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

/// Reject a move rule that could only ever rename a file in place.
///
/// A destination of `.` or empty resolves to the watched root itself, so the
/// "move" becomes `fs::rename(<root>/report.pdf, <root>/report_0.pdf)`: the same
/// folder, a suffixed name. The watcher then reads that as the user creating a
/// file, and the next pass produces `report_0_0.pdf`, and so on until the app is
/// killed. Layer 1 now catches it and skips the file instead, so this is a
/// belt-and-braces check on the way in rather than the only thing standing
/// between a user and that loop — the guard in `rules::is_already_filed` does not
/// depend on this validation having run, and rules already in the database are
/// covered by it.
///
/// An absolute destination is exempt: it is resolved as written, so pointing at
/// a fixed folder is a legitimate thing to ask for.
fn validate_rule_destination(rule: &Rule) -> Result<(), String> {
    if rule.action != "move" {
        return Ok(());
    }
    let destination = rule.destination.trim();
    if destination.is_empty() || destination == "." || destination == ".." {
        return Err(format!(
            "Destination '{}' resolves to the folder itself, which would rename files in place forever. \
             Choose a subfolder, or a fixed path outside it.",
            rule.destination
        ));
    }
    Ok(())
}

#[tauri::command]
pub fn add_rule_cmd(rule: Rule) -> Result<i64, String> {
    validate_rule_destination(&rule)?;
    add_rule(&rule).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn update_rule_cmd(rule: Rule) -> Result<(), String> {
    validate_rule_destination(&rule)?;
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
// Undo
//
// The database is one global connection behind one mutex. Holding it while the
// filesystem is touched stalls every other database caller, and stalls the
// watcher's `notify` callback, which takes the same `ignored_files` lock on
// every event — so events arrive after the 30 s suppression window has closed,
// and a freshly restored file is read as new user activity, queued, and moved
// straight back after the grace period with its log row already marked undone.
// The app undoes its own undo. Do not hoist either lock back over the loop.
//
// Phase 1 reads the candidate rows under the DB lock and releases it; phase 2
// does the moves holding neither lock, after arming the whole batch's
// suppression window; phase 3 writes each row's outcome under a short lock.
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

/// The watcher's self-suppression guards, shared with `AppState`.
type IgnoredFiles = Arc<SuppressionSet>;

/// One `action_logs` row, copied out of the database in phase 1.
struct UndoTarget {
    id: i64,
    source: String,
    dest: Option<String>,
}

/// What phase 2 intends to do with a target, decided while holding no lock.
enum UndoPlan {
    /// Nothing to move back: no destination was recorded, or the file is no
    /// longer where the action left it.
    Gone,
    /// Move `dest` back onto `source`.
    Restore { dest: PathBuf, source: String },
}

/// What the filesystem half of an undo produced, before the database is told.
enum UndoMove {
    /// The file is back. `actual` is where it landed, which differs from
    /// `source` when a collision forced a new name.
    Restored { actual: String, status: &'static str },
    Gone,
    Failed(String),
}

/// Open the watcher's self-suppression window for `paths`.
///
/// Callers arm before the rename, never after: the watcher can deliver the
/// event for a restore the moment the move returns, and an event that arrives
/// before its guard exists is indistinguishable from the user creating a file.
fn arm_suppression(ignored_files: &IgnoredFiles, paths: &[String]) -> Result<(), String> {
    if paths.is_empty() {
        return Ok(());
    }
    let now = Instant::now();
    ignored_files.arm(
        &paths.iter().map(PathBuf::from).collect::<Vec<_>>(),
        now,
        SUPPRESSION_TTL,
    );
    Ok(())
}

/// Decide what has to happen for one target. `Path::exists` is a stat, so this
/// needs no lock and does no database work.
fn plan_undo(target: &UndoTarget) -> UndoPlan {
    let dest = match target.dest.as_deref() {
        Some(dest) if !dest.is_empty() => PathBuf::from(dest),
        _ => return UndoPlan::Gone,
    };
    if !dest.exists() {
        return UndoPlan::Gone;
    }
    UndoPlan::Restore {
        dest,
        source: target.source.clone(),
    }
}

/// The filesystem half of an undo. Takes no lock of any kind: the caller has
/// armed suppression already and writes the log row afterwards.
fn execute_undo_move(plan: &UndoPlan) -> UndoMove {
    let (dest, source) = match plan {
        UndoPlan::Gone => return UndoMove::Gone,
        UndoPlan::Restore { dest, source } => (dest.as_path(), source.as_str()),
    };
    let source_path = Path::new(source);
    match move_file(dest, source_path) {
        Ok(MoveOutcome::Moved) => UndoMove::Restored {
            actual: source.to_string(),
            status: "ok",
        },
        Ok(MoveOutcome::MovedWithNewName(name)) => UndoMove::Restored {
            actual: source_path
                .with_file_name(&name)
                .to_string_lossy()
                .to_string(),
            status: "collision",
        },
        Err(e) => UndoMove::Failed(e),
    }
}

/// Phases 2c and 3 for one planned undo: move with no lock held, arm any path
/// the move revealed, then record the outcome under a short lock.
fn finish_undo(
    target: &UndoTarget,
    plan: &UndoPlan,
    ignored_files: &IgnoredFiles,
) -> UndoResult {
    notify_move_observer();
    let moved = execute_undo_move(plan);

    // A collision renames the file, so the path the watcher will report is not
    // knowable until the move has run. Arm it the moment it exists.
    if let UndoMove::Restored { actual, .. } = &moved {
        if actual != &target.source {
            if let Err(e) = arm_suppression(ignored_files, std::slice::from_ref(actual)) {
                return failed_undo(e);
            }
        }
    }

    record_undo(target.id, &moved).unwrap_or_else(|e| failed_undo(e))
}

/// Phase 3: one short lock acquisition per row, after its move has finished.
fn record_undo(id: i64, moved: &UndoMove) -> Result<UndoResult, String> {
    let result = match moved {
        UndoMove::Restored { actual, status } => UndoResult {
            status: (*status).to_string(),
            message: None,
            restored_to: Some(actual.clone()),
        },
        // A row whose file is already gone is still marked undone and reported
        // as a success. That has always been this command's behaviour; the
        // audit recorded it separately and it is deliberately unchanged here.
        UndoMove::Gone => UndoResult {
            status: "missing".to_string(),
            message: None,
            restored_to: None,
        },
        UndoMove::Failed(e) => UndoResult {
            status: "failed".to_string(),
            message: Some(e.clone()),
            restored_to: None,
        },
    };

    if matches!(moved, UndoMove::Restored { .. } | UndoMove::Gone) {
        let conn = lock_db()?;
        conn.execute("UPDATE action_logs SET undone=1 WHERE id=?1", [id])
            .map_err(|e| e.to_string())?;
    }
    Ok(result)
}

fn failed_undo(message: String) -> UndoResult {
    UndoResult {
        status: "failed".to_string(),
        message: Some(message),
        restored_to: None,
    }
}

fn read_undo_target(id: i64) -> Result<UndoTarget, String> {
    let conn = lock_db()?;
    conn.query_row(
        "SELECT source_path, destination_path FROM action_logs WHERE id=?1 AND undone=0",
        [id],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
    )
    .map(|(source, dest)| UndoTarget { id, source, dest })
    .map_err(|e| format!("No undoable action found for id={}: {}", id, e))
}

fn undo_action_with(id: i64, ignored_files: &IgnoredFiles) -> Result<UndoResult, String> {
    // Phase 1 — read the row, then let the lock go.
    let target = read_undo_target(id)?;
    let plan = plan_undo(&target);

    // Phase 2 — arm before the move. This path used to insert its guard after
    // the rename, so a fast watcher event could arrive while the restored file
    // still looked brand new.
    if let UndoPlan::Restore { dest, source } = &plan {
        arm_suppression(
            ignored_files,
            &[dest.to_string_lossy().to_string(), source.clone()],
        )?;
    }

    // Phases 2c and 3.
    Ok(finish_undo(&target, &plan, ignored_files))
}

fn undo_all_with(ignored_files: &IgnoredFiles) -> Result<UndoAllResult, String> {
    // Phase 1 — read the candidates. The callee takes the DB lock and releases
    // it before returning.
    let targets: Vec<UndoTarget> = crate::db::get_undoable_logs()
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|(id, source, dest)| UndoTarget { id, source, dest })
        .collect();

    // Phase 2a — decide what each row needs, holding nothing.
    let plans: Vec<UndoPlan> = targets.iter().map(plan_undo).collect();

    // Phase 2b — arm the whole batch before the first rename, so every path is
    // covered up front instead of one at a time as the moves land.
    let mut armed: Vec<String> = Vec::with_capacity(plans.len() * 2);
    for plan in &plans {
        if let UndoPlan::Restore { dest, source } = plan {
            armed.push(dest.to_string_lossy().to_string());
            armed.push(source.clone());
        }
    }
    arm_suppression(ignored_files, &armed)?;

    // Phase 2c + 3 — move, then record, one row at a time in input order.
    let mut results = Vec::with_capacity(targets.len());
    for (target, plan) in targets.iter().zip(plans) {
        results.push(finish_undo(target, &plan, ignored_files));
    }

    // "missing" has always counted towards `count`. Separate audit finding,
    // deliberately unchanged.
    let count = results.iter().filter(|r| r.status != "failed").count();
    Ok(UndoAllResult { count, results })
}

/// Test-only hook, run immediately before each rename with no lock held.
///
/// The concurrency tests read the lock state from inside the undo loop rather
/// than from another thread racing it, so the assertion lands on the exact
/// moment the filesystem work begins instead of an arbitrary one. That is the
/// only way to test the property without depending on how long a rename takes.
#[cfg(test)]
type MoveObserver = Arc<dyn Fn() + Send + Sync>;

#[cfg(test)]
static MOVE_OBSERVER: Mutex<Option<MoveObserver>> = Mutex::new(None);

#[cfg(test)]
fn set_move_observer(observer: Option<MoveObserver>) -> Option<MoveObserver> {
    let mut slot = MOVE_OBSERVER.lock().unwrap_or_else(|p| p.into_inner());
    std::mem::replace(&mut *slot, observer)
}

#[cfg(test)]
fn notify_move_observer() {
    let observer = MOVE_OBSERVER
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone();
    if let Some(observer) = observer {
        observer();
    }
}

#[cfg(not(test))]
#[inline]
fn notify_move_observer() {}

#[tauri::command(async)]
pub fn undo_action_cmd(id: i64, state: tauri::State<AppState>) -> Result<UndoResult, String> {
    undo_action_with(id, &state.ignored_files)
}

#[tauri::command(async)]
pub fn undo_all_cmd(state: tauri::State<AppState>) -> Result<UndoAllResult, String> {
    undo_all_with(&state.ignored_files)
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
#[tauri::command]
pub fn import_rules_cmd(path: String, replace: bool) -> Result<usize, String> {
    let data = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let rules: Vec<Rule> = serde_json::from_str(&data).map_err(|e| e.to_string())?;

    // Validate the whole file before touching the table. A file of rules where
    // one is unusable has no useful half-applied state, and the alternative is a
    // `delete_all_rules` that has already run.
    for (index, rule) in rules.iter().enumerate() {
        validate_rule_destination(rule)
            .map_err(|e| format!("Rule {} of {} ({}) is not usable: {e}", index + 1, rules.len(), rule.name))?;
    }

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

/// The watched root that contains `path`, most specific first.
///
/// Containment rather than parent equality, because the file may sit in a
/// subfolder — that is the normal case once the watch is recursive, and
/// ambiguous the moment two watched roots nest. Of the roots that contain the
/// file, the one with the most path components is the one it "belongs" to, so
/// the answer does not depend on the order rows happen to come back in.
///
/// `None` when no watched root contains the file. The caller then falls back to
/// the file's own parent, which is the pre-existing behaviour for a file outside
/// every watched folder.
fn owning_watched_root(path: &Path) -> Option<PathBuf> {
    let roots: Vec<PathBuf> = get_watched_folders()
        .ok()?
        .into_iter()
        .map(|f| PathBuf::from(f.path))
        .collect();
    roots
        .iter()
        .filter(|root| is_within_any_root(path, std::slice::from_ref(*root)))
        .max_by_key(|root| root.components().count())
        .cloned()
}

/// Move a file into its suggested category folder and optionally create a
/// matching rule. Returns a per-file status; never panics on missing files.
#[tauri::command(async)]
pub fn accept_suggestion_cmd(
    path: String,
    suggested_category: String,
    create_rule: bool,
    state: tauri::State<AppState>,
) -> Result<AcceptOutcome, String> {
    let root = owning_watched_root(Path::new(&path))
        .or_else(|| Path::new(&path).parent().map(|p| p.to_path_buf()))
        .unwrap_or_default();
    accept_suggestion_with(
        path,
        suggested_category,
        create_rule,
        &state.ignored_files,
        &root,
    )
}

/// The body of `accept_suggestion_cmd`, callable without a Tauri state.
///
/// This command does not go through `rules::process_file`, so it builds its own
/// destination and needs its own copy of both guards. Before this, the watcher
/// saw the result of every accepted suggestion as brand new user activity with
/// nothing standing in front of it — and under a recursive watch accepting a
/// suggestion for `C:\W\Documents\report.pdf` would file it at
/// `C:\W\Documents\Documents\report.pdf`, the app's own output feeding its own
/// input.
fn accept_suggestion_with(
    path: String,
    suggested_category: String,
    create_rule: bool,
    ignored_files: &IgnoredFiles,
    root: &Path,
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

    // Layer 1, applied by hand because this path never reaches
    // `rules::is_already_filed`. `suggested_category` is a bare folder name, so
    // the destination directory has the same shape `resolve_dest_dir` produces:
    // the category under the watched root.
    let dest_dir = root.join(&suggested_category);
    if let Some(src_parent) = src.parent() {
        if is_within_any_root(src_parent, std::slice::from_ref(&dest_dir)) {
            // Already filed, so nothing moves and nothing is logged. The early
            // return is the point: falling through to `log_action` would write
            // the exact row this check exists to prevent — `source` equal to
            // `destination`.
            return Ok(AcceptOutcome {
                path: path.clone(),
                status: "ok".to_string(),
                message: None,
                // Where the file actually is, which is where it stays. The UI
                // shows this next to "accepted", and "no arrow" would read as a
                // failure rather than as "nothing needed doing".
                dest: Some(path.clone()),
            });
        }
    }

    // `move_file` runs its own collision naming, and it does not count upwards
    // the way `unique_destination` does, so the predicted name can differ from
    // the name the move actually takes. That is why the real path is armed again
    // afterwards rather than only predicted now.
    let predicted = crate::safe_fs::unique_destination(&dest_dir, &file_name);
    arm_suppression(
        ignored_files,
        &[src.to_string_lossy().into_owned(), predicted.to_string_lossy().into_owned()],
    )?;

    notify_move_observer();
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
        if let Some(actual) = &outcome.dest {
            arm_suppression(ignored_files, std::slice::from_ref(actual))?;
        }
    }

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

#[cfg(test)]
mod undo_tests {
    use super::*;
    use crate::db::{get_db, init_test_db, lock_db, serialise_test_db};
    use std::sync::atomic::AtomicBool;
    use std::sync::mpsc;
    use std::time::Duration;

    /// Empties `action_logs`. Reports rather than unwraps so `UndoFixture::drop`
    /// can use it while the test is already unwinding.
    fn clear_logs() -> Result<(), String> {
        lock_db()?
            .execute("DELETE FROM action_logs", [])
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    /// Everything one undo test puts on disk: the shared-database lock, a
    /// private temp tree, and the `action_logs` rows it writes.
    ///
    /// The lock is held for the test's whole body, so `Drop` still runs inside
    /// the critical section and cannot interleave with another module's fixture.
    /// Cleanup on drop is the point: the batch query is "every row with
    /// undone = 0", so the ~200 rows a failed test would leave behind would be
    /// swept into the next test's batch and counted against its assertions.
    struct UndoFixture {
        _serialised: MutexGuard<'static, ()>,
        root: PathBuf,
    }

    impl UndoFixture {
        fn new(name: &str) -> Self {
            let serialised = serialise_test_db();
            init_test_db();
            // Rows another module left behind would decide what this batch
            // contains, so the fixture owns the whole table while it runs.
            clear_logs().expect("clear action_logs");

            let root = std::env::temp_dir()
                .join(format!("mouzi-undo-{}-{}", name, std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&root).expect("create test root");
            UndoFixture { _serialised: serialised, root }
        }

        fn root(&self) -> &Path {
            &self.root
        }
    }

    impl Drop for UndoFixture {
        fn drop(&mut self) {
            // Reported, never unwrapped: a second panic while the test is
            // unwinding would abort the binary and lose every other result.
            if let Err(e) = clear_logs() {
                eprintln!("undo test fixture: action_logs not cleared: {e}");
            }
            if let Err(e) = std::fs::remove_dir_all(&self.root) {
                if e.kind() != std::io::ErrorKind::NotFound {
                    eprintln!("undo test fixture: {} not removed: {e}", self.root.display());
                }
            }
        }
    }

    /// Puts the previous observer back when the test ends, panic or not. The
    /// observer is process-global, and one left installed would fire inside the
    /// next test's moves and fill its samples with foreign events.
    struct ObserverReset(Option<MoveObserver>);

    impl Drop for ObserverReset {
        fn drop(&mut self) {
            set_move_observer(self.0.take());
        }
    }

    fn insert_log(timestamp: &str, source: &str, dest: Option<&str>) -> i64 {
        let conn = lock_db().expect("lock db");
        conn.execute(
            "INSERT INTO action_logs (timestamp, source_path, destination_path, action, file_name, file_type, undone)
             VALUES (?1, ?2, ?3, 'move', 'f.undocheck', 'Other', 0)",
            rusqlite::params![timestamp, source, dest],
        )
        .expect("insert log");
        conn.last_insert_rowid()
    }

    fn undone_of(source: &str) -> i64 {
        let conn = lock_db().expect("lock db");
        conn.query_row(
            "SELECT undone FROM action_logs WHERE source_path = ?1",
            [source],
            |row| row.get(0),
        )
        .expect("log row")
    }

    fn ignored_files() -> IgnoredFiles {
        Arc::new(SuppressionSet::new())
    }

    /// Put a file where a logged move left it, and return (source, dest).
    fn stage(root: &Path, name: &str) -> (String, String) {
        let source = root.join("in").join(name);
        let dest = root.join("out").join(name);
        std::fs::create_dir_all(source.parent().unwrap()).unwrap();
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        std::fs::write(&dest, b"payload").unwrap();
        (
            source.to_string_lossy().into_owned(),
            dest.to_string_lossy().into_owned(),
        )
    }

    /// Log `source -> dest` at a timestamp that fixes its position in the
    /// `ORDER BY timestamp DESC` batch.
    fn log_at(rank: u32, source: &str, dest: Option<&str>) {
        insert_log(&format!("2026-01-01T00:00:{rank:02}Z"), source, dest);
    }

    /// A hook that records which paths are already guarded the instant the
    /// first rename is about to run, and does nothing on later moves.
    fn snapshot_guard_on_first_move(
        ignored: &IgnoredFiles,
        into: &Arc<Mutex<Vec<String>>>,
    ) -> Option<MoveObserver> {
        let fired = Arc::new(AtomicBool::new(false));
        let into = into.clone();
        let ignored = ignored.clone();
        set_move_observer(Some(Arc::new(move || {
            if fired.swap(true, Ordering::SeqCst) {
                return;
            }
            let mut guarded = ignored.guarded_keys();
            guarded.sort();
            *into.lock().unwrap() = guarded;
        })))
    }

    /// The key the guard map stores a path under.
    ///
    /// The set normalises lexically and lowercases, so an expectation written
    /// with the temp directory's original casing can never match an armed guard
    /// and the test would fail for a reason that has nothing to do with arming.
    /// Normalising the expectation with the same function the map uses keeps the
    /// assertion about what it was always about: this path was guarded.
    fn guarded_key(path: &str) -> String {
        crate::safe_fs::normalize_lexically(Path::new(path))
            .to_string_lossy()
            .to_lowercase()
    }

    #[test]
    fn undo_batch_holds_neither_lock_while_it_moves_a_file() {
        let fixture = UndoFixture::new("locks");
        let root = fixture.root();
        let ignored = ignored_files();
        for rank in 0..3 {
            let (source, dest) = stage(root, &format!("a{rank}.undocheck"));
            log_at(rank, &source, Some(&dest));
        }

        let samples = Arc::new(Mutex::new(Vec::new()));
        let observer_samples = samples.clone();
        let observer_ignored = ignored.clone();
        let _observer = ObserverReset(set_move_observer(Some(Arc::new(move || {
            let db_free = get_db().try_lock().is_ok();
            let watcher_free = observer_ignored.is_lock_free();
            observer_samples.lock().unwrap().push((db_free, watcher_free));
        }))));

        let outcome = undo_all_with(&ignored);

        let outcome = outcome.expect("undo_all_with");
        assert_eq!(outcome.results.len(), 3);
        assert!(outcome.results.iter().all(|r| r.status == "ok"), "{:?}", outcome.results);

        let samples = samples.lock().unwrap();
        assert_eq!(samples.len(), 3, "the hook must run once per move");
        for (db_free, watcher_free) in samples.iter() {
            assert!(*db_free, "the global DB mutex was held while a file was being moved");
            assert!(*watcher_free, "the watcher suppression mutex was held while a file was being moved");
        }
    }

    /// The regression the three-phase split exists for: the old batch owned the
    /// DB mutex across every move, so a reader on another thread — the
    /// scanner's 500-row flush, the dashboard's stat queries — could not get in
    /// until the batch was already over.
    #[test]
    fn a_reader_on_another_thread_reaches_the_database_during_an_undo_batch() {
        let fixture = UndoFixture::new("concurrent");
        let root = fixture.root();
        let ignored = ignored_files();
        for rank in 0..200u32 {
            let (source, dest) = stage(root, &format!("b{rank}.undocheck"));
            insert_log(
                &format!("2026-01-01T00:00:{rank:03}Z"),
                &source,
                Some(&dest),
            );
        }

        // The batch parks inside its first move and only then releases the
        // reader, so the reader is guaranteed to be competing for the database
        // while a rename is in flight.
        let (go_tx, go_rx) = mpsc::channel::<()>();
        let (got_tx, got_rx) = mpsc::channel::<bool>();
        let got_rx = Mutex::new(got_rx);
        let batch_done = Arc::new(AtomicBool::new(false));
        let handshake = Arc::new(Mutex::new(Vec::new()));

        let reader_db = get_db();
        let reader_done = batch_done.clone();
        let reader = std::thread::spawn(move || {
            // Bounded, so a batch that never reaches a move fails the test
            // instead of hanging it.
            if go_rx.recv_timeout(Duration::from_secs(30)).is_err() {
                return;
            }
            let conn = reader_db.lock().expect("db lock");
            let batch_was_still_running = !reader_done.load(Ordering::SeqCst);
            drop(conn);
            let _ = got_tx.send(batch_was_still_running);
        });

        let fired = Arc::new(AtomicBool::new(false));
        let _observer = ObserverReset(set_move_observer(Some({
            let fired = fired.clone();
            let go_tx = go_tx.clone();
            let handshake = handshake.clone();
            Arc::new(move || {
                if fired.swap(true, Ordering::SeqCst) {
                    return;
                }
                go_tx.send(()).expect("the reader is waiting");
                let reached = got_rx.lock().unwrap().recv_timeout(Duration::from_secs(10)).ok();
                handshake.lock().unwrap().push(reached);
            })
        })));

        let batch_ignored = ignored.clone();
        let batch_done = batch_done.clone();
        let batch = std::thread::spawn(move || {
            let result = undo_all_with(&batch_ignored);
            batch_done.store(true, Ordering::SeqCst);
            result
        });

        let outcome = batch.join().expect("the undo batch thread panicked");
        reader.join().expect("the reader thread panicked");

        let outcome = outcome.expect("undo_all_with");
        assert_eq!(outcome.count, 200);
        assert!(outcome.results.iter().all(|r| r.status == "ok"));

        let handshake = handshake.lock().unwrap();
        assert_eq!(handshake.len(), 1, "the handshake runs once, on the first move");
        assert!(
            handshake[0].is_some(),
            "a reader on another thread could not reach the database during a move"
        );
        assert!(
            handshake[0].unwrap(),
            "the reader only reached the database after the batch had finished"
        );
    }

    /// The old batch armed each path after its own move, so a watcher event
    /// racing a later row found no guard at all.
    #[test]
    fn undo_batch_arms_every_path_before_the_first_move() {
        let fixture = UndoFixture::new("armed");
        let root = fixture.root();
        let ignored = ignored_files();
        let mut sources = Vec::new();
        for rank in 0..4 {
            let (source, dest) = stage(root, &format!("c{rank}.undocheck"));
            log_at(rank, &source, Some(&dest));
            sources.push(source);
        }

        let at_first_move = Arc::new(Mutex::new(Vec::new()));
        let _observer = ObserverReset(snapshot_guard_on_first_move(&ignored, &at_first_move));

        let outcome = undo_all_with(&ignored);

        assert_eq!(outcome.expect("undo_all_with").count, 4);

        let guarded = at_first_move.lock().unwrap();
        assert_eq!(
            guarded.len(),
            8,
            "both paths of all four rows must be guarded before the first rename: {guarded:?}"
        );
        for source in &sources {
            assert!(
                guarded.contains(&guarded_key(source)),
                "{source} had no guard when the first rename ran"
            );
        }
    }

    /// The single-undo path inserted its guard after the rename, so a fast
    /// watcher event could arrive while the restored file still looked new.
    #[test]
    fn single_undo_arms_the_guard_before_the_move() {
        let fixture = UndoFixture::new("single_arm");
        let root = fixture.root();
        let ignored = ignored_files();
        let (source, dest) = stage(root, "d0.undocheck");
        let id = insert_log("2026-01-01T00:00:00Z", &source, Some(&dest));

        let at_first_move = Arc::new(Mutex::new(Vec::new()));
        let _observer = ObserverReset(snapshot_guard_on_first_move(&ignored, &at_first_move));

        let outcome = undo_action_with(id, &ignored);

        let outcome = outcome.expect("undo_action_with");
        assert_eq!(outcome.status, "ok");
        assert_eq!(outcome.restored_to.as_deref(), Some(source.as_str()));
        assert!(Path::new(&source).exists(), "the file should be back");
        assert_eq!(undone_of(&source), 1);

        let guarded = at_first_move.lock().unwrap();
        assert!(
            guarded.contains(&guarded_key(&source)),
            "the restored path had no guard when the rename ran: {guarded:?}"
        );
        assert!(
            guarded.contains(&guarded_key(&dest)),
            "the path the file left had no guard when the rename ran: {guarded:?}"
        );
    }

    /// Pins the frontend contract: one result per input row, in the order the
    /// batch query returned them, with the four status values unchanged.
    #[test]
    fn undo_all_reports_one_result_per_row_in_input_order() {
        let fixture = UndoFixture::new("statuses");
        let root = fixture.root();
        let ignored = ignored_files();

        // 0: a plain restore.
        let (source0, dest0) = stage(root, "e0.undocheck");
        log_at(0, &source0, Some(&dest0));

        // 1: something already occupies the source, so the restore is renamed.
        let (source1, dest1) = stage(root, "e1.undocheck");
        std::fs::write(&source1, b"in the way").unwrap();
        log_at(1, &source1, Some(&dest1));

        // 2: a destination was recorded but the file is no longer there.
        let source2 = root.join("in").join("e2.undocheck");
        let source2 = source2.to_string_lossy().into_owned();
        let dest2 = root.join("out").join("e2.undocheck");
        let dest2 = dest2.to_string_lossy().into_owned();
        log_at(2, &source2, Some(&dest2));

        // 3: no destination was ever recorded.
        let source3 = root.join("in").join("e3.undocheck");
        let source3 = source3.to_string_lossy().into_owned();
        log_at(3, &source3, None);

        // 4: a plain file sits where the source's parent has to be, so
        // move_file cannot create the parents and reports a real failure.
        let blocker = root.join("in").join("blocker");
        std::fs::write(&blocker, b"not a directory").unwrap();
        let dest4 = root.join("out").join("e4.undocheck");
        let source4 = blocker.join("e4.undocheck").to_string_lossy().into_owned();
        let dest4_str = dest4.to_string_lossy().into_owned();
        std::fs::write(&dest4, b"payload").unwrap();
        log_at(4, &source4, Some(&dest4_str));

        let outcome = undo_all_with(&ignored).expect("undo_all_with");

        // The batch query is ORDER BY timestamp DESC, so rank 4 comes first.
        let statuses: Vec<&str> = outcome.results.iter().map(|r| r.status.as_str()).collect();
        assert_eq!(statuses, vec!["failed", "missing", "missing", "collision", "ok"]);
        assert_eq!(outcome.count, 4, "only a failure is excluded from count");

        let restored: Vec<Option<&str>> = outcome
            .results
            .iter()
            .map(|r| r.restored_to.as_deref())
            .collect();
        assert_eq!(restored[4], Some(source0.as_str()));
        assert!(restored[3].unwrap().ends_with("e1_0.undocheck"), "{restored:?}");
        assert!(Path::new(&source1).exists(), "the file that was in the way must survive");
        assert!(!Path::new(&dest1).exists(), "the restored file must leave its organised location");
        assert!(dest4.exists(), "a failed move must leave the file alone");

        // A failure is the one outcome that does not mark the row undone.
        assert_eq!(undone_of(&source4), 0);
        assert_eq!(undone_of(&source0), 1);
        assert_eq!(undone_of(&source2), 1);
        assert_eq!(undone_of(&source3), 1);

        // The suffixed name is only knowable after the move, so it is armed
        // then — the path the watcher will actually report.
        {
            assert!(
                ignored.is_guarded(restored[3].unwrap()),
                "the collision's new name was never guarded"
            );
        }
    }
}

#[cfg(test)]
mod accept_tests {
    use super::*;
    use crate::db::{get_db, init_test_db, lock_db, serialise_test_db};

    /// Everything one accept test puts on disk: the shared-database lock, a
    /// private temp tree, and a clean `action_logs`. `UndoFixture`'s copy is not
    /// reused because its cleanup contract is stated in terms of the undo batch
    /// query, and "this table is empty" is a different promise.
    struct AcceptFixture {
        _serialised: MutexGuard<'static, ()>,
        root: PathBuf,
    }

    impl AcceptFixture {
        fn new(name: &str) -> Self {
            let serialised = serialise_test_db();
            init_test_db();
            lock_db()
                .expect("lock db")
                .execute("DELETE FROM action_logs", [])
                .expect("clear action_logs");

            let root = std::env::temp_dir()
                .join(format!("mouzi-accept-{}-{}", name, std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&root).expect("create test root");
            AcceptFixture { _serialised: serialised, root }
        }

        fn root(&self) -> &Path {
            &self.root
        }

        fn put(&self, relative: &str) -> PathBuf {
            let path = self.root.join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).expect("create parent");
            std::fs::write(&path, b"payload").expect("write fixture file");
            path
        }
    }

    impl Drop for AcceptFixture {
        fn drop(&mut self) {
            if let Err(e) = lock_db().and_then(|c| {
                c.execute("DELETE FROM action_logs", [])
                    .map_err(|e| e.to_string())
            }) {
                eprintln!("accept test fixture: action_logs not cleared: {e}");
            }
            if let Err(e) = std::fs::remove_dir_all(&self.root) {
                if e.kind() != std::io::ErrorKind::NotFound {
                    eprintln!("accept test fixture: {} not removed: {e}", self.root.display());
                }
            }
        }
    }

    /// Puts the previous observer back when the test ends, panic or not, so a
    /// failure here cannot fire inside the next test's move.
    struct ObserverReset(Option<MoveObserver>);

    impl Drop for ObserverReset {
        fn drop(&mut self) {
            set_move_observer(self.0.take());
        }
    }

    /// T10, and T8's arming half, on the one move site stage 1 adds.
    ///
    /// `accept_suggestion_with` moves a file inside a watched root, and before
    /// this change it did so with the guard wide open. Two properties, sampled
    /// by the same hook at the exact instant before the rename: the paths are
    /// already guarded, and neither the database nor the suppression set is held
    /// while the filesystem is touched. The lock half is the same assertion the
    /// undo batch makes, applied to the path that was not covered by it.
    #[test]
    fn accept_suggestion_arms_before_the_move_and_holds_neither_lock() {
        let fixture = AcceptFixture::new("armed");
        let source = fixture.put("report.pdf");
        let ignored: IgnoredFiles = Arc::new(SuppressionSet::new());

        let samples = Arc::new(Mutex::new(Vec::new()));
        let observer_samples = samples.clone();
        let observer_ignored = ignored.clone();
        let _observer = ObserverReset(set_move_observer(Some(Arc::new(move || {
            let db_free = get_db().try_lock().is_ok();
            let watcher_free = observer_ignored.is_lock_free();
            let mut guarded = observer_ignored.guarded_keys();
            guarded.sort();
            observer_samples.lock().unwrap().push((db_free, watcher_free, guarded));
        }))));

        let outcome = accept_suggestion_with(
            source.to_string_lossy().into_owned(),
            "Documents".to_string(),
            false,
            &ignored,
            fixture.root(),
        )
        .expect("accept_suggestion_with");

        assert_eq!(outcome.status, "ok", "{outcome:?}");
        let dest = PathBuf::from(outcome.dest.expect("a move reports a destination"));
        assert_eq!(dest, fixture.root().join("Documents").join("report.pdf"));
        assert!(dest.is_file(), "the file should have moved: {dest:?}");

        let samples = samples.lock().unwrap();
        assert_eq!(samples.len(), 1, "the hook must run once, before the move");
        let (db_free, watcher_free, guarded) = &samples[0];
        assert!(*db_free, "the global DB mutex was held while a file was being moved");
        assert!(*watcher_free, "the suppression set was held while a file was being moved");

        let source_key = crate::safe_fs::normalize_lexically(&source).to_string_lossy().to_lowercase();
        assert!(
            guarded.contains(&source_key),
            "the source had no guard when the rename ran: {guarded:?}"
        );
        assert!(
            guarded.contains(&dest.to_string_lossy().to_lowercase()),
            "the predicted destination had no guard when the rename ran: {guarded:?}"
        );
    }

    /// T8's Layer 1 half: accepting a suggestion for a file that is already in
    /// the folder the suggestion names must move nothing.
    ///
    /// This is the shape the recursive watch would feed itself — the app's own
    /// output, offered back as new work — and the reason this command carries
    /// its own path-shape check rather than relying on `process_file`.
    #[test]
    fn accept_suggestion_leaves_a_file_that_is_already_in_its_category_alone() {
        let fixture = AcceptFixture::new("layer1");
        let source = fixture.put("Documents/report.pdf");
        let ignored: IgnoredFiles = Arc::new(SuppressionSet::new());

        let outcome = accept_suggestion_with(
            source.to_string_lossy().into_owned(),
            "Documents".to_string(),
            false,
            &ignored,
            fixture.root(),
        )
        .expect("accept_suggestion_with");

        assert_eq!(outcome.status, "ok", "{outcome:?}");
        assert!(
            source.is_file(),
            "the file must be left where it already is: {source:?}"
        );
        assert!(
            !fixture.root().join("Documents").join("Documents").exists(),
            "the file was filed a second time into its own category folder"
        );
        assert!(
            ignored.guarded_keys().is_empty(),
            "nothing was armed, because nothing was moved"
        );

        // The row this check exists to prevent: an action logged for a move that
        // never happened, with the source equal to the destination.
        let same_path_rows: i64 = lock_db()
            .expect("lock db")
            .query_row(
                "SELECT COUNT(*) FROM action_logs WHERE source_path = destination_path",
                [],
                |row| row.get(0),
            )
            .expect("count rows");
        assert_eq!(
            same_path_rows, 0,
            "a no-op acceptance wrote an action_logs row with source == destination"
        );
    }

    /// A missing file is still a per-file status, not an error: the popup offers
    /// a list and any entry in it can be gone by the time it is clicked.
    #[test]
    fn accept_suggestion_reports_a_missing_file_without_touching_anything() {
        let fixture = AcceptFixture::new("missing");
        let absent = fixture.root().join("not-here.pdf");
        let ignored: IgnoredFiles = Arc::new(SuppressionSet::new());

        let outcome = accept_suggestion_with(
            absent.to_string_lossy().into_owned(),
            "Documents".to_string(),
            false,
            &ignored,
            fixture.root(),
        )
        .expect("accept_suggestion_with");

        assert_eq!(outcome.status, "missing");
        assert!(outcome.dest.is_none());
        assert!(ignored.guarded_keys().is_empty());
    }
}
