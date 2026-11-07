use crate::db::*;
use crate::ignore::{load_mouziignore, save_mouziignore};
use crate::rules::{scan_watched_manual_folder, WatchedManualRoot};
use crate::scan::{self, ScanEvent};
use crate::suppress::{SuppressionSet, SUPPRESSION_TTL};
use crate::AppState;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Instant;
use std::collections::HashMap;
use once_cell::sync::Lazy;
use std::sync::Mutex as PlanMutex;
// `MOVE_OBSERVER` and the two test modules that read it are the only users.
#[cfg(test)]
use std::sync::{Mutex, MutexGuard};
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_autostart::ManagerExt;
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_notification::NotificationExt;

#[tauri::command]
pub async fn preview_cmd(
    window: tauri::WebviewWindow, paths: Option<Vec<String>>,
) -> Result<crate::operations::Preview, String> {
    if window.label() != "app" {
        return Err("Organization review is available only in the workspace window".into());
    }
    let suppression = window.state::<AppState>().ignored_files.clone();
    tauri::async_runtime::spawn_blocking(move || crate::operations::preview(paths, &suppression))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn apply_preview_cmd(
    window: tauri::WebviewWindow,
    id: String,
    selected: Vec<String>,
) -> Result<Vec<crate::operations::OperationResult>, String> {
    if window.label() != "app" {
        return Err("Organization review is available only in the workspace window".into());
    }
    let suppression = window.state::<AppState>().ignored_files.clone();
    let app = window.app_handle().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let results = crate::operations::apply(&id, &selected, &suppression)?;
        if !results.is_empty() {
            let _ = app.emit("mutation-completed", ());
        }
        Ok(results)
    })
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub fn discard_preview_cmd(window: tauri::WebviewWindow, id: String) -> Result<(), String> {
    if window.label() != "app" {
        return Err("Organization review is available only in the workspace window".into());
    }
    crate::operations::discard(&id);
    Ok(())
}

#[tauri::command]
pub fn history_cmd(filter: crate::operations::HistoryFilter) -> Result<Vec<OrganizationLog>, String> {
    crate::operations::history(&filter)
}

#[tauri::command]
pub async fn undo_selected_cmd(
    window: tauri::WebviewWindow, mut ids: Vec<i64>,
) -> Result<Vec<crate::operations::OperationResult>, String> {
    if window.label() != "app" {
        return Err("History restoration is available only in the workspace window".into());
    }
    let ignored = window.state::<AppState>().ignored_files.clone();
    let app = window.app_handle().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let _operation = ignored.with_operation();
        ids.sort_unstable_by(|a, b| b.cmp(a));
        ids.dedup();
        let mut results = Vec::with_capacity(ids.len());
        for id in ids {
            let target = read_undo_target(id);
            let (source, undo) = match target {
                Ok(target) => (target.source.clone(), finish_undo(&target, &ignored).unwrap_or_else(failed_undo)),
                Err(error) => (String::new(), failed_undo(error)),
            };
            let success = matches!(undo.status.as_str(), "ok" | "collision");
            results.push(crate::operations::OperationResult {
                id: id.to_string(), source, success,
                error: (!success).then(|| undo.message.unwrap_or_else(|| "history.unverified".into())),
            });
        }
        if !results.is_empty() {
            let _ = app.emit("mutation-completed", ());
        }
        Ok(results)
    }).await.map_err(|error| error.to_string())?
}

#[tauri::command(async)]
pub fn set_only_new_cmd(window: tauri::WebviewWindow, id: i64, enabled: bool) -> Result<(), String> {
    if window.label() != "app" { return Err("Watched folder settings are available only in the workspace window".into()); }
    let app = window.app_handle();
    with_configuration(app, || crate::db::set_only_new(id, enabled))?;
    let _ = app.emit("folders-changed", ());
    Ok(())
}

#[tauri::command]
pub fn show_review_cmd(app: AppHandle, paths: Vec<String>) -> Result<(), String> {
    let state = app.try_state::<AppState>().ok_or("Application coordinator is unavailable")?;
    *state.review_request.lock().map_err(|_| "Organization review request is unavailable")? = Some(paths);
    crate::tray::show_settings_window(&app);
    let _ = app.emit("open-review", ());
    Ok(())
}

#[tauri::command]
pub fn take_review_request_cmd(window: tauri::WebviewWindow) -> Result<Option<Vec<String>>, String> {
    if window.label() != "app" { return Err("Organization review is available only in the workspace window".into()); }
    let state = window.state::<AppState>();
    let request = state.review_request.lock().map_err(|_| "Organization review request is unavailable")?.take();
    Ok(request)
}

#[tauri::command]
pub fn take_add_folder_request_cmd(window: tauri::WebviewWindow) -> Result<Option<String>, String> {
    if window.label() != "app" { return Err("Watched folder requests are available only in the workspace window".into()); }
    let state = window.state::<AppState>();
    let request = state.add_folder_request.lock().map_err(|_| "Watched folder request is unavailable")?.take();
    Ok(request)
}

#[tauri::command]
pub fn explorer_integration_cmd(window: tauri::WebviewWindow, enabled: bool) -> Result<(), String> {
    if window.label() != "app" || cfg!(debug_assertions) {
        return Err("Explorer integration requires the production workspace".into());
    }
    crate::integration::set_enabled(enabled)
}

#[tauri::command]
pub fn explorer_integration_status_cmd() -> (bool, bool) {
    let available = cfg!(target_os = "windows") && !cfg!(debug_assertions);
    (available, available && crate::integration::enabled())
}

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
        "zh" => "zh-CN".to_string(),
        "vi" => "vi".to_string(),
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
    if !matches!(rule.action.as_str(), "move" | "rename" | "delete" | "ignore") {
        return Err(format!("Unsupported rule action: {}", rule.action));
    }
    if rule.name.trim().is_empty() {
        return Err("Rule name cannot be empty".into());
    }
    let extensions = normalize_extensions(&rule.extensions);
    if extensions.is_empty() || extensions.iter().any(|ext| ext != "*" && !ext.chars().all(|ch| ch.is_alphanumeric() || matches!(ch, '_' | '-'))) {
        return Err("Provide at least one extension or *".into());
    }
    if let Some(pattern) = rule.pattern.as_deref().filter(|p| !p.is_empty()) {
        regex::Regex::new(pattern).map_err(|e| format!("Invalid filename pattern: {e}"))?;
    }
    let options = &rule.options;
    if options.min_size.zip(options.max_size).is_some_and(|(min,max)| min > max) {
        return Err("validation.size".into());
    }
    let date = |value: Option<&str>| {
        value.map(|text| chrono::NaiveDate::parse_from_str(text, "%Y-%m-%d")).transpose()
            .map_err(|_| "validation.date")
    };
    let after = date(options.modified_after.as_deref())?;
    let before = date(options.modified_before.as_deref())?;
    if after.zip(before).is_some_and(|(after,before)| after > before) {
        return Err("validation.date".into());
    }
    match rule.action.as_str() {
        "move" => crate::rules::validate_move_destination_template(&rule.destination),
        "rename" => crate::rules::validate_rename_template(&options.rename_template),
        _ => Ok(()),
    }
}

fn with_configuration<T>(app: &AppHandle, write: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    let state = app.try_state::<AppState>().ok_or("Application coordinator is unavailable")?;
    let _operation = state.ignored_files.with_operation();
    write()
}

#[tauri::command(async)]
pub fn add_rule_cmd(app: AppHandle, rule: Rule) -> Result<i64, String> {
    validate_rule_destination(&rule)?;
    with_configuration(&app, || add_rule(&rule).map_err(|e| e.to_string()))
}

#[tauri::command(async)]
pub fn update_rule_cmd(app: AppHandle, rule: Rule) -> Result<(), String> {
    validate_rule_destination(&rule)?;
    with_configuration(&app, || update_rule(&rule).map_err(|e| e.to_string()))
}

#[tauri::command(async)]
pub fn delete_rule_cmd(app: AppHandle, id: i64) -> Result<(), String> {
    with_configuration(&app, || delete_rule(id).map_err(|e| e.to_string()))
}

#[tauri::command]
pub fn get_folders_cmd() -> Result<Vec<WatchedFolder>, String> {
    get_watched_folders().map_err(|e| e.to_string())
}

#[tauri::command(async)]
pub fn add_folder_cmd(app: tauri::AppHandle, path: String, mode: String) -> Result<i64, String> {
    if !is_valid_folder_mode(&mode) {
        return Err(format!("Invalid folder mode: {}", mode));
    }
    let folder = crate::safe_fs::validate_root(Path::new(&path))?;
    let identity = crate::safe_fs::file_identity(&folder)?;
    let id = with_configuration(&app, || {
    for existing in get_watched_folders().map_err(|e| e.to_string())? {
        if crate::safe_fs::file_identity(Path::new(&existing.path)).ok().as_ref() == Some(&identity) {
            return existing.id.ok_or_else(|| "Existing folder has no identifier".into());
        }
    }
    add_watched_folder(&path, &mode).map_err(|e| e.to_string())
    })?;
    if let Some(state) = app.try_state::<crate::AppState>() {
        let mut watcher = state.watcher.lock().unwrap();
        watcher.refresh(app.clone()).map_err(|e| format!("Folder saved; watcher refresh failed: {e}"))?;
    }
    let _ = app.emit("folders-changed", ());
    Ok(id)
}

#[tauri::command(async)]
pub fn remove_folder_cmd(app: tauri::AppHandle, id: i64) -> Result<(), String> {
    with_configuration(&app, || remove_watched_folder(id).map_err(|e| e.to_string()))?;
    if let Some(state) = app.try_state::<crate::AppState>() {
        let mut watcher = state.watcher.lock().unwrap();
        watcher.refresh(app.clone()).map_err(|e| format!("Folder removed; watcher refresh failed: {e}"))?;
    }
    let _ = app.emit("folders-changed", ());
    let _ = app.emit("mutation-completed", ());
    Ok(())
}

#[tauri::command(async)]
pub fn update_folder_mode_cmd(app: tauri::AppHandle, id: i64, mode: String) -> Result<(), String> {
    if !is_valid_folder_mode(&mode) {
        return Err(format!("Invalid folder mode: {}", mode));
    }

    let old_mode = get_watched_folders()
        .ok()
        .and_then(|folders| folders.into_iter().find(|f| f.id == Some(id)))
        .map(|f| f.mode);

    with_configuration(&app, || update_folder_mode(id, &mode).map_err(|e| e.to_string()))?;

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

        watcher.refresh(app.clone()).map_err(|e| format!("Mode saved; watcher refresh failed: {e}"))?;
    }
    let _ = app.emit("folders-changed", ());
    Ok(())
}

#[tauri::command]
pub fn get_logs_cmd(limit: i64) -> Result<Vec<ActionLog>, String> {
    get_recent_logs(limit.clamp(1, 500)).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn get_history_counts_cmd() -> Result<HistoryCounts, String> {
    history_counts().map_err(|e| e.to_string())
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
    file_identity: Option<String>,
    fingerprint: Option<String>,
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

fn related_pending_undo(target: &UndoTarget) -> Result<Option<PendingUndo>, String> {
    let pending = pending_undos()?;
    if pending.is_empty() { return Ok(None); }

    let target_paths: Vec<_> = [Some(target.source.as_str()), target.dest.as_deref()]
        .into_iter().flatten().filter(|path| !path.is_empty())
        .map(|path| crate::suppress::key(Path::new(path))).collect();
    Ok(pending.into_iter().find(|receipt| {
        receipt.action_log_id == Some(target.id)
            || target.file_identity.as_deref().is_some_and(|identity| {
                receipt.expected_identity.as_deref() == Some(identity)
                    || receipt.observed_identity.as_deref() == Some(identity)
            })
            || [Some(receipt.source.as_str()), receipt.requested.as_deref(), receipt.observed.as_deref()]
                .into_iter().flatten().filter(|path| !path.is_empty())
                .any(|path| target_paths.contains(&crate::suppress::key(Path::new(path))))
    }))
}

fn record_undo_move_failure(intent: i64, error: crate::safe_fs::MoveFailure, ignored_files: &IgnoredFiles) -> String {
    use crate::safe_fs::MoveFailure;
    match error {
        MoveFailure::Failed(error) | MoveFailure::SourceSharingViolation(error) => match finalize_operation_intent(intent, "failed", None, Some(&error)) {
            Ok(()) => error,
            Err(journal) => format!("{error}. Undo recovery record {intent} remains pending. Journal error: {journal}"),
        },
        MoveFailure::RetainedOutput { path, identity, verified, source_preserved, message } => {
            ignored_files.arm(std::slice::from_ref(&path), Instant::now(), SUPPRESSION_TTL);
            let actual = path.to_string_lossy();
            let identity = identity.map(|identity| identity.key());
            let output = if verified { "Verified copy" } else { "Unverified output" };
            let source = if source_preserved { "The organized file was preserved." }
                else { "The organized file's state could not be confirmed." };
            let detail = format!("{output} retained at {actual}. {source} {message}");
            match observe_undo_retained_output(intent, &actual, identity.as_deref(), &detail) {
                Ok(()) => format!("{detail} Undo recovery record {intent} remains pending."),
                Err(journal) => format!("{detail} Undo recovery record {intent} remains pending. Its retained path could not be saved: {journal}"),
            }
        }
    }
}

/// Evaluate the recorded destination while the caller holds the operation gate.
/// A prior row in Undo All may have just restored this row's destination.
fn finish_undo(target: &UndoTarget, ignored_files: &IgnoredFiles) -> Result<UndoResult, String> {
    if let Some(receipt) = related_pending_undo(target)? {
        if let Some(detail) = receipt.retained_message {
            return Err(format!("{detail} Inspect both locations before restoring manually. Undo recovery record {} remains pending.", receipt.id));
        }
        let location = receipt.observed.map(|path| format!(" Observed restore path: {path}.")).unwrap_or_default();
        let requested = receipt.requested.as_deref().unwrap_or("(unrecorded)");
        return Err(format!("Undo was interrupted (recovery record {}). Inspect {} and {requested} before restoring manually.{location}", receipt.id, receipt.source));
    }

    let Some(dest) = target.dest.as_deref().filter(|path| !path.is_empty()) else {
        return record_missing_undo(target.id);
    };
    let dest_path = Path::new(dest);
    match std::fs::symlink_metadata(dest_path) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return record_missing_undo(target.id),
        Err(error) => return Err(format!("Cannot inspect the recorded destination {dest}: {error}")),
    }
    let expected = target.file_identity.as_deref().ok_or_else(|| format!(
        "This older history entry has no saved file identity. Verify the file at {dest} and restore it manually to {}.",
        target.source,
    ))?;
    let identity = crate::safe_fs::file_identity(dest_path)?;
    if identity.key() != expected {
        return Err("The file at the recorded destination was replaced; undo stopped".into());
    }
    let digest = target.fingerprint.as_deref().map(|fingerprint| {
        fingerprint.strip_prefix("blake3:").ok_or("history.unverified")
    }).transpose()?;

    let source_path = Path::new(&target.source);
    ignored_files.invalidate(&[dest_path.to_path_buf(), source_path.to_path_buf()]);
    arm_suppression(ignored_files, &[dest.to_owned(), target.source.clone()])?;
    let intent = begin_undo_intent(target.id, dest, &target.source, expected)
        .map_err(|error| format!("Undo was not started: could not persist operation intent: {error}"))?;

    notify_move_observer();
    let moved = match crate::safe_fs::move_file_checked_expected_content_detailed(dest_path, source_path, &identity, digest) {
        Ok(moved) => moved,
        Err(error) => return Err(record_undo_move_failure(intent, error, ignored_files)),
    };
    let actual = moved.destination.to_string_lossy().into_owned();
    // A collision can select a new path. Arm it as soon as the move returns.
    arm_suppression(ignored_files, &[actual.clone(), target.source.clone()])?;
    let resulting_identity = moved.identity.key();
    let status = if moved.renamed { "collision" } else { "ok" };
    let finalized = observe_undo_restore(intent, &actual, &resulting_identity)
        .and_then(|()| finalize_undo_intent(intent, target.id, dest, &actual, &resulting_identity));
    Ok(match finalized {
        Ok(()) => UndoResult {
            status: status.into(),
            message: None,
            restored_to: Some(actual),
        },
        Err(error) => UndoResult {
            status: "failed".into(),
            message: Some(format!("File moved to {actual}. Undo history or inventory could not be finalized: {error}. Recovery record {intent} remains pending.")),
            restored_to: Some(actual),
        },
    })
}

fn record_missing_undo(id: i64) -> Result<UndoResult, String> {
    lock_db()?.execute("UPDATE action_logs SET undone=1 WHERE id=?1", [id])
        .map_err(|error| error.to_string())?;
    Ok(UndoResult { status: "missing".into(), message: None, restored_to: None })
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
        "SELECT source_path, destination_path, file_identity, fingerprint FROM action_logs WHERE id=?1 AND undone=0 AND action IN ('move','rename','accept')",
        [id],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?, row.get::<_, Option<String>>(2)?, row.get::<_, Option<String>>(3)?)),
    )
    .map(|(source, dest, file_identity, fingerprint)| UndoTarget { id, source, dest, file_identity, fingerprint })
    .map_err(|e| format!("No undoable action found for id={}: {}", id, e))
}

fn undo_action_with(id: i64, ignored_files: &IgnoredFiles) -> Result<UndoResult, String> {
    let _operation = ignored_files.with_operation();
    let target = read_undo_target(id)?;
    Ok(finish_undo(&target, ignored_files).unwrap_or_else(failed_undo))
}

fn undo_all_with(ignored_files: &IgnoredFiles) -> Result<UndoAllResult, String> {
    let _operation = ignored_files.with_operation();
    let targets: Vec<UndoTarget> = crate::db::get_undoable_logs()
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|(id, source, dest, file_identity, fingerprint)| UndoTarget { id, source, dest, file_identity, fingerprint })
        .collect();

    // Include currently absent destinations that a preceding restore may create.
    let armed: Vec<_> = targets.iter().flat_map(|target| {
        std::iter::once(target.source.clone()).chain(target.dest.iter().filter(|path| !path.is_empty()).cloned())
    }).collect();
    arm_suppression(ignored_files, &armed)?;

    let results: Vec<_> = targets.iter().map(|target| {
        finish_undo(target, ignored_files).unwrap_or_else(failed_undo)
    }).collect();

    // Preserve the wire count of handled rows. The UI counts restores separately.
    let count = results.iter().filter(|r| r.status != "failed").count();
    Ok(UndoAllResult { count, results })
}

/// Test-only hook, run before each rename with the DB and suppression map unlocked.
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
pub fn undo_action_cmd(app: AppHandle, id: i64, state: tauri::State<AppState>) -> Result<UndoResult, String> {
    let target = read_undo_target(id)?;
    let mut paths = vec![PathBuf::from(&target.source)];
    if let Some(dest) = target.dest { paths.push(PathBuf::from(dest)); }
    state.watcher.lock().map_err(|_| "Watcher state unavailable")?.cancel_paths(&paths);
    let result = undo_action_with(id, &state.ignored_files);
    let _ = app.emit("mutation-completed", ());
    result
}

#[tauri::command(async)]
pub fn undo_all_cmd(app: AppHandle, state: tauri::State<AppState>) -> Result<UndoAllResult, String> {
    let targets = get_undoable_logs().map_err(|e| e.to_string())?;
    let paths: Vec<PathBuf> = targets.iter().flat_map(|(_, source, dest, _, _)| {
        std::iter::once(PathBuf::from(source)).chain(dest.as_ref().map(PathBuf::from))
    }).collect();
    state.watcher.lock().map_err(|_| "Watcher state unavailable")?.cancel_paths(&paths);
    let result = undo_all_with(&state.ignored_files);
    let _ = app.emit("mutation-completed", ());
    result
}

#[tauri::command]
pub fn get_cleanup_logs_cmd(limit: i64) -> Result<Vec<CleanupAction>, String> {
    get_cleanup_logs(limit).map_err(|e| e.to_string())
}

/// Return the current app version and release date.
#[tauri::command]
pub fn get_version_cmd(app: AppHandle) -> Result<(String, String), String> {
    let version = app.package_info().version.to_string();
    let release_date = "2026-09-27".to_string();
    Ok((version, release_date))
}

#[tauri::command]
pub fn get_settings_cmd() -> Result<AppSettings, String> {
    get_settings().map_err(|e| e.to_string())
}

#[tauri::command(async)]
pub fn update_settings_cmd(app: AppHandle, settings: PreferencesPatch) -> Result<AppSettings, String> {
    if settings.theme.as_deref().is_some_and(|theme| !matches!(theme, "light" | "dark" | "system")) { return Err("Choose a supported theme".into()); }
    if settings.language.as_deref().is_some_and(|language| !matches!(language, "en" | "de" | "es" | "fr" | "it" | "ja" | "pl" | "ru" | "uk" | "vi" | "zh-CN")) { return Err("Choose a supported language".into()); }
    if settings.grace_period_seconds.is_some_and(|seconds| !(0..=86_400).contains(&seconds)) { return Err("Grace period must be between 0 and 86400 seconds".into()); }
    if settings.local_ai_model.as_deref().is_some_and(|model| model.trim().is_empty() || model.len() > 128 || model.chars().any(char::is_control)) { return Err("Choose a valid local model name".into()); }
    let committed = crate::classify::update_preferences(&settings)?;
    let _ = app.emit("settings-changed", &committed);
    crate::tray::update_tray_language(&app, &committed.language);
    Ok(committed)
}

#[tauri::command(async)]
pub fn clear_logs_cmd(app: AppHandle, state: tauri::State<AppState>) -> Result<(), String> {
    {
        let _operation = state.ignored_files.with_operation();
        clear_logs().map_err(|e| e.to_string())?;
    }
    let _ = app.emit("mutation-completed", ());
    Ok(())
}

#[tauri::command(async)]
pub fn scan_folder_cmd(app: AppHandle, path: String, state: tauri::State<AppState>) -> Result<Vec<(String, String, String)>, String> {
    let folders = get_watched_folders().map_err(|e| e.to_string())?;
    let resolved = crate::safe_fs::validate_root(Path::new(&path))?;
    let identity = crate::safe_fs::file_identity(&resolved)?;
    let eligible = folders.into_iter().find(|folder| {
        folder.enabled && !is_folder_paused_mode(&folder.mode)
            && crate::safe_fs::file_identity(Path::new(&folder.path)).ok().as_ref() == Some(&identity)
    }).ok_or("Folder is no longer an eligible watched root")?;
    let authority = WatchedManualRoot::capture(&eligible)?;
    let results = scan_watched_manual_folder(&authority, &state.ignored_files);
    let _ = app.emit("mutation-completed", ());
    results
}

#[tauri::command(async)]
pub fn clean_now_cmd(app: AppHandle, state: tauri::State<AppState>) -> Result<Vec<(String, String, String)>, String> {
    let folders = get_watched_folders().map_err(|e| e.to_string())?;
    let authorities = folders.iter().filter(|f| f.enabled && !is_folder_paused_mode(&f.mode))
        .map(WatchedManualRoot::capture).collect::<Vec<_>>();
    let results = (|| {
        let mut results = Vec::new();
        for authority in authorities {
            results.extend(scan_watched_manual_folder(&authority?, &state.ignored_files)?);
        }
        Ok(results)
    })();
    let _ = app.emit("mutation-completed", ());
    results
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
pub fn import_archive_cmd(app: AppHandle, path: String, state: tauri::State<AppState>) -> Result<ArchiveImportSummary, String> {
    let job = crate::archive::ImportJob::start()?;
    let intent = begin_operation_intent("archive_import", &path, None).map_err(|e| format!("Import was not started: cannot persist intent: {e}"))?;
    let mut observed_staging = None;
    let extraction = match crate::archive::extract_archive(
        Path::new(&path),
        &job.cancelled,
        |staging| {
            let path = staging.to_string_lossy().into_owned();
            let observation = observe_pending_archive_staging(intent, &path);
            observed_staging = Some(path);
            observation.map_err(|error| format!("Cannot record import staging before extraction: {error}"))
        },
        |bytes, files| {
            let _ = app.emit("archive-import-progress", serde_json::json!({"bytes":bytes,"files":files}));
        },
    ) {
        Ok(extraction) => extraction,
        Err(error) => {
            let journal = finalize_operation_intent(intent, "failed", observed_staging.as_deref(), Some(&error));
            return Err(match journal { Ok(()) => error, Err(e) => format!("{error}; import journal finalization failed: {e}") });
        }
    };
    let staging = &extraction.staging_dir;
    let sorting = crate::rules::manual_scan_folder_cancelable(staging, &state.ignored_files, &job.cancelled);
    let result = finish_archive_sort(intent, staging, extraction.extracted_files, sorting);
    let _ = app.emit("mutation-completed", ());
    result
}

pub(crate) fn finish_archive_sort(
    intent: i64,
    staging: &Path,
    extracted_count: usize,
    sorting: Result<crate::rules::SortedFiles, crate::rules::FolderScanError>,
) -> Result<ArchiveImportSummary, String> {
    let staging_path = staging.to_string_lossy().into_owned();
    let (status, results, mut errors) = match sorting {
        Ok(results) => ("completed", results, Vec::new()),
        Err(error) => ("partial", error.results, vec![error.message]),
    };
    let message = errors.first().map(String::as_str);
    if let Err(error) = crate::archive::save_manifest(staging, None, status, &results, message) {
        errors.push(format!("Manifest finalization failed: {error}"));
    }
    let journal_message = (!errors.is_empty()).then(|| errors.join("; "));
    if let Err(error) = finalize_operation_intent(intent, status, Some(&staging_path), journal_message.as_deref()) {
        errors.push(format!("Import journal finalization failed: {error}"));
    }
    if !errors.is_empty() {
        if status == "completed" {
            errors.insert(0, format!("Organized {} of {extracted_count} extracted files", results.len()));
        }
        return Err(format!("{}. Import data retained at {staging_path}", errors.join("; ")));
    }
    Ok(ArchiveImportSummary {
        extracted_count,
        sorted_count: results.len(),
        staging_path,
        results,
    })
}

#[tauri::command]
pub fn cancel_archive_import_cmd() -> Result<(), String> { crate::archive::cancel_import() }

#[tauri::command]
pub fn open_folder_cmd(path: String) -> Result<(), String> {
    if path.starts_with("http://") || path.starts_with("https://") {
        return open_web_url(&path);
    }
    let requested = Path::new(&path);
    if requested.is_file() {
        return open_folder_path(&requested.parent().ok_or("File has no parent")?.to_string_lossy());
    }
    open_folder_path(&path)
}

/// All notification and direct-folder entry points use argument passing.
pub fn open_folder_path(path: &str) -> Result<(), String> {
    if path.is_empty() {
        return Err("Path is empty".to_string());
    }
    let folder = Path::new(path);
    if !folder.is_absolute() || !folder.is_dir() {
        return Err("Choose an existing absolute folder path".into());
    }
    folder_open_command(path).spawn().map_err(|e| e.to_string())?;
    Ok(())
}

fn folder_open_command(path: &str) -> std::process::Command {
    #[cfg(target_os = "windows")]
    {
        let mut command = std::process::Command::new("explorer.exe");
        command.arg(path.replace('/', "\\"));
        command
    }
    #[cfg(target_os = "macos")]
    {
        let mut command = std::process::Command::new("open");
        command.arg(path);
        command
    }
    #[cfg(target_os = "linux")]
    {
        let mut command = std::process::Command::new("xdg-open");
        command.arg(path);
        command
    }
}

fn open_web_url(url: &str) -> Result<(), String> {
    if !(url.starts_with("https://") || url.starts_with("http://")) || url.chars().any(char::is_control) {
        return Err("Only HTTP(S) web links are supported".into());
    }
    #[cfg(target_os = "windows")]
    std::process::Command::new("rundll32.exe").args(["url.dll,FileProtocolHandler", url]).spawn().map_err(|e| e.to_string())?;
    #[cfg(target_os = "macos")]
    std::process::Command::new("open").arg(url).spawn().map_err(|e| e.to_string())?;
    #[cfg(target_os = "linux")]
    std::process::Command::new("xdg-open").arg(url).spawn().map_err(|e| e.to_string())?;
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

#[tauri::command(async)]
pub fn initialize_defaults_cmd(app: AppHandle) -> Result<(), String> {
    let downloads = get_downloads_folder();
    crate::safe_fs::validate_root(Path::new(&downloads))?;
    with_configuration(&app, || initialize_defaults_atomic(&downloads, FOLDER_MODE_MANUAL).map_err(|e| e.to_string()))
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
    if cfg!(debug_assertions) { return Err("Autostart is unavailable in the isolated development profile".into()); }
    app.autolaunch().enable().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn disable_autostart_cmd(app: AppHandle) -> Result<(), String> {
    if cfg!(debug_assertions) { return Err("Autostart is unavailable in the isolated development profile".into()); }
    app.autolaunch().disable().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn is_autostart_enabled_cmd(app: AppHandle) -> Result<bool, String> {
    app.autolaunch().is_enabled().map_err(|e| e.to_string())
}

fn registered_ignore_root(folder_path: &str) -> Result<(PathBuf, crate::safe_fs::AncestorGuard), String> {
    let root = crate::safe_fs::validate_root(Path::new(folder_path))?;
    let guard = crate::safe_fs::guard_directory(&root)?;
    let identity = crate::safe_fs::file_identity(&root)?;
    if !get_watched_folders().map_err(|e| e.to_string())?.iter().any(|folder| {
        crate::safe_fs::validate_root(Path::new(&folder.path)).ok()
            .and_then(|path| crate::safe_fs::file_identity(&path).ok()).as_ref() == Some(&identity)
    }) {
        return Err("Folder is no longer registered".into());
    }
    Ok((root, guard))
}

#[tauri::command(async)]
pub fn load_mouziignore_cmd(folder_path: String, state: tauri::State<AppState>) -> Result<Vec<String>, String> {
    let _operation = state.ignored_files.with_operation();
    let (root, _guard) = registered_ignore_root(&folder_path)?;
    load_mouziignore(&root.to_string_lossy())
}

#[tauri::command(async)]
pub fn save_mouziignore_cmd(app: AppHandle, folder_path: String, patterns: Vec<String>) -> Result<(), String> {
    with_configuration(&app, || {
        let (root, _guard) = registered_ignore_root(&folder_path)?;
        save_mouziignore(&root.to_string_lossy(), &patterns)
    })
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
#[tauri::command(async)]
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
    get_settings().map(crate::scheduler::schedule_from_settings).map_err(|e| e.to_string())
}

/// Update scheduled-clean settings.
#[tauri::command(async)]
pub fn update_schedule_cmd(app: AppHandle, schedule: ScheduleSettings, state: tauri::State<AppState>) -> Result<AppSettings, String> {
    let committed = state.scheduler.save_schedule(&state.ignored_files, &schedule)?;
    let _ = app.emit("settings-changed", &committed);
    Ok(committed)
}

#[tauri::command]
pub fn get_recent_scheduler_runs_cmd() -> Result<Vec<ScheduleRun>, String> {
    recent_schedule_runs().map_err(|e| e.to_string())
}

const MAX_RULE_TRANSFER_BYTES: u64 = 16 * 1024 * 1024;
static RULE_TRANSFER: PlanMutex<()> = PlanMutex::new(());

fn validate_rule_transfer_path(path: &Path) -> Result<(), String> {
    if !path.is_absolute() || !path.extension().is_some_and(|extension| extension.eq_ignore_ascii_case("json")) {
        return Err("Choose an absolute path to a JSON file".into());
    }
    #[cfg(windows)]
    if path.file_name().is_some_and(|name| name.as_encoded_bytes().contains(&b':')) {
        return Err("Alternate data streams are not supported for rule transfers".into());
    }
    Ok(())
}

fn write_rule_export(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::{ErrorKind, Write};

    validate_rule_transfer_path(path)?;
    if bytes.len() as u64 > MAX_RULE_TRANSFER_BYTES {
        return Err("Rules exceed the 16 MiB transfer limit".into());
    }
    let parent = path.parent().ok_or("Rule export has no parent folder")?;
    let _parent_guard = crate::safe_fs::guard_directory(parent)?;
    match std::fs::symlink_metadata(path) {
        Ok(_) => return Err("The export file already exists. Choose a new filename.".into()),
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => return Err(error.to_string()),
    }

    static NEXT_STAGE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let serial = NEXT_STAGE.fetch_add(1, Ordering::Relaxed);
    let stage = parent.join(format!(".mouzi-rules-{}-{serial}.tmp", std::process::id()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(0);
    }
    let mut file = options.open(&stage).map_err(|error| format!("Cannot stage rule export: {error}"))?;
    let identity = crate::safe_fs::identity_from_handle(&file)
        .map_err(|error| format!("Rule export stage retained at {}: {error}", stage.display()))?;
    let written = file.write_all(bytes).and_then(|()| file.sync_all());
    drop(file);
    let result = written.map_err(|error| error.to_string()).and_then(|()| {
        let digest = blake3::hash(bytes).to_hex();
        crate::safe_fs::move_reviewed_file(&stage, path, &identity, digest.as_str()).map(|_| ())
    });
    if let Err(error) = result {
        if crate::safe_fs::file_identity(path).ok().as_ref() == Some(&identity) {
            return Err(format!("Rule export reached its destination, but finalization failed. Stage may remain at {}: {error}", stage.display()));
        }
        return match crate::safe_fs::remove_regular_file_expected(&stage, &identity) {
            Ok(()) => Err(format!("Rule export failed: {error}")),
            Err(cleanup) => Err(format!("Rule export failed: {error}. Stage could not be cleaned at {}: {cleanup}", stage.display())),
        };
    }
    Ok(())
}

fn read_rule_import(path: &Path) -> Result<Vec<Rule>, String> {
    use std::io::Read;

    validate_rule_transfer_path(path)?;
    let (file, _parent_guard) = crate::safe_fs::open_guarded_regular_file(path)?;
    if crate::safe_fs::link_count(&file)? != 1 {
        return Err("Choose a rule file with one link".into());
    }
    let size_error = "Rule import exceeds the 16 MiB transfer limit";
    if file.metadata().map_err(|error| error.to_string())?.len() > MAX_RULE_TRANSFER_BYTES {
        return Err(size_error.into());
    }
    let mut data = Vec::new();
    file.take(MAX_RULE_TRANSFER_BYTES + 1).read_to_end(&mut data).map_err(|error| error.to_string())?;
    if data.len() as u64 > MAX_RULE_TRANSFER_BYTES {
        return Err(size_error.into());
    }
    let rules: Vec<Rule> = serde_json::from_slice(&data).map_err(|error| error.to_string())?;
    // Validate every rule before offering to replace the saved set.
    for (index, rule) in rules.iter().enumerate() {
        validate_rule_destination(rule)
            .map_err(|error| format!("Rule {} of {} ({}) is not usable: {error}", index + 1, rules.len(), rule.name))?;
    }
    Ok(rules)
}

/// Pick the export destination in Rust, keeping its path outside renderer IPC.
#[tauri::command]
pub async fn export_rules_cmd(window: tauri::WebviewWindow) -> Result<bool, String> {
    if window.label() != "app" {
        return Err("Rule transfer is available only in the workspace window".into());
    }
    tauri::async_runtime::spawn_blocking(move || {
        let _transfer = RULE_TRANSFER.try_lock().map_err(|_| "A rule transfer is already open")?;
        let Some(selected) = window.dialog().file()
            .set_parent(&window).set_title("Export Mouzi rules to a new JSON file")
            .add_filter("JSON", &["json"]).set_file_name("mouzi-rules.json").blocking_save_file()
        else { return Ok(false); };
        let path = selected.into_path().map_err(|error| error.to_string())?;
        let rules = get_rules().map_err(|error| error.to_string())?;
        let bytes = serde_json::to_vec_pretty(&rules).map_err(|error| error.to_string())?;
        write_rule_export(&path, &bytes)?;
        Ok(true)
    }).await.map_err(|error| error.to_string())?
}

#[cfg(windows)]
fn confirm_rule_replacement(window: &tauri::WebviewWindow, count: usize) -> Result<bool, String> {
    use std::ffi::c_void;

    #[link(name = "user32")]
    unsafe extern "system" {
        fn MessageBoxW(owner: *mut c_void, text: *const u16, caption: *const u16, flags: u32) -> i32;
    }

    const MB_YESNOCANCEL: u32 = 0x0003;
    const MB_ICONWARNING: u32 = 0x0030;
    const MB_DEFBUTTON2: u32 = 0x0100;
    const IDYES: i32 = 6;

    let text: Vec<u16> = format!("Remove all saved Mouzi rules and import {count} rules from the selected file?")
        .encode_utf16().chain(std::iter::once(0)).collect();
    let caption: Vec<u16> = "Replace all Mouzi rules?".encode_utf16().chain(std::iter::once(0)).collect();
    let (result_tx, result_rx) = std::sync::mpsc::sync_channel(1);
    let owner = window.clone();
    window.run_on_main_thread(move || {
        let result = owner.hwnd().map_err(|error| error.to_string()).and_then(|hwnd| {
            // HWND belongs to the workspace, and both UTF-16 buffers live through this call.
            let answer = unsafe {
                MessageBoxW(hwnd.0, text.as_ptr(), caption.as_ptr(), MB_YESNOCANCEL | MB_ICONWARNING | MB_DEFBUTTON2)
            };
            if answer == 0 {
                return Err(format!("Cannot confirm rule replacement: {}", std::io::Error::last_os_error()));
            }
            Ok(answer == IDYES)
        });
        let _ = result_tx.send(result);
    }).map_err(|error| error.to_string())?;
    result_rx.recv().map_err(|error| error.to_string())?
}

#[cfg(not(windows))]
fn confirm_rule_replacement(window: &tauri::WebviewWindow, count: usize) -> Result<bool, String> {
    use tauri_plugin_dialog::{MessageDialogButtons, MessageDialogKind, MessageDialogResult};

    let answer = window.dialog()
        .message(format!("Remove all saved Mouzi rules and import {count} rules from the selected file?"))
        .parent(window).title("Replace all Mouzi rules?")
        .kind(MessageDialogKind::Warning).buttons(MessageDialogButtons::YesNo)
        .blocking_show_with_result();
    Ok(answer == MessageDialogResult::Yes)
}

/// Pick and validate a rules file in Rust, and confirm replacement natively.
#[tauri::command]
pub async fn import_rules_cmd(window: tauri::WebviewWindow, replace: bool) -> Result<Option<usize>, String> {
    if window.label() != "app" {
        return Err("Rule transfer is available only in the workspace window".into());
    }
    tauri::async_runtime::spawn_blocking(move || {
        let _transfer = RULE_TRANSFER.try_lock().map_err(|_| "A rule transfer is already open")?;
        let Some(selected) = window.dialog().file()
            .set_parent(&window).set_title("Import Mouzi rules")
            .add_filter("JSON", &["json"]).blocking_pick_file()
        else { return Ok(None); };
        let path = selected.into_path().map_err(|error| error.to_string())?;
        let rules = read_rule_import(&path)?;
        if replace && !confirm_rule_replacement(&window, rules.len())? {
            return Ok(None);
        }
        with_configuration(window.app_handle(), || {
            import_rules_atomic(&rules, replace).map(Some).map_err(|error| error.to_string())
        })
    }).await.map_err(|error| error.to_string())?
}

#[cfg(test)]
mod rule_transfer_tests {
    use super::*;
    use std::fs;

    #[test]
    fn exports_valid_rules_without_replacing_a_selected_file() {
        let root = std::env::temp_dir().join(format!("mouzi-rule-transfer-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        let path = root.join("Rules.JSON");
        let rules = br#"[{"id":null,"name":"Invoices","priority":10,"enabled":true,"extensions":["pdf"],"pattern":null,"destination":"Documents","action":"move","folder_id":0}]"#;
        write_rule_export(&path, rules).unwrap();
        assert_eq!(read_rule_import(&path).unwrap()[0].name, "Invoices");
        assert!(write_rule_export(&path, b"[]").unwrap_err().contains("already exists"));
        assert_eq!(fs::read(&path).unwrap().as_slice(), rules);
        assert_eq!(fs::read_dir(&root).unwrap().count(), 1);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_oversized_and_linked_rule_inputs() {
        let root = std::env::temp_dir().join(format!("mouzi-rule-transfer-limits-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        let path = root.join("rules.json");
        let file = fs::File::create(&path).unwrap();
        file.set_len(MAX_RULE_TRANSFER_BYTES + 1).unwrap();
        drop(file);
        assert!(read_rule_import(&path).unwrap_err().contains("16 MiB"));
        fs::write(&path, b"[]").unwrap();
        let linked = root.join("linked.json");
        fs::hard_link(&path, &linked).unwrap();
        assert!(read_rule_import(&linked).unwrap_err().contains("one link"));
        assert_eq!(fs::read(&path).unwrap().as_slice(), b"[]");
        fs::remove_dir_all(root).unwrap();
    }
}

// ---------------------------------------------------------------------------
// Scan / Dashboard commands
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanStarted {
    pub started: bool,
    pub reason: Option<String>,
    pub run_id: Option<String>,
}

/// Scan all watched (non-paused) folders in the background.
/// Returns immediately; emits `scan-progress` events during the walk and
/// `scan-complete` per root when done.
#[tauri::command]
pub fn start_scan_cmd(app: AppHandle, state: tauri::State<'_, AppState>) -> Result<ScanStarted, String> {
    let roots: Vec<String> = get_watched_folders()
        .map_err(|e| e.to_string())?
        .into_iter()
        .filter(|f| f.enabled && !is_folder_paused_mode(&f.mode))
        .map(|f| f.path)
        .collect();
    if roots.is_empty() {
        return Ok(ScanStarted {
            started: false,
            reason: Some("no_roots".into()),
            run_id: None,
        });
    }
    let is_scanning = state.is_scanning.clone();
    if is_scanning.swap(true, Ordering::SeqCst) {
        return Ok(ScanStarted {
            started: false,
            reason: Some("already_running".into()),
            run_id: None,
        });
    }
    let run_id = chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0).to_string();
    let response_run_id = run_id.clone();
    std::thread::spawn(move || {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| scan::scan_roots(&roots, |event| match event {
            ScanEvent::Progress(p) => {
                let mut payload = serde_json::to_value(p).unwrap_or_default();
                payload["runId"] = serde_json::Value::String(run_id.clone());
                let _ = app.emit("scan-progress", payload);
            }
            ScanEvent::Complete(s) => {
                let mut payload = serde_json::to_value(s).unwrap_or_default();
                payload["runId"] = serde_json::Value::String(run_id.clone());
                let _ = app.emit("scan-complete", payload);
            }
        })));
        let errors = match &result {
            Ok(summaries) => summaries.iter().filter_map(|summary| summary.error.clone()).collect::<Vec<_>>(),
            Err(_) => {
                let error = "Inventory scan worker panicked";
                match recover_interrupted_inventory(error) {
                    Ok(()) => vec![format!("{error}. Previous snapshots retained.")],
                    Err(recovery) => vec![format!("{error}. Could not reconcile interrupted scans: {recovery}")],
                }
            }
        };
        is_scanning.store(false, Ordering::SeqCst);
        let _ = app.emit("scan-finished", serde_json::json!({
            "runId": run_id,
            "status": if errors.is_empty() { "finished" } else { "failed" },
            "errors": errors,
        }));
    });
    Ok(ScanStarted {
        started: true,
        reason: None,
        run_id: Some(response_run_id),
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
    pub revision: i64,
}

/// Aggregate stats for the dashboard view.
#[tauri::command(async)]
pub fn get_dashboard_stats_cmd() -> Result<DashboardStats, String> {
    let snapshot = get_dashboard_snapshot().map_err(|e| e.to_string())?;
    Ok(DashboardStats {
        total_files: snapshot.total_files,
        total_bytes: snapshot.total_bytes,
        category_breakdown: snapshot.category_breakdown,
        largest_files: snapshot.largest_files,
        recent_files: snapshot.recent_files,
        watched_roots: snapshot.watched_roots,
        last_scan_at: snapshot.last_scan_at,
        insights: snapshot.insights,
        age_buckets: snapshot.age_buckets,
        revision: snapshot.revision,
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
    crate::cleanup::find_empty_dirs()
}

#[tauri::command(async)]
pub fn prepare_cleanup_cmd(actions: Vec<crate::cleanup::CleanupRequest>, state: tauri::State<AppState>) -> Result<crate::cleanup::CleanupPlanPreview, String> {
    let _operation = state.ignored_files.with_operation();
    crate::cleanup::prepare_cleanup(&actions)
}

/// Execute a batch of cleanup actions (trash files / remove empty dirs).
#[tauri::command(async)]
pub fn execute_cleanup_cmd(
    app: AppHandle,
    plan_id: String,
    state: tauri::State<AppState>,
) -> Result<Vec<crate::cleanup::CleanupOutcome>, String> {
    let _operation = state.ignored_files.with_operation();
    let result = crate::cleanup::execute_cleanup_plan(&plan_id);
    let _ = app.emit("mutation-completed", ());
    result
}

// ---------------------------------------------------------------------------
// Suggestion commands (AI-assisted organization)
// ---------------------------------------------------------------------------

/// Get category suggestions for unclassified files in the inventory.
#[tauri::command(async)]
pub fn get_suggestions_cmd(limit: i64) -> Result<Vec<crate::classify::Suggestion>, String> {
    let limit = limit.clamp(1, 200) as usize;
    crate::classify::get_suggestions(limit)
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AcceptOutcome {
    pub path: String,
    pub status: String, // "ok" | "missing" | "failed"
    pub message: Option<String>,
    pub dest: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SuggestionRulePreview {
    pub extension: Option<String>,
    pub destination: String,
    pub scope: String,
    pub priority: i32,
    pub disposition: String,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SuggestionPlanPreview {
    pub plan_id: String,
    pub path: String,
    pub category: String,
    pub destination: String,
    pub rule: Option<SuggestionRulePreview>,
}

struct SuggestionPlan {
    preview: SuggestionPlanPreview,
    root: PathBuf,
    identity: crate::safe_fs::FileIdentity,
    digest: String,
    configuration: String,
    expires: Instant,
    create_rule: bool,
}

static SUGGESTION_PLANS: Lazy<PlanMutex<HashMap<String, SuggestionPlan>>> = Lazy::new(|| PlanMutex::new(HashMap::new()));

fn suggestion_configuration() -> Result<String, String> {
    serde_json::to_string(&get_watched_folders().map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())
}

fn live_suggestion_fingerprint(path: &Path, scope: &crate::safe_fs::RootScope) -> Result<(crate::safe_fs::FileIdentity, String), String> {
    use std::io::Read;
    let mut validated = scope.validate_file(path)?;
    let identity = validated.identity.clone();
    let before = validated.file.metadata().map_err(|e| e.to_string())?;
    let mut digest = blake3::Hasher::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = validated.file.read(&mut buffer).map_err(|e| e.to_string())?;
        if read == 0 { break; }
        digest.update(&buffer[..read]);
    }
    let after = validated.file.metadata().map_err(|e| e.to_string())?;
    if before.len() != after.len() || before.modified().ok() != after.modified().ok()
        || crate::safe_fs::file_identity(path)? != identity
    { return Err("File changed while its suggestion was being reviewed".into()); }
    Ok((identity, digest.finalize().to_hex().to_string()))
}

fn suggestion_rule_preview(file_name: &str, category: &str) -> Result<SuggestionRulePreview, String> {
    let extension = Path::new(file_name).extension().map(|e| e.to_string_lossy().to_lowercase()).filter(|e| !e.is_empty());
    let mut preview = SuggestionRulePreview {
        extension: extension.clone(), destination: category.into(),
        scope: "All eligible watched folders, including folders added later".into(),
        priority: 10, disposition: "create".into(), message: None,
    };
    let Some(extension) = extension else { preview.disposition = "not_applicable".into(); preview.message = Some("An extensionless file cannot create an extension rule".into()); return Ok(preview); };
    let rules = get_rules().map_err(|e| e.to_string())?;
    for rule in rules.iter().filter(|r| r.enabled) {
        if rule.extensions.iter().any(|e| e == &extension || e == "*") {
            // Lower-priority catch-all rules remain fallbacks after this rule.
            if !rule.extensions.contains(&extension) && rule.priority > preview.priority && rule.destination != category { continue; }
            if rule.action == "move" && rule.destination == category && rule.pattern.is_none() {
                preview.disposition = "already_covered".into();
                preview.message = Some(format!("Already covered by {}", rule.name));
                return Ok(preview);
            }
            preview.disposition = "conflict".into();
            preview.message = Some(format!("Existing rule '{}' can affect .{} files; edit the rules explicitly before adding conflicting automation", rule.name, extension));
            return Ok(preview);
        }
    }
    Ok(preview)
}

fn prepare_suggestion(path: String, suggested_category: String, create_rule: bool) -> Result<SuggestionPlanPreview, String> {
    if !crate::categories::is_valid_category(&suggested_category) || suggested_category == "Other" {
        return Err("Choose a supported category".into());
    }
    let folders = get_watched_folders().map_err(|e| e.to_string())?;
    let scope = crate::safe_fs::eligible_scope(&folders)?;
    let root = scope.validate_file(Path::new(&path))?.owning_root;
    {
        let conn = lock_db()?;
        let present: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM file_inventory WHERE path=?1)", [&path], |r| r.get(0)).map_err(|e| e.to_string())?;
        if !present { return Err("Suggestion is no longer in the inventory; scan again".into()); }
    }
    let (identity, digest) = live_suggestion_fingerprint(Path::new(&path), &scope)?;
    let file_name = Path::new(&path).file_name().ok_or("Suggestion has no filename")?.to_string_lossy().into_owned();
    let category_dir = root.join(&suggested_category);
    let canonical_source = scope.validate_file(Path::new(&path))?.canonical_path;
    let already_filed = canonical_source.parent().is_some_and(|parent| crate::safe_fs::directory_contains(parent, &category_dir).unwrap_or(false));
    let destination = if already_filed { path.clone() } else { crate::safe_fs::try_unique_destination(&category_dir, &file_name)?.to_string_lossy().into_owned() };
    let rule = if create_rule { Some(suggestion_rule_preview(&file_name, &suggested_category)?) } else { None };
    let serial = chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0);
    static NEXT_PLAN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let plan_id = format!("suggestion-{serial}-{}-{}", std::process::id(), NEXT_PLAN.fetch_add(1, Ordering::Relaxed));
    let preview = SuggestionPlanPreview { plan_id: plan_id.clone(), path, category: suggested_category, destination, rule };
    let mut plans = SUGGESTION_PLANS.lock().map_err(|_| "Suggestion plan store unavailable")?;
    plans.retain(|_, p| p.expires > Instant::now());
    if plans.len() >= 512 { return Err("Too many pending previews; close older previews and retry".into()); }
    plans.insert(plan_id, SuggestionPlan { preview: preview.clone(), root, identity, digest, configuration: suggestion_configuration()?, expires: Instant::now() + std::time::Duration::from_secs(600), create_rule });
    Ok(preview)
}

#[tauri::command(async)]
pub fn prepare_suggestion_cmd(path: String, suggested_category: String, create_rule: bool, state: tauri::State<AppState>) -> Result<SuggestionPlanPreview, String> {
    let _operation = state.ignored_files.with_operation();
    prepare_suggestion(path, suggested_category, create_rule)
}

#[tauri::command]
pub fn reset_dismissed_suggestions_cmd() -> Result<(), String> {
    reset_dismissed_suggestions().map_err(|e| e.to_string())
}

/// Dismiss a suggestion so it no longer appears in the list.
#[tauri::command]
pub fn dismiss_suggestion_cmd(path: String) -> Result<(), String> {
    crate::db::dismiss_suggestion(&path).map_err(|e| e.to_string())
}

/// Move a file into its suggested category folder and optionally create a
/// matching rule. Returns a per-file status; never panics on missing files.
#[tauri::command(async)]
pub fn accept_suggestion_cmd(
    app: AppHandle,
    plan_id: String,
    state: tauri::State<AppState>,
) -> Result<AcceptOutcome, String> {
    let result = accept_reviewed_suggestion(&plan_id, &state.ignored_files);
    let _ = app.emit("mutation-completed", ());
    result
}

fn accept_reviewed_suggestion(plan_id: &str, suppression: &IgnoredFiles) -> Result<AcceptOutcome, String> {
    let plan = SUGGESTION_PLANS.lock().map_err(|_| "Suggestion plan store unavailable")?.remove(plan_id).ok_or("Suggestion preview expired or was already used; preview again")?;
    let _operation = suppression.with_operation();
    if plan.expires <= Instant::now() || plan.configuration != suggestion_configuration()? {
        return Err("Suggestion configuration changed; preview again".into());
    }
    if let Some(reviewed) = &plan.preview.rule {
        let name = Path::new(&plan.preview.path).file_name().ok_or("Suggestion has no filename")?.to_string_lossy();
        let current = suggestion_rule_preview(&name, &plan.preview.category)?;
        let compatible = current.disposition == reviewed.disposition
            || (reviewed.disposition == "create" && current.disposition == "already_covered");
        if !compatible { return Err("Future rule coverage changed; preview again".into()); }
    }
    let scope = crate::safe_fs::eligible_scope(&get_watched_folders().map_err(|e| e.to_string())?)?;
    let (identity, digest) = live_suggestion_fingerprint(Path::new(&plan.preview.path), &scope)?;
    if identity != plan.identity || digest != plan.digest { return Err("Suggestion file changed; preview again".into()); }
    accept_suggestion_validated_with(plan.preview.path, plan.preview.category, plan.create_rule, suppression, &plan.root, Some(&identity), Some(&plan.digest), Some(Path::new(&plan.preview.destination)))
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
#[cfg(test)]
fn accept_suggestion_with(
    path: String,
    suggested_category: String,
    create_rule: bool,
    ignored_files: &IgnoredFiles,
    root: &Path,
) -> Result<AcceptOutcome, String> {
    let _operation = ignored_files.with_operation();
    accept_suggestion_validated_with(path, suggested_category, create_rule, ignored_files, root, None, None, None)
}

fn accept_suggestion_validated_with(
    path: String,
    suggested_category: String,
    create_rule: bool,
    ignored_files: &IgnoredFiles,
    root: &Path,
    expected_identity: Option<&crate::safe_fs::FileIdentity>,
    expected_digest: Option<&str>,
    reviewed_destination: Option<&Path>,
) -> Result<AcceptOutcome, String> {
    if !crate::categories::is_valid_category(&suggested_category) || suggested_category == "Other" {
        return Err("Choose a supported category".into());
    }
    let src = Path::new(&path);
    if !src.exists() {
        return Ok(AcceptOutcome {
            path,
            status: "missing".to_string(),
            message: None,
            dest: None,
        });
    }

    let validated_root = crate::safe_fs::validate_root(root)?;
    let trusted_scope = crate::safe_fs::eligible_scope(&[WatchedFolder { id: None, path: validated_root.to_string_lossy().into_owned(), enabled: true, mode: FOLDER_MODE_MANUAL.into(), only_new: false }])?;
    let validated = trusted_scope.validate_file(src)?;
    let identity = validated.identity.clone();
    let canonical_source = validated.canonical_path.clone();
    if expected_identity.is_some_and(|expected| expected != &identity) { return Err("Suggestion file was replaced".into()); }
    let file_name = src
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let dest = reviewed_destination.map(Path::to_path_buf).unwrap_or_else(||validated_root.join(&suggested_category).join(&file_name));

    // Layer 1, applied by hand because this path never reaches
    // `rules::is_already_filed`. `suggested_category` is a bare folder name, so
    // the destination directory has the same shape `resolve_dest_dir` produces:
    // the category under the watched root.
    let dest_dir = validated_root.join(&suggested_category);
    if let Some(src_parent) = canonical_source.parent() {
        if crate::safe_fs::directory_contains(src_parent, &dest_dir).unwrap_or(false) {
            // Already filed, so nothing moves and nothing is logged. The early
            // return is the point: falling through to `log_action` would write
            // the exact row this check exists to prevent — `source` equal to
            // `destination`.
            record_suggestion_feedback(&path, &suggested_category, None)?;
            let message = if create_rule { match create_suggestion_rule(&file_name, &suggested_category) { Ok(message) => message, Err(error) => Some(error) } } else { None };
            return Ok(AcceptOutcome {
                path: path.clone(),
                status: "ok".to_string(),
                message,
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
    drop(validated);
    let predicted = if reviewed_destination.is_some() { dest.clone() } else { crate::safe_fs::try_unique_destination(&dest_dir, &file_name)? };
    arm_suppression(
        ignored_files,
        &[src.to_string_lossy().into_owned(), predicted.to_string_lossy().into_owned()],
    )?;

    let intent = begin_operation_intent("suggestion", &path, Some(&dest.to_string_lossy())).map_err(|e| format!("Suggestion was not started: could not persist intent: {e}"))?;
    notify_move_observer();
    let moved = match expected_digest {
        Some(digest) => crate::safe_fs::move_reviewed_file_detailed(src, &dest, &identity, digest),
        None => crate::safe_fs::move_file_checked_expected_detailed(src, &dest, &identity),
    };
    let moved_identity = moved.as_ref().ok().map(|result| result.identity.key());
    let mut outcome = match moved {
        Ok(result) => AcceptOutcome {
            path: path.clone(),
            status: "ok".to_string(),
            message: None,
            dest: Some(result.destination.to_string_lossy().into_owned()),
        },
        Err(error) => {
            if let Some(message) = crate::rules::record_retained_move(intent, "suggestion", src, &error, ignored_files) {
                // Only a completed move supplies `dest`; the retained path is in the receipt and message.
                return Ok(AcceptOutcome { path, status: "partial".into(), message: Some(message), dest: None });
            }
            AcceptOutcome { path: path.clone(), status: "failed".into(), message: Some(error.to_string()), dest: None }
        }
    };

    if outcome.status == "ok" {
        if let Some(actual) = &outcome.dest {
            arm_suppression(ignored_files, std::slice::from_ref(actual))?;
        }
    }

    if outcome.status == "ok" {
        let log = ActionLog {
            id: None,
            timestamp: chrono::Utc::now(),
            source_path: path.clone(),
            destination_path: outcome.dest.clone(),
            action: "accept".to_string(),
            file_name: file_name.clone(),
            file_type: suggested_category.clone(),
            undone: false,
        };
        if let Some(actual) = outcome.dest.as_deref() {
            if let Err(e) = finalize_suggestion_intent(intent, &log, actual, &suggested_category, moved_identity.as_deref().unwrap_or("")) {
                outcome.status = "partial".into();
                outcome.message = Some(format!("File moved to {actual}; history or classification finalization failed: {e}. Recovery intent retained."));
            } else if create_rule {
                match create_suggestion_rule(&file_name, &suggested_category) {
                    Ok(message) => outcome.message = message,
                    Err(e) => { outcome.status = "partial".into(); outcome.message = Some(format!("File moved; future rule was not created: {e}")); }
                }
            }
        }
    } else {
        if let Err(e) = finalize_operation_intent(intent, "failed", None, outcome.message.as_deref()) {
            outcome.message = Some(format!("{}; recovery journal finalization failed: {e}", outcome.message.unwrap_or_default()));
        }
    }

    Ok(outcome)
}

/// Create a rule from a suggestion acceptance. Skips insertion if an enabled
/// move rule already covers the same extension with the same destination.
fn create_suggestion_rule(file_name: &str, category: &str) -> Result<Option<String>, String> {
    let preview = suggestion_rule_preview(file_name, category)?;
    if preview.disposition != "create" {
        return Ok(preview.message);
    }
    let ext = file_name
        .rfind('.')
        .and_then(|dot| file_name.get(dot + 1..))
        .map(str::to_lowercase);
    let ext = match ext {
        Some(e) if !e.is_empty() => e,
        _ => return Ok(Some("Extensionless file accepted; no extension rule created".into())),
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
        return Ok(Some("Already covered by an existing rule".into()));
    }

    add_rule(&Rule {
        id: None,
        name: category.to_string(),
        priority: 10,
        enabled: true,
        extensions: vec![ext.clone()],
        pattern: None,
        destination: category.to_string(),
        action: "move".to_string(),
        folder_id: 0,
        ..Rule::default()
    }).map_err(|e| e.to_string())?;
    Ok(Some(format!("Created global .{ext} rule to {category}")))
}

#[cfg(test)]
mod undo_tests {
    use super::*;
    use crate::db::{get_db, init_test_db, lock_db, serialise_test_db};
    use std::sync::atomic::AtomicBool;
    use std::sync::mpsc;
    use std::time::Duration;

    /// Empties move history and Undo intents. Reports errors so `UndoFixture::drop`
    /// can use it while the test is already unwinding.
    fn clear_logs() -> Result<(), String> {
        lock_db()?
            .execute_batch("DELETE FROM operation_intents WHERE action='undo'; DELETE FROM action_logs;")
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
            UndoFixture { _serialised: serialised, root: std::fs::canonicalize(root).unwrap() }
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
        let identity = dest.and_then(|path| crate::safe_fs::file_identity(Path::new(path)).ok())
            .map(|identity| identity.key());
        insert_log_with_identity(timestamp, source, dest, identity.as_deref())
    }

    fn insert_log_with_identity(timestamp: &str, source: &str, dest: Option<&str>, identity: Option<&str>) -> i64 {
        let conn = lock_db().expect("lock db");
        conn.execute(
            "INSERT INTO action_logs (timestamp, source_path, destination_path, action, file_name, file_type, undone, file_identity)
             VALUES (?1, ?2, ?3, 'move', 'f.undocheck', 'Other', 0, ?4)",
            rusqlite::params![timestamp, source, dest, identity],
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

    #[test]
    fn undo_leaves_present_legacy_destination_and_history_alone() {
        let fixture = UndoFixture::new("legacy_identity");
        let (source, dest) = stage(fixture.root(), "legacy.undocheck");
        let id = insert_log_with_identity("2026-01-01T00:00:00Z", &source, Some(&dest), None);
        let result = undo_action_with(id, &ignored_files()).unwrap();

        assert_eq!(result.status, "failed");
        assert!(result.message.unwrap().contains("restore it manually"));
        assert_eq!(std::fs::read(&dest).unwrap(), b"payload");
        assert!(!Path::new(&source).exists());
        assert_eq!(undone_of(&source), 0);
    }

    #[test]
    fn undo_lookup_error_keeps_history_retryable() {
        let fixture = UndoFixture::new("lookup_error");
        let source = fixture.root().join("source.undocheck");
        let source = source.to_str().unwrap();
        let id = insert_log_with_identity("2026-01-01T00:00:00Z", source, Some("invalid\0path"), None);
        let result = undo_action_with(id, &ignored_files()).unwrap();

        assert_eq!(result.status, "failed");
        assert!(result.message.unwrap().contains("Cannot inspect"));
        assert_eq!(undone_of(source), 0);
    }

    #[test]
    fn undo_all_rechecks_a_chain_with_tied_timestamps() {
        let fixture = UndoFixture::new("move_chain");
        let (source, middle) = stage(fixture.root(), "chain.undocheck");
        let end = fixture.root().join("latest.undocheck");
        let time = "2026-01-01T00:00:00Z";
        insert_log(time, &source, Some(&middle));
        std::fs::rename(&middle, &end).unwrap();
        insert_log(time, &middle, Some(end.to_str().unwrap()));

        let results = undo_all_with(&ignored_files()).unwrap().results;
        assert_eq!(results.iter().map(|row| row.status.as_str()).collect::<Vec<_>>(), ["ok", "ok"]);
        assert_eq!(std::fs::read(&source).unwrap(), b"payload");
        assert!(!Path::new(&middle).exists());
        assert!(!end.exists());
        assert_eq!(undone_of(&source), 1);
        assert_eq!(undone_of(&middle), 1);
    }

    #[test]
    fn pending_undo_blocks_overlapping_chain_before_and_after_the_move() {
        let fixture = UndoFixture::new("pending_chain");
        let (source, middle) = stage(fixture.root(), "chain.undocheck");
        let end = fixture.root().join("latest.undocheck");
        let end = end.to_str().unwrap();
        let middle_path = Path::new(&middle);
        let old_identity = crate::safe_fs::file_identity(middle_path).unwrap();
        let separator = std::path::MAIN_SEPARATOR;
        let middle_alias = format!("{}{separator}{separator}{}",
            middle_path.parent().unwrap().display(), middle_path.file_name().unwrap().to_string_lossy());
        let older = insert_log_with_identity("2026-01-01T00:00:01Z", &source, Some(&middle_alias), Some(&old_identity.key()));
        // A copy-then-remove move gives the newer row a different physical identity.
        std::fs::copy(&middle, end).unwrap();
        std::fs::remove_file(&middle).unwrap();
        let new_identity = crate::safe_fs::file_identity(Path::new(end)).unwrap();
        assert_ne!(old_identity, new_identity);
        let newer = insert_log("2026-01-01T00:00:02Z", &middle, Some(end));
        let intent = begin_undo_intent(newer, end, &middle, &new_identity.key()).unwrap();
        let (unrelated_source, unrelated_dest) = stage(fixture.root(), "unrelated.undocheck");
        log_at(0, &unrelated_source, Some(&unrelated_dest));
        let ignored = ignored_files();
        let receipt = format!("recovery record {intent}");

        for moved in [false, true] {
            if moved {
                crate::safe_fs::move_file_checked_expected(Path::new(end), Path::new(&middle), &new_identity).unwrap();
            }
            for id in [older, newer] {
                let result = undo_action_with(id, &ignored).unwrap();
                assert_eq!(result.status, "failed");
                assert!(result.message.unwrap().contains(receipt.as_str()));
            }
            let batch = undo_all_with(&ignored).unwrap();
            assert_eq!(batch.results.len(), if moved { 2 } else { 3 });
            for result in &batch.results[..2] {
                assert_eq!(result.status, "failed");
                assert!(result.message.as_deref().unwrap().contains(receipt.as_str()));
            }
            if !moved { assert_eq!(batch.results[2].status, "ok"); }
            assert_eq!(undone_of(&source), 0);
            assert_eq!(undone_of(&middle), 0);
            assert!(!Path::new(&source).exists());
            assert_eq!(Path::new(&middle).exists(), moved);
            assert_eq!(Path::new(end).exists(), !moved);
            assert!(pending_undos().unwrap()[0].observed.is_none());
        }
        assert_eq!(std::fs::read(unrelated_source).unwrap(), b"payload");
    }

    #[test]
    fn interrupted_undo_preserves_observation_and_blocks_a_blind_retry() {
        let fixture = UndoFixture::new("interrupted_undo");
        let (source, dest) = stage(fixture.root(), "interrupted.undocheck");
        std::fs::write(&source, b"occupied").unwrap();
        let identity = crate::safe_fs::file_identity(Path::new(&dest)).unwrap();
        let id = insert_log("2026-01-01T00:00:00Z", &source, Some(&dest));
        let intent = begin_undo_intent(id, &dest, &source, &identity.key()).unwrap();
        let moved = crate::safe_fs::move_file_checked_expected(Path::new(&dest), Path::new(&source), &identity).unwrap();
        let actual = moved.destination.to_str().unwrap();
        assert!(moved.renamed);

        let retry = undo_action_with(id, &ignored_files()).unwrap();
        assert_eq!(retry.status, "failed");
        assert!(retry.message.unwrap().contains("Undo was interrupted"));
        let pending = pending_undos().unwrap();
        assert_eq!((pending[0].id, pending[0].observed.as_deref()), (intent, None));
        assert_eq!(undone_of(&source), 0);

        observe_undo_restore(intent, actual, &moved.identity.key()).unwrap();
        let rescued = fixture.root().join("rescued.undocheck");
        std::fs::rename(actual, &rescued).unwrap();
        std::fs::write(actual, b"replacement").unwrap();
        let error = finalize_undo_intent(intent, id, &dest, actual, &moved.identity.key()).unwrap_err();
        assert!(error.contains("replaced"));
        let pending = pending_undos().unwrap();
        assert_eq!((pending[0].id, pending[0].observed.as_deref()), (intent, Some(actual)));
        assert_eq!(undone_of(&source), 0);
        assert_eq!(std::fs::read(actual).unwrap(), b"replacement");
        assert_eq!(std::fs::read(&source).unwrap(), b"occupied");
        assert_eq!(std::fs::read(rescued).unwrap(), b"payload");
    }

    #[test]
    fn retained_undo_output_stays_pending_and_cannot_finalize_or_retry() {
        let fixture = UndoFixture::new("retained_output");
        let (source, dest) = stage(fixture.root(), "retained.undocheck");
        let input_identity = crate::safe_fs::file_identity(Path::new(&dest)).unwrap();
        let id = insert_log("2026-01-01T00:00:00Z", &source, Some(&dest));
        let intent = begin_undo_intent(id, &dest, &source, &input_identity.key()).unwrap();
        let actual = fixture.root().join("retained_0.undocheck");
        std::fs::copy(&dest, &actual).unwrap();
        let retained_identity = crate::safe_fs::file_identity(&actual).unwrap();
        let ignored = ignored_files();
        let receipt = format!("record {intent}");
        let message = record_undo_move_failure(intent, crate::safe_fs::MoveFailure::RetainedOutput {
            path: actual.clone(), identity: Some(retained_identity.clone()), verified: true,
            source_preserved: true, message: "Injected source removal failure".into(),
        }, &ignored);

        assert!(message.contains(receipt.as_str()));
        assert!(message.contains(actual.to_str().unwrap()));
        assert!(ignored.guarded_keys().contains(&crate::suppress::key(&actual)));
        let pending = pending_undos().unwrap();
        assert_eq!(pending[0].observed.as_deref(), actual.to_str());
        assert_eq!(pending[0].observed_identity.as_deref(), Some(retained_identity.key().as_str()));
        assert!(pending[0].retained_message.as_deref().unwrap().contains("Verified copy"));
        assert!(finalize_undo_intent(intent, id, &dest, actual.to_str().unwrap(), &retained_identity.key()).is_err());
        let retry = undo_action_with(id, &ignored).unwrap();
        assert_eq!(retry.status, "failed");
        assert!(retry.message.unwrap().contains(receipt.as_str()));
        assert_eq!(undone_of(&source), 0);
        assert!(!Path::new(&source).exists());
        assert_eq!(std::fs::read(&dest).unwrap(), b"payload");
        assert_eq!(std::fs::read(actual).unwrap(), b"payload");
    }

    #[test]
    fn unverified_undo_output_reports_path_even_when_observation_cannot_be_saved() {
        let fixture = UndoFixture::new("unverified_output");
        let (source, dest) = stage(fixture.root(), "unverified.undocheck");
        let identity = crate::safe_fs::file_identity(Path::new(&dest)).unwrap();
        let id = insert_log("2026-01-01T00:00:00Z", &source, Some(&dest));
        let intent = begin_undo_intent(id, &dest, &source, &identity.key()).unwrap();
        let ignored = ignored_files();
        let retained = |path| crate::safe_fs::MoveFailure::RetainedOutput {
            path, identity: None, verified: false, source_preserved: false,
            message: "Injected output inspection failure".into(),
        };
        let actual = fixture.root().join("unverified-output.undocheck");
        let message = record_undo_move_failure(intent, retained(actual.clone()), &ignored);
        assert!(message.contains("Unverified output"));
        let pending = pending_undos().unwrap();
        assert_eq!(pending[0].observed.as_deref(), actual.to_str());
        assert!(pending[0].observed_identity.is_none());

        // The first observation is immutable, so a second write must fail.
        let other = fixture.root().join("another-output.undocheck");
        let message = record_undo_move_failure(intent, retained(other.clone()), &ignored);
        assert!(message.contains(other.to_str().unwrap()));
        assert!(message.contains(format!("record {intent}").as_str()));
        assert!(message.contains("could not be saved"));
        assert_eq!(undone_of(&source), 0);
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
        crate::suppress::key(Path::new(path))
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
            AcceptFixture { _serialised: serialised, root: std::fs::canonicalize(root).unwrap() }
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
            let folders = get_watched_folders().unwrap();
            for folder in folders.into_iter().filter(|folder| Path::new(&folder.path) == self.root) {
                if let Some(id) = folder.id {
                    remove_watched_folder(id).unwrap();
                }
            }
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

        let source_key = crate::suppress::key(&source);
        assert!(
            guarded.contains(&source_key),
            "the source had no guard when the rename ran: {guarded:?}"
        );
        assert!(
            guarded.contains(&crate::suppress::key(&dest)),
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

    fn inventory_fixture(fixture: &AcceptFixture) -> PathBuf {
        lock_db().unwrap().execute_batch("DELETE FROM watched_folders; DELETE FROM file_inventory; DELETE FROM inventory_fingerprints; DELETE FROM accepted_suggestion_feedback;").unwrap();
        add_watched_folder(&fixture.root().to_string_lossy(), FOLDER_MODE_MANUAL).unwrap();
        let path = fixture.put("report.xyz");
        let summary = crate::scan::scan_roots(&[fixture.root().to_string_lossy().into_owned()], |_| {});
        assert_eq!(summary[0].status,"complete");
        path
    }

    fn ignored_files() -> IgnoredFiles { Arc::new(SuppressionSet::new()) }

    #[test]
    fn retained_suggestion_records_recovery_without_adopting_the_output() {
        let fixture = AcceptFixture::new("retained_suggestion");
        let source = inventory_fixture(&fixture);
        let output = fixture.put("Documents/.mouzi-transfer-test.part");
        let source_path = source.to_string_lossy();
        let output_path = output.to_string_lossy();
        let requested = fixture.root().join("Documents/report.xyz");
        let intent = begin_operation_intent("suggestion", &source_path, Some(&requested.to_string_lossy())).unwrap();
        let suppression = ignored_files();
        let error = crate::safe_fs::MoveFailure::RetainedOutput {
            path: output.clone(), identity: None, verified: false, source_preserved: true,
            message: "Injected staging cleanup failure".into(),
        };

        let message = crate::rules::record_retained_move(intent, "suggestion", &source, &error, &suppression).unwrap();
        assert!(message.contains(output_path.as_ref()));
        assert!(suppression.is_suppressed(&output, Instant::now()));
        assert!(get_undoable_logs().unwrap().is_empty());
        assert!(get_accepted_suggestion_feedback().unwrap().is_empty());
        let conn = lock_db().unwrap();
        let receipt: (String, String, String) = conn.query_row(
            "SELECT status,actual_destination,message FROM operation_intents WHERE id=?1",
            [intent], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        ).unwrap();
        assert_eq!(receipt, ("pending".into(), output_path.into_owned(), message));
        let root = fixture.root().to_string_lossy();
        let stale: bool = conn.query_row("SELECT stale FROM inventory_roots WHERE root_path=?1", [root.as_ref()], |row| row.get(0)).unwrap();
        assert!(stale);
        let inventory: i64 = conn.query_row("SELECT COUNT(*) FROM file_inventory WHERE root_path=?1", [root.as_ref()], |row| row.get(0)).unwrap();
        assert_eq!(inventory, 1);
        conn.execute("DELETE FROM operation_intents WHERE id=?1", [intent]).unwrap();
    }

    #[test]
    fn reviewed_suggestion_rejects_preserved_time_edits_and_is_single_use() {
        let fixture=AcceptFixture::new("review_content");let path=inventory_fixture(&fixture);
        let preview=prepare_suggestion(path.to_string_lossy().into_owned(),"Documents".into(),false).unwrap();
        let modified=std::fs::metadata(&path).unwrap().modified().unwrap();
        std::fs::write(&path,b"changed").unwrap();
        std::fs::File::options().write(true).open(&path).unwrap().set_times(std::fs::FileTimes::new().set_modified(modified)).unwrap();
        let ignored=ignored_files();
        assert!(accept_reviewed_suggestion(&preview.plan_id,&ignored).unwrap_err().contains("changed"));
        assert!(path.exists());
        assert!(accept_reviewed_suggestion(&preview.plan_id,&ignored).is_err());
    }

    #[test]
    fn reviewed_suggestion_keeps_exact_destination_and_reconciles_real_acceptance() {
        let fixture=AcceptFixture::new("review_destination");let path=inventory_fixture(&fixture);
        let occupied=fixture.put("Documents/report.xyz");
        let preview=prepare_suggestion(path.to_string_lossy().into_owned(),"Documents".into(),false).unwrap();
        assert!(preview.destination.ends_with("report_0.xyz"));
        let ignored=ignored_files();
        let outcome=accept_reviewed_suggestion(&preview.plan_id,&ignored).unwrap();
        assert_eq!(outcome.status,"ok");assert_eq!(outcome.dest.as_deref(),Some(preview.destination.as_str()));
        assert_eq!(std::fs::read(&occupied).unwrap(),b"payload");
        let undoable=get_undoable_logs().unwrap();assert_eq!(undoable.len(),1,"accepted moves must support Undo All");
        assert_eq!(crate::db::get_accepted_suggestion_feedback().unwrap().len(),1);
        crate::scan::scan_roots(&[fixture.root().to_string_lossy().into_owned()], |_| {});
        let category:String=lock_db().unwrap().query_row("SELECT category FROM file_inventory WHERE path=?1",[&preview.destination],|row|row.get(0)).unwrap();
        assert_eq!(category,"Documents","explicit category must survive a scan");
        assert_eq!(undo_action_with(undoable[0].0,&ignored).unwrap().status,"ok");
        assert!(crate::db::get_accepted_suggestion_feedback().unwrap().is_empty(),"undone acceptance must not train the model");
    }

    #[test]
    fn reviewed_suggestion_refuses_new_collisions_and_paused_roots() {
        let fixture=AcceptFixture::new("review_scope");let path=inventory_fixture(&fixture);
        let preview=prepare_suggestion(path.to_string_lossy().into_owned(),"Documents".into(),false).unwrap();
        fixture.put("Documents/report.xyz");
        let ignored=ignored_files();
        let result=accept_reviewed_suggestion(&preview.plan_id,&ignored).unwrap();
        assert_eq!(result.status,"failed");assert!(path.exists());
        let preview=prepare_suggestion(path.to_string_lossy().into_owned(),"Documents".into(),false).unwrap();
        let id=get_watched_folders().unwrap()[0].id.unwrap();update_folder_mode(id,FOLDER_MODE_PAUSED).unwrap();
        assert!(accept_reviewed_suggestion(&preview.plan_id,&ignored).is_err());
        assert!(path.exists());
    }

    #[test]
    fn undo_rejects_a_replacement_at_the_recorded_destination() {
        let fixture = AcceptFixture::new("undo_replacement");
        let source = inventory_fixture(&fixture);
        let preview = prepare_suggestion(source.to_string_lossy().into_owned(), "Documents".into(), false).unwrap();
        let ignored = ignored_files();
        assert_eq!(accept_reviewed_suggestion(&preview.plan_id, &ignored).unwrap().status, "ok");
        let undoable = get_undoable_logs().unwrap();
        assert_eq!(undoable.len(), 1);
        assert!(undoable[0].3.is_some(), "new history must save physical identity");

        let destination = Path::new(&preview.destination);
        let preserved = fixture.root().join("preserved-original.xyz");
        std::fs::rename(destination, &preserved).unwrap();
        std::fs::write(destination, b"another file").unwrap();

        let result = undo_action_with(undoable[0].0, &ignored).unwrap();
        assert_eq!(result.status, "failed");
        assert!(result.message.unwrap().contains("replaced"));
        assert_eq!(std::fs::read(destination).unwrap(), b"another file");
        assert_eq!(std::fs::read(&preserved).unwrap(), b"payload");
        assert!(!source.exists());
        let undone: bool = lock_db().unwrap().query_row("SELECT undone FROM action_logs WHERE id=?1", [undoable[0].0], |row| row.get(0)).unwrap();
        assert!(!undone, "a refused Undo must remain retryable");
    }

    #[test]
    fn finalization_never_records_a_foreign_destination_as_the_moved_file() {
        let fixture = AcceptFixture::new("finalize_replacement");
        let source = inventory_fixture(&fixture);
        let destination = fixture.root().join("Documents/report.xyz");
        let source_string = source.to_string_lossy().into_owned();
        let intent = begin_operation_intent("accept", &source_string, Some(&destination.to_string_lossy())).unwrap();
        let moved = crate::safe_fs::move_file_checked(&source, &destination).unwrap();
        let preserved = fixture.root().join("preserved-original.xyz");
        std::fs::rename(&moved.destination, &preserved).unwrap();
        std::fs::write(&moved.destination, b"foreign file").unwrap();
        let actual = moved.destination.to_string_lossy().into_owned();
        let log = ActionLog { id: None, timestamp: chrono::Utc::now(), source_path: source_string,
            destination_path: Some(actual.clone()), action: "accept".into(), file_name: "report.xyz".into(), file_type: "Documents".into(), undone: false };
        assert!(finalize_suggestion_intent(intent, &log, &actual, "Documents", &moved.identity.key()).unwrap_err().contains("replaced"));
        assert!(get_undoable_logs().unwrap().is_empty());
        let status: String = lock_db().unwrap().query_row("SELECT status FROM operation_intents WHERE id=?1", [intent], |row| row.get(0)).unwrap();
        assert_eq!(status, "pending");
        assert_eq!(std::fs::read(&preserved).unwrap(), b"payload");
        assert_eq!(std::fs::read(&moved.destination).unwrap(), b"foreign file");
    }
}

#[cfg(test)]
#[path = "beta_tests.rs"]
mod organization_integration_tests;
