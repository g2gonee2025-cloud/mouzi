use crate::db::{get_settings, get_watched_folders, is_folder_auto_mode, is_folder_manual_mode, WatchedFolder};
use crate::rules::{is_file_ignored_by_mouziignore, process_watched_file, same_path, should_ignore_file, ProcessOutcome, WatchedRoot};
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

fn render_notification_message(template: &str, file: &str, rule: &str, destination: &str) -> String {
    static PLACEHOLDERS: once_cell::sync::Lazy<regex::Regex> =
        once_cell::sync::Lazy::new(|| regex::Regex::new(r"\{(file|rule|destination)\}").unwrap());
    PLACEHOLDERS.replace_all(template, |captures: &regex::Captures<'_>| match &captures[1] {
        "file" => file,
        "rule" => rule,
        _ => destination,
    }).into_owned()
}

#[derive(Default)]
struct BatchOutcome {
    organized: usize,
    last_file_name: String,
    last_rule_name: String,
    last_dest_folder: String,
    last_notification: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Fingerprint { size: u64, modified: SystemTime }

fn fingerprint(path: &Path) -> Result<Fingerprint, String> {
    let metadata = std::fs::metadata(path).map_err(|error| error.to_string())?;
    Ok(Fingerprint { size: metadata.len(), modified: metadata.modified().map_err(|error| error.to_string())? })
}

#[derive(Clone)]
struct PendingFile {
    path: PathBuf,
    root: Arc<WatchedRoot>,
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
    known_roots: HashMap<i64, Arc<WatchedRoot>>,
    pending: Arc<Mutex<Vec<PendingFile>>>,
    pending_manual: Arc<Mutex<HashMap<String, Arc<WatchedRoot>>>>,
    suppression: Arc<SuppressionSet>,
    handle: Option<std::thread::JoinHandle<()>>,
    app_handle: Option<tauri::AppHandle>,
}

impl FolderWatcher {
    pub fn new(suppression: Arc<SuppressionSet>) -> Self {
        Self { watchers: HashMap::new(), known_roots: HashMap::new(), pending: Arc::new(Mutex::new(Vec::new())),
            pending_manual: Arc::new(Mutex::new(HashMap::new())), suppression, handle: None, app_handle: None }
    }

    fn processing_view(&self, app_handle: tauri::AppHandle) -> Self {
        Self { watchers: HashMap::new(), known_roots: HashMap::new(), pending: self.pending.clone(), pending_manual: self.pending_manual.clone(),
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
            let body = if outcome.organized == 1 { outcome.last_notification
                .unwrap_or_else(|| format!("{} → {}", outcome.last_file_name, outcome.last_rule_name)) }
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
            match process_watched_file(&item.path, &item.root, &self.suppression,
                &item.ticket, item.identity.as_ref()) {
                Ok(ProcessOutcome::Organized { rule, destination, warning }) => {
                    outcome.organized += 1;
                    outcome.last_file_name = item.path.file_name().unwrap_or_default().to_string_lossy().to_string();
                    outcome.last_rule_name = rule.name.clone();
                    outcome.last_notification = rule.notification_message.as_deref()
                        .filter(|message| !message.trim().is_empty())
                        .map(|message| render_notification_message(message, &outcome.last_file_name, &rule.name, &destination));
                    outcome.last_dest_folder = Path::new(&destination).parent().unwrap_or(Path::new(&destination)).to_string_lossy().to_string();
                    if let Some(app) = self.app_handle.as_ref() {
                        let _ = app.emit("file-organized", serde_json::json!({
                            "file": outcome.last_file_name, "rule": rule.name, "destination": destination,
                            "destination_folder": outcome.last_dest_folder, "message": outcome.last_notification,
                            "success": true, "warning": warning
                        }));
                    }
                }
                Ok(ProcessOutcome::Skipped) => {}
                Ok(ProcessOutcome::Retry { reason }) => self.retry(item, now, &reason),
                Ok(ProcessOutcome::Retained { message }) => {
                    self.report_file_error(&item.path, &message);
                    if let Some(app) = self.app_handle.as_ref() {
                        let _ = app.emit("mutation-completed", ());
                    }
                }
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
        self.known_roots.retain(|id, _| folders.iter().any(|folder| folder.id == Some(*id)));
        self.prune_queues(&folders);
        if recursive_watch_enabled() {
            eprintln!("[watcher] Recursive organization remains disabled; folders are watched non-recursively");
        }
        let mut errors = Vec::new();
        for folder in folders.into_iter().filter(|folder| folder.enabled &&
            (is_folder_auto_mode(&folder.mode) || is_folder_manual_mode(&folder.mode))) {
            let root = match self.remember_root(&folder) {
                Ok(root) => root,
                Err(error) => {
                    report_health(&app_handle, &folder.path, "error", Some(&error));
                    errors.push(format!("{}: {error}", folder.path));
                    continue;
                }
            };
            let pending = self.pending.clone();
            let manual = self.pending_manual.clone();
            let suppression = self.suppression.clone();
            let app = app_handle.clone();
            let callback_root = root.clone();
            let registration = register_root_watch(&root, move |result: Result<Event, notify::Error>| {
                match result {
                    Ok(event) => {
                        let may_remove = matches!(event.kind, notify::EventKind::Remove(_)
                            | notify::EventKind::Modify(notify::event::ModifyKind::Name(
                                notify::event::RenameMode::From | notify::event::RenameMode::Both)));
                        for path in event.paths {
                            let result = (|| {
                                if may_remove && path.parent() == Some(callback_root.path.as_path()) {
                                    let _guard = callback_root.guard_physical()?;
                                    crate::db::forget_removed_baseline(callback_root.id, &path, &callback_root.path)
                                        .map_err(|error| error.to_string())?;
                                }
                                enqueue_path(&path, &callback_root, &pending, &manual, &suppression, Some(&app))
                            })();
                            if let Err(error) = result {
                                report_health(&app, &callback_root.path.to_string_lossy(), "error", Some(&error));
                            }
                        }
                    }
                    Err(error) => report_health(&app, &callback_root.path.to_string_lossy(), "error", Some(&error.to_string())),
                }
            });
            let watcher = match registration {
                Ok(watcher) => watcher,
                Err(error) => {
                    report_health(&app_handle, &folder.path, "error", Some(&error.to_string()));
                    errors.push(format!("{}: {error}", folder.path));
                    continue;
                }
            };
            // Registration precedes enumeration. Both paths feed the same
            // deduplicated queue, including files arriving during enumeration.
            self.watchers.insert(folder.path.clone(), watcher);
            let scan = (|| {
                let _guard = root.guard_physical()?;
                let entries = std::fs::read_dir(&root.path).map_err(|error| error.to_string())?;
                for entry in entries {
                    let path = entry.map_err(|error| error.to_string())?.path();
                    enqueue_path(&path, &root, &self.pending, &self.pending_manual, &self.suppression, Some(&app_handle))?;
                }
                Ok::<(), String>(())
            })();
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

    fn remember_root(&mut self, folder: &WatchedFolder) -> Result<Arc<WatchedRoot>, String> {
        let id = folder.id.ok_or("Watched folder has no identifier")?;
        if let Some(root) = self.known_roots.get(&id) {
            if !root.matches_registration(folder) {
                return Err("Watched folder registration changed its path".into());
            }
            return Ok(root.clone());
        }
        let root = Arc::new(WatchedRoot::capture(folder)?);
        self.known_roots.insert(id, root.clone());
        Ok(root)
    }

    pub fn refresh(&mut self, app_handle: tauri::AppHandle) -> Result<(), String> {
        self.watchers.clear();
        self.watch_folders(app_handle)
    }

    fn prune_queues(&self, folders: &[WatchedFolder]) {
        let valid = |root: &WatchedRoot, manual: bool| folders.iter().any(|folder| {
            folder.enabled && root.matches_registration(folder)
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
            .retain(|path, root| {
                let path = Path::new(path);
                path.is_file() && (valid(root, true)
                    || (valid(root, false) && crate::operations::suspicious_name(path)))
            });
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
        let Some(root) = self.known_roots.values().find(|root| root.path.as_path() == Path::new(folder_path)) else { return; };
        if !auto_root_is_current(root) { return; }
        let Ok(settings) = get_settings() else { return; };
        let paths = {
            let mut manual = self.pending_manual.lock().unwrap_or_else(|error| error.into_inner());
            let paths = manual.iter().filter(|(path, observing_root)| Arc::ptr_eq(observing_root, root)
                && !crate::operations::suspicious_name(Path::new(path)))
                .map(|(path, _)| PathBuf::from(path)).collect::<Vec<_>>();
            for path in &paths { manual.remove(&path.to_string_lossy().to_string()); }
            paths
        };
        for path in paths {
            if self.suppression.is_suppressed(&path, Instant::now()) { continue; }
            let item = pending_item(&path, root, &self.suppression,
                Duration::from_secs(settings.grace_period_seconds.max(2) as u64));
            let mut pending = self.pending.lock().unwrap_or_else(|error| error.into_inner());
            pending.retain(|previous| !same_path(&previous.path, &path));
            pending.push(item);
        }
        self.update_tray_tooltip();
    }
}

fn register_root_watch(root: &WatchedRoot, callback: impl notify::EventHandler) -> Result<RecommendedWatcher, String> {
    let _guard = root.guard_physical()?;
    let mut watcher = RecommendedWatcher::new(callback,
        Config::default().with_poll_interval(Duration::from_secs(2)).with_compare_contents(true))
        .map_err(|error| error.to_string())?;
    watcher.watch(&root.path, RecursiveMode::NonRecursive).map_err(|error| error.to_string())?;
    Ok(watcher)
}

fn pending_item(path: &Path, root: &Arc<WatchedRoot>, suppression: &SuppressionSet, grace: Duration) -> PendingFile {
    PendingFile { path: path.to_path_buf(), root: root.clone(), scheduled: Instant::now() + grace.max(MIN_SETTLING),
        attempts: 0, ticket: suppression.ticket(path), identity: crate::safe_fs::file_identity(path).ok(),
        fingerprint: fingerprint(path).ok() }
}

fn auto_root_is_current(root: &WatchedRoot) -> bool {
    get_watched_folders().map(|folders| folders.iter().any(|folder|
        folder.enabled && is_folder_auto_mode(&folder.mode) && root.matches_registration(folder))).unwrap_or(false)
        && root.guard_physical().is_ok()
}

fn enqueue_path(path: &Path, root: &Arc<WatchedRoot>, pending: &Mutex<Vec<PendingFile>>,
    manual: &Mutex<HashMap<String, Arc<WatchedRoot>>>, suppression: &SuppressionSet, app: Option<&tauri::AppHandle>) -> Result<(), String> {
    if path.parent() != Some(root.path.as_path()) || !path.is_file() { return Ok(()); }
    let folders = get_watched_folders().map_err(|error| error.to_string())?;
    let Some(folder) = folders.iter().find(|folder| folder.enabled && root.matches_registration(folder)
        && (is_folder_auto_mode(&folder.mode) || is_folder_manual_mode(&folder.mode)))
        else { return Ok(()); };
    let _guard = root.guard_physical()?;
    if !path.is_file() || should_ignore_file(path) || suppression.is_suppressed(path, Instant::now())
        || is_file_ignored_by_mouziignore(path)? { return Ok(()); }
    if folder.only_new {
        let canonical = std::fs::canonicalize(path).map_err(|error| error.to_string())?;
        if crate::db::is_organization_baseline(&path.to_string_lossy(), &canonical.to_string_lossy())
            .map_err(|error| error.to_string())? { return Ok(()); }
    }
    if crate::operations::already_processed(path)? { return Ok(()); }
    if is_folder_manual_mode(&folder.mode) || crate::operations::suspicious_name(path) {
        let count = {
            let mut manual = manual.lock().unwrap_or_else(|error| error.into_inner());
            manual.insert(path.to_string_lossy().to_string(), root.clone());
            manual.len()
        };
        if let Some(app) = app {
            let _ = app.emit("file-detected", serde_json::json!({
                "folder": root.path.to_string_lossy(), "file": path.file_name().unwrap_or_default().to_string_lossy()
            }));
            crate::tray::update_tray_tooltip(app, count);
        }
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
        let root = std::fs::canonicalize(std::env::temp_dir()).unwrap();
        let path = root.join("queue-test-file.txt");
        let root = Arc::new(WatchedRoot::capture(&WatchedFolder {
            id: Some(1), path: root.to_string_lossy().into_owned(), enabled: true,
            mode: crate::db::FOLDER_MODE_SILENT.into(),
            only_new: false,
        }).unwrap());
        let mut pending = PendingFile { path: path.clone(), root,
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

    #[cfg(windows)]
    #[test]
    fn native_watch_accepts_a_new_same_name_file_and_rejects_a_stale_root() {
        use crate::db::{self, Rule};
        use std::fs;
        use std::os::windows::fs::OpenOptionsExt;
        use std::sync::mpsc;

        let _serial = db::serialise_test_db();
        db::init_test_db();
        let fixture_path = std::env::temp_dir().join(format!("mouzi-native-watch-{}-{}",
            std::process::id(), chrono::Utc::now().timestamp_nanos_opt().unwrap()));
        fs::create_dir(&fixture_path).unwrap();
        let fixture_path = fs::canonicalize(fixture_path).unwrap();
        let root_path = fixture_path.join("watched");
        fs::create_dir(&root_path).unwrap();
        let folder_id = db::add_watched_folder(&root_path.to_string_lossy(), db::FOLDER_MODE_SILENT).unwrap();
        let rule_id = db::add_rule(&Rule {
            id: None, name: "Native watcher fixture".into(), priority: i32::MIN,
            enabled: true, extensions: vec!["mouziwatchfixture".into()], pattern: None,
            destination: "Filed".into(), action: "move".into(), folder_id,
            ..Rule::default()
        }).unwrap();
        let source = root_path.join("report.mouziwatchfixture");
        struct Fixture { path: PathBuf, source: PathBuf, folder_id: i64, rule_id: i64 }
        impl Drop for Fixture {
            fn drop(&mut self) {
                if let Ok(conn) = db::lock_db() {
                    let source = self.source.to_string_lossy();
                    let _ = conn.execute("DELETE FROM action_logs WHERE source_path=?1", [source.as_ref()]);
                    let _ = conn.execute("DELETE FROM operation_intents WHERE source_path=?1", [source.as_ref()]);
                }
                let _ = db::delete_rule(self.rule_id);
                let _ = db::remove_watched_folder(self.folder_id);
                let _ = fs::remove_dir_all(&self.path);
            }
        }
        let _fixture = Fixture { path: fixture_path.clone(), source: source.clone(), folder_id, rule_id };
        let row = get_watched_folders().unwrap().into_iter().find(|folder| folder.id == Some(folder_id)).unwrap();
        let suppression = Arc::new(SuppressionSet::new());
        let mut watcher = FolderWatcher::new(suppression.clone());
        let root = watcher.remember_root(&row).unwrap();
        let (sender, receiver) = mpsc::channel();
        let native = register_root_watch(&root, move |event: Result<Event, notify::Error>| {
            let _ = sender.send(event);
        }).unwrap();

        let arrive = |contents: &[u8]| {
            let staged = fixture_path.join("staged");
            fs::write(&staged, contents).unwrap();
            fs::OpenOptions::new().write(true).open(&staged).unwrap()
                .set_modified(SystemTime::now() - Duration::from_secs(86_401)).unwrap();
            fs::rename(staged, &source).unwrap();
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                let event = receiver.recv_timeout(deadline.saturating_duration_since(Instant::now()))
                    .expect("Windows notification for the new file").unwrap();
                let arrived = matches!(event.kind, notify::EventKind::Create(_)
                    | notify::EventKind::Modify(notify::event::ModifyKind::Name(
                        notify::event::RenameMode::To | notify::event::RenameMode::Both)));
                if !arrived || !event.paths.contains(&source) { continue; }
                enqueue_path(&source, &root, &watcher.pending, &watcher.pending_manual, &suppression, None).unwrap();
                let pending = watcher.pending.lock().unwrap();
                break pending.iter().find(|item| item.path == source)
                    .expect("the native arrival should enter the production organizer queue").clone();
            }
        };

        let first = arrive(b"first file");
        assert_eq!(watcher.tick(first.scheduled).organized, 1);
        let first_output = root_path.join("Filed").join(source.file_name().unwrap());
        assert_eq!(fs::read(&first_output).unwrap(), b"first file");
        assert!(suppression.is_suppressed(&first_output, Instant::now()));

        let second = arrive(b"second file");
        assert_ne!(first.identity, second.identity);
        let read_handle = fs::OpenOptions::new().read(true).share_mode(1 | 2).open(&source).unwrap();
        assert!(fs::OpenOptions::new().write(true).open(&source).is_ok(), "the write probe should pass");
        assert_eq!(watcher.tick(second.scheduled).organized, 0);
        assert_eq!(fs::read(&source).unwrap(), b"second file");
        let deferred = {
            let pending = watcher.pending.lock().unwrap();
            assert_eq!(pending.len(), 1);
            pending[0].clone()
        };
        assert_eq!(deferred.identity, second.identity);
        assert_eq!(deferred.attempts, 1);
        assert!(deferred.scheduled > second.scheduled);
        {
            let conn = db::lock_db().unwrap();
            let failed_count: i64 = conn.query_row(
                "SELECT COUNT(*) FROM operation_intents WHERE source_path=?1 AND action='move' AND status='failed'",
                [source.to_string_lossy().as_ref()], |row| row.get(0),
            ).unwrap();
            assert_eq!(failed_count, 1);
        }
        drop(read_handle);
        assert_eq!(watcher.tick(deferred.scheduled).organized, 1);
        let second_output = root_path.join("Filed").join("report_0.mouziwatchfixture");
        assert_eq!(fs::read(&second_output).unwrap(), b"second file");
        assert_eq!(fs::read(&first_output).unwrap(), b"first file");

        let paused = arrive(b"keep when paused");
        {
            let _operation = suppression.with_operation();
            db::update_folder_mode(folder_id, db::FOLDER_MODE_PAUSED).unwrap();
        }
        assert!(matches!(process_watched_file(&source, &root, &suppression, &paused.ticket,
            paused.identity.as_ref()).unwrap(), ProcessOutcome::Skipped));
        assert_eq!(watcher.tick(paused.scheduled).organized, 0);
        assert_eq!(fs::read(&source).unwrap(), b"keep when paused");

        db::update_folder_mode(folder_id, db::FOLDER_MODE_SILENT).unwrap();
        drop(native);
        let renamed = fixture_path.join("renamed-root");
        fs::rename(&root_path, &renamed).unwrap();
        fs::create_dir(&root_path).unwrap();
        fs::hard_link(renamed.join(source.file_name().unwrap()), &source).unwrap();
        assert_eq!(crate::safe_fs::file_identity(&source).ok(), paused.identity);
        assert!(process_watched_file(&source, &root, &suppression, &paused.ticket,
            paused.identity.as_ref()).unwrap_err().contains("changed identity"));
        assert!(Arc::ptr_eq(&watcher.remember_root(&row).unwrap(), &root));
        assert!(register_root_watch(&root, |_: Result<Event, notify::Error>| {}).is_err());
        assert_eq!(fs::read(&source).unwrap(), b"keep when paused");

        let conn = db::lock_db().unwrap();
        let history_count: i64 = conn.query_row("SELECT COUNT(*) FROM action_logs WHERE source_path=?1",
            [source.to_string_lossy().as_ref()], |row| row.get(0)).unwrap();
        assert_eq!(history_count, 2);
    }
}
