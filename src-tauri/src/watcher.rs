use crate::db::{get_settings, get_watched_folders, is_folder_auto_mode, is_folder_manual_mode, WatchedFolder};
use crate::rules::{is_file_ignored_by_mouziignore, process_file_with_suppression, same_path, should_ignore_file, ProcessOutcome};
use crate::safe_fs::FileIdentity;
use crate::suppress::{CancellationTicket, SuppressionSet};
use notify::{Config, Event, RecommendedWatcher, RecursiveMode, Watcher};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};
use tauri::Emitter;
#[cfg(not(target_os = "windows"))]
use tauri_plugin_notification::NotificationExt;

const MAX_RETRIES: u8 = 8;
const MIN_SETTLING: Duration = Duration::from_secs(2);

#[derive(Default)]
struct BatchOutcome {
    organized: usize,
    last_file_name: String,
    last_rule_name: String,
    last_dest_folder: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Fingerprint { size: u64, modified: SystemTime }

fn fingerprint(path: &Path) -> Result<Fingerprint, String> {
    let metadata = std::fs::metadata(path).map_err(|error| error.to_string())?;
    Ok(Fingerprint { size: metadata.len(), modified: metadata.modified().map_err(|error| error.to_string())? })
}

#[derive(Debug, Clone)]
struct PendingFile {
    path: PathBuf,
    root: PathBuf,
    scheduled: Instant,
    attempts: u8,
    ticket: CancellationTicket,
    identity: Option<FileIdentity>,
    fingerprint: Option<Fingerprint>,
}

impl PendingFile {
    fn defer(&mut self, now: Instant) -> bool {
        if self.attempts >= MAX_RETRIES { return false; }
        self.attempts += 1;
        self.scheduled = now + retry_delay(self.attempts);
        true
    }
}

fn retry_delay(attempt: u8) -> Duration {
    Duration::from_secs((2u64 << attempt.min(5)).min(60))
}

pub struct FolderWatcher {
    watchers: HashMap<String, RecommendedWatcher>,
    pending: Arc<Mutex<Vec<PendingFile>>>,
    pending_manual: Arc<Mutex<HashMap<String, PathBuf>>>,
    suppression: Arc<SuppressionSet>,
    handle: Option<std::thread::JoinHandle<()>>,
    app_handle: Option<tauri::AppHandle>,
}

impl FolderWatcher {
    pub fn new(suppression: Arc<SuppressionSet>) -> Self {
        Self { watchers: HashMap::new(), pending: Arc::new(Mutex::new(Vec::new())),
            pending_manual: Arc::new(Mutex::new(HashMap::new())), suppression, handle: None, app_handle: None }
    }

    fn processing_view(&self, app_handle: tauri::AppHandle) -> Self {
        Self { watchers: HashMap::new(), pending: self.pending.clone(), pending_manual: self.pending_manual.clone(),
            suppression: self.suppression.clone(), handle: None, app_handle: Some(app_handle) }
    }

    pub fn start(&mut self, app_handle: tauri::AppHandle) {
        self.app_handle = Some(app_handle.clone());
        if self.handle.is_some() { return; }
        let worker = self.processing_view(app_handle);
        self.handle = Some(std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_millis(500));
            let outcome = worker.tick(Instant::now());
            if outcome.organized == 0 { continue; }
            if let Some(app) = worker.app_handle.as_ref() {
                let _ = app.emit("mutation-completed", ());
            }
            let body = if outcome.organized == 1 { format!("{} → {}", outcome.last_file_name, outcome.last_rule_name) }
                else { format!("Organized {} files", outcome.organized) };
            #[cfg(target_os = "windows")]
            {
                let destination = outcome.last_dest_folder;
                let identifier = worker.app_handle.as_ref().map(|app| app.config().identifier.clone());
                let _ = std::thread::spawn(move || {
                    let Some(identifier) = identifier else { return; };
                    let _ = tauri_winrt_notification::Toast::new(&identifier)
                        .title("Mouzi – click to open folder").text1(&body)
                        .on_activated(move |_action| {
                            if let Err(error) = crate::commands::open_folder_path(&destination) {
                                eprintln!("[notification] {error}");
                            }
                            Ok(())
                        }).show();
                });
            }
            #[cfg(not(target_os = "windows"))]
            if let Some(app) = worker.app_handle.as_ref() {
                let _ = app.notification().builder().title("Mouzi – click to open folder")
                    .body(body).extra("destFolder", outcome.last_dest_folder).show();
            }
        }));
    }

    fn tick(&self, now: Instant) -> BatchOutcome {
        self.suppression.sweep(now);
        let ready = {
            let mut pending = self.pending.lock().unwrap_or_else(|error| error.into_inner());
            let ready = pending.iter().filter(|item| now >= item.scheduled).cloned().collect::<Vec<_>>();
            pending.retain(|item| now < item.scheduled);
            ready
        };
        let mut outcome = BatchOutcome::default();
        for mut item in ready {
            if !item.ticket.is_current() || self.suppression.is_suppressed(&item.path, Instant::now()) { continue; }
            if !auto_root_is_current(&item.root) || !item.path.exists() { continue; }
            let current = fingerprint(&item.path);
            let observed = current.as_ref().ok();
            if observed.is_none() || item.identity.is_none() || item.fingerprint.as_ref() != observed {
                item.fingerprint = current.ok();
                item.identity = crate::safe_fs::file_identity(&item.path).ok();
                self.retry(item, now, "File metadata is unavailable or still changing");
                continue;
            }
            match process_file_with_suppression(&item.path, false, &item.root, &self.suppression,
                &item.ticket, item.identity.as_ref(), true) {
                Ok(ProcessOutcome::Organized { rule, destination, warning }) => {
                    outcome.organized += 1;
                    outcome.last_file_name = item.path.file_name().unwrap_or_default().to_string_lossy().to_string();
                    outcome.last_rule_name = rule.name.clone();
                    outcome.last_dest_folder = Path::new(&destination).parent().unwrap_or(Path::new(&destination)).to_string_lossy().to_string();
                    if let Some(app) = self.app_handle.as_ref() {
                        let _ = app.emit("file-organized", serde_json::json!({
                            "file": outcome.last_file_name, "rule": rule.name, "destination": destination,
                            "destination_folder": outcome.last_dest_folder, "success": true, "warning": warning
                        }));
                    }
                }
                Ok(ProcessOutcome::Skipped) => {}
                Ok(ProcessOutcome::Retry { reason }) => self.retry(item, now, &reason),
                Err(error) => self.report_file_error(&item.path, &error),
            }
        }
        outcome
    }

    fn retry(&self, mut item: PendingFile, now: Instant, reason: &str) {
        if !item.ticket.is_current() || self.suppression.is_suppressed(&item.path, Instant::now())
            || !auto_root_is_current(&item.root) { return; }
        if !item.defer(now) {
            self.report_file_error(&item.path, &format!("Automatic organization deferred after {MAX_RETRIES} retries: {reason}"));
            return;
        }
        let mut pending = self.pending.lock().unwrap_or_else(|error| error.into_inner());
        if !pending.iter().any(|newer| same_path(&newer.path, &item.path)) { pending.push(item); }
    }

    fn report_file_error(&self, path: &Path, error: &str) {
        eprintln!("[watcher] {}: {error}", path.display());
        if let Some(app) = self.app_handle.as_ref() {
            let _ = app.emit("file-organized", serde_json::json!({ "file": path.to_string_lossy(), "error": error, "success": false }));
        }
    }

    /// Invalidates already-drained generations without waiting for the operation
    /// coordinator. Callers may safely invoke this under the watcher lock.
    pub fn cancel_paths(&self, paths: &[PathBuf]) {
        self.suppression.invalidate(paths);
        self.pending.lock().unwrap_or_else(|error| error.into_inner())
            .retain(|item| !paths.iter().any(|path| same_path(path, &item.path)));
        self.pending_manual.lock().unwrap_or_else(|error| error.into_inner())
            .retain(|path, _| !paths.iter().any(|cancelled| same_path(cancelled, Path::new(path))));
        self.update_tray_tooltip();
    }

    pub fn watch_folders(&mut self, app_handle: tauri::AppHandle) -> Result<(), String> {
        self.start(app_handle.clone());
        let folders = get_watched_folders().map_err(|error| error.to_string())?;
        self.prune_queues(&folders);
        if recursive_watch_enabled() {
            eprintln!("[watcher] Recursive organization remains disabled; folders are watched non-recursively");
        }
        let mut errors = Vec::new();
        for folder in folders.into_iter().filter(|folder| folder.enabled &&
            (is_folder_auto_mode(&folder.mode) || is_folder_manual_mode(&folder.mode))) {
            let root = PathBuf::from(&folder.path);
            if let Err(error) = crate::safe_fs::validate_root(&root) {
                report_health(&app_handle, &folder.path, "error", Some(&error));
                errors.push(format!("{}: {error}", folder.path));
                continue;
            }
            let pending = self.pending.clone();
            let manual = self.pending_manual.clone();
            let suppression = self.suppression.clone();
            let app = app_handle.clone();
            let callback_root = root.clone();
            let registration = RecommendedWatcher::new(move |result: Result<Event, notify::Error>| {
                match result {
                    Ok(event) => for path in event.paths {
                        if let Err(error) = enqueue_path(&path, &callback_root, &pending, &manual, &suppression, &app) {
                            report_health(&app, &callback_root.to_string_lossy(), "error", Some(&error));
                        }
                    },
                    Err(error) => report_health(&app, &callback_root.to_string_lossy(), "error", Some(&error.to_string())),
                }
            }, Config::default().with_poll_interval(Duration::from_secs(2)).with_compare_contents(true));
            let mut watcher = match registration {
                Ok(watcher) => watcher,
                Err(error) => {
                    report_health(&app_handle, &folder.path, "error", Some(&error.to_string()));
                    errors.push(format!("{}: {error}", folder.path));
                    continue;
                }
            };
            if let Err(error) = watcher.watch(&root, RecursiveMode::NonRecursive) {
                report_health(&app_handle, &folder.path, "error", Some(&error.to_string()));
                errors.push(format!("{}: {error}", folder.path));
                continue;
            }
            // Registration precedes enumeration. Both paths feed the same
            // deduplicated queue, including files arriving during enumeration.
            self.watchers.insert(folder.path.clone(), watcher);
            let scan = std::fs::read_dir(&root).map_err(|error| error.to_string()).and_then(|entries| {
                for entry in entries {
                    let path = entry.map_err(|error| error.to_string())?.path();
                    enqueue_path(&path, &root, &self.pending, &self.pending_manual, &self.suppression, &app_handle)?;
                }
                Ok(())
            });
            match scan {
                Ok(()) => report_health(&app_handle, &folder.path, "active", None),
                Err(error) => {
                    report_health(&app_handle, &folder.path, "error", Some(&error));
                    errors.push(format!("{}: {error}", folder.path));
                }
            }
        }
        if errors.is_empty() { Ok(()) } else { Err(errors.join("; ")) }
    }

    pub fn refresh(&mut self, app_handle: tauri::AppHandle) -> Result<(), String> {
        self.watchers.clear();
        self.watch_folders(app_handle)
    }

    fn prune_queues(&self, folders: &[WatchedFolder]) {
        let valid = |root: &Path, manual: bool| folders.iter().any(|folder| {
            folder.enabled && same_path(Path::new(&folder.path), root)
                && if manual { is_folder_manual_mode(&folder.mode) } else { is_folder_auto_mode(&folder.mode) }
        });
        let removed = {
            let mut pending = self.pending.lock().unwrap_or_else(|error| error.into_inner());
            let removed = pending.iter().filter(|item| !valid(&item.root, false)).map(|item| item.path.clone()).collect::<Vec<_>>();
            pending.retain(|item| valid(&item.root, false));
            removed
        };
        self.suppression.invalidate(&removed);
        self.pending_manual.lock().unwrap_or_else(|error| error.into_inner())
            .retain(|path, root| valid(root, true) && Path::new(path).is_file());
        self.update_tray_tooltip();
    }

    fn update_tray_tooltip(&self) {
        if let Some(app) = self.app_handle.as_ref() {
            let count = self.pending_manual.lock().unwrap_or_else(|error| error.into_inner()).len();
            crate::tray::update_tray_tooltip(app, count);
        }
    }

    pub fn get_pending_files(&self) -> Vec<(String, String)> {
        let Ok(folders) = get_watched_folders() else { return Vec::new(); };
        self.prune_queues(&folders);
        self.pending_manual.lock().unwrap_or_else(|error| error.into_inner()).keys()
            .filter_map(|path| {
                let path = Path::new(path);
                Some((path.parent()?.to_string_lossy().to_string(), path.file_name()?.to_string_lossy().to_string()))
            }).collect()
    }

    pub fn flush_manual_to_pending(&mut self, folder_path: &str) {
        let root = PathBuf::from(folder_path);
        if !auto_root_is_current(&root) { return; }
        let Ok(settings) = get_settings() else { return; };
        let paths = {
            let mut manual = self.pending_manual.lock().unwrap_or_else(|error| error.into_inner());
            let paths = manual.iter().filter(|(_, observing_root)| same_path(observing_root, &root))
                .map(|(path, _)| PathBuf::from(path)).collect::<Vec<_>>();
            for path in &paths { manual.remove(&path.to_string_lossy().to_string()); }
            paths
        };
        for path in paths {
            if self.suppression.is_suppressed(&path, Instant::now()) { continue; }
            let item = pending_item(&path, &root, &self.suppression,
                Duration::from_secs(settings.grace_period_seconds.max(2) as u64));
            let mut pending = self.pending.lock().unwrap_or_else(|error| error.into_inner());
            pending.retain(|previous| !same_path(&previous.path, &path));
            pending.push(item);
        }
        self.update_tray_tooltip();
    }
}

fn pending_item(path: &Path, root: &Path, suppression: &SuppressionSet, grace: Duration) -> PendingFile {
    PendingFile { path: path.to_path_buf(), root: root.to_path_buf(), scheduled: Instant::now() + grace.max(MIN_SETTLING),
        attempts: 0, ticket: suppression.ticket(path), identity: crate::safe_fs::file_identity(path).ok(),
        fingerprint: fingerprint(path).ok() }
}

fn auto_root_is_current(root: &Path) -> bool {
    get_watched_folders().map(|folders| folders.iter().any(|folder|
        folder.enabled && is_folder_auto_mode(&folder.mode) && same_path(Path::new(&folder.path), root))).unwrap_or(false)
}

fn enqueue_path(path: &Path, root: &Path, pending: &Mutex<Vec<PendingFile>>,
    manual: &Mutex<HashMap<String, PathBuf>>, suppression: &SuppressionSet, app: &tauri::AppHandle) -> Result<(), String> {
    if !path.is_file() || should_ignore_file(path) || suppression.is_suppressed(path, Instant::now()) { return Ok(()); }
    if is_file_ignored_by_mouziignore(path)? { return Ok(()); }
    let folders = get_watched_folders().map_err(|error| error.to_string())?;
    let Some(folder) = folders.iter().find(|folder| folder.enabled && same_path(Path::new(&folder.path), root))
        else { return Ok(()); };
    if is_folder_manual_mode(&folder.mode) {
        let count = {
            let mut manual = manual.lock().unwrap_or_else(|error| error.into_inner());
            manual.insert(path.to_string_lossy().to_string(), root.to_path_buf());
            manual.len()
        };
        let _ = app.emit("file-detected", serde_json::json!({
            "folder": root.to_string_lossy(), "file": path.file_name().unwrap_or_default().to_string_lossy()
        }));
        crate::tray::update_tray_tooltip(app, count);
    } else if is_folder_auto_mode(&folder.mode) {
        let settings = get_settings().map_err(|error| error.to_string())?;
        suppression.invalidate(&[path.to_path_buf()]);
        let item = pending_item(path, root, suppression, Duration::from_secs(settings.grace_period_seconds.max(2) as u64));
        let mut pending = pending.lock().unwrap_or_else(|error| error.into_inner());
        pending.retain(|previous| !same_path(&previous.path, path));
        pending.push(item);
    }
    Ok(())
}

fn report_health(app: &tauri::AppHandle, root: &str, status: &str, error: Option<&str>) {
    let _ = app.emit("watcher-health", serde_json::json!({ "folder": root, "status": status, "error": error }));
    if let Some(error) = error { eprintln!("[watcher] {root}: {error}"); }
}

fn recursive_watch_enabled() -> bool {
    match std::env::var("MOUZI_RECURSIVE") {
        Ok(value) => recursive_watch_enabled_from(Some(&value), false),
        Err(_) => get_settings().map(|settings| recursive_watch_enabled_from(None, settings.recursive_watch)).unwrap_or(false),
    }
}

fn recursive_watch_enabled_from(env: Option<&str>, stored: bool) -> bool {
    match env { Some(value) => value == "1", None => stored }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recursive_flag_environment_wins_both_ways() {
        assert!(!recursive_watch_enabled_from(None, false));
        assert!(recursive_watch_enabled_from(None, true));
        assert!(recursive_watch_enabled_from(Some("1"), false));
        assert!(!recursive_watch_enabled_from(Some("0"), true));
        assert!(!recursive_watch_enabled_from(Some("true"), true));
    }

    #[test]
    fn retries_are_bounded_without_sleeping() {
        let suppression = SuppressionSet::new();
        let now = Instant::now();
        let path = PathBuf::from("C:/queue-test/file.txt");
        let mut pending = PendingFile { path: path.clone(), root: PathBuf::from("C:/queue-test"),
            scheduled: now, attempts: 0, ticket: suppression.ticket(&path), identity: None, fingerprint: None };
        for _ in 0..MAX_RETRIES {
            assert!(pending.defer(now));
            assert!(pending.scheduled > now);
            assert!(pending.scheduled <= now + Duration::from_secs(60));
        }
        assert!(!pending.defer(now));
    }

    #[test]
    fn drained_work_stays_cancelled_after_undo_lease_expires() {
        let suppression = SuppressionSet::new();
        let path = PathBuf::from("C:/queue-test/file.txt");
        let drained = suppression.ticket(&path);
        suppression.invalidate(std::slice::from_ref(&path));
        assert!(!drained.is_current());
        assert!(suppression.ticket(&path).is_current());
    }
}
