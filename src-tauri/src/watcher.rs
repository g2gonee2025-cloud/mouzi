use crate::db::{get_watched_folders, is_folder_manual_mode, is_folder_paused_mode};
use crate::rules::{is_file_ignored_by_mouziignore, process_file, should_ignore_file};
use crate::suppress::SuppressionSet;
use notify::{Config, Event, RecommendedWatcher, RecursiveMode, Watcher};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::Emitter;
#[cfg(not(target_os = "windows"))]
use tauri_plugin_notification::NotificationExt;

/// What one pass of the 500 ms loop organised, handed back to the thread so the
/// notification block can stay there and the filesystem work stays testable.
#[derive(Default)]
struct BatchOutcome {
    organized: usize,
    last_file_name: String,
    last_rule_name: String,
    last_dest_folder: String,
}

#[derive(Debug, Clone)]
struct PendingFile {
    path: PathBuf,
    /// The watched root this file was found under, carried rather than
    /// re-derived. Under a non-recursive watch `path.parent()` *is* the root, so
    /// carrying it changes nothing today; the moment the watch is recursive,
    /// `parent()` becomes an arbitrary subfolder and the two stop being
    /// interchangeable.
    root: PathBuf,
    scheduled: Instant,
}

pub struct FolderWatcher {
    watchers: HashMap<String, RecommendedWatcher>,
    pending: Arc<Mutex<Vec<PendingFile>>>,
    /// Files detected in manual-mode folders, waiting for the user to trigger Clean Now.
    pending_manual: Arc<Mutex<HashSet<String>>>,
    suppression: Arc<SuppressionSet>,
    /// Shared with AppState — stores the last destination folder to open on notification click
    pending_open_folder: Arc<Mutex<Option<String>>>,
    handle: Option<std::thread::JoinHandle<()>>,
    app_handle: Option<tauri::AppHandle>,
}

impl FolderWatcher {
    pub fn new(
        suppression: Arc<SuppressionSet>,
        pending_open_folder: Arc<Mutex<Option<String>>>,
    ) -> Self {
        Self {
            watchers: HashMap::new(),
            pending: Arc::new(Mutex::new(Vec::new())),
            pending_manual: Arc::new(Mutex::new(HashSet::new())),
            suppression,
            pending_open_folder,
            handle: None,
            app_handle: None,
        }
    }

    /// A second view over the same queues, for the processing thread.
    ///
    /// The thread never touches `watchers` — that map belongs to whichever
    /// thread called `watch_folders`, and sharing it would mean putting the
    /// watcher lock on the per-file path, which is precisely what the three-phase
    /// undo split exists to avoid. Every queue here is already an `Arc`, so this
    /// is a shallow copy and no state is duplicated.
    fn processing_view(&self, app_handle: tauri::AppHandle) -> Self {
        Self {
            watchers: HashMap::new(),
            pending: self.pending.clone(),
            pending_manual: self.pending_manual.clone(),
            suppression: self.suppression.clone(),
            pending_open_folder: self.pending_open_folder.clone(),
            handle: None,
            app_handle: Some(app_handle),
        }
    }

    pub fn start(&mut self, app_handle: tauri::AppHandle) {
        self.app_handle = Some(app_handle.clone());
        let worker = self.processing_view(app_handle.clone());

        // Spawn a thread that processes pending files after a delay
        let handle_thread = std::thread::spawn(move || {
            loop {
                std::thread::sleep(Duration::from_millis(500));
                let outcome = worker.tick(Instant::now());

                // Show a single notification for this batch
                if outcome.organized > 0 {
                    // Store the destination folder so single-instance handler can open it
                    // when the user clicks the notification (Windows activates the app)
                    *worker.pending_open_folder.lock().unwrap() =
                        Some(outcome.last_dest_folder.clone());

                    let body = if outcome.organized == 1 {
                        format!("{} → {}", outcome.last_file_name, outcome.last_rule_name)
                    } else {
                        format!("Organized {} files", outcome.organized)
                    };

                    #[cfg(target_os = "windows")]
                    {
                        let dest_folder_clone = outcome.last_dest_folder.clone();
                        let body_clone = body.clone();
                        let _ = std::thread::spawn(move || {
                            let _ = tauri_winrt_notification::Toast::new("cc.mouzi.app")
                                .title("Mouzi – click to open folder")
                                .text1(&body_clone)
                                .on_activated(move |_action| {
                                    // Open the folder in Explorer robustly
                                    let _ = std::process::Command::new("cmd")
                                        .args(["/c", "start", "", &dest_folder_clone])
                                        .spawn();
                                    Ok(())
                                })
                                .show();
                        });
                    }

                    #[cfg(not(target_os = "windows"))]
                    {
                        if let Some(app) = worker.app_handle.as_ref() {
                            let _ = app
                                .notification()
                                .builder()
                                .title("Mouzi – click to open folder")
                                .body(body)
                                .extra("destFolder", outcome.last_dest_folder)
                                .show();
                        }
                    }
                }
            }
        });

        self.handle = Some(handle_thread);
    }

    /// One pass of the processing loop: sweep the guards, drain whatever the
    /// grace period has released, and organise it.
    ///
    /// Split out of the thread so a test can drive a full grace cycle by calling
    /// this directly, instead of waiting five real minutes for a scheduled time
    /// to arrive. `now` is a parameter for the same reason `SuppressionSet`
    /// takes one: the loop's only clock is the instant the thread woke up.
    fn tick(&self, now: Instant) -> BatchOutcome {
        let mut outcome = BatchOutcome::default();
        // Read once. `None` only in a test that never had a Tauri app, which is
        // what lets a test drive a whole batch with no app handle at all.
        let app = self.app_handle.as_ref();

        let to_process: Vec<PendingFile> = {
            let mut guard = self.pending.lock().unwrap();
            let ready: Vec<_> = guard
                .iter()
                .filter(|p| now >= p.scheduled)
                .cloned()
                .collect();
            guard.retain(|p| now < p.scheduled);
            ready
        };

        // Expired entries used to be dropped only when an event happened
        // to arrive for that exact path after the window closed. An undo
        // produces a single event inside the window, so those entries
        // were never revisited and the map grew for the life of the app.
        // First on the tick, before the drain: it is the only thing that takes
        // the suppression lock, it holds nothing else, and it does no I/O, so
        // the map is at its smallest when the drain below starts looking at paths.
        self.suppression.sweep(now);

        for pending in to_process {
            let path = &pending.path;
            if path.exists() && path.is_file() {
                // Defensive check: if the folder has been switched to manual or paused
                // since the file was queued, skip it instead of auto-organizing.
                // Exact root equality, not a containment test: the root is the
                // one the enqueue site saw, so a folder that stopped being
                // watched is recognised as such. An unknown root skips too — a
                // file whose folder was removed during the grace period should
                // not be organised out of a folder the user just un-watched.
                let watched = get_watched_folders()
                    .ok()
                    .and_then(|folders| {
                        let root = pending.root.to_string_lossy();
                        folders.into_iter().find(|f| f.path == root)
                    });
                if watched
                    .as_ref()
                    .map(|f| {
                        !f.enabled || is_folder_paused_mode(&f.mode) || is_folder_manual_mode(&f.mode)
                    })
                    .unwrap_or(true)
                {
                    continue;
                }
                // Files in the pending queue have already waited for the grace period,
                // so we bypass the grace check here to avoid files being skipped forever
                // if their modification time changes while queued.
                match process_file(path, true, &pending.root) {
                    Ok(Some((rule, dest))) => {
                        let file_name = path.file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .to_string();
                        let dest_folder = std::path::Path::new(&dest)
                            .parent()
                            .map(|p| p.to_string_lossy().to_string())
                            .unwrap_or_else(|| {
                                path.parent()
                                    .map(|p| p.to_string_lossy().to_string())
                                    .unwrap_or_default()
                            });

                        outcome.last_file_name = file_name.clone();
                        outcome.last_rule_name = rule.name.clone();
                        outcome.last_dest_folder = dest_folder.clone();
                        outcome.organized += 1;

                        // Emit event to frontend (in-app toast)
                        if let Some(app) = app {
                            let _ = app.emit(
                                "file-organized",
                                serde_json::json!({
                                    "file": file_name,
                                    "rule": rule.name,
                                    "destination": dest,
                                    "destination_folder": dest_folder,
                                    "success": true
                                }),
                            );
                        }
                    }
                    Ok(None) => {}
                    Err(e) => {
                        if let Some(app) = app {
                            let _ = app.emit(
                                "file-organized",
                                serde_json::json!({
                                    "file": path.to_string_lossy(),
                                    "error": e,
                                    "success": false
                                }),
                            );
                        }
                    }
                }
            }
        }

        outcome
    }

    pub fn watch_folders(&mut self, app_handle: tauri::AppHandle) -> Result<(), String> {
        self.app_handle = Some(app_handle.clone());
        let folders = get_watched_folders().map_err(|e| e.to_string())?;
        let pending = self.pending.clone();
        let pending_manual = self.pending_manual.clone();
        let ignored = self.suppression.clone();
        let handle = app_handle.clone();

        // Read once per call, not once per folder: this is a database read or an
        // environment lookup, and its answer cannot differ between two folders
        // watched in the same breath. `recursive_requested` is deliberately
        // unused by the `watch()` calls below — see the STAGE 2 SEAM there.
        if recursive_watch_enabled() {
            // Loud, on purpose. A flag that is read and then ignored without a
            // word is indistinguishable from a flag whose plumbing is broken, and
            // the next person to set it concludes the wrong thing about the code.
            eprintln!(
                "[watcher] recursive watch was requested but is not enabled at this stage; \
                 every folder is being watched non-recursively"
            );
        }

        for folder in folders {
            if !folder.enabled {
                continue;
            }
            if is_folder_paused_mode(&folder.mode) {
                // Paused folders are kept in the DB but not watched.
                continue;
            }
            if !Path::new(&folder.path).exists() {
                eprintln!("[watcher] Skipping missing folder: {}", folder.path);
                continue;
            }

            let is_manual = is_folder_manual_mode(&folder.mode);
            let folder_path = folder.path.clone();
            let p = pending.clone();
            let pm = pending_manual.clone();
            let ig = ignored.clone();
            let h = handle.clone();

            // Initial scan: process files that already exist in the folder.
            // This ensures files present before the folder was added are not ignored forever.
            if let Ok(entries) = std::fs::read_dir(&folder.path) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_file()
                        && !should_ignore_file(&path)
                        && !is_file_ignored_by_mouziignore(&path)
                    {
                        let path_str = path.to_string_lossy().to_string();
                        if ig.is_suppressed(&path, Instant::now()) {
                            continue;
                        }

                        if is_manual {
                            let file_name = path.file_name()
                                .unwrap_or_default()
                                .to_string_lossy()
                                .to_string();
                            let mut manual = pm.lock().unwrap();
                            manual.insert(path_str.clone());
                            let count = manual.len();
                            drop(manual);
                            let _ = h.emit("file-detected", serde_json::json!({
                                "folder": folder_path,
                                "file": file_name,
                            }));
                            crate::tray::update_tray_tooltip(&h, count);
                        } else {
                            let grace = crate::db::get_settings()
                                .map(|s| s.grace_period_seconds as u64)
                                .unwrap_or(300);
                            let mut guard = p.lock().unwrap();
                            guard.retain(|x| x.path != path);
                            guard.push(PendingFile {
                                root: PathBuf::from(&folder_path),
                                path,
                                scheduled: Instant::now() + Duration::from_secs(grace),
                            });
                        }
                    }
                }
            }

            let p = pending.clone();
            let pm = pending_manual.clone();
            let ig = ignored.clone();
            let h = handle.clone();

            let mut watcher = RecommendedWatcher::new(
                move |res: Result<Event, notify::Error>| {
                    if let Ok(event) = res {
                        for path in event.paths {
                            if path.is_file()
                                && !should_ignore_file(&path)
                                && !is_file_ignored_by_mouziignore(&path)
                            {
                                let path_str = path.to_string_lossy().to_string();
                                if ig.is_suppressed(&path, Instant::now()) {
                                    continue;
                                }

                                if is_manual {
                                    // Manual mode: collect for later, do not auto-organize.
                                    let file_name = path.file_name()
                                        .unwrap_or_default()
                                        .to_string_lossy()
                                        .to_string();
                                    let mut manual = pm.lock().unwrap();
                                    manual.insert(path_str.clone());
                                    let count = manual.len();
                                    drop(manual);

                                    let _ = h.emit("file-detected", serde_json::json!({
                                        "folder": folder_path,
                                        "file": file_name,
                                    }));
                                    crate::tray::update_tray_tooltip(&h, count);
                                } else {
                                    // Silent mode: schedule for auto-organize after grace period.
                                    let grace = crate::db::get_settings()
                                        .map(|s| s.grace_period_seconds as u64)
                                        .unwrap_or(300);
                                    let mut guard = p.lock().unwrap();
                                    // Remove existing pending entry for this path to reschedule
                                    guard.retain(|x| x.path != path);
                                    guard.push(PendingFile {
                                        root: PathBuf::from(&folder_path),
                                        path,
                                        scheduled: Instant::now() + Duration::from_secs(grace),
                                    });
                                }
                            }
                        }
                    }
                },
                Config::default()
                    .with_poll_interval(Duration::from_secs(2))
                    .with_compare_contents(true),
            )
            .map_err(|e| e.to_string())?;

            // STAGE 2 SEAM. This is the only line that changes when recursion is
            // enabled, and it must not change alone: with the watch recursive and
            // the self-trigger guard not yet proven, the app re-queues its own
            // output and writes an `action_logs` row per generation, forever, in a
            // tool that runs all day. Recursion and the guard ship together.
            let mode = RecursiveMode::NonRecursive;
            watcher
                .watch(Path::new(&folder.path), mode)
                .map_err(|e| e.to_string())?;

            self.watchers.insert(folder.path.clone(), watcher);
        }

        // Only start the processing thread once. refresh() reuses the same thread.
        if self.handle.is_none() {
            self.start(app_handle);
        }
        Ok(())
    }

    pub fn refresh(&mut self, app_handle: tauri::AppHandle) -> Result<(), String> {
        self.watchers.clear();
        self.watch_folders(app_handle)
    }

    fn update_tray_tooltip(&self) {
        if let Some(app) = &self.app_handle {
            let count = self.pending_manual.lock().unwrap().len();
            crate::tray::update_tray_tooltip(app, count);
        }
    }

    /// Return the list of files detected in manual-mode folders that have not yet
    /// been organized. Non-existent files are pruned automatically.
    pub fn get_pending_files(&self) -> Vec<(String, String)> {
        let mut manual = self.pending_manual.lock().unwrap();
        manual.retain(|p| Path::new(p).exists());
        let result: Vec<(String, String)> = manual
            .iter()
            .filter_map(|path| {
                let p = Path::new(path);
                let folder = p.parent()?.to_string_lossy().to_string();
                let name = p.file_name()?.to_string_lossy().to_string();
                Some((folder, name))
            })
            .collect();
        drop(manual);
        self.update_tray_tooltip();
        result
    }

    /// Move any manually-collected files for the given folder into the auto-organize
    /// queue. Called when a folder is switched from manual back to silent.
    pub fn flush_manual_to_pending(&mut self, folder_path: &str) {
        let root = PathBuf::from(folder_path);
        let mut manual = self.pending_manual.lock().unwrap();
        // Containment, not parent equality. For every file a non-recursive watch
        // can have collected the two are the same test, so this changes nothing
        // today; the moment the watch is recursive, parent equality silently
        // drops every subfolder file the user was told was detected.
        let to_move: Vec<String> = manual
            .iter()
            .filter(|p| crate::safe_fs::is_within_any_root(Path::new(p), std::slice::from_ref(&root)))
            .cloned()
            .collect();
        for p in &to_move {
            manual.remove(p);
        }
        drop(manual);
        self.update_tray_tooltip();

        let grace = crate::db::get_settings()
            .map(|s| s.grace_period_seconds as u64)
            .unwrap_or(300);
        let mut pending = self.pending.lock().unwrap();
        for path_str in to_move {
            let path = PathBuf::from(path_str);
            pending.retain(|x| x.path != path);
            pending.push(PendingFile {
                root: root.clone(),
                path,
                scheduled: Instant::now() + Duration::from_secs(grace),
            });
        }
    }
}

/// Whether a recursive watch has been asked for.
///
/// `MOUZI_RECURSIVE` wins over the settings row, **in both directions**, and is
/// read first so the database is never consulted when it is set. The `=0`
/// direction is the one that matters: it is the rollback that still works when
/// the data directory is unwritable, when the settings window cannot be reached,
/// and for a user who has been migrated onto a recursive watch they have no way
/// to undo otherwise. `=1` exists so the flag can be exercised in `tauri dev`
/// without a migration.
///
/// An unreadable settings row resolves to `false`. Both the migration default
/// and this fallback point the same way on purpose: a file-moving application
/// that cannot tell whether it was asked to change where files end up must not
/// change where files end up.
fn recursive_watch_enabled() -> bool {
    match std::env::var("MOUZI_RECURSIVE") {
        Ok(value) => recursive_watch_enabled_from(Some(value.as_str()), false),
        Err(_) => crate::db::get_settings()
            .map(|s| s.recursive_watch)
            .map(|stored| recursive_watch_enabled_from(None, stored))
            .unwrap_or(false),
    }
}

/// The resolution rule behind [`recursive_watch_enabled`], with the two inputs
/// injected so it can be tested without touching the process environment or the
/// database. The stored value is irrelevant when the variable is set, which is
/// exactly the point.
fn recursive_watch_enabled_from(env: Option<&str>, stored: bool) -> bool {
    match env {
        Some(value) => value == "1",
        None => stored,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The flag's two directions, and the precedence between them. Asserted
    /// through the injected resolution rule rather than by mutating
    /// `MOUZI_RECURSIVE`, which is process-global and would race every other
    /// test in the binary.
    ///
    /// `tick` deliberately has no test here. Constructing a `FolderWatcher` in a
    /// unit test materialises the `tauri::AppHandle` drop path inside a binary
    /// that otherwise links no Tauri app stack at all, and on this machine that
    /// pulls in system GUI libraries the test binary then fails to load against.
    /// The end-to-end coverage `tick` was extracted to enable belongs to the
    /// recursive stage, where a real watcher is constructed anyway.
    #[test]
    fn the_recursive_flag_resolves_both_ways_with_the_env_var_winning() {
        assert!(!recursive_watch_enabled_from(None, false));
        assert!(recursive_watch_enabled_from(None, true));
        assert!(recursive_watch_enabled_from(Some("1"), false));
        assert!(!recursive_watch_enabled_from(Some("0"), true));
        // Anything that is not exactly "1" is off, so a typo fails closed.
        assert!(!recursive_watch_enabled_from(Some("true"), false));
        assert!(!recursive_watch_enabled_from(Some(""), true));
    }
}
