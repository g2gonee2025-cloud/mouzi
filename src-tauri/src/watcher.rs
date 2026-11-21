use crate::db::{get_settings, get_watched_folders, is_folder_auto_mode, is_folder_manual_mode, WatchedFolder};
use crate::ignore::IgnorePatterns;
use crate::operations::ProcessedCheckError;
use crate::rules::{is_file_ignored_by_mouziignore, organization_grace_period, organization_path_is_protected_with, process_watched_file, same_path, should_ignore_file, ProcessOutcome, WatchedRoot};
use crate::safe_fs::FileIdentity;
use crate::suppress::{CancellationTicket, SuppressionSet};
use notify::{Config, Event, RecommendedWatcher, RecursiveMode, Watcher};
use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant, SystemTime};
use tauri::{Emitter, Manager};
#[cfg(not(target_os = "windows"))]
use tauri_plugin_notification::NotificationExt;

const MAX_RETRIES: u8 = 8;
const WATCH_CHECK_INTERVAL: Duration = Duration::from_secs(5);
const MAX_WATCH_RETRY_DELAY: Duration = Duration::from_secs(30);

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
    queued_at: Instant,
    retry_at: Option<Instant>,
    grace_recheck: Option<(Duration, Instant)>,
    attempts: u8,
    ticket: CancellationTicket,
    identity: Option<FileIdentity>,
    fingerprint: Option<Fingerprint>,
}

impl PendingFile {
    fn ready_at(&self, grace: Duration) -> Option<Instant> {
        let mut ready = self.queued_at.checked_add(grace)?;
        if let Some(retry_at) = self.retry_at {
            ready = ready.max(retry_at);
        }
        if let Some((period, recheck_at)) = self.grace_recheck {
            if period == grace {
                ready = ready.max(recheck_at);
            }
        }
        Some(ready)
    }

    fn defer(&mut self, now: Instant) -> bool {
        if self.attempts >= MAX_RETRIES { return false; }
        self.attempts += 1;
        self.retry_at = Some(now + retry_delay(self.attempts));
        true
    }
}

fn retry_delay(attempt: u8) -> Duration {
    Duration::from_secs((2u64 << attempt.min(5)).min(60))
}

struct ActiveWatch {
    _watcher: RecommendedWatcher,
    root: Arc<WatchedRoot>,
    mode: String,
}

impl Drop for ActiveWatch {
    fn drop(&mut self) {
        self.root.retire_watch();
    }
}

struct WatchFailure {
    cause: CatchUpFailure,
    retry_at: Instant,
    delay: Duration,
}

#[derive(Debug)]
struct CatchUpFailure {
    message: String,
    // Present only after a complete enumeration under this active watcher.
    // New changes remain the responsibility of the installed notification callback.
    entries: Option<Vec<OsString>>,
}

impl From<String> for CatchUpFailure {
    fn from(message: String) -> Self { Self { message, entries: None } }
}

#[derive(Debug)]
enum EnqueueError {
    Entry(String),
    Root(String),
}

impl From<String> for EnqueueError {
    fn from(message: String) -> Self { Self::Root(message) }
}

impl EnqueueError {
    fn into_message(self) -> String {
        match self { Self::Entry(message) | Self::Root(message) => message }
    }
}

#[derive(PartialEq, Eq)]
enum EnqueueSource {
    CatchUp,
    Notification,
}

struct PendingReview {
    root: Arc<WatchedRoot>,
    ticket: CancellationTicket,
    excluded: bool,
}

#[derive(Default)]
struct PendingManualState {
    arrivals: HashMap<String, PendingReview>,
    exclusions_unavailable: bool,
}

#[derive(Default)]
struct PendingManual {
    state: Mutex<PendingManualState>,
    visible_count: AtomicUsize,
}

impl PendingManual {
    fn update_count(&self, state: &PendingManualState) {
        let count = if state.exclusions_unavailable { 0 } else {
            state.arrivals.values().filter(|arrival| !arrival.excluded).count()
        };
        self.visible_count.store(count, Ordering::Release);
    }

    fn visible_files(&self) -> Result<Vec<(String, String)>, String> {
        // A policy snapshot belongs to these tickets, even if a watcher callback
        // is waiting to queue a new version of the same path.
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.arrivals.retain(|_, arrival| arrival.ticket.is_current());
        let exclusions = pending_exclusions(&state.arrivals, |directory| {
            IgnorePatterns::from_folder(&directory.to_string_lossy())
        });
        state.exclusions_unavailable = exclusions.is_err();
        if let Ok(excluded) = &exclusions {
            for (path, arrival) in &mut state.arrivals {
                arrival.excluded = excluded.contains(path);
            }
        }
        self.update_count(&state);
        exclusions?;
        Ok(state.arrivals.iter().filter_map(|(path, arrival)| {
            if arrival.excluded || !arrival.ticket.is_current() { return None; }
            let path = Path::new(path);
            Some((path.parent()?.to_string_lossy().into_owned(), path.file_name()?.to_string_lossy().into_owned()))
        }).collect())
    }
}

fn pending_exclusions(
    arrivals: &HashMap<String, PendingReview>,
    mut read_patterns: impl FnMut(&Path) -> Result<IgnorePatterns, String>,
) -> Result<HashSet<String>, String> {
    let mut patterns = HashMap::<PathBuf, IgnorePatterns>::new();
    let mut roots = HashMap::new();
    let mut excluded = HashSet::new();
    for (path, arrival) in arrivals {
        if !arrival.ticket.is_current() { continue; }
        let root = &arrival.root;
        if !roots.contains_key(&root.path) {
            roots.insert(root.path.clone(), root.guard_physical()?);
        }
        let is_excluded = organization_path_is_protected_with(Path::new(path), &root.path, |directory, name| {
            if !patterns.contains_key(directory) {
                let _guard = if directory != root.path.as_path() { Some(crate::safe_fs::guard_directory(directory)?) } else { None };
                patterns.insert(directory.to_path_buf(), read_patterns(directory)?);
            }
            Ok(name.is_some_and(|name| patterns[directory].is_ignored(name)))
        })?;
        if is_excluded { excluded.insert(path.clone()); }
    }
    Ok(excluded)
}

pub struct FolderWatcher {
    watchers: HashMap<String, ActiveWatch>,
    failures: HashMap<String, WatchFailure>,
    callback_warnings: Arc<Mutex<HashMap<String, String>>>,
    pending: Arc<Mutex<Vec<PendingFile>>>,
    pending_manual: Arc<PendingManual>,
    suppression: Arc<SuppressionSet>,
    handle: Option<std::thread::JoinHandle<()>>,
    app_handle: Option<tauri::AppHandle>,
}

impl FolderWatcher {
    pub fn new(suppression: Arc<SuppressionSet>) -> Self {
        Self { watchers: HashMap::new(), failures: HashMap::new(), callback_warnings: Arc::new(Mutex::new(HashMap::new())), pending: Arc::new(Mutex::new(Vec::new())),
            pending_manual: Arc::new(PendingManual::default()), suppression, handle: None, app_handle: None }
    }

    fn processing_view(&self, app_handle: tauri::AppHandle) -> Self {
        Self { watchers: HashMap::new(), failures: HashMap::new(), callback_warnings: self.callback_warnings.clone(), pending: self.pending.clone(), pending_manual: self.pending_manual.clone(),
            suppression: self.suppression.clone(), handle: None, app_handle: Some(app_handle) }
    }

    pub fn start(&mut self, app_handle: tauri::AppHandle) {
        self.app_handle = Some(app_handle.clone());
        if self.handle.is_some() { return; }
        // A disconnected filesystem can stall a probe. Check registrations
        // separately from the worker draining files that are already queued.
        let recovery_app = app_handle.clone();
        std::thread::spawn(move || loop {
            std::thread::sleep(WATCH_CHECK_INTERVAL);
            if let Some(state) = recovery_app.try_state::<crate::AppState>() {
                if let Ok(mut watcher) = state.watcher.try_lock() {
                    let _ = watcher.reconcile_watches(Instant::now(), false, false);
                }
            }
        });
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
        let Some(grace) = crate::db::watcher_grace_seconds().map(organization_grace_period) else {
            return BatchOutcome::default();
        };
        let ready = {
            let mut pending = self.pending.lock().unwrap_or_else(|error| error.into_inner());
            let mut ready = Vec::new();
            pending.retain(|item| {
                if item.ready_at(grace).is_some_and(|deadline| now >= deadline) {
                    ready.push(item.clone());
                    false
                } else {
                    true
                }
            });
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
                Ok(ProcessOutcome::Settling { period, remaining }) => {
                    // A fresh age check may see a newer write, a recent settings
                    // save, or a wall clock that advanced less than this tick.
                    if let Some(deadline) = now.checked_add(remaining) {
                        item.grace_recheck = Some((period, deadline));
                        self.requeue(item);
                    } else {
                        self.retry(item, now, "Cannot schedule the file settling deadline");
                    }
                }
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
        if !item.defer(now) {
            if self.work_is_current(&item) {
                self.report_file_error(&item.path, &format!("Automatic organization deferred after {MAX_RETRIES} retries: {reason}"));
            }
            return;
        }
        self.requeue(item);
    }

    fn work_is_current(&self, item: &PendingFile) -> bool {
        item.ticket.is_current() && !self.suppression.is_suppressed(&item.path, Instant::now())
            && auto_root_is_current(&item.root)
    }

    fn requeue(&self, item: PendingFile) {
        if !self.work_is_current(&item) { return; }
        let mut pending = self.pending.lock().unwrap_or_else(|error| error.into_inner());
        if !pending.iter().any(|newer| same_path(&newer.path, &item.path)) { pending.push(item); }
    }

    fn report_file_error(&self, path: &Path, error: &str) {
        eprintln!("[watcher] {}: {error}", path.display());
        if let Some(app) = self.app_handle.as_ref() {
            let _ = app.emit("file-organized", serde_json::json!({ "file": path.to_string_lossy(), "error": error, "success": false }));
        }
    }

    pub(crate) fn queue_cancellation(&self) -> WatcherQueueCancellation {
        WatcherQueueCancellation {
            pending: self.pending.clone(),
            pending_manual: self.pending_manual.clone(),
            app_handle: self.app_handle.clone(),
        }
    }

    pub fn watch_folders(&mut self, app_handle: tauri::AppHandle) -> Result<(), String> {
        self.start(app_handle);
        if recursive_watch_enabled() {
            eprintln!("[watcher] Recursive organization remains disabled; folders are watched non-recursively");
        }
        self.reconcile_watches(Instant::now(), true, true)
    }

    fn reconcile_watches(&mut self, now: Instant, allow_legacy: bool, force: bool) -> Result<(), String> {
        let folders = get_watched_folders().map_err(|error| error.to_string())?;
        self.reconcile_watches_for_folders(&folders, now, allow_legacy, force)
    }

    fn reconcile_watches_for_folders(&mut self, folders: &[WatchedFolder], now: Instant, allow_legacy: bool, force: bool) -> Result<(), String> {
        let configured = |folder: &WatchedFolder| folder.enabled
            && (is_folder_auto_mode(&folder.mode) || is_folder_manual_mode(&folder.mode));
        let previous_count = self.watchers.len();
        self.watchers.retain(|path, watch| folders.iter().any(|folder|
            &folder.path == path && configured(folder) && watch.root.matches_registration(folder)));
        self.failures.retain(|path, _| folders.iter().any(|folder| &folder.path == path && configured(folder)));
        self.callback_warnings.lock().unwrap_or_else(|error| error.into_inner())
            .retain(|path, _| self.watchers.contains_key(path));
        let mut queues_changed = force || previous_count != self.watchers.len();
        let mut errors = Vec::new();
        for folder in folders.iter().filter(|folder| configured(folder)) {
            if let Some(watch) = self.watchers.get(&folder.path) {
                if let Err(error) = watch.root.guard_physical() {
                    let retired = !watch.root.watch_is_active();
                    self.watchers.remove(&folder.path);
                    let warning = self.callback_warnings.lock().unwrap_or_else(|error| error.into_inner())
                        .remove(&folder.path);
                    let error = if retired { warning.unwrap_or(error) } else { error };
                    queues_changed = true;
                    errors.push(format!("{}: {error}", folder.path));
                    self.record_watch_failure(&folder.path, error.into(), now);
                    continue;
                }
                if watch.mode == folder.mode && !self.failures.contains_key(&folder.path) {
                    continue;
                }
            }
            if !force && self.failures.get(&folder.path).is_some_and(|failure| now < failure.retry_at) {
                continue;
            }
            let retry_entries = !force && self.watchers.get(&folder.path)
                .is_some_and(|watch| watch.mode == folder.mode);
            let result = self.ensure_watch(folder, allow_legacy).map_err(CatchUpFailure::from)
                .and_then(|root| {
                    let entries = if retry_entries {
                        self.failures.get(&folder.path).and_then(|failure| failure.cause.entries.as_deref())
                    } else { None };
                    self.enqueue_existing_files(&root, self.app_handle.as_ref(), entries)
                });
            match result {
                Ok(()) => {
                    queues_changed = true;
                    self.failures.remove(&folder.path);
                    if let Some(app) = self.app_handle.as_ref() {
                        report_health(app, &folder.path, "active", None);
                    }
                }
                Err(failure) => {
                    errors.push(format!("{}: {}", folder.path, failure.message));
                    self.record_watch_failure(&folder.path, failure, now);
                }
            }
        }
        if queues_changed {
            match self.prune_queues(folders) {
                Ok(()) => self.update_tray_tooltip(),
                Err(error) => errors.push(error),
            }
        }
        if errors.is_empty() { Ok(()) } else { Err(errors.join("; ")) }
    }

    fn record_watch_failure(&mut self, path: &str, cause: CatchUpFailure, now: Instant) {
        let previous = self.failures.get(path);
        let changed = previous.is_none_or(|failure| failure.cause.message != cause.message);
        let delay = previous.map_or(WATCH_CHECK_INTERVAL, |failure| (failure.delay * 2).min(MAX_WATCH_RETRY_DELAY));
        self.failures.insert(path.to_owned(), WatchFailure { cause, retry_at: now + delay, delay });
        if changed {
            if let Some(app) = self.app_handle.as_ref() {
                report_health(app, path, "error", Some(&self.failures[path].cause.message));
            }
        }
    }

    fn ensure_watch(&mut self, folder: &WatchedFolder, allow_legacy: bool) -> Result<Arc<WatchedRoot>, String> {
        if let Some(watch) = self.watchers.get_mut(&folder.path) {
            watch.mode.clone_from(&folder.mode);
            return Ok(watch.root.clone());
        }
        let root = Arc::new(WatchedRoot::capture_unattended(folder, allow_legacy)?);
        if self.watchers.values().any(|watch| watch.root.same_physical_root(&root)) {
            return Err("This physical folder already has a watcher under another registration".into());
        }
        let pending = self.pending.clone();
        let manual = self.pending_manual.clone();
        let suppression = self.suppression.clone();
        let warnings = self.callback_warnings.clone();
        let app = self.app_handle.clone();
        let callback_root = root.clone();
        let watcher = register_root_watch(&root, move |result: Result<Event, notify::Error>| {
            if !callback_root.watch_is_active() { return; }
            let event = match result {
                Ok(event) => event,
                Err(error) => {
                    record_callback_health(app.as_ref(), &warnings, &callback_root, Some(error.to_string()), true);
                    return;
                }
            };
            let may_remove = matches!(event.kind, notify::EventKind::Remove(_)
                | notify::EventKind::Modify(notify::event::ModifyKind::Name(
                    notify::event::RenameMode::From | notify::event::RenameMode::Both)));
            let mut warning = None;
            let mut ignore_changed = false;
            for path in event.paths {
                if !callback_root.watch_is_active() { return; }
                ignore_changed |= path.file_name().and_then(|name| name.to_str()).is_some_and(|name| {
                    if cfg!(windows) { name.eq_ignore_ascii_case(".mouziignore") } else { name == ".mouziignore" }
                });
                let result = (|| {
                    if may_remove && path.parent() == Some(callback_root.path.as_path()) {
                        let _guard = callback_root.guard_physical()?;
                        crate::db::forget_removed_baseline(callback_root.id, &path, &callback_root.path)
                            .map_err(|error| error.to_string())?;
                    }
                    enqueue_path(&path, EnqueueSource::Notification, &callback_root, &pending,
                        &manual, &suppression, app.as_ref())
                        .map_err(EnqueueError::into_message)
                })();
                if let Err(error) = result {
                    if warning.is_none() {
                        warning = Some(format!("{}: {error}", path.display()));
                    }
                }
            }
            if ignore_changed {
                if let Err(error) = manual.visible_files() {
                    if warning.is_none() { warning = Some(error); }
                }
                if let Some(app) = app.as_ref() { update_pending_tooltip(app, &manual); }
            }
            record_callback_health(app.as_ref(), &warnings, &callback_root, warning, false);
        })?;
        // Install the notifier before enumerating arrivals missed offline.
        self.watchers.insert(folder.path.clone(), ActiveWatch { _watcher: watcher, root: root.clone(), mode: folder.mode.clone() });
        Ok(root)
    }

    fn enqueue_existing_files(&self, root: &Arc<WatchedRoot>, app: Option<&tauri::AppHandle>,
        retry_entries: Option<&[OsString]>) -> Result<(), CatchUpFailure> {
        let _guard = root.guard_physical()?;
        let mut failed_entries = Vec::new();
        let mut example = None;
        let mut enqueue = |name: OsString| -> Result<(), String> {
            let path = root.path.join(&name);
            match enqueue_path(&path, EnqueueSource::CatchUp, root, &self.pending,
                &self.pending_manual, &self.suppression, app) {
                Ok(()) => Ok(()),
                Err(EnqueueError::Root(error)) => Err(error),
                Err(EnqueueError::Entry(error)) => {
                    example.get_or_insert_with(|| format!("{}: {error}", path.display()));
                    failed_entries.push(name);
                    Ok(())
                }
            }
        };
        if let Some(entries) = retry_entries {
            for name in entries { enqueue(name.clone())?; }
        } else {
            for entry in std::fs::read_dir(&root.path).map_err(|error| error.to_string())? {
                enqueue(entry.map_err(|error| error.to_string())?.file_name())?;
            }
        }
        if let Some(example) = example {
            let message = if failed_entries.len() == 1 {
                format!("Could not check this file during watcher catch-up: {example}")
            } else {
                format!("Could not check {} files during watcher catch-up. First error: {example}", failed_entries.len())
            };
            return Err(CatchUpFailure {
                message,
                entries: Some(failed_entries),
            });
        }
        Ok(())
    }

    pub(crate) fn reconcile_baseline(&mut self, id: i64) -> Result<(), String> {
        let folders = get_watched_folders().map_err(|error| error.to_string())?;
        self.prune_queues(&folders)?;
        self.update_tray_tooltip();
        let Some(folder) = folders.iter().find(|folder| {
            folder.id == Some(id) && folder.enabled && !folder.only_new
                && (is_folder_auto_mode(&folder.mode) || is_folder_manual_mode(&folder.mode))
        }) else { return Ok(()); };
        let result = self.ensure_watch(folder, false).map_err(CatchUpFailure::from)
            .and_then(|root| self.enqueue_existing_files(&root, self.app_handle.as_ref(), None));
        if let Err(failure) = result {
            let message = failure.message.clone();
            self.record_watch_failure(&folder.path, failure, Instant::now());
            return Err(message);
        }
        if self.failures.remove(&folder.path).is_some() {
            if let Some(app) = self.app_handle.as_ref() { report_health(app, &folder.path, "active", None); }
        }
        Ok(())
    }

    pub fn refresh(&mut self, app_handle: tauri::AppHandle) -> Result<(), String> {
        self.start(app_handle);
        self.reconcile_watches(Instant::now(), false, true)
    }

    pub(crate) fn issues(&self) -> HashMap<String, String> {
        let mut issues = self.callback_warnings.lock().unwrap_or_else(|error| error.into_inner()).clone();
        issues.extend(self.failures.iter().map(|(path, failure)| (path.clone(), failure.cause.message.clone())));
        issues
    }

    fn prune_queues(&self, folders: &[WatchedFolder]) -> Result<(), String> {
        let valid = |root: &WatchedRoot, manual: bool| folders.iter().any(|folder| {
            folder.enabled && root.matches_registration(folder)
                && if manual { is_folder_manual_mode(&folder.mode) } else { is_folder_auto_mode(&folder.mode) }
        });
        let removed = {
            let mut pending = self.pending.lock().unwrap_or_else(|error| error.into_inner());
            let removed = pending.iter().filter(|item| item.root.watch_is_active() && !valid(&item.root, false))
                .map(|item| item.path.clone()).collect::<Vec<_>>();
            pending.retain(|item| item.ticket.is_current() && item.root.watch_is_active() && valid(&item.root, false));
            removed
        };
        self.suppression.invalidate(&removed);
        {
            let mut manual = self.pending_manual.state.lock().unwrap_or_else(|error| error.into_inner());
            let mut stale = Vec::new();
            for (path, arrival) in &manual.arrivals {
                if !arrival.ticket.is_current() || !pending_review_is_current(Path::new(path), &arrival.root, folders)? {
                    stale.push(path.clone());
                }
            }
            for path in stale { manual.arrivals.remove(&path); }
            self.pending_manual.update_count(&manual);
        }
        Ok(())
    }

    fn update_tray_tooltip(&self) {
        if let Some(app) = self.app_handle.as_ref() {
            update_pending_tooltip(app, &self.pending_manual);
        }
    }

    pub(crate) fn pending_review_count(&self) -> usize {
        self.pending_manual.visible_count.load(Ordering::Acquire)
    }

    pub fn get_pending_files(&self) -> Result<Vec<(String, String)>, String> {
        let folders = get_watched_folders().map_err(|error| error.to_string())?;
        self.prune_queues(&folders)?;
        let result = self.pending_manual.visible_files();
        self.update_tray_tooltip();
        result
    }

    pub fn flush_manual_to_pending(&mut self, folder_path: &str) {
        let Some(root) = self.watchers.get(folder_path).map(|watch| &watch.root) else { return; };
        if !auto_root_is_current(root) { return; }
        let arrivals = {
            let mut manual = self.pending_manual.state.lock().unwrap_or_else(|error| error.into_inner());
            let mut arrivals = Vec::new();
            manual.arrivals.retain(|path, arrival| {
                if !Arc::ptr_eq(&arrival.root, root) || crate::operations::suspicious_name(Path::new(path)) {
                    return true;
                }
                if arrival.ticket.is_current() {
                    arrivals.push((PathBuf::from(path), arrival.ticket.clone()));
                }
                false
            });
            self.pending_manual.update_count(&manual);
            arrivals
        };
        for (path, ticket) in arrivals {
            let item = pending_item(&path, root, ticket);
            let mut pending = self.pending.lock().unwrap_or_else(|error| error.into_inner());
            if !item.ticket.is_current() || self.suppression.is_suppressed(&path, Instant::now()) { continue; }
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

fn pending_item(path: &Path, root: &Arc<WatchedRoot>, ticket: CancellationTicket) -> PendingFile {
    PendingFile { path: path.to_path_buf(), root: root.clone(), queued_at: Instant::now(),
        retry_at: None, grace_recheck: None, attempts: 0, ticket, identity: crate::safe_fs::file_identity(path).ok(),
        fingerprint: fingerprint(path).ok() }
}

fn auto_root_is_current(root: &WatchedRoot) -> bool {
    get_watched_folders().map(|folders| folders.iter().any(|folder|
        folder.enabled && is_folder_auto_mode(&folder.mode) && root.matches_registration(folder))).unwrap_or(false)
        && root.guard_physical().is_ok()
}

fn path_is_baselined(path: &Path, folder: &WatchedFolder) -> Result<bool, EnqueueError> {
    if !folder.only_new { return Ok(false); }
    let canonical = std::fs::canonicalize(path).map_err(|error| EnqueueError::Entry(error.to_string()))?;
    crate::db::is_organization_baseline(&path.to_string_lossy(), &canonical.to_string_lossy())
        .map_err(|error| EnqueueError::Root(error.to_string()))
}

fn pending_review_is_current(path: &Path, root: &WatchedRoot, folders: &[WatchedFolder]) -> Result<bool, String> {
    if !root.watch_is_active() { return Ok(false); }
    let Some(folder) = folders.iter().find(|folder| {
        folder.enabled && root.matches_registration(folder) && (is_folder_manual_mode(&folder.mode)
            || (is_folder_auto_mode(&folder.mode) && crate::operations::suspicious_name(path)))
    }) else { return Ok(false); };
    match std::fs::metadata(path) {
        Ok(metadata) => Ok(metadata.is_file() && !path_is_baselined(path, folder).map_err(EnqueueError::into_message)?),
        Err(error) if matches!(error.kind(), std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory) => Ok(false),
        Err(error) => Err(format!("Cannot inspect pending file {}: {error}", path.display())),
    }
}

fn update_pending_tooltip(app: &tauri::AppHandle, manual: &Arc<PendingManual>) {
    let queue = manual.clone();
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || {
        let count = queue.visible_count.load(Ordering::Acquire);
        crate::tray::update_tray_tooltip(&handle, count);
    });
}

fn enqueue_path(path: &Path, source: EnqueueSource, root: &Arc<WatchedRoot>, pending: &Mutex<Vec<PendingFile>>,
    manual: &Arc<PendingManual>, suppression: &SuppressionSet, app: Option<&tauri::AppHandle>) -> Result<(), EnqueueError> {
    if !root.watch_is_active() || path.parent() != Some(root.path.as_path()) || !path.is_file() { return Ok(()); }
    let folders = get_watched_folders().map_err(|error| error.to_string())?;
    let Some(folder) = folders.iter().find(|folder| folder.enabled && root.matches_registration(folder)
        && (is_folder_auto_mode(&folder.mode) || is_folder_manual_mode(&folder.mode)))
        else { return Ok(()); };
    let _guard = root.guard_physical()?;
    if !path.is_file() || should_ignore_file(path) || suppression.is_suppressed(path, Instant::now())
        || is_file_ignored_by_mouziignore(path)? { return Ok(()); }
    if path_is_baselined(path, folder)? { return Ok(()); }
    let ticket = suppression.ticket(path);
    if crate::operations::already_processed(path).map_err(|error| match error {
        ProcessedCheckError::File(message) => EnqueueError::Entry(message),
        ProcessedCheckError::Database(error) => EnqueueError::Root(error.to_string()),
    })? { return Ok(()); }
    if is_folder_manual_mode(&folder.mode) || crate::operations::suspicious_name(path) {
        {
            let mut queue = manual.state.lock().unwrap_or_else(|error| error.into_inner());
            // A configuration write may have committed while this event was
            // being checked. Reconcile against its current baseline before
            // inserting, under the same queue lock as pruning.
            let folders = get_watched_folders().map_err(|error| error.to_string())?;
            if !pending_review_is_current(path, root, &folders)? || !ticket.is_current()
                || suppression.is_suppressed(path, Instant::now()) { return Ok(()); }
            let key = path.to_string_lossy();
            if source == EnqueueSource::CatchUp && queue.arrivals.get(key.as_ref())
                .is_some_and(|arrival| arrival.ticket.is_current() && !arrival.excluded && Arc::ptr_eq(&arrival.root, root)) {
                return Ok(());
            }
            let previous = queue.arrivals.insert(
                key.into_owned(),
                PendingReview { root: root.clone(), ticket, excluded: false },
            );
            if !queue.exclusions_unavailable && previous.is_none_or(|arrival| arrival.excluded) {
                manual.visible_count.fetch_add(1, Ordering::Release);
            }
        }
        if let Some(app) = app {
            let _ = app.emit("file-detected", serde_json::json!({
                "folder": root.path.to_string_lossy(), "file": path.file_name().unwrap_or_default().to_string_lossy()
            }));
            update_pending_tooltip(app, manual);
        }
    } else if is_folder_auto_mode(&folder.mode) {
        let mut item = pending_item(path, root, ticket);
        let mut pending = pending.lock().unwrap_or_else(|error| error.into_inner());
        if !root.watch_is_active() || !item.ticket.is_current()
            || suppression.is_suppressed(path, Instant::now()) { return Ok(()); }
        let existing = pending.iter().position(|previous| same_path(&previous.path, path));
        if let Some(index) = existing {
            let previous = &pending[index];
            // A real notification may have queued a recovery entry already.
            // Keep its deadline if catch-up observes the same exact version.
            if source == EnqueueSource::CatchUp && previous.ticket.is_current()
                && Arc::ptr_eq(&previous.root, root) && previous.identity.is_some() && previous.identity == item.identity
                && previous.fingerprint.is_some() && previous.fingerprint == item.fingerprint {
                return Ok(());
            }
        }
        if source == EnqueueSource::Notification && !item.ticket.renew() { return Ok(()); }
        if let Some(index) = existing { pending.remove(index); }
        pending.push(item);
    }
    Ok(())
}

fn report_health(app: &tauri::AppHandle, root: &str, status: &str, error: Option<&str>) {
    let _ = app.emit("watcher-health", serde_json::json!({ "folder": root, "status": status, "error": error }));
    if let Some(error) = error { eprintln!("[watcher] {root}: {error}"); }
}

fn record_callback_health(
    app: Option<&tauri::AppHandle>,
    warnings: &Mutex<HashMap<String, String>>,
    root: &WatchedRoot,
    warning: Option<String>,
    retire: bool,
) {
    let path = root.path.to_string_lossy();
    let changed = {
        let mut warnings = warnings.lock().unwrap_or_else(|error| error.into_inner());
        if !root.watch_is_active() { return; }
        if retire { root.retire_watch(); }
        if let Some(message) = warning.as_ref() {
            if warnings.get(path.as_ref()) == Some(message) { false }
            else { warnings.insert(path.to_string(), message.clone()); true }
        } else {
            warnings.remove(path.as_ref()).is_some()
        }
    };
    if let Some(app) = app.filter(|_| changed) {
        report_health(app, &path, if warning.is_some() { "error" } else { "active" }, warning.as_deref());
    }
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

pub(crate) struct WatcherQueueCancellation {
    pending: Arc<Mutex<Vec<PendingFile>>>,
    pending_manual: Arc<PendingManual>,
    app_handle: Option<tauri::AppHandle>,
}

impl WatcherQueueCancellation {
    /// Undo invalidates the exact publication's ticket under the operation
    /// gate. Prune after releasing the gate so queue admission can finish.
    pub(crate) fn prune_invalidated(&self) {
        self.pending.lock().unwrap_or_else(|error| error.into_inner())
            .retain(|item| item.ticket.is_current());
        {
            let mut manual = self.pending_manual.state.lock().unwrap_or_else(|error| error.into_inner());
            manual.arrivals.retain(|_, arrival| arrival.ticket.is_current());
            self.pending_manual.update_count(&manual);
        }
        if let Some(app) = self.app_handle.as_ref() {
            update_pending_tooltip(app, &self.pending_manual);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queue_cancellation_runs_while_watcher_owner_is_busy() {
        let suppression = Arc::new(SuppressionSet::new());
        let watcher = Mutex::new(FolderWatcher::new(suppression.clone()));
        let cancellation = watcher.lock().unwrap().queue_cancellation();
        let root = Arc::new(WatchedRoot::capture(&WatchedFolder {
            id: Some(1), path: std::env::temp_dir().canonicalize().unwrap().to_string_lossy().into_owned(),
            mode: crate::db::FOLDER_MODE_SILENT.into(), enabled: true, only_new: false,
        }).unwrap());
        let reviewed = root.path.join("reviewed.undocheck");
        let later = root.path.join("later.undocheck");
        for path in [&reviewed, &later] {
            cancellation.pending.lock().unwrap().push(pending_item(path, &root, suppression.ticket(path)));
            cancellation.pending_manual.state.lock().unwrap().arrivals.insert(path.to_string_lossy().into_owned(), PendingReview {
                root: root.clone(), ticket: suppression.ticket(path), excluded: false,
            });
        }
        let _busy_watcher = watcher.lock().unwrap();
        {
            let _operation = suppression.with_operation();
            suppression.invalidate(&[reviewed]);
            assert!(cancellation.pending.try_lock().is_ok());
            assert!(cancellation.pending_manual.state.try_lock().is_ok());
        }
        cancellation.prune_invalidated();

        let pending = cancellation.pending.try_lock().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].path, later);
        let state = cancellation.pending_manual.state.try_lock().unwrap();
        let manual = &state.arrivals;
        assert_eq!(manual.len(), 1);
        assert!(manual.contains_key(later.to_str().unwrap()));
    }

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
        let mut pending = pending_item(&path, &root, suppression.ticket(&path));
        for _ in 0..MAX_RETRIES {
            assert!(pending.defer(now));
            let retry_at = pending.retry_at.unwrap();
            assert!(retry_at > now);
            assert!(retry_at <= now + Duration::from_secs(60));
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

    #[test]
    fn only_new_registration_and_toggles_reconcile_the_review_queue() {
        use crate::db::{self, FOLDER_MODE_MANUAL, FOLDER_MODE_SUGGEST};
        use std::fs;

        let _serial = db::serialise_test_db();
        db::init_test_db();
        let root_path = std::env::temp_dir().join(format!("mouzi-only-new-{}", crate::operations::new_run_id()));
        fs::create_dir(&root_path).unwrap();
        struct Fixture { path: PathBuf, id: Option<i64> }
        impl Drop for Fixture {
            fn drop(&mut self) {
                if let Some(id) = self.id { let _ = db::remove_watched_folder(id); }
                let _ = fs::remove_dir_all(&self.path);
            }
        }
        let mut fixture = Fixture { path: root_path.clone(), id: None };
        fs::write(root_path.join("old.txt"), "existing file").unwrap();
        let id = db::register_watched_folder(root_path.to_str().unwrap(), FOLDER_MODE_MANUAL, true).unwrap();
        fixture.id = Some(id);
        fs::write(root_path.join("new.txt"), "new arrival").unwrap();

        let alias = fs::canonicalize(&root_path).unwrap();
        assert_eq!(db::register_watched_folder(alias.to_str().unwrap(), FOLDER_MODE_SUGGEST, false).unwrap(), id);
        let row = db::get_watched_folders().unwrap().into_iter().find(|folder| folder.id == Some(id)).unwrap();
        assert_eq!(row.mode, FOLDER_MODE_MANUAL);
        assert!(row.only_new);
        let mut watcher = FolderWatcher::new(Arc::new(SuppressionSet::new()));
        let root = watcher.ensure_watch(&row, false).unwrap();
        watcher.enqueue_existing_files(&root, None, None).unwrap();
        let pending = || ["new.txt".to_string(), "old.txt".to_string()];
        assert_eq!(watcher.get_pending_files().unwrap().into_iter().map(|(_, name)| name).collect::<Vec<_>>(), ["new.txt"]);

        for mode in [FOLDER_MODE_MANUAL, FOLDER_MODE_SUGGEST] {
            db::update_folder_mode(id, mode).unwrap();
            assert!(db::set_only_new(id, false).unwrap());
            watcher.reconcile_baseline(id).unwrap();
            let mut names = watcher.get_pending_files().unwrap().into_iter().map(|(_, name)| name).collect::<Vec<_>>();
            names.sort();
            assert_eq!(names, pending());

            assert!(db::set_only_new(id, true).unwrap());
            watcher.reconcile_baseline(id).unwrap();
            assert!(watcher.pending_manual.state.lock().unwrap().arrivals.is_empty());
        }
    }

    #[test]
    fn pending_exclusions_revalidate_the_public_queue_without_dropping_arrivals() {
        use crate::db::{self, FOLDER_MODE_MANUAL};
        use crate::ignore::save_mouziignore;
        use std::fs;

        let _serial = db::serialise_test_db();
        db::init_test_db();
        let mut fixture = RecoveryFixture::new();
        let directory = fixture.root();
        let id = db::register_watched_folder(directory.to_str().unwrap(), FOLDER_MODE_MANUAL, false).unwrap();
        fixture.id = Some(id);
        let row = get_watched_folders().unwrap().into_iter().find(|folder| folder.id == Some(id)).unwrap();
        let root = Arc::new(WatchedRoot::capture(&row).unwrap());
        let watcher = FolderWatcher::new(Arc::new(SuppressionSet::new()));
        let protected = root.path.join("protected.txt");
        let sibling = root.path.join("sibling.txt");
        let original = db::history_counts().unwrap().total;
        for (path, bytes) in [(&protected, b"protected contents".as_slice()), (&sibling, b"sibling contents".as_slice())] {
            fs::write(path, bytes).unwrap();
            enqueue_path(path, EnqueueSource::Notification, &root, &watcher.pending,
                &watcher.pending_manual, &watcher.suppression, None).unwrap();
        }
        let names = || {
            let mut found = watcher.get_pending_files().unwrap().into_iter().map(|(_, name)| name).collect::<Vec<_>>();
            found.sort();
            found
        };
        let folder_path = root.path.to_str().unwrap();
        assert_eq!(names(), ["protected.txt", "sibling.txt"]);
        assert_eq!(watcher.pending_manual.visible_count.load(Ordering::Acquire), 2);
        save_mouziignore(folder_path, &["protected.txt".into()]).unwrap();
        assert_eq!(names(), ["sibling.txt"]);
        assert_eq!(watcher.pending_manual.visible_count.load(Ordering::Acquire), 1);
        assert_eq!(watcher.pending_manual.state.lock().unwrap().arrivals.len(), 2);

        // A decoding failure must be visible and must leave both queued arrivals intact.
        fs::write(root.path.join(".mouziignore"), [0xff]).unwrap();
        assert!(watcher.get_pending_files().unwrap_err().contains("Unable to read ignore protection file"));
        assert_eq!(watcher.pending_manual.visible_count.load(Ordering::Acquire), 0);
        assert_eq!(watcher.pending_manual.state.lock().unwrap().arrivals.len(), 2);
        fs::remove_file(root.path.join(".mouziignore")).unwrap();

        assert_eq!(names(), ["protected.txt", "sibling.txt"]);
        assert_eq!(watcher.pending_manual.visible_count.load(Ordering::Acquire), 2);
        assert_eq!(fs::read(&protected).unwrap(), b"protected contents");
        assert_eq!(fs::read(&sibling).unwrap(), b"sibling contents");
        assert_eq!(db::history_counts().unwrap().total, original);
    }

    #[test]
    fn pending_exclusions_match_review_and_read_each_directory_once() {
        use crate::ignore::save_mouziignore;
        use std::fs;

        let fixture = RecoveryFixture::new();
        let folder = WatchedFolder {
            id: Some(1), path: fixture.root().to_string_lossy().into_owned(),
            enabled: true, mode: crate::db::FOLDER_MODE_MANUAL.into(), only_new: false,
        };
        let root = Arc::new(WatchedRoot::capture(&folder).unwrap());
        let child = root.path.join("child");
        fs::create_dir(&child).unwrap();
        save_mouziignore(root.path.to_str().unwrap(), &["root-*.txt".into()]).unwrap();
        save_mouziignore(child.to_str().unwrap(), &["parent-*.txt".into()]).unwrap();

        // The watcher currently enqueues only top-level files. Review also
        // applies these protections to explicit requests for nested files.
        let mut arrivals = HashMap::new();
        let suppression = SuppressionSet::new();
        for path in [root.path.join("root-top.txt"), child.join("root-nested.txt"),
            child.join("parent-nested.txt"), child.join("sibling.txt")] {
            fs::write(&path, b"fixture").unwrap();
            arrivals.insert(path.to_string_lossy().into_owned(), PendingReview {
                root: root.clone(), ticket: suppression.ticket(&path), excluded: false,
            });
        }
        let mut reads = HashMap::new();
        let actual = pending_exclusions(&arrivals, |directory| {
            *reads.entry(directory.to_path_buf()).or_insert(0) += 1;
            IgnorePatterns::from_folder(directory.to_str().unwrap())
        }).unwrap();
        assert_eq!(reads, HashMap::from([(root.path.clone(), 1), (child, 1)]));
        assert_eq!(actual.len(), 3);
        for source in arrivals.keys() {
            let expected = crate::rules::organization_path_is_protected(Path::new(source), &root.path).unwrap();
            assert_eq!(actual.contains(source), expected, "{source}");
        }
    }

    #[test]
    fn pending_read_errors_do_not_remove_unverified_arrivals() {
        let fixture = RecoveryFixture::new();
        let folder = WatchedFolder {
            id: Some(1), path: fixture.root().to_string_lossy().into_owned(),
            enabled: true, mode: crate::db::FOLDER_MODE_MANUAL.into(), only_new: false,
        };
        let root = Arc::new(WatchedRoot::capture(&folder).unwrap());
        let watcher = FolderWatcher::new(Arc::new(SuppressionSet::new()));
        // A path the OS cannot inspect gives a deterministic metadata error.
        let unreadable = fixture.root().join("invalid\0entry").to_string_lossy().into_owned();
        let present = fixture.root().join("present.txt");
        std::fs::write(&present, b"pending file").unwrap();
        let present = present.to_string_lossy().into_owned();
        let absent = fixture.root().join("absent.txt").to_string_lossy().into_owned();
        let folders = [folder];
        {
            let mut pending = watcher.pending_manual.state.lock().unwrap();
            for path in [&unreadable, &present, &absent] {
                pending.arrivals.insert(path.clone(), PendingReview {
                    root: root.clone(), ticket: watcher.suppression.ticket(Path::new(path)), excluded: false,
                });
            }
        }
        assert!(watcher.prune_queues(&folders).is_err());
        let mut pending = watcher.pending_manual.state.lock().unwrap();
        assert!(pending.arrivals.contains_key(&unreadable));
        assert!(pending.arrivals.contains_key(&present));
        pending.arrivals.remove(&unreadable);
        drop(pending);
        watcher.prune_queues(&folders).unwrap();
        let pending = watcher.pending_manual.state.lock().unwrap();
        assert!(pending.arrivals.contains_key(&present));
        assert!(!pending.arrivals.contains_key(&absent));
    }

    struct RecoveryFixture {
        parent: PathBuf,
        id: Option<i64>,
    }

    impl RecoveryFixture {
        fn new() -> Self {
            let parent = std::env::temp_dir().join(format!("mouzi-watch-recovery-{}", crate::operations::new_run_id()));
            std::fs::create_dir(&parent).unwrap();
            std::fs::create_dir(parent.join("watched")).unwrap();
            Self { parent, id: None }
        }

        fn root(&self) -> PathBuf { self.parent.join("watched") }
    }

    impl Drop for RecoveryFixture {
        fn drop(&mut self) {
            if let Some(id) = self.id { let _ = crate::db::remove_watched_folder(id); }
            let _ = std::fs::remove_dir_all(&self.parent);
        }
    }

    fn save_grace_period(seconds: i64) {
        let saved = crate::db::update_preferences(&crate::db::PreferencesPatch {
            grace_period_seconds: Some(seconds), ..Default::default()
        }).unwrap();
        assert_eq!(saved.grace_period_seconds, seconds);
    }

    fn with_queued_grace_fixture(
        initial_grace: i64,
        age: Duration,
        check: impl FnOnce(&FolderWatcher, &Path, &Path, Instant),
    ) {
        use crate::db::{self, FOLDER_MODE_SILENT};
        use std::fs;

        let _serial = db::serialise_test_db();
        db::init_test_db();
        let mut fixture = RecoveryFixture::new();
        let root_path = fs::canonicalize(fixture.root()).unwrap();
        let folder = db::register_watched_folder(root_path.to_str().unwrap(), FOLDER_MODE_SILENT, false).unwrap();
        fixture.id = Some(folder);
        let rule = db::add_rule(&db::Rule {
            name: "Silent grace fixture".into(), priority: i32::MIN, enabled: true,
            extensions: vec!["mouziwatchgrace".into()], destination: "Filed".into(),
            action: "move".into(), folder_id: folder, ..db::Rule::default()
        }).unwrap();
        let source = root_path.join("report.mouziwatchgrace");
        let destination = root_path.join("Filed").join(source.file_name().unwrap());
        struct Restore<'a> { rule: i64, grace: i64, source: &'a Path, destination: &'a Path }
        impl Drop for Restore<'_> {
            fn drop(&mut self) {
                use crate::db;
                if let Ok(conn) = db::lock_db() {
                    let source = self.source.to_str().unwrap();
                    let destination = self.destination.to_str().unwrap();
                    let _ = conn.execute("DELETE FROM action_logs WHERE source_path=?1", [source]);
                    let _ = conn.execute("DELETE FROM operation_intents WHERE source_path=?1", [source]);
                    let _ = conn.execute("DELETE FROM processed_files WHERE path IN (?1,?2)", [source, destination]);
                }
                let _ = db::delete_rule(self.rule);
                let _ = db::update_preferences(&db::PreferencesPatch {
                    grace_period_seconds: Some(self.grace), ..Default::default()
                });
            }
        }
        let _restore = Restore { rule, grace: get_settings().unwrap().grace_period_seconds, source: &source, destination: &destination };
        save_grace_period(initial_grace);
        fs::write(&source, b"Silent grace fixture").unwrap();
        fs::OpenOptions::new().write(true).open(&source).unwrap()
            .set_modified(SystemTime::now() - age).unwrap();
        let settings = get_watched_folders().unwrap().into_iter().find(|entry| entry.id == Some(folder)).unwrap();
        let root = Arc::new(WatchedRoot::capture_unattended(&settings, false).unwrap());
        let suppression = Arc::new(SuppressionSet::new());
        let watcher = FolderWatcher::new(suppression.clone());
        enqueue_path(&source, EnqueueSource::Notification, &root, &watcher.pending,
            &watcher.pending_manual, &suppression, None).unwrap();
        assert_eq!(watcher.pending.lock().unwrap().len(), 1);
        check(&watcher, &source, &destination, Instant::now());
    }

    #[test]
    fn changed_silent_grace_preserves_a_file_past_the_failure_retry_budget() {
        with_queued_grace_fixture(10, Duration::ZERO, |watcher, source, destination, queued| {
            save_grace_period(360);
            for second in [11, 15, 23, 39, 71, 131, 191, 251, 311, 330] {
                assert_eq!(watcher.tick(queued + Duration::from_secs(second)).organized, 0);
            }
            let pending = watcher.pending.lock().unwrap();
            assert_eq!(pending.len(), 1, "the unchanged file still has time left to settle");
            assert_eq!(pending[0].attempts, 0, "waiting for grace does not spend failure retries");
            assert_eq!(std::fs::read(source).unwrap(), b"Silent grace fixture");
            assert!(!destination.exists());
        });
    }

    #[test]
    fn changed_silent_grace_moves_a_queued_file_on_the_shorter_deadline() {
        with_queued_grace_fixture(10, Duration::from_secs(20), |watcher, source, destination, queued| {
            save_grace_period(0);
            assert_eq!(watcher.tick(queued + Duration::from_secs(1)).organized, 0);
            assert!(source.is_file(), "the two-second minimum still applies");
            let identity = crate::safe_fs::file_identity(source).unwrap();
            let shortened = queued + Duration::from_secs(3);
            assert_eq!(watcher.tick(shortened).organized, 1, "the current grace applies to queued work");
            assert!(!source.exists());
            assert_eq!(std::fs::read(destination).unwrap(), b"Silent grace fixture");
            assert_eq!(crate::safe_fs::file_identity(destination).unwrap(), identity);
            assert_eq!(watcher.tick(shortened + Duration::from_secs(10)).organized, 0);
        });
    }

    #[test]
    fn silent_grace_change_replaces_a_wait_from_the_live_file_age_check() {
        with_queued_grace_fixture(120, Duration::from_secs(20), |watcher, source, destination, queued| {
            // Advance the monotonic queue clock while the file's wall-clock
            // age is still below the saved grace period.
            let first_check = queued + Duration::from_secs(121);
            assert_eq!(watcher.tick(first_check).organized, 0);
            assert!(source.is_file());
            let pending = watcher.pending.lock().unwrap();
            assert_eq!(pending.len(), 1);
            assert_eq!(pending[0].attempts, 0);
            drop(pending);

            save_grace_period(0);
            assert_eq!(watcher.tick(first_check + Duration::from_secs(1)).organized, 1);
            assert!(!source.exists());
            assert_eq!(std::fs::read(destination).unwrap(), b"Silent grace fixture");
        });
    }

    #[cfg(windows)]
    #[test]
    fn silent_grace_reduction_preserves_the_backoff_after_a_sharing_failure() {
        use std::os::windows::fs::OpenOptionsExt;

        with_queued_grace_fixture(10, Duration::from_secs(20), |watcher, source, destination, queued| {
            let handle = std::fs::OpenOptions::new().read(true).share_mode(1 | 2).open(source).unwrap();
            let first_check = queued + Duration::from_secs(11);
            assert_eq!(watcher.tick(first_check).organized, 0);
            assert_eq!(watcher.pending.lock().unwrap()[0].attempts, 1);

            save_grace_period(0);
            drop(handle);
            assert_eq!(watcher.tick(first_check + Duration::from_secs(3)).organized, 0);
            assert!(source.is_file());
            assert_eq!(watcher.tick(first_check + Duration::from_secs(4)).organized, 1);
            assert!(!source.exists());
            assert_eq!(std::fs::read(destination).unwrap(), b"Silent grace fixture");
        });
    }

    #[test]
    fn switching_to_silent_keeps_live_manual_work_without_reviving_cancelled_tickets() {
        use crate::db::{self, FOLDER_MODE_MANUAL, FOLDER_MODE_SILENT};

        let _serial = db::serialise_test_db();
        db::init_test_db();
        let mut fixture = RecoveryFixture::new();
        let root = fixture.root();
        let configured = root.to_str().unwrap();
        for name in ["restored.txt", "new.txt"] {
            std::fs::write(root.join(name), name).unwrap();
        }
        let id = db::register_watched_folder(configured, FOLDER_MODE_MANUAL, false).unwrap();
        fixture.id = Some(id);
        let folders: Vec<_> = get_watched_folders().unwrap().into_iter().filter(|f| f.id == Some(id)).collect();
        let suppression = Arc::new(SuppressionSet::new());
        let mut watcher = FolderWatcher::new(suppression.clone());
        watcher.reconcile_watches_for_folders(&folders, Instant::now(), false, true).unwrap();
        assert_eq!(watcher.get_pending_files().unwrap().len(), 2);

        let watched = &watcher.watchers[configured].root.path;
        let restored = watched.join("restored.txt");
        let new_file = watched.join("new.txt");
        suppression.invalidate(&[restored]);
        db::update_folder_mode(id, FOLDER_MODE_SILENT).unwrap();
        watcher.flush_manual_to_pending(configured);

        let pending = watcher.pending.lock().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].path, new_file);
        assert!(pending[0].ticket.is_current());
    }

    #[test]
    fn watcher_recovers_only_the_registered_physical_folder() {
        use crate::db::{self, FOLDER_MODE_MANUAL};
        use std::fs;

        let _serial = db::serialise_test_db();
        db::init_test_db();
        let mut fixture = RecoveryFixture::new();
        let root_path = fixture.root();
        let path = root_path.to_str().unwrap();
        fixture.id = Some(db::register_watched_folder(path, FOLDER_MODE_MANUAL, false).unwrap());
        let folders = get_watched_folders().unwrap().into_iter().filter(|folder| folder.id == fixture.id).collect::<Vec<_>>();
        let offline = fixture.parent.join("offline");
        fs::rename(&root_path, &offline).unwrap();
        let mut watcher = FolderWatcher::new(Arc::new(SuppressionSet::new()));
        assert!(watcher.reconcile_watches_for_folders(&folders, Instant::now(), true, true).is_err());
        assert!(watcher.issues().contains_key(path));

        fs::create_dir(&root_path).unwrap();
        fs::write(root_path.join("wrong-drive.txt"), "keep").unwrap();
        let retry_at = watcher.failures[path].retry_at;
        assert!(watcher.reconcile_watches_for_folders(&folders, retry_at, false, false).is_err());
        assert!(watcher.issues()[path].contains("different physical folder"));
        assert!(!watcher.watchers.contains_key(path));
        assert!(watcher.get_pending_files().unwrap().is_empty());

        fs::rename(&root_path, fixture.parent.join("replacement")).unwrap();
        fs::rename(&offline, &root_path).unwrap();
        let arrival = root_path.join("missed-while-offline.txt");
        fs::write(&arrival, "original").unwrap();
        let retry_at = watcher.failures[path].retry_at;
        assert!(watcher.reconcile_watches_for_folders(&folders, retry_at - Duration::from_millis(1), false, false).is_ok());
        assert!(!watcher.watchers.contains_key(path));
        watcher.reconcile_watches_for_folders(&folders, retry_at, false, false).unwrap();
        assert!(!watcher.issues().contains_key(path));
        assert_eq!(watcher.get_pending_files().unwrap().into_iter().map(|(_, name)| name).collect::<Vec<_>>(),
            ["missed-while-offline.txt"]);
        let recovered = watcher.watchers[path].root.clone();
        record_callback_health(None, &watcher.callback_warnings, &recovered, Some("Fixture entry could not be read".into()), false);
        watcher.reconcile_watches_for_folders(&folders, retry_at + WATCH_CHECK_INTERVAL, false, false).unwrap();
        assert!(Arc::ptr_eq(&watcher.watchers[path].root, &recovered));
        assert_eq!(watcher.issues()[path], "Fixture entry could not be read");
        record_callback_health(None, &watcher.callback_warnings, &recovered, None, false);
        assert!(!watcher.issues().contains_key(path));
        assert_eq!(fs::read_to_string(fixture.parent.join("replacement/wrong-drive.txt")).unwrap(), "keep");

        record_callback_health(None, &watcher.callback_warnings, &recovered, Some("Fixture notifier stopped".into()), true);
        let after_retirement = retry_at + WATCH_CHECK_INTERVAL * 2;
        assert!(watcher.reconcile_watches_for_folders(&folders, after_retirement, false, false).is_err());
        assert_eq!(watcher.issues()[path], "Fixture notifier stopped");
        assert!(watcher.get_pending_files().unwrap().is_empty());
        let retry_at = watcher.failures[path].retry_at;
        watcher.reconcile_watches_for_folders(&folders, retry_at, false, false).unwrap();
        assert!(!Arc::ptr_eq(&watcher.watchers[path].root, &recovered));
        enqueue_path(&arrival, EnqueueSource::Notification, &recovered, &watcher.pending,
            &watcher.pending_manual, &watcher.suppression, None).unwrap();
        assert!(Arc::ptr_eq(&watcher.pending_manual.state.lock().unwrap().arrivals[arrival.to_str().unwrap()].root, &watcher.watchers[path].root));
    }

    #[cfg(windows)]
    #[test]
    fn catch_up_delivers_healthy_siblings_while_a_processed_file_is_write_open() {
        use crate::db::{self, FOLDER_MODE_SILENT};
        use std::fs;
        use std::os::windows::fs::OpenOptionsExt;

        let _serial = db::serialise_test_db();
        db::init_test_db();
        let mut fixture = RecoveryFixture::new();
        let root = fs::canonicalize(fixture.root()).unwrap();
        let configured = root.to_str().unwrap();
        let folder = db::register_watched_folder(configured, FOLDER_MODE_SILENT, false).unwrap();
        fixture.id = Some(folder);
        let rule = db::add_rule(&db::Rule {
            name: "Catch-up recovery fixture".into(), priority: i32::MIN, enabled: true,
            extensions: vec!["mouziwatchcatchup".into()], destination: "Filed".into(),
            action: "move".into(), folder_id: folder, ..db::Rule::default()
        }).unwrap();
        let folders = get_watched_folders().unwrap().into_iter()
            .filter(|entry| entry.id == Some(folder)).collect::<Vec<_>>();
        let age = Duration::from_secs(get_settings().unwrap().grace_period_seconds.max(2) as u64 + 10);
        for name in ["one", "two", "three"] {
            let path = root.join(format!("{name}.mouziwatchcatchup"));
            fs::write(&path, name.as_bytes()).unwrap();
            fs::OpenOptions::new().write(true).open(path).unwrap()
                .set_modified(SystemTime::now() - age).unwrap();
        }
        let entries = fs::read_dir(&root).unwrap().map(|entry| entry.unwrap().path()).collect::<Vec<_>>();
        let [earlier, protected, later] = entries.as_slice() else { panic!("Expected three observed arrivals") };
        let stored = format!("blake3:{}", blake3::hash(&fs::read(protected).unwrap()).to_hex());
        struct RemoveRecords<'a> { root: &'a Path, entries: &'a [PathBuf], rule: i64 }
        impl Drop for RemoveRecords<'_> {
            fn drop(&mut self) {
                if let Ok(conn) = db::lock_db() {
                    for source in self.entries {
                        let source_path = source.to_str().unwrap();
                        let destination = self.root.join("Filed").join(source.file_name().unwrap());
                        let _ = conn.execute("DELETE FROM action_logs WHERE source_path=?1", [source_path]);
                        let _ = conn.execute("DELETE FROM operation_intents WHERE source_path=?1", [source_path]);
                        let _ = conn.execute("DELETE FROM processed_files WHERE path IN (?1,?2)",
                            [source_path, destination.to_str().unwrap()]);
                    }
                }
                let _ = db::delete_rule(self.rule);
            }
        }
        let _records = RemoveRecords { root: &root, entries: &entries, rule };
        db::lock_db().unwrap().execute("INSERT INTO processed_files(path,fingerprint) VALUES(?1,?2)",
            [protected.to_str().unwrap(), &stored]).unwrap();
        assert!(crate::operations::already_processed(protected).unwrap());
        // Ordinary Windows writers allow readers yet a guarded fingerprint read
        // must still refuse them. Keep the writer open through both deliveries.
        let writer = fs::OpenOptions::new().write(true).share_mode(1 | 2 | 4).open(protected).unwrap();
        assert!(crate::safe_fs::open_guarded_regular_file(protected).err().unwrap().contains("os error 32"));
        let protected_bytes = fs::read(protected).unwrap();
        let sibling_bytes = [fs::read(earlier).unwrap(), fs::read(later).unwrap()];

        let mut watcher = FolderWatcher::new(Arc::new(SuppressionSet::new()));
        let now = Instant::now();
        assert!(watcher.reconcile_watches_for_folders(&folders, now, false, true).is_err());
        let snapshot = |watcher: &FolderWatcher| {
            watcher.pending.lock().unwrap().iter()
                .map(|item| (item.path.clone(), item.clone())).collect::<HashMap<_, _>>()
        };
        let initial = snapshot(&watcher);
        assert_eq!(initial.len(), 2, "both observed healthy siblings must remain eligible");
        for sibling in [earlier, later] { assert!(initial.contains_key(sibling)); }
        assert!(!initial.contains_key(protected));
        assert!(watcher.issues()[configured].contains(protected.to_str().unwrap()));

        let first_retry = watcher.failures[configured].retry_at;
        assert!(watcher.reconcile_watches_for_folders(&folders, first_retry - Duration::from_millis(1), false, false).is_ok());
        assert_eq!(watcher.failures[configured].retry_at, first_retry);
        assert!(watcher.reconcile_watches_for_folders(&folders, first_retry, false, false).is_err());
        assert!(watcher.failures[configured].delay > WATCH_CHECK_INTERVAL);
        let after_retry = snapshot(&watcher);
        for sibling in [earlier, later] {
            assert!(initial[sibling].ticket.is_current());
            assert_eq!(initial[sibling].queued_at, after_retry[sibling].queued_at);
            assert_eq!(initial[sibling].retry_at, after_retry[sibling].retry_at);
            assert_eq!(initial[sibling].attempts, after_retry[sibling].attempts);
        }
        let grace = organization_grace_period(get_settings().unwrap().grace_period_seconds);
        let ready = initial.values().filter_map(|item| item.ready_at(grace)).max().unwrap();
        assert_eq!(watcher.tick(ready).organized, 2);
        for (sibling, bytes) in [earlier, later].into_iter().zip(sibling_bytes) {
            assert!(!sibling.exists());
            assert_eq!(fs::read(root.join("Filed").join(sibling.file_name().unwrap())).unwrap(), bytes);
        }
        assert!(watcher.issues().contains_key(configured));
        assert_eq!(fs::read(protected).unwrap(), protected_bytes);

        drop(writer);
        assert!(crate::operations::already_processed(protected).unwrap());
        let next_retry = watcher.failures[configured].retry_at;
        watcher.reconcile_watches_for_folders(&folders, next_retry, false, false).unwrap();
        assert!(!watcher.issues().contains_key(configured));
        assert!(snapshot(&watcher).is_empty());
        assert_eq!(fs::read(protected).unwrap(), protected_bytes);

        let writer = fs::OpenOptions::new().write(true).share_mode(1 | 2 | 4).open(protected).unwrap();
        assert!(watcher.reconcile_baseline(folder).is_err());
        assert!(watcher.issues().contains_key(configured));
        drop(writer);
        let moved_away = fixture.parent.join("dismissed");
        fs::rename(protected, &moved_away).unwrap();
        let next_retry = watcher.failures[configured].retry_at;
        watcher.reconcile_watches_for_folders(&folders, next_retry, false, false).unwrap();
        assert!(!watcher.issues().contains_key(configured));
        assert!(snapshot(&watcher).is_empty());
        assert_eq!(fs::read(moved_away).unwrap(), protected_bytes);
    }

    #[test]
    fn catch_up_keeps_pending_grace_until_a_real_change() {
        use crate::db::{self, FOLDER_MODE_SILENT};
        use std::fs;

        let _serial = db::serialise_test_db();
        db::init_test_db();
        let mut fixture = RecoveryFixture::new();
        let path = fs::canonicalize(fixture.root()).unwrap();
        fixture.id = Some(db::register_watched_folder(path.to_str().unwrap(), FOLDER_MODE_SILENT, false).unwrap());
        let folder = get_watched_folders().unwrap().into_iter().find(|folder| folder.id == fixture.id).unwrap();
        let root = Arc::new(WatchedRoot::capture_unattended(&folder, false).unwrap());
        let watcher = FolderWatcher::new(Arc::new(SuppressionSet::new()));
        let name = OsString::from("grace.txt");
        let file = root.path.join(&name);
        fs::write(&file, b"incoming").unwrap();
        watcher.enqueue_existing_files(&root, None, None).unwrap();
        let before = {
            let mut pending = watcher.pending.lock().unwrap();
            pending[0].retry_at = Some(Instant::now() - Duration::from_secs(10));
            pending[0].attempts = 2;
            pending[0].clone()
        };
        watcher.enqueue_existing_files(&root, None, Some(std::slice::from_ref(&name))).unwrap();
        {
            let pending = watcher.pending.lock().unwrap();
            assert_eq!(pending.len(), 1);
            assert!(before.ticket.is_current());
            assert_eq!(pending[0].queued_at, before.queued_at);
            assert_eq!(pending[0].retry_at, before.retry_at);
            assert_eq!(pending[0].attempts, 2);
        }

        // Deliver a genuine content change through the live-notification entry point
        // without racing an asynchronous second callback in this deadline test.
        fs::write(&file, b"updated arrival").unwrap();
        let event = Instant::now();
        enqueue_path(&file, EnqueueSource::Notification, &root, &watcher.pending,
            &watcher.pending_manual, &watcher.suppression, None).unwrap();
        let pending = watcher.pending.lock().unwrap();
        assert_eq!(pending.len(), 1);
        assert!(!before.ticket.is_current());
        assert!(pending[0].ticket.is_current());
        assert_ne!(pending[0].fingerprint, before.fingerprint);
        assert!(pending[0].queued_at >= event);
        assert!(pending[0].retry_at.is_none());
        assert_eq!(pending[0].attempts, 0);
    }

    #[test]
    fn missing_legacy_watcher_requires_startup_or_explicit_registration_to_bind() {
        use crate::db::{self, FOLDER_MODE_MANUAL};
        use std::fs;

        let _serial = db::serialise_test_db();
        db::init_test_db();
        let mut fixture = RecoveryFixture::new();
        let path = fixture.root();
        let configured = path.to_str().unwrap();
        fixture.id = Some(db::add_watched_folder(configured, FOLDER_MODE_MANUAL).unwrap());
        let row = get_watched_folders().unwrap().into_iter().find(|folder| folder.id == fixture.id).unwrap();
        let folders = std::slice::from_ref(&row);
        let offline = fixture.parent.join("offline");
        fs::rename(&path, &offline).unwrap();
        let mut watcher = FolderWatcher::new(Arc::new(SuppressionSet::new()));
        assert!(watcher.reconcile_watches_for_folders(folders, Instant::now(), true, true).is_err());
        fs::rename(&offline, &path).unwrap();

        assert!(watcher.reconcile_watches_for_folders(folders, watcher.failures[configured].retry_at, false, false).is_err());
        assert!(watcher.issues()[configured].contains("no saved folder identity"));
        assert!(!watcher.watchers.contains_key(configured));
        watcher.reconcile_watches_for_folders(folders, Instant::now(), true, true).unwrap();
        assert!(WatchedRoot::capture_unattended(&row, false).is_ok());
    }

    #[test]
    fn public_automatic_processing_leaves_a_replacement_folder_untouched() {
        use crate::db::{self, FOLDER_MODE_MANUAL, FOLDER_MODE_SILENT};
        use std::fs;

        let _serial = db::serialise_test_db();
        db::init_test_db();
        let mut fixture = RecoveryFixture::new();
        let root = fixture.root();
        let id = db::register_watched_folder(root.to_str().unwrap(), FOLDER_MODE_SILENT, false).unwrap();
        fixture.id = Some(id);
        let rule = db::add_rule(&db::Rule {
            name: "Automatic watcher authority fixture".into(), priority: i32::MIN, enabled: true,
            extensions: vec!["mouziwatchrecovery".into()], destination: "Filed".into(),
            action: "move".into(), folder_id: id, ..db::Rule::default()
        }).unwrap();
        struct RemoveRule(i64);
        impl Drop for RemoveRule {
            fn drop(&mut self) { let _ = db::delete_rule(self.0); }
        }
        let _rule = RemoveRule(rule);

        fs::rename(&root, fixture.parent.join("original")).unwrap();
        fs::create_dir(&root).unwrap();
        let source = root.join("replacement.mouziwatchrecovery");
        fs::write(&source, b"keep this folder's file").unwrap();
        let suppression = SuppressionSet::new();
        let run = || crate::rules::process_file_with_suppression(
            &source, true, &root, &suppression, &suppression.ticket(&source), None, true,
        );
        assert!(run().err().unwrap().contains("different physical folder"));
        assert_eq!(fs::read(&source).unwrap(), b"keep this folder's file");
        assert!(!root.join("Filed").exists());

        db::update_folder_mode(id, FOLDER_MODE_MANUAL).unwrap();
        assert!(matches!(run().unwrap(), ProcessOutcome::Skipped));
        assert_eq!(fs::read(&source).unwrap(), b"keep this folder's file");
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
        let folder_id = db::register_watched_folder(&root_path.to_string_lossy(), db::FOLDER_MODE_SILENT, false).unwrap();
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
        let watcher = FolderWatcher::new(suppression.clone());
        let root = Arc::new(WatchedRoot::capture_unattended(&row, false).unwrap());
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
                enqueue_path(&source, EnqueueSource::Notification, &root, &watcher.pending,
                    &watcher.pending_manual, &suppression, None).unwrap();
                let pending = watcher.pending.lock().unwrap();
                break pending.iter().find(|item| item.path == source)
                    .expect("the native arrival should enter the production organizer queue").clone();
            }
        };

        let grace = organization_grace_period(get_settings().unwrap().grace_period_seconds);
        let first = arrive(b"first file");
        assert_eq!(watcher.tick(first.ready_at(grace).unwrap()).organized, 1);
        let first_output = root_path.join("Filed").join(source.file_name().unwrap());
        assert_eq!(fs::read(&first_output).unwrap(), b"first file");
        assert!(suppression.is_suppressed(&first_output, Instant::now()));

        let second = arrive(b"second file");
        assert_ne!(first.identity, second.identity);
        let read_handle = fs::OpenOptions::new().read(true).share_mode(1 | 2).open(&source).unwrap();
        assert!(fs::OpenOptions::new().write(true).open(&source).is_ok(), "the write probe should pass");
        let second_deadline = second.ready_at(grace).unwrap();
        assert_eq!(watcher.tick(second_deadline).organized, 0);
        assert_eq!(fs::read(&source).unwrap(), b"second file");
        let deferred = {
            let pending = watcher.pending.lock().unwrap();
            assert_eq!(pending.len(), 1);
            pending[0].clone()
        };
        assert_eq!(deferred.identity, second.identity);
        assert_eq!(deferred.attempts, 1);
        assert_eq!(deferred.retry_at, Some(second_deadline + Duration::from_secs(4)));
        {
            let conn = db::lock_db().unwrap();
            let failed_count: i64 = conn.query_row(
                "SELECT COUNT(*) FROM operation_intents WHERE source_path=?1 AND action='move' AND status='failed'",
                [source.to_string_lossy().as_ref()], |row| row.get(0),
            ).unwrap();
            assert_eq!(failed_count, 1);
        }
        drop(read_handle);
        assert_eq!(watcher.tick(deferred.retry_at.unwrap() - Duration::from_millis(1)).organized, 0);
        assert_eq!(watcher.tick(deferred.retry_at.unwrap()).organized, 1);
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
        assert_eq!(watcher.tick(paused.ready_at(grace).unwrap()).organized, 0);
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
        assert!(WatchedRoot::capture_unattended(&row, false).err().unwrap().contains("different physical folder"));
        assert!(register_root_watch(&root, |_: Result<Event, notify::Error>| {}).is_err());
        assert_eq!(fs::read(&source).unwrap(), b"keep when paused");

        let conn = db::lock_db().unwrap();
        let history_count: i64 = conn.query_row("SELECT COUNT(*) FROM action_logs WHERE source_path=?1",
            [source.to_string_lossy().as_ref()], |row| row.get(0)).unwrap();
        assert_eq!(history_count, 2);
    }
}
