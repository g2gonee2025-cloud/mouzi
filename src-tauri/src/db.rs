use chrono::{DateTime, TimeZone, Utc};
use rusqlite::{params, Connection, OptionalExtension, Result as SqliteResult};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};
use once_cell::sync::OnceCell;

/// Number of inventory rows to insert per transaction batch.
pub const INVENTORY_BATCH_SIZE: usize = 500;

// ---------------------------------------------------------------------------
// Folder modes
// ---------------------------------------------------------------------------

pub const FOLDER_MODE_SILENT: &str = "silent";
pub const FOLDER_MODE_MANUAL: &str = "manual";
pub const FOLDER_MODE_PAUSED: &str = "paused";

pub const FOLDER_MODES: &[&str] = &[FOLDER_MODE_SILENT, FOLDER_MODE_MANUAL, FOLDER_MODE_PAUSED];

pub fn is_folder_auto_mode(mode: &str) -> bool {
    mode == FOLDER_MODE_SILENT
}

pub fn is_folder_manual_mode(mode: &str) -> bool {
    mode == FOLDER_MODE_MANUAL
}

pub fn is_folder_paused_mode(mode: &str) -> bool {
    mode == FOLDER_MODE_PAUSED
}

pub fn is_valid_folder_mode(mode: &str) -> bool {
    FOLDER_MODES.contains(&mode)
}

// ---------------------------------------------------------------------------
// Data structures
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rule {
    pub id: Option<i64>,
    pub name: String,
    pub priority: i32,
    pub enabled: bool,
    pub extensions: Vec<String>,
    pub pattern: Option<String>,
    pub destination: String,
    pub action: String, // "move", "rename", "delete", "ignore"
    pub folder_id: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatchedFolder {
    pub id: Option<i64>,
    pub path: String,
    pub enabled: bool,
    /// One of: "silent" (real-time auto-organize), "manual" (collect only),
    /// "paused" (do not watch).
    pub mode: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionLog {
    pub id: Option<i64>,
    pub timestamp: DateTime<Utc>,
    pub source_path: String,
    pub destination_path: Option<String>,
    pub action: String,
    pub file_name: String,
    pub file_type: String,
    pub undone: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CleanupAction {
    pub id: Option<i64>,
    pub timestamp: DateTime<Utc>,
    pub path: String,
    pub prev_path: Option<String>,
    pub dest: Option<String>,
    pub action: String,
    pub status: String,
    pub undoable: bool,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppSettings {
    pub id: Option<i64>,
    pub language: String,
    pub theme: String,
    pub telemetry_enabled: bool,
    pub first_run: bool,
    pub autostart: bool,
    pub grace_period_seconds: i64,
    pub lock_check_enabled: bool,
    pub schedule_enabled: bool,
    pub schedule_times_per_day: i64,
    pub schedule_time_1: Option<String>,
    pub schedule_time_2: Option<String>,
    pub schedule_time_3: Option<String>,
    pub schedule_time_4: Option<String>,
    /// Whether watched folders should be watched recursively. `false` on every
    /// existing install and on every new one: this is a file-moving
    /// application, and a change to where files end up must never arrive
    /// unrequested on upgrade.
    pub recursive_watch: bool,
    pub local_ai_enabled: bool,
    pub local_ai_model: String,
    pub revision: i64,
}

/// Only independently editable preferences. Schedule and onboarding state have
/// separate writers so a delayed window cannot replace another window's work.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PreferencesPatch {
    pub language: Option<String>,
    pub theme: Option<String>,
    pub telemetry_enabled: Option<bool>,
    pub autostart: Option<bool>,
    pub grace_period_seconds: Option<i64>,
    pub lock_check_enabled: Option<bool>,
    pub recursive_watch: Option<bool>,
    pub local_ai_enabled: Option<bool>,
    pub local_ai_model: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduleSettings {
    pub schedule_enabled: bool,
    pub schedule_times_per_day: i64,
    pub schedule_time_1: Option<String>,
    pub schedule_time_2: Option<String>,
    pub schedule_time_3: Option<String>,
    pub schedule_time_4: Option<String>,
}

static DB: OnceCell<Arc<Mutex<Connection>>> = OnceCell::new();

pub fn init_db(app_dir: PathBuf) -> SqliteResult<()> {
    let conn = open_database(&app_dir.join("mouzi.db"))?;
    DB.set(Arc::new(Mutex::new(conn)))
        .map_err(|_| rusqlite::Error::ExecuteReturnedResults)?;
    Ok(())
}

fn open_database(db_path: &Path) -> SqliteResult<Connection> {
    let mut conn = Connection::open(db_path)?;
    initialize_schema(&conn)?;
    recover_interrupted_inventory_on(&mut conn, "Inventory scan was interrupted when the app closed")?;
    Ok(conn)
}

fn initialize_schema(conn: &Connection) -> SqliteResult<()> {

    conn.execute(
        "CREATE TABLE IF NOT EXISTS watched_folders (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            path TEXT NOT NULL UNIQUE,
            enabled INTEGER NOT NULL DEFAULT 1,
            mode TEXT NOT NULL DEFAULT 'silent'
        )",
        [],
    )?;

    conn.execute(
        "CREATE TABLE IF NOT EXISTS rules (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL,
            priority INTEGER NOT NULL DEFAULT 0,
            enabled INTEGER NOT NULL DEFAULT 1,
            extensions TEXT NOT NULL,
            pattern TEXT,
            destination TEXT NOT NULL,
            action TEXT NOT NULL DEFAULT 'move',
            folder_id INTEGER NOT NULL DEFAULT 0
        )",
        [],
    )?;

    conn.execute(
        "CREATE TABLE IF NOT EXISTS action_logs (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            timestamp TEXT NOT NULL,
            source_path TEXT NOT NULL,
            destination_path TEXT,
            action TEXT NOT NULL,
            file_name TEXT NOT NULL,
            file_type TEXT NOT NULL,
            undone INTEGER NOT NULL DEFAULT 0
        )",
        [],
    )?;

    conn.execute(
        "CREATE TABLE IF NOT EXISTS settings (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            language TEXT NOT NULL DEFAULT 'en',
            theme TEXT NOT NULL DEFAULT 'system',
            telemetry_enabled INTEGER NOT NULL DEFAULT 0,
            first_run INTEGER NOT NULL DEFAULT 1,
            autostart INTEGER NOT NULL DEFAULT 0
        )",
        [],
    )?;

    conn.execute(
        "CREATE TABLE IF NOT EXISTS cleanup_actions (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            timestamp TEXT NOT NULL,
            path TEXT NOT NULL,
            prev_path TEXT,
            dest TEXT,
            action TEXT NOT NULL,
            status TEXT NOT NULL,
            undoable INTEGER NOT NULL DEFAULT 0
        )",
        [],
    )?;

    // File inventory table for scan/dashboard
    conn.execute(
        "CREATE TABLE IF NOT EXISTS file_inventory (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            root_path TEXT NOT NULL,
            path TEXT NOT NULL UNIQUE,
            size INTEGER NOT NULL DEFAULT 0,
            mtime INTEGER NOT NULL DEFAULT 0,
            category TEXT NOT NULL DEFAULT 'Other',
            scanned_at INTEGER NOT NULL DEFAULT 0
        )",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_file_inventory_root ON file_inventory(root_path)",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_file_inventory_size ON file_inventory(size)",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_file_inventory_category ON file_inventory(category)",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_file_inventory_mtime ON file_inventory(mtime)",
        [],
    )?;

    // Dismissed suggestions for AI-assisted organization
    conn.execute(
        "CREATE TABLE IF NOT EXISTS dismissed_suggestions (
            path TEXT PRIMARY KEY,
            dismissed_at INTEGER NOT NULL DEFAULT 0
        )",
        [],
    )?;

    // Persistent content-hash cache for duplicate detection, keyed by
    // (path, size, mtime): any change to size or mtime invalidates the entry.
    conn.execute(
        "CREATE TABLE IF NOT EXISTS hash_cache (
            path TEXT NOT NULL,
            size INTEGER NOT NULL,
            mtime INTEGER NOT NULL,
            hash TEXT NOT NULL,
            is_full INTEGER NOT NULL DEFAULT 0,
            PRIMARY KEY (path, size, mtime)
        )",
        [],
    )?;

    let action_columns: Vec<String> = conn.prepare("PRAGMA table_info(action_logs)")?.query_map([], |row| row.get(1))?.collect::<SqliteResult<_>>()?;
    if !action_columns.iter().any(|column| column == "file_identity") { conn.execute("ALTER TABLE action_logs ADD COLUMN file_identity TEXT", [])?; }

    // Migration: add missing columns
    let cols: Vec<String> = conn.prepare("PRAGMA table_info(settings)")?
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?;
    if !cols.iter().any(|c| c == "autostart") {
        conn.execute("ALTER TABLE settings ADD COLUMN autostart INTEGER NOT NULL DEFAULT 0", [])?;
    }
    if !cols.iter().any(|c| c == "grace_period_seconds") {
        conn.execute("ALTER TABLE settings ADD COLUMN grace_period_seconds INTEGER NOT NULL DEFAULT 300", [])?;
    }
    if !cols.iter().any(|c| c == "lock_check_enabled") {
        conn.execute("ALTER TABLE settings ADD COLUMN lock_check_enabled INTEGER NOT NULL DEFAULT 1", [])?;
    }
    if !cols.iter().any(|c| c == "schedule_enabled") {
        conn.execute("ALTER TABLE settings ADD COLUMN schedule_enabled INTEGER NOT NULL DEFAULT 0", [])?;
    }
    if !cols.iter().any(|c| c == "schedule_times_per_day") {
        conn.execute("ALTER TABLE settings ADD COLUMN schedule_times_per_day INTEGER NOT NULL DEFAULT 1", [])?;
    }
    if !cols.iter().any(|c| c == "schedule_time_1") {
        conn.execute("ALTER TABLE settings ADD COLUMN schedule_time_1 TEXT", [])?;
    }
    if !cols.iter().any(|c| c == "schedule_time_2") {
        conn.execute("ALTER TABLE settings ADD COLUMN schedule_time_2 TEXT", [])?;
    }
    if !cols.iter().any(|c| c == "schedule_time_3") {
        conn.execute("ALTER TABLE settings ADD COLUMN schedule_time_3 TEXT", [])?;
    }
    if !cols.iter().any(|c| c == "schedule_time_4") {
        conn.execute("ALTER TABLE settings ADD COLUMN schedule_time_4 TEXT", [])?;
    }
    // Additive only, and defaulting to 0: an install that upgrades must keep
    // behaving exactly as it did, because the alternative reading of this column
    // is "start re-filing files the user never asked to be re-filed".
    if !cols.iter().any(|c| c == "recursive_watch") {
        conn.execute("ALTER TABLE settings ADD COLUMN recursive_watch INTEGER NOT NULL DEFAULT 0", [])?;
    }
    for (name, definition) in [
        ("local_ai_enabled", "INTEGER NOT NULL DEFAULT 0"),
        ("local_ai_model", "TEXT NOT NULL DEFAULT 'llama3.2'"),
        ("revision", "INTEGER NOT NULL DEFAULT 0"),
    ] {
        if !cols.iter().any(|c| c == name) {
            conn.execute(&format!("ALTER TABLE settings ADD COLUMN {name} {definition}"), [])?;
        }
    }
    let cleanup_cols: Vec<String> = conn.prepare("PRAGMA table_info(cleanup_actions)")?
        .query_map([], |row| row.get(1))?.collect::<SqliteResult<_>>()?;
    if !cleanup_cols.iter().any(|c| c == "message") {
        conn.execute("ALTER TABLE cleanup_actions ADD COLUMN message TEXT", [])?;
    }
    let dismissal_cols: Vec<String> = conn.prepare("PRAGMA table_info(dismissed_suggestions)")?
        .query_map([], |row| row.get(1))?.collect::<SqliteResult<_>>()?;
    for (name, definition) in [("identity", "TEXT"), ("size", "INTEGER"), ("mtime_ns", "TEXT")] {
        if !dismissal_cols.iter().any(|c| c == name) {
            conn.execute(&format!("ALTER TABLE dismissed_suggestions ADD COLUMN {name} {definition}"), [])?;
        }
    }
    conn.execute_batch("
        CREATE TABLE IF NOT EXISTS inventory_state (id INTEGER PRIMARY KEY CHECK(id=1), revision INTEGER NOT NULL DEFAULT 0, mutation_revision INTEGER NOT NULL DEFAULT 0);
        INSERT OR IGNORE INTO inventory_state(id,revision) VALUES(1,0);
        CREATE TABLE IF NOT EXISTS inventory_roots (
            root_path TEXT PRIMARY KEY, generation INTEGER NOT NULL DEFAULT 0,
            status TEXT NOT NULL DEFAULT 'unscanned', stale INTEGER NOT NULL DEFAULT 1,
            last_scan_at INTEGER, error TEXT
        );
        CREATE TABLE IF NOT EXISTS inventory_fingerprints (
            path TEXT PRIMARY KEY, identity TEXT NOT NULL, size INTEGER NOT NULL, mtime_ns TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS inventory_scan_stage (
            root_path TEXT NOT NULL, generation INTEGER NOT NULL, path TEXT NOT NULL,
            size INTEGER NOT NULL, mtime INTEGER NOT NULL, category TEXT NOT NULL,
            identity TEXT NOT NULL, mtime_ns TEXT NOT NULL,
            PRIMARY KEY(root_path,generation,path)
        );
        CREATE TABLE IF NOT EXISTS category_overrides (
            path TEXT PRIMARY KEY, identity TEXT NOT NULL, size INTEGER NOT NULL,
            mtime_ns TEXT NOT NULL, category TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS operation_intents (
            id INTEGER PRIMARY KEY AUTOINCREMENT, created_at TEXT NOT NULL,
            completed_at TEXT, action TEXT NOT NULL, source_path TEXT NOT NULL,
            requested_destination TEXT, actual_destination TEXT,
            status TEXT NOT NULL DEFAULT 'pending', message TEXT, cleanup_id INTEGER
        );
        CREATE TABLE IF NOT EXISTS accepted_suggestion_feedback (
            id INTEGER PRIMARY KEY AUTOINCREMENT, action_log_id INTEGER NOT NULL,
            file_name TEXT NOT NULL, category TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS scheduler_state (key TEXT PRIMARY KEY, value TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS schedule_occurrences (key TEXT PRIMARY KEY, claimed_at TEXT NOT NULL);
    ")?;
    let intent_columns: Vec<String> = conn.prepare("PRAGMA table_info(operation_intents)")?
        .query_map([], |row| row.get(1))?.collect::<SqliteResult<_>>()?;
    for (name, column_type) in [
        ("undo_action_log_id", "INTEGER"),
        ("undo_expected_identity", "TEXT"),
        ("undo_observed_identity", "TEXT"),
    ] {
        if !intent_columns.iter().any(|column| column == name) {
            conn.execute(&format!("ALTER TABLE operation_intents ADD COLUMN {name} {column_type}"), [])?;
        }
    }
    conn.execute_batch("CREATE UNIQUE INDEX IF NOT EXISTS pending_undo_action_idx
        ON operation_intents(undo_action_log_id) WHERE action='undo' AND status='pending'")?;
    let inventory_state_cols:Vec<String>=conn.prepare("PRAGMA table_info(inventory_state)")?.query_map([],|r|r.get(1))?.collect::<SqliteResult<_>>()?;
    if !inventory_state_cols.iter().any(|name|name=="mutation_revision") {
        conn.execute("ALTER TABLE inventory_state ADD COLUMN mutation_revision INTEGER NOT NULL DEFAULT 0",[])?;
    }
    conn.execute("INSERT OR IGNORE INTO inventory_roots(root_path) SELECT path FROM watched_folders",[])?;
    // Insert default settings if empty
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM settings",
        [],
        |row| row.get(0),
    )?;

    if count == 0 {
        conn.execute(
            "INSERT INTO settings (language, theme, telemetry_enabled, first_run, autostart) VALUES ('en', 'system', 0, 1, 0)",
            [],
        )?;
        conn.execute("INSERT OR REPLACE INTO scheduler_state(key,value) VALUES('first_run_language_pending','1')",[])?;
    }

    Ok(())
}

pub fn get_db() -> Arc<Mutex<Connection>> {
    DB.get().expect("Database not initialized").clone()
}

/// Acquire the global connection for one short piece of work.
///
/// A poisoned mutex means another thread panicked while holding the lock; that
/// is a real error to report, not a reason to panic this thread as well, so
/// this never unwraps. Never hold the guard across filesystem work: every
/// other database caller is blocked while it is alive.
pub fn lock_db() -> Result<MutexGuard<'static, Connection>, String> {
    let db = DB
        .get()
        .ok_or_else(|| "Database not initialized".to_string())?;
    db.lock()
        .map_err(|_| "Database lock is poisoned".to_string())
}

pub fn insert_default_rules(_folder_path: &str) -> SqliteResult<()> {
    let mut conn = sqlite_lock()?;
    let tx = conn.transaction()?;
    insert_default_rules_on(&tx)?;
    tx.commit()
}

fn insert_default_rules_on(conn:&Connection)->SqliteResult<()> {

    // Only insert defaults if no rules exist yet
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM rules", [], |row| row.get(0))?;
    if count > 0 {
        return Ok(());
    }

    let defaults = vec![
        ("Images", 1, vec!["jpg", "jpeg", "png", "gif", "webp", "bmp", "svg", "ico", "heic", "heif"], "Images"),
        ("Documents", 2, vec!["pdf", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "txt", "rtf", "odt"], "Documents"),
        ("Archives", 3, vec!["zip", "rar", "7z", "tar", "gz", "bz2", "xz"], "Archives"),
        ("Installers", 4, vec!["exe", "msi", "msix", "appx"], "Installers"),
        ("Music", 5, vec!["mp3", "wav", "flac", "aac", "ogg", "wma", "m4a"], "Music"),
        ("Videos", 6, vec!["mp4", "avi", "mkv", "mov", "wmv", "flv", "webm"], "Videos"),
        ("Others", 99, vec!["*"], "Others"),
    ];

    for (name, priority, exts, dest) in defaults {
        let extensions = exts.join(",");
        let destination = dest.to_string();
        conn.execute(
            "INSERT INTO rules (name, priority, extensions, destination, action, folder_id) VALUES (?1, ?2, ?3, ?4, 'move', 0)",
            params![name, priority, extensions, destination],
        )?;
    }

    Ok(())
}

pub fn initialize_defaults_atomic(downloads:&str,mode:&str)->SqliteResult<()> {
    let ancestors = configured_inventory_ancestors(downloads, &get_watched_folders()?);
    let mut conn=sqlite_lock()?;let tx=conn.transaction()?;
    let changed = tx.execute("INSERT OR IGNORE INTO watched_folders(path,enabled,mode) VALUES(?1,1,?2)",params![downloads,mode])?;
    tx.execute("INSERT OR IGNORE INTO inventory_roots(root_path) VALUES(?1)",[downloads])?;
    if changed > 0 {
        mark_inventory_ancestors_stale(&tx, &ancestors)?;
    }
    insert_default_rules_on(&tx)?;
    tx.execute("UPDATE inventory_state SET revision=revision+1,mutation_revision=mutation_revision+1 WHERE id=1",[])?;
    tx.commit()
}

pub fn complete_first_run(language:&str)->SqliteResult<AppSettings> {
    let mut conn=sqlite_lock()?;let tx=conn.transaction()?;
    let existing=get_settings_on(&tx)?;
    if !existing.first_run {tx.commit()?;return Ok(existing);}
    insert_default_rules_on(&tx)?;
    tx.execute("UPDATE settings SET language=CASE WHEN EXISTS(SELECT 1 FROM scheduler_state WHERE key='first_run_language_pending') THEN ?1 ELSE language END,first_run=0,revision=revision+1 WHERE first_run=1 AND id=(SELECT id FROM settings ORDER BY id LIMIT 1)",[language])?;
    tx.execute("DELETE FROM scheduler_state WHERE key='first_run_language_pending'",[])?;
    let settings=get_settings_on(&tx)?;tx.commit()?;Ok(settings)
}

pub fn import_rules_atomic(rules: &[Rule], replace: bool) -> SqliteResult<usize> {
    let mut conn = sqlite_lock()?;
    import_rules_atomic_on(&mut conn,rules,replace)
}

fn import_rules_atomic_on(conn:&mut Connection,rules:&[Rule],replace:bool)->SqliteResult<usize> {
    let tx = conn.transaction()?;
    if replace { tx.execute("DELETE FROM rules", [])?; }
    for rule in rules {
        tx.execute(
            "INSERT INTO rules (name,priority,enabled,extensions,pattern,destination,action,folder_id) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            params![rule.name, rule.priority, rule.enabled, rule.extensions.join(","), rule.pattern, rule.destination, rule.action, rule.folder_id],
        )?;
    }
    tx.commit()?;
    Ok(rules.len())
}

fn sqlite_lock() -> SqliteResult<MutexGuard<'static, Connection>> {
    lock_db().map_err(|_| rusqlite::Error::InvalidQuery)
}

pub fn get_rules() -> SqliteResult<Vec<Rule>> {
    let conn = sqlite_lock()?;
    let mut stmt = conn.prepare(
        "SELECT id, name, priority, enabled, extensions, pattern, destination, action, folder_id FROM rules ORDER BY priority"
    )?;

    let rules = stmt.query_map([], |row| {
        let exts_str: String = row.get(4)?;
        Ok(Rule {
            id: row.get(0)?,
            name: row.get(1)?,
            priority: row.get(2)?,
            enabled: row.get::<_, i32>(3)? != 0,
            extensions: exts_str.split(',').map(|s| s.trim().to_lowercase()).collect(),
            pattern: row.get(5)?,
            destination: row.get(6)?,
            action: row.get(7)?,
            folder_id: row.get(8)?,
        })
    })?
    .collect::<SqliteResult<Vec<_>>>()?;

    Ok(rules)
}

pub fn add_rule(rule: &Rule) -> SqliteResult<i64> {
    let conn = sqlite_lock()?;
    let exts = rule.extensions.join(",");
    conn.execute(
        "INSERT INTO rules (name, priority, enabled, extensions, pattern, destination, action, folder_id) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![rule.name, rule.priority, rule.enabled as i32, exts, rule.pattern, rule.destination, rule.action, rule.folder_id],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn update_rule(rule: &Rule) -> SqliteResult<()> {
    let conn = sqlite_lock()?;
    let exts = rule.extensions.join(",");
    conn.execute(
        "UPDATE rules SET name=?1, priority=?2, enabled=?3, extensions=?4, pattern=?5, destination=?6, action=?7, folder_id=?8 WHERE id=?9",
        params![rule.name, rule.priority, rule.enabled as i32, exts, rule.pattern, rule.destination, rule.action, rule.folder_id, rule.id],
    )?;
    Ok(())
}

pub fn delete_rule(id: i64) -> SqliteResult<()> {
    let conn = sqlite_lock()?;
    conn.execute("DELETE FROM rules WHERE id=?1", params![id])?;
    Ok(())
}

pub fn get_watched_folders() -> SqliteResult<Vec<WatchedFolder>> {
    let conn = sqlite_lock()?;
    let mut stmt = conn.prepare("SELECT id, path, enabled, mode FROM watched_folders")?;
    let folders = stmt.query_map([], |row| {
        Ok(WatchedFolder {
            id: row.get(0)?,
            path: row.get(1)?,
            enabled: row.get::<_, i32>(2)? != 0,
            mode: row.get(3)?,
        })
    })?
    .collect::<SqliteResult<Vec<_>>>()?;
    Ok(folders)
}

fn configured_inventory_ancestors(path: &str, folders: &[WatchedFolder]) -> Vec<String> {
    let path = Path::new(path);
    if !path.is_absolute() {
        return Vec::new();
    }
    // The root being removed may already be gone. Check the closest remaining
    // directory above it against each registered root's physical identity.
    let Some(parent) = path.ancestors().skip(1).find(|parent| parent.is_dir()) else {
        return Vec::new();
    };
    folders.iter().filter_map(|folder| {
        crate::safe_fs::directory_contains(parent, Path::new(&folder.path))
            .unwrap_or(false)
            .then(|| folder.path.clone())
    }).collect()
}

fn mark_inventory_ancestors_stale(conn: &Connection, ancestors: &[String]) -> SqliteResult<()> {
    let mut update = conn.prepare("UPDATE inventory_roots SET stale=1 WHERE root_path=?1")?;
    for path in ancestors {
        update.execute([path])?;
    }
    Ok(())
}

pub fn add_watched_folder(path: &str, mode: &str) -> SqliteResult<i64> {
    let ancestors = configured_inventory_ancestors(path, &get_watched_folders()?);
    let mut conn=sqlite_lock()?;let tx=conn.transaction()?;
    let changed=tx.execute(
        "INSERT OR IGNORE INTO watched_folders (path, enabled, mode) VALUES (?1, 1, ?2)",
        params![path, mode],
    )?;
    // INSERT OR IGNORE leaves last_insert_rowid unchanged on an existing row.
    let id = tx.query_row("SELECT id FROM watched_folders WHERE path=?1", [path], |row| row.get(0))?;
    if changed > 0 {
        mark_inventory_ancestors_stale(&tx, &ancestors)?;
        tx.execute("UPDATE inventory_state SET revision=revision+1,mutation_revision=mutation_revision+1 WHERE id=1", [])?;
    }
    tx.execute("INSERT OR IGNORE INTO inventory_roots(root_path) VALUES(?1)", [path])?;
    tx.commit()?;
    Ok(id)
}

pub fn remove_watched_folder(id: i64) -> SqliteResult<()> {
    let folders = get_watched_folders()?;
    let ancestors = folders.iter().find(|folder| folder.id == Some(id))
        .map(|folder| configured_inventory_ancestors(&folder.path, &folders))
        .unwrap_or_default();
    let mut conn = sqlite_lock()?;
    let tx = conn.transaction()?;
    let path: Option<String> = tx.query_row("SELECT path FROM watched_folders WHERE id=?1", [id], |row| row.get(0)).optional()?;
    if let Some(path) = path {
        mark_inventory_ancestors_stale(&tx, &ancestors)?;
        tx.execute("DELETE FROM hash_cache WHERE path IN (SELECT path FROM file_inventory WHERE root_path=?1)", [&path])?;
        tx.execute("DELETE FROM dismissed_suggestions WHERE path IN (SELECT path FROM file_inventory WHERE root_path=?1)", [&path])?;
        tx.execute("DELETE FROM inventory_fingerprints WHERE path IN (SELECT path FROM file_inventory WHERE root_path=?1)", [&path])?;
        tx.execute("DELETE FROM file_inventory WHERE root_path=?1", [&path])?;
        tx.execute("DELETE FROM inventory_scan_stage WHERE root_path=?1", [&path])?;
        tx.execute("DELETE FROM inventory_roots WHERE root_path=?1", [&path])?;
    }
    tx.execute("DELETE FROM watched_folders WHERE id=?1", [id])?;
    tx.execute("UPDATE inventory_state SET revision=revision+1,mutation_revision=mutation_revision+1 WHERE id=1", [])?;
    tx.commit()
}

pub fn update_folder_mode(id: i64, mode: &str) -> SqliteResult<()> {
    let mut conn=sqlite_lock()?;let tx=conn.transaction()?;
    let changed=tx.execute("UPDATE watched_folders SET mode=?1 WHERE id=?2 AND mode<>?1", params![mode, id])?;
    if changed>0 {tx.execute("UPDATE inventory_state SET revision=revision+1,mutation_revision=mutation_revision+1 WHERE id=1", [])?;}
    tx.commit()
}

pub fn log_action(log: &ActionLog) -> SqliteResult<i64> {
    let conn = sqlite_lock()?;
    conn.execute(
        "INSERT INTO action_logs (timestamp, source_path, destination_path, action, file_name, file_type, undone) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0)",
        params![
            log.timestamp.to_rfc3339(),
            log.source_path,
            log.destination_path,
            log.action,
            log.file_name,
            log.file_type
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn get_recent_logs(limit: i64) -> SqliteResult<Vec<ActionLog>> {
    let conn = sqlite_lock()?;
    let mut stmt = conn.prepare(
        "SELECT id, timestamp, source_path, destination_path, action, file_name, file_type, undone FROM action_logs ORDER BY timestamp DESC LIMIT ?1"
    )?;
    let logs = stmt.query_map(params![limit], |row| {
        let ts_str: String = row.get(1)?;
        Ok(ActionLog {
            id: row.get(0)?,
            timestamp: DateTime::parse_from_rfc3339(&ts_str).map(|d| d.with_timezone(&Utc)).unwrap_or_else(|_| Utc.timestamp_opt(0, 0).unwrap()),
            source_path: row.get(2)?,
            destination_path: row.get(3)?,
            action: row.get(4)?,
            file_name: row.get(5)?,
            file_type: row.get(6)?,
            undone: row.get::<_, i32>(7)? != 0,
        })
    })?
    .collect::<SqliteResult<Vec<_>>>()?;
    Ok(logs)
}

pub fn get_undoable_logs() -> SqliteResult<Vec<(i64, String, Option<String>, Option<String>)>> {
    let conn = sqlite_lock()?;
    let mut stmt = conn.prepare(
        "SELECT id, source_path, destination_path, file_identity FROM action_logs WHERE undone = 0 AND action IN ('move','rename','accept') ORDER BY timestamp DESC, id DESC"
    )?;
    let logs = stmt.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, Option<String>>(3)?,
        ))
    })?
    .collect::<SqliteResult<Vec<_>>>()?;
    Ok(logs)
}

pub fn get_weekly_stats() -> SqliteResult<Vec<(String, i64)>> {
    let conn = sqlite_lock()?;
    let mut stmt = conn.prepare(
        "SELECT file_type, COUNT(*) FROM action_logs WHERE julianday(timestamp) > julianday('now', '-7 days') AND undone = 0 GROUP BY file_type"
    )?;
    let stats = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
    })?
    .collect::<SqliteResult<Vec<_>>>()?;
    Ok(stats)
}

pub fn get_settings() -> SqliteResult<AppSettings> {
    let conn = sqlite_lock()?;
    get_settings_on(&conn)
}

fn get_settings_on(conn: &Connection) -> SqliteResult<AppSettings> {
    conn.query_row(
        "SELECT id, language, theme, telemetry_enabled, first_run, autostart, grace_period_seconds, lock_check_enabled, schedule_enabled, schedule_times_per_day, schedule_time_1, schedule_time_2, schedule_time_3, schedule_time_4, recursive_watch, local_ai_enabled, local_ai_model, revision FROM settings ORDER BY id LIMIT 1",
        [],
        |row| {
            Ok(AppSettings {
                id: row.get(0)?,
                language: row.get(1)?,
                theme: row.get(2)?,
                telemetry_enabled: row.get::<_, i32>(3)? != 0,
                first_run: row.get::<_, i32>(4)? != 0,
                autostart: row.get::<_, i32>(5).unwrap_or(1) != 0,
                grace_period_seconds: row.get::<_, i64>(6).unwrap_or(300),
                lock_check_enabled: row.get::<_, i32>(7).unwrap_or(1) != 0,
                schedule_enabled: row.get::<_, i32>(8).unwrap_or(0) != 0,
                schedule_times_per_day: row.get::<_, i64>(9).unwrap_or(1),
                schedule_time_1: row.get(10).ok(),
                schedule_time_2: row.get(11).ok(),
                schedule_time_3: row.get(12).ok(),
                schedule_time_4: row.get(13).ok(),
                // `unwrap_or(0)`, not `unwrap_or(1)`: a row written before this
                // column existed, or a database whose migration did not run,
                // must resolve to the pre-recursion behaviour.
                recursive_watch: row.get::<_, i32>(14).unwrap_or(0) != 0,
                local_ai_enabled: row.get(15)?,
                local_ai_model: row.get(16)?,
                revision: row.get(17)?,
            })
        },
    )
}

fn update_preferences_on(conn: &Connection, patch: &PreferencesPatch) -> SqliteResult<AppSettings> {
    conn.execute(
        "UPDATE settings SET language=COALESCE(?1,language), theme=COALESCE(?2,theme),
         telemetry_enabled=COALESCE(?3,telemetry_enabled), autostart=COALESCE(?4,autostart),
         grace_period_seconds=COALESCE(?5,grace_period_seconds), lock_check_enabled=COALESCE(?6,lock_check_enabled),
         recursive_watch=COALESCE(?7,recursive_watch), local_ai_enabled=COALESCE(?8,local_ai_enabled),
         local_ai_model=COALESCE(?9,local_ai_model), revision=revision+1
         WHERE id=(SELECT id FROM settings ORDER BY id LIMIT 1)",
        params![patch.language,patch.theme,patch.telemetry_enabled,patch.autostart,patch.grace_period_seconds,
            patch.lock_check_enabled,patch.recursive_watch,patch.local_ai_enabled,patch.local_ai_model],
    )?;
    get_settings_on(conn)
}

pub fn update_preferences(patch: &PreferencesPatch) -> SqliteResult<AppSettings> {
    let mut conn=sqlite_lock()?;let tx=conn.transaction()?;
    let settings=update_preferences_on(&tx,patch)?;tx.commit()?;Ok(settings)
}

fn update_schedule_settings_on(conn: &Connection, schedule: &ScheduleSettings) -> SqliteResult<AppSettings> {
    conn.execute(
        "UPDATE settings SET schedule_enabled=?1,schedule_times_per_day=?2,schedule_time_1=?3,
         schedule_time_2=?4,schedule_time_3=?5,schedule_time_4=?6,revision=revision+1
         WHERE id=(SELECT id FROM settings ORDER BY id LIMIT 1)",
        params![schedule.schedule_enabled,schedule.schedule_times_per_day,schedule.schedule_time_1,
            schedule.schedule_time_2,schedule.schedule_time_3,schedule.schedule_time_4],
    )?;
    get_settings_on(conn)
}

pub fn update_schedule_settings(schedule: &ScheduleSettings) -> SqliteResult<AppSettings> {
    let mut conn=sqlite_lock()?;let tx=conn.transaction()?;
    let settings=update_schedule_settings_on(&tx,schedule)?;tx.commit()?;Ok(settings)
}

#[derive(Debug, Clone, Serialize)]
pub struct HistoryCounts { pub total: i64, pub undoable: i64 }

pub fn history_counts() -> SqliteResult<HistoryCounts> {
    sqlite_lock()?.query_row(
        "SELECT COUNT(*),COALESCE(SUM(CASE WHEN undone=0 AND action IN ('move','rename','accept') THEN 1 ELSE 0 END),0) FROM action_logs",
        [], |row| Ok(HistoryCounts{total:row.get(0)?,undoable:row.get(1)?}),
    )
}

pub fn get_schedule_last_check() -> SqliteResult<Option<String>> {
    sqlite_lock()?.query_row("SELECT value FROM scheduler_state WHERE key='last_check'", [], |row| row.get(0)).optional()
}

pub(crate) fn claim_schedule_occurrences(keys: &[&str], checkpoint: &str) -> SqliteResult<usize> {
    claim_schedule_occurrences_on(&mut *sqlite_lock()?, keys, checkpoint)
}

fn claim_schedule_occurrences_on(conn: &mut Connection, keys: &[&str], checkpoint: &str) -> SqliteResult<usize> {
    let tx = conn.transaction()?;
    let claimed_at = Utc::now().to_rfc3339();
    let mut count = 0;
    {
        // A legacy key includes a configuration segment before date and time.
        // Honor any old claim for the slot, regardless of that configuration.
        let mut insert = tx.prepare(
            "INSERT OR IGNORE INTO schedule_occurrences(key,claimed_at)
             SELECT ?1,?2 WHERE NOT EXISTS (
               SELECT 1 FROM schedule_occurrences WHERE key GLOB ('v1|*' || substr(?1,3))
             )"
        )?;
        for key in keys { count += insert.execute(params![key, &claimed_at])?; }
    }
    tx.execute("INSERT INTO scheduler_state(key,value) VALUES('last_check',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value", [checkpoint])?;
    tx.commit()?;
    Ok(count)
}

#[cfg(test)]
#[test]
fn scheduler_claims_legacy_slots_once_and_rolls_back_the_whole_checkpoint() {
    let mut conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE schedule_occurrences (key TEXT PRIMARY KEY, claimed_at TEXT NOT NULL);
         CREATE TABLE scheduler_state (key TEXT PRIMARY KEY, value TEXT NOT NULL);
         INSERT INTO schedule_occurrences VALUES('v1|08:00:00,14:00:00|2026-11-06|14:00:00','legacy');"
    ).unwrap();
    let today = ["v2|2026-11-06|10:00:00", "v2|2026-11-06|14:00:00"];
    assert_eq!(claim_schedule_occurrences_on(&mut conn, &today, "first").unwrap(), 1);
    assert_eq!(claim_schedule_occurrences_on(&mut conn, &today, "second").unwrap(), 0);
    conn.execute_batch(
        "CREATE TRIGGER fail_second_claim BEFORE INSERT ON schedule_occurrences
         WHEN NEW.key='v2|2026-11-07|14:00:00' BEGIN SELECT RAISE(ABORT,'fixture failure'); END;"
    ).unwrap();
    let tomorrow = ["v2|2026-11-07|10:00:00", "v2|2026-11-07|14:00:00"];
    assert!(claim_schedule_occurrences_on(&mut conn, &tomorrow, "third").is_err());
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM schedule_occurrences", [], |row| row.get(0)).unwrap();
    let checkpoint: String = conn.query_row("SELECT value FROM scheduler_state WHERE key='last_check'", [], |row| row.get(0)).unwrap();
    assert_eq!((count, checkpoint.as_str()), (2, "second"));
}

pub fn clear_logs() -> SqliteResult<()> {
    let conn = sqlite_lock()?;
    conn.execute("DELETE FROM action_logs", [])?;
    Ok(())
}

// ---------------------------------------------------------------------------
// cleanup_actions helpers
// ---------------------------------------------------------------------------

pub fn insert_cleanup_action(action: &CleanupAction) -> SqliteResult<i64> {
    let conn = sqlite_lock()?;
    conn.execute(
        "INSERT INTO cleanup_actions (timestamp, path, prev_path, dest, action, status, undoable,message)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7,?8)",
        params![
            action.timestamp.to_rfc3339(),
            action.path,
            action.prev_path,
            action.dest,
            action.action,
            action.status,
            action.undoable as i32,
            action.message,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn get_cleanup_logs(limit: i64) -> SqliteResult<Vec<CleanupAction>> {
    let conn = sqlite_lock()?;
    let mut stmt = conn.prepare(
        "SELECT id, timestamp, path, prev_path, dest, action, status, undoable,message
         FROM cleanup_actions ORDER BY timestamp DESC LIMIT ?1",
    )?;
    let logs = stmt
        .query_map(params![limit], |row| {
            let ts_str: String = row.get(1)?;
            Ok(CleanupAction {
                id: row.get(0)?,
                timestamp: DateTime::parse_from_rfc3339(&ts_str)
                    .map(|d| d.with_timezone(&Utc))
                    .unwrap_or_else(|_| Utc.timestamp_opt(0, 0).unwrap()),
                path: row.get(2)?,
                prev_path: row.get(3)?,
                dest: row.get(4)?,
                action: row.get(5)?,
                status: row.get(6)?,
                undoable: row.get::<_, i32>(7)? != 0,
                message: row.get(8)?,
            })
        })?
        .collect::<SqliteResult<Vec<_>>>()?;
    Ok(logs)
}

pub fn begin_operation_intent(action:&str,source:&str,requested_destination:Option<&str>)->SqliteResult<i64> {
    let conn=sqlite_lock()?;
    conn.execute("INSERT INTO operation_intents(created_at,action,source_path,requested_destination) VALUES(?1,?2,?3,?4)",params![Utc::now().to_rfc3339(),action,source,requested_destination])?;
    Ok(conn.last_insert_rowid())
}

pub fn finalize_operation_intent(id:i64,status:&str,actual_destination:Option<&str>,message:Option<&str>)->SqliteResult<()> {
    let conn=sqlite_lock()?;
    let changed=conn.execute("UPDATE operation_intents SET completed_at=?2,status=?3,actual_destination=?4,message=?5 WHERE id=?1 AND status='pending'",params![id,Utc::now().to_rfc3339(),status,actual_destination,message])?;
    if changed!=1 {return Err(rusqlite::Error::QueryReturnedNoRows);}
    Ok(())
}

pub fn begin_cleanup_intent(action:&str,path:&str)->SqliteResult<i64> {
    let mut conn=sqlite_lock()?;let tx=conn.transaction()?;
    let timestamp=Utc::now().to_rfc3339();
    tx.execute("INSERT INTO cleanup_actions(timestamp,path,action,status,undoable) VALUES(?1,?2,?3,'pending',0)",params![timestamp,path,action])?;
    let cleanup_id=tx.last_insert_rowid();
    tx.execute("INSERT INTO operation_intents(created_at,action,source_path,cleanup_id) VALUES(?1,?2,?3,?4)",params![timestamp,action,path,cleanup_id])?;
    let id=tx.last_insert_rowid();tx.commit()?;Ok(id)
}

fn inventory_paths_for_spelling(conn:&Connection,path:&str)->SqliteResult<Vec<String>> {
    #[cfg(target_os="windows")]
    {
        let normalized=crate::safe_fs::normalize_lexically(std::path::Path::new(path)).to_string_lossy().replace("\\\\?\\UNC\\","//").replace("\\\\?\\","").replace('\\',"/");
        // Prefix and separator aliases preserve path component spelling. Case
        // folding would also match different entries in case-sensitive folders.
        let mut stmt=conn.prepare(r"SELECT path FROM file_inventory WHERE REPLACE(REPLACE(REPLACE(path,'\\?\UNC\','//'),'\\?\',''),'\','/')=?1 COLLATE BINARY ORDER BY (path=?2) DESC, path")?;
        let mut paths=stmt.query_map(params![normalized,path],|r|r.get::<_,String>(0))?.collect::<SqliteResult<Vec<_>>>()?;
        if !paths.iter().any(|value|value==path) {paths.push(path.to_string());}
        Ok(paths)
    }
    #[cfg(not(target_os="windows"))]
    {let _=conn;Ok(vec![path.to_string()])}
}

fn remove_inventory_path_on(conn:&Connection,path:&str)->SqliteResult<()> {
    for path in inventory_paths_for_spelling(conn,path)? {
        conn.execute("DELETE FROM hash_cache WHERE path=?1",[&path])?;
        conn.execute("DELETE FROM dismissed_suggestions WHERE path=?1",[&path])?;
        conn.execute("DELETE FROM category_overrides WHERE path=?1",[&path])?;
        conn.execute("DELETE FROM inventory_fingerprints WHERE path=?1",[&path])?;
        conn.execute("UPDATE inventory_roots SET stale=1 WHERE root_path IN (SELECT root_path FROM file_inventory WHERE path=?1)",[&path])?;
        conn.execute("DELETE FROM file_inventory WHERE path=?1",[&path])?;
    }
    conn.execute("UPDATE inventory_state SET revision=revision+1,mutation_revision=mutation_revision+1 WHERE id=1",[])?;
    Ok(())
}

pub fn remove_inventory_path(path:&str)->SqliteResult<()> {
    let mut conn=sqlite_lock()?;let tx=conn.transaction()?;remove_inventory_path_on(&tx,path)?;tx.commit()
}

pub fn finalize_cleanup_intent(id:i64,status:&str,message:Option<&str>,source_removed:bool)->SqliteResult<()> {
    let mut conn=sqlite_lock()?;let tx=conn.transaction()?;
    let (path,cleanup_id):(String,i64)=tx.query_row("SELECT source_path,cleanup_id FROM operation_intents WHERE id=?1 AND status='pending'",[id],|r|Ok((r.get(0)?,r.get(1)?)))?;
    tx.execute("UPDATE operation_intents SET completed_at=?2,status=?3,message=?4 WHERE id=?1",params![id,Utc::now().to_rfc3339(),status,message])?;
    tx.execute("UPDATE cleanup_actions SET status=?2,message=?3,undoable=0 WHERE id=?1",params![cleanup_id,status,message])?;
    if source_removed {remove_inventory_path_on(&tx,&path)?;}
    tx.commit()
}

pub fn metadata_mtime_ns(metadata:&std::fs::Metadata)->String {
    metadata.modified().ok().and_then(|time|time.duration_since(UNIX_EPOCH).ok()).map(|elapsed|elapsed.as_nanos().to_string()).unwrap_or_else(||"0".into())
}

struct PreparedMove { destination:String, size:i64,mtime:i64,identity:String,mtime_ns:String,owner:Option<(i64,String)> }

fn prepare_move(destination:&str)->Result<PreparedMove,String> {
    // Every filesystem read happens before acquiring the mutation transaction.
    let path=std::fs::canonicalize(destination).map_err(|e|format!("Moved file metadata unavailable: {e}"))?;
    let metadata=std::fs::metadata(&path).map_err(|e|e.to_string())?;
    if !metadata.is_file() {return Err("Moved destination is not a regular file".into());}
    let identity=crate::safe_fs::file_identity(&path)?.key();
    let folders=get_watched_folders().map_err(|e|e.to_string())?;
    let mut owner:Option<(usize,i64,String)>=None;
    for folder in folders {
        let Ok(root)=crate::safe_fs::validate_root(std::path::Path::new(&folder.path)) else {continue;};
        if path.starts_with(&root) && path!=root {
            let depth=root.components().count();
            if owner.as_ref().is_none_or(|existing|depth>existing.0) {
                if let Some(id)=folder.id {owner=Some((depth,id,folder.path));}
            }
        }
    }
    let mtime=metadata.modified().ok().and_then(|t|t.duration_since(UNIX_EPOCH).ok()).map(|d|d.as_secs() as i64).unwrap_or(0);
    Ok(PreparedMove{destination:path.to_string_lossy().into_owned(),size:metadata.len() as i64,mtime,identity,mtime_ns:metadata_mtime_ns(&metadata),owner:owner.map(|(_,id,path)|(id,path))})
}

fn reconcile_move_on(conn:&Connection,old:&str,prepared:&PreparedMove,category:Option<&str>)->SqliteResult<()> {
    let mut previous_category=None;
    for path in inventory_paths_for_spelling(conn,old)? {
        previous_category=conn.query_row("SELECT category FROM category_overrides WHERE path=?1",[&path],|r|r.get::<_,String>(0)).optional()?;
        if previous_category.is_some(){break;}
    }
    remove_inventory_path_on(conn,old)?;
    // Also clear a prior destination row/cache (including its different spelling).
    remove_inventory_path_on(conn,&prepared.destination)?;
    if let Some((id,root))=&prepared.owner {
        let still_configured:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM watched_folders WHERE id=?1 AND path=?2)",params![id,root],|r|r.get(0))?;
        if !still_configured {return Ok(());}
        let selected=category.or(previous_category.as_deref()).unwrap_or_else(||crate::scan::categorize(&prepared.destination));
        conn.execute("INSERT INTO file_inventory(root_path,path,size,mtime,category,scanned_at) VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(path) DO UPDATE SET root_path=excluded.root_path,size=excluded.size,mtime=excluded.mtime,category=excluded.category,scanned_at=excluded.scanned_at",params![root,prepared.destination,prepared.size,prepared.mtime,selected,now_epoch()])?;
        conn.execute("INSERT INTO inventory_fingerprints(path,identity,size,mtime_ns) VALUES(?1,?2,?3,?4) ON CONFLICT(path) DO UPDATE SET identity=excluded.identity,size=excluded.size,mtime_ns=excluded.mtime_ns",params![prepared.destination,prepared.identity,prepared.size,prepared.mtime_ns])?;
        if category.is_some() || previous_category.is_some() {
            conn.execute("INSERT INTO category_overrides(path,identity,size,mtime_ns,category) VALUES(?1,?2,?3,?4,?5) ON CONFLICT(path) DO UPDATE SET identity=excluded.identity,size=excluded.size,mtime_ns=excluded.mtime_ns,category=excluded.category",params![prepared.destination,prepared.identity,prepared.size,prepared.mtime_ns,selected])?;
        }
        conn.execute("UPDATE inventory_roots SET stale=1 WHERE root_path=?1",[root])?;
    }
    Ok(())
}

pub fn reconcile_move(old:&str,new:&str,category:Option<&str>)->Result<(),String> {
    if category.is_some_and(|category|!crate::categories::is_valid_category(category)) {return Err("Invalid canonical category".into());}
    let prepared=prepare_move(new)?;let mut conn=lock_db()?;let tx=conn.transaction().map_err(|e|e.to_string())?;
    reconcile_move_on(&tx,old,&prepared,category).map_err(|e|e.to_string())?;tx.commit().map_err(|e|e.to_string())
}

fn finalize_moved(id:i64,log:&ActionLog,actual_destination:&str,category:Option<&str>,accepted:bool,expected_identity:&str,inventory_source:&str)->Result<(),String> {
    if category.is_some_and(|category|!crate::categories::is_valid_category(category)) {return Err("Invalid canonical category".into());}
    let prepared=prepare_move(actual_destination)?;
    if prepared.identity != expected_identity { return Err("Moved file was replaced before history finalization; recovery intent retained".into()); }
    let mut conn=lock_db()?;let tx=conn.transaction().map_err(|e|e.to_string())?;
    let changed=tx.execute("UPDATE operation_intents SET completed_at=?2,status='completed',actual_destination=?3 WHERE id=?1 AND status='pending'",params![id,Utc::now().to_rfc3339(),actual_destination]).map_err(|e|e.to_string())?;
    if changed!=1 {return Err("Operation journal intent is unavailable".into());}
    tx.execute("INSERT INTO action_logs(timestamp,source_path,destination_path,action,file_name,file_type,undone,file_identity) VALUES(?1,?2,?3,?4,?5,?6,0,?7)",params![log.timestamp.to_rfc3339(),log.source_path,actual_destination,log.action,log.file_name,log.file_type,prepared.identity]).map_err(|e|e.to_string())?;
    let log_id=tx.last_insert_rowid();
    reconcile_move_on(&tx,inventory_source,&prepared,category).map_err(|e|e.to_string())?;
    if accepted {
        tx.execute("INSERT INTO accepted_suggestion_feedback(action_log_id,file_name,category) VALUES(?1,?2,?3)",params![log_id,log.file_name,category]).map_err(|e|e.to_string())?;
    }
    tx.commit().map_err(|e|e.to_string())
}

pub(crate) fn finalize_move_intent_with_inventory_source(id:i64,log:&ActionLog,actual_destination:&str,category:Option<&str>,expected_identity:&str,inventory_source:&str)->Result<(),String> {
    finalize_moved(id,log,actual_destination,category,false,expected_identity,inventory_source)
}

pub fn finalize_suggestion_intent(id:i64,log:&ActionLog,actual_destination:&str,category:&str,expected_identity:&str)->Result<(),String> {
    finalize_moved(id,log,actual_destination,Some(category),true,expected_identity,&log.source_path)
}

pub(crate) struct PendingUndo {
    pub id: i64,
    pub action_log_id: Option<i64>,
    pub source: String,
    pub requested: Option<String>,
    pub observed: Option<String>,
    pub expected_identity: Option<String>,
    pub observed_identity: Option<String>,
}

pub(crate) fn pending_undos() -> Result<Vec<PendingUndo>, String> {
    let conn = lock_db()?;
    let mut stmt = conn.prepare(
        "SELECT id, undo_action_log_id, source_path, requested_destination, actual_destination,
                undo_expected_identity, undo_observed_identity
         FROM operation_intents WHERE action='undo' AND status='pending' ORDER BY id",
    ).map_err(|error| error.to_string())?;
    let rows = stmt.query_map([], |row| Ok(PendingUndo {
        id: row.get(0)?,
        action_log_id: row.get(1)?,
        source: row.get(2)?,
        requested: row.get(3)?,
        observed: row.get(4)?,
        expected_identity: row.get(5)?,
        observed_identity: row.get(6)?,
    })).map_err(|error| error.to_string())?;
    rows.collect::<SqliteResult<_>>().map_err(|error| error.to_string())
}

pub fn begin_undo_intent(action_log_id: i64, source: &str, destination: &str, expected: &str) -> SqliteResult<i64> {
    let conn = sqlite_lock()?;
    let changed = conn.execute(
        "INSERT INTO operation_intents(created_at,action,source_path,requested_destination,undo_action_log_id,undo_expected_identity)
         SELECT ?1,'undo',?2,?3,id,file_identity FROM action_logs
         WHERE id=?4 AND undone=0 AND destination_path=?2 AND source_path=?3 AND file_identity=?5",
        params![Utc::now().to_rfc3339(), source, destination, action_log_id, expected],
    )?;
    if changed != 1 { return Err(rusqlite::Error::QueryReturnedNoRows); }
    Ok(conn.last_insert_rowid())
}

pub fn observe_undo_restore(id: i64, actual: &str, identity: &str) -> Result<(), String> {
    let changed = lock_db()?.execute(
        "UPDATE operation_intents SET actual_destination=?2,undo_observed_identity=?3
         WHERE id=?1 AND action='undo' AND status='pending' AND actual_destination IS NULL",
        params![id, actual, identity],
    ).map_err(|error| error.to_string())?;
    if changed != 1 { return Err("Undo recovery record is unavailable".into()); }
    Ok(())
}

pub fn finalize_undo_intent(id: i64, action_log_id: i64, old: &str, new: &str, expected: &str) -> Result<(), String> {
    let path = std::path::Path::new(new);
    let _ancestors = crate::safe_fs::guard_directory(path.parent().ok_or("Restore path has no parent")?)?;
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(windows)] {
        use std::os::windows::fs::OpenOptionsExt;
        // Hold the restored file against write or replacement through commit.
        options.share_mode(1).custom_flags(0x00200000); // OPEN_REPARSE_POINT
    }
    let restored = options.open(path).map_err(|error| error.to_string())?;
    if crate::safe_fs::identity_from_handle(&restored)?.key() != expected {
        return Err("Restored file was replaced before history finalization".into());
    }
    let prepared = prepare_move(new)?;
    if prepared.identity != expected {
        return Err("Restored file was replaced before history finalization".into());
    }
    let mut conn = lock_db()?;
    let tx = conn.transaction().map_err(|error| error.to_string())?;
    let changed = tx.execute(
        "UPDATE operation_intents SET completed_at=?2,status='completed'
         WHERE id=?1 AND status='pending' AND undo_action_log_id=?3 AND source_path=?4
         AND actual_destination=?5 AND undo_observed_identity=?6",
        params![id, Utc::now().to_rfc3339(), action_log_id, old, new, expected],
    ).map_err(|error| error.to_string())?;
    if changed != 1 { return Err("Undo recovery record is unavailable".into()); }
    let accepted: bool = tx.query_row("SELECT action='accept' FROM action_logs WHERE id=?1 AND undone=0", [action_log_id], |row| row.get(0))
        .map_err(|error| error.to_string())?;
    tx.execute("UPDATE action_logs SET undone=1 WHERE id=?1", [action_log_id]).map_err(|error| error.to_string())?;
    if accepted {
        for path in inventory_paths_for_spelling(&tx,old).map_err(|e|e.to_string())? { tx.execute("DELETE FROM category_overrides WHERE path=?1",[path]).map_err(|e|e.to_string())?; }
    }
    reconcile_move_on(&tx,old,&prepared,None).map_err(|e|e.to_string())?;tx.commit().map_err(|e|e.to_string())
}

#[derive(Debug,Clone)]
pub struct AcceptedSuggestionFeedback {pub file_name:String,pub category:String}

pub fn get_accepted_suggestion_feedback()->Result<Vec<AcceptedSuggestionFeedback>,String> {
    let conn=lock_db()?;let mut stmt=conn.prepare("SELECT f.file_name,f.category FROM accepted_suggestion_feedback f LEFT JOIN action_logs l ON l.id=f.action_log_id WHERE f.action_log_id=0 OR l.undone=0 ORDER BY f.id").map_err(|e|e.to_string())?;
    let rows=stmt.query_map([],|r|Ok(AcceptedSuggestionFeedback{file_name:r.get(0)?,category:r.get(1)?})).map_err(|e|e.to_string())?.collect::<SqliteResult<Vec<_>>>().map_err(|e|e.to_string())?;Ok(rows)
}

// ---------------------------------------------------------------------------
// file_inventory types and helpers
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct InventoryRow {
    pub path: String,
    pub size: i64,
    pub mtime: i64,
    pub category: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct InventoryStats {
    pub total_files: i64,
    pub total_bytes: i64,
    pub last_scan_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CategoryStat {
    pub category: String,
    pub files: i64,
    pub bytes: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct LargestFile {
    pub path: String,
    pub size: i64,
    pub mtime: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all="camelCase")]
pub struct RootStat {
    pub path: String,
    pub files: i64,
    pub bytes: i64,
    pub status: String,
    pub stale: bool,
    pub last_scan_at: Option<i64>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardSnapshot {
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

pub const LARGE_FILE_BYTES: i64 = 100 * 1024 * 1024;
pub const STALE_FILE_DAYS: i64 = 365;
pub const SAME_SIZE_MIN_BYTES: i64 = 1024;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardInsights {
    pub large_files: i64,
    pub large_bytes: i64,
    pub stale_files: i64,
    pub stale_bytes: i64,
    pub same_size_groups: i64,
    pub same_size_extra_bytes: i64,
    pub other_files: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgeBucket {
    pub bucket: String,
    pub files: i64,
    pub bytes: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InventoryFile {
    pub path: String,
    pub size: i64,
    pub mtime: i64,
    pub category: String,
    pub root_path: String,
}

#[cfg(test)]
fn create_file_inventory_table(conn: &Connection) -> SqliteResult<()> {
    initialize_schema(conn)?;
    conn.execute(
        "CREATE TABLE IF NOT EXISTS file_inventory (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            root_path TEXT NOT NULL,
            path TEXT NOT NULL UNIQUE,
            size INTEGER NOT NULL DEFAULT 0,
            mtime INTEGER NOT NULL DEFAULT 0,
            category TEXT NOT NULL DEFAULT 'Other',
            scanned_at INTEGER NOT NULL DEFAULT 0
        )",
        [],
    )?;
    conn.execute("CREATE INDEX IF NOT EXISTS idx_file_inventory_root ON file_inventory(root_path)", [])?;
    conn.execute("CREATE INDEX IF NOT EXISTS idx_file_inventory_size ON file_inventory(size)", [])?;
    conn.execute("CREATE INDEX IF NOT EXISTS idx_file_inventory_category ON file_inventory(category)", [])?;
    conn.execute("CREATE INDEX IF NOT EXISTS idx_file_inventory_mtime ON file_inventory(mtime)", [])?;
    Ok(())
}

// Internal helpers taking a &Connection (testable with in-memory DB).
fn now_epoch() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn append_inventory_batch_on(conn: &mut Connection, root: &str, rows: &[InventoryRow]) -> SqliteResult<()> {
    let scanned_at = now_epoch();
    let tx = conn.transaction()?;
    {
        let mut stmt = tx.prepare(
            "INSERT INTO file_inventory (root_path, path, size, mtime, category, scanned_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6) ON CONFLICT(path) DO UPDATE SET
             root_path=excluded.root_path,size=excluded.size,mtime=excluded.mtime,category=excluded.category,scanned_at=excluded.scanned_at",
        )?;
        for row in rows {
            stmt.execute(params![root, row.path, row.size, row.mtime, row.category, scanned_at])?;
        }
    }
    tx.commit()
}

fn replace_inventory_on(conn: &mut Connection, root: &str, rows: &[InventoryRow]) -> SqliteResult<()> {
    let tx = conn.transaction()?;
    let scanned_at = now_epoch();
    tx.execute("CREATE TEMP TABLE IF NOT EXISTS replacement_paths(path TEXT PRIMARY KEY)", [])?;
    tx.execute("DELETE FROM replacement_paths", [])?;
    for row in rows {
        tx.execute("INSERT INTO replacement_paths(path) VALUES(?1)", [&row.path])?;
        tx.execute(
            "INSERT INTO file_inventory(root_path,path,size,mtime,category,scanned_at) VALUES(?1,?2,?3,?4,?5,?6)
             ON CONFLICT(path) DO UPDATE SET root_path=excluded.root_path,size=excluded.size,mtime=excluded.mtime,category=excluded.category,scanned_at=excluded.scanned_at",
            params![root,row.path,row.size,row.mtime,row.category,scanned_at],
        )?;
    }
    tx.execute("DELETE FROM file_inventory WHERE root_path=?1 AND path NOT IN (SELECT path FROM replacement_paths)",[root])?;
    tx.execute("INSERT INTO inventory_roots(root_path,status,stale,last_scan_at) VALUES(?1,'complete',0,?2) ON CONFLICT(root_path) DO UPDATE SET status='complete',stale=0,last_scan_at=excluded.last_scan_at,error=NULL",params![root,scanned_at])?;
    tx.execute("UPDATE inventory_state SET revision=revision+1,mutation_revision=mutation_revision+1 WHERE id=1", [])?;
    tx.commit()
}

#[cfg(test)]
fn clear_inventory_on(conn: &Connection, root: &str) -> SqliteResult<()> {
    conn.execute("DELETE FROM file_inventory WHERE root_path = ?1", params![root]).map(|_| ())
}

fn get_inventory_stats_on(conn: &Connection) -> SqliteResult<InventoryStats> {
    conn.query_row(
        "SELECT COUNT(*), COALESCE(SUM(size), 0), (SELECT MAX(last_scan_at) FROM inventory_roots) FROM file_inventory",
        [],
        |row| {
            Ok(InventoryStats {
                total_files: row.get(0)?,
                total_bytes: row.get(1)?,
                last_scan_at: row.get(2)?,
            })
        },
    )
}

fn get_largest_files_on(conn: &Connection, limit: i64) -> SqliteResult<Vec<LargestFile>> {
    let mut stmt = conn.prepare(
        "SELECT path, size, mtime FROM file_inventory ORDER BY size DESC LIMIT ?1",
    )?;
    let rows = stmt
        .query_map(params![limit], |row| {
            Ok(LargestFile {
                path: row.get(0)?,
                size: row.get(1)?,
                mtime: row.get(2)?,
            })
        })?
        .collect::<SqliteResult<Vec<_>>>()?;
    Ok(rows)
}

fn get_category_distribution_on(conn: &Connection) -> SqliteResult<Vec<CategoryStat>> {
    let mut stmt = conn.prepare(
        "SELECT category, COUNT(*), COALESCE(SUM(size), 0) FROM file_inventory GROUP BY category ORDER BY 3 DESC",
    )?;
    let rows = stmt
        .query_map([], |row| {
            Ok(CategoryStat {
                category: row.get(0)?,
                files: row.get(1)?,
                bytes: row.get(2)?,
            })
        })?
        .collect::<SqliteResult<Vec<_>>>()?;
    Ok(rows)
}

fn get_root_summaries_on(conn: &Connection) -> SqliteResult<Vec<RootStat>> {
    let mut stmt = conn.prepare(
        "SELECT r.root_path, COUNT(i.id), COALESCE(SUM(i.size),0),r.status,r.stale,r.last_scan_at,r.error
         FROM inventory_roots r LEFT JOIN file_inventory i ON i.root_path=r.root_path
         GROUP BY r.root_path ORDER BY 3 DESC,r.root_path",
    )?;
    let rows = stmt
        .query_map([], |row| {
            Ok(RootStat {
                path: row.get(0)?,
                files: row.get(1)?,
                bytes: row.get(2)?,
                status: row.get(3)?,
                stale: row.get(4)?,
                last_scan_at: row.get(5)?,
                error: row.get(6)?,
            })
        })?
        .collect::<SqliteResult<Vec<_>>>()?;
    Ok(rows)
}

fn get_insights_on(conn: &Connection, now: i64) -> SqliteResult<DashboardInsights> {
    let (large_files, large_bytes): (i64, i64) = conn.query_row(
        "SELECT COUNT(*), COALESCE(SUM(size), 0) FROM file_inventory WHERE size >= ?1",
        params![LARGE_FILE_BYTES],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let stale_cutoff = now - STALE_FILE_DAYS * 86_400;
    let (stale_files, stale_bytes): (i64, i64) = conn.query_row(
        "SELECT COUNT(*), COALESCE(SUM(size), 0) FROM file_inventory WHERE mtime > 0 AND mtime < ?1",
        params![stale_cutoff],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let (same_size_groups, same_size_extra_bytes): (i64, i64) = conn.query_row(
        "SELECT COUNT(*), COALESCE(SUM((cnt - 1) * size), 0) FROM (
            SELECT size, COUNT(*) AS cnt FROM file_inventory
            WHERE size > ?1 GROUP BY size HAVING COUNT(*) > 1
         )",
        params![SAME_SIZE_MIN_BYTES],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let other_files: i64 = conn.query_row(
        "SELECT COUNT(*) FROM file_inventory WHERE category = 'Other'",
        [],
        |row| row.get(0),
    )?;
    Ok(DashboardInsights {
        large_files,
        large_bytes,
        stale_files,
        stale_bytes,
        same_size_groups,
        same_size_extra_bytes,
        other_files,
    })
}

fn get_age_buckets_on(conn: &Connection, now: i64) -> SqliteResult<Vec<AgeBucket>> {
    let d7 = now - 7 * 86_400;
    let d30 = now - 30 * 86_400;
    let d90 = now - 90 * 86_400;
    let d365 = now - 365 * 86_400;
    let mut stmt = conn.prepare(
        "SELECT CASE
            WHEN mtime >= ?1 THEN '7d'
            WHEN mtime >= ?2 THEN '30d'
            WHEN mtime >= ?3 THEN '90d'
            WHEN mtime >= ?4 THEN '365d'
            ELSE 'older'
         END AS bucket,
         COUNT(*),
         COALESCE(SUM(size), 0)
         FROM file_inventory
         GROUP BY 1",
    )?;
    let rows = stmt
        .query_map(params![d7, d30, d90, d365], |row| {
            Ok(AgeBucket {
                bucket: row.get(0)?,
                files: row.get(1)?,
                bytes: row.get(2)?,
            })
        })?
        .collect::<SqliteResult<Vec<_>>>()?;
    let mut by_key = std::collections::HashMap::new();
    for row in rows {
        by_key.insert(row.bucket.clone(), row);
    }
    const ORDER: [&str; 5] = ["7d", "30d", "90d", "365d", "older"];
    Ok(ORDER
        .iter()
        .map(|key| {
            by_key.get(*key).cloned().unwrap_or(AgeBucket {
                bucket: (*key).to_string(),
                files: 0,
                bytes: 0,
            })
        })
        .collect())
}

fn get_inventory_files_on(
    conn: &Connection,
    sort: &str,
    category: Option<&str>,
    root: Option<&str>,
    query: Option<&str>,
    limit: i64,
) -> SqliteResult<Vec<InventoryFile>> {
    let order = if sort == "mtime" {
        "mtime DESC, path ASC"
    } else {
        "size DESC, path ASC"
    };
    let cat = category.unwrap_or("");
    let root_path = root.unwrap_or("");
    let q = query.unwrap_or("").trim();
    let sql = format!(
        "SELECT path, size, mtime, category, root_path FROM file_inventory
         WHERE root_path IN (SELECT path FROM watched_folders)
           AND (?1 = '' OR category = ?1)
           AND (?2 = '' OR root_path = ?2)
           AND (?3 = '' OR INSTR(LOWER(path),LOWER(?3)) > 0)
         ORDER BY {order}
         LIMIT ?4"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(params![cat, root_path, q, limit.max(0)], |row| {
            Ok(InventoryFile {
                path: row.get(0)?,
                size: row.get(1)?,
                mtime: row.get(2)?,
                category: row.get(3)?,
                root_path: row.get(4)?,
            })
        })?
        .collect::<SqliteResult<Vec<_>>>()?;
    Ok(rows)
}

// Public wrappers that use the global DB.

pub fn replace_inventory_for_root(root: &str, rows: &[InventoryRow]) -> SqliteResult<()> {
    let mut conn = sqlite_lock()?;
    replace_inventory_on(&mut conn, root, rows)
}

pub fn append_inventory_batch(root: &str, rows: &[InventoryRow]) -> SqliteResult<()> {
    let mut conn = sqlite_lock()?;
    append_inventory_batch_on(&mut conn, root, rows)
}

pub fn clear_inventory_for_root(root: &str) -> SqliteResult<()> {
    let conn = sqlite_lock()?;
    conn.execute("DELETE FROM file_inventory WHERE root_path = ?1", params![root])
        .map(|_| ())
}

pub fn get_inventory_stats() -> SqliteResult<InventoryStats> {
    let conn = sqlite_lock()?;
    get_inventory_stats_on(&conn)
}

pub fn get_largest_files(limit: i64) -> SqliteResult<Vec<LargestFile>> {
    let conn = sqlite_lock()?;
    get_largest_files_on(&conn, limit)
}

pub fn get_category_distribution() -> SqliteResult<Vec<CategoryStat>> {
    let conn = sqlite_lock()?;
    get_category_distribution_on(&conn)
}

pub fn get_root_summaries() -> SqliteResult<Vec<RootStat>> {
    let conn = sqlite_lock()?;
    get_root_summaries_on(&conn)
}

pub fn get_dashboard_insights() -> SqliteResult<DashboardInsights> {
    let conn = sqlite_lock()?;
    get_insights_on(&conn, now_epoch())
}

pub fn get_age_buckets() -> SqliteResult<Vec<AgeBucket>> {
    let conn = sqlite_lock()?;
    get_age_buckets_on(&conn, now_epoch())
}

pub fn get_inventory_files(
    sort: &str,
    category: Option<&str>,
    root: Option<&str>,
    query: Option<&str>,
    limit: i64,
) -> SqliteResult<Vec<InventoryFile>> {
    let conn = sqlite_lock()?;
    get_inventory_files_on(&conn, sort, category, root, query, limit)
}

pub fn get_dashboard_snapshot() -> SqliteResult<DashboardSnapshot> {
    let mut conn = sqlite_lock()?;
    let tx = conn.transaction()?;
    // Legacy inventory left by a previously removed root is excluded without
    // altering the user's stored evidence during an upgrade.
    let (total_files,total_bytes): (i64,i64) = tx.query_row(
        "SELECT COUNT(*),COALESCE(SUM(size),0) FROM file_inventory WHERE root_path IN (SELECT path FROM watched_folders)",
        [],|r| Ok((r.get(0)?,r.get(1)?)),
    )?;
    let mut category_stmt = tx.prepare("SELECT category,COUNT(*),COALESCE(SUM(size),0) FROM file_inventory WHERE root_path IN (SELECT path FROM watched_folders) GROUP BY category ORDER BY 3 DESC")?;
    let category_breakdown = category_stmt.query_map([],|r| Ok(CategoryStat{category:r.get(0)?,files:r.get(1)?,bytes:r.get(2)?}))?.collect::<SqliteResult<Vec<_>>>()?;
    drop(category_stmt);
    let mut roots_stmt=tx.prepare("SELECT w.path,COUNT(i.id),COALESCE(SUM(i.size),0),COALESCE(r.status,'unscanned'),COALESCE(r.stale,1),r.last_scan_at,r.error FROM watched_folders w LEFT JOIN inventory_roots r ON r.root_path=w.path LEFT JOIN file_inventory i ON i.root_path=w.path GROUP BY w.id ORDER BY 3 DESC,w.path")?;
    let roots=roots_stmt.query_map([],|r|Ok(RootStat{path:r.get(0)?,files:r.get(1)?,bytes:r.get(2)?,status:r.get(3)?,stale:r.get(4)?,last_scan_at:r.get(5)?,error:r.get(6)?}))?.collect::<SqliteResult<Vec<_>>>()?;
    drop(roots_stmt);
    let last_scan_at = roots.iter().filter_map(|root|root.last_scan_at).max();
    let largest_files=get_snapshot_files_on(&tx,"size")?;
    let recent_files=get_snapshot_files_on(&tx,"mtime")?;
    let now=now_epoch();
    let mut insights = get_insights_on(&tx,now)?;
    // Use the current-root relation for all snapshot aggregates.
    let (large_files,large_bytes,stale_files,stale_bytes,other_files):(i64,i64,i64,i64,i64)=tx.query_row(
        "SELECT COALESCE(SUM(size>=?1),0),COALESCE(SUM(CASE WHEN size>=?1 THEN size ELSE 0 END),0),
         COALESCE(SUM(mtime>0 AND mtime<?2),0),COALESCE(SUM(CASE WHEN mtime>0 AND mtime<?2 THEN size ELSE 0 END),0),COALESCE(SUM(category='Other'),0)
         FROM file_inventory WHERE root_path IN (SELECT path FROM watched_folders)",
        params![LARGE_FILE_BYTES,now-STALE_FILE_DAYS*86_400],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)),
    )?;
    insights.large_files=large_files; insights.large_bytes=large_bytes; insights.stale_files=stale_files; insights.stale_bytes=stale_bytes; insights.other_files=other_files;
    let (groups,extras):(i64,i64)=tx.query_row("SELECT COUNT(*),COALESCE(SUM((cnt-1)*size),0) FROM (SELECT size,COUNT(*) AS cnt FROM file_inventory WHERE size>?1 AND root_path IN (SELECT path FROM watched_folders) GROUP BY size HAVING COUNT(*)>1)",[SAME_SIZE_MIN_BYTES],|r|Ok((r.get(0)?,r.get(1)?)))?;
    insights.same_size_groups=groups; insights.same_size_extra_bytes=extras;
    let mut age_buckets:Vec<AgeBucket>=["7d","30d","90d","365d","older"].iter().map(|b|AgeBucket{bucket:b.to_string(),files:0,bytes:0}).collect();
    let mut age_stmt=tx.prepare("SELECT CASE WHEN mtime>=?1 THEN 0 WHEN mtime>=?2 THEN 1 WHEN mtime>=?3 THEN 2 WHEN mtime>=?4 THEN 3 ELSE 4 END,COUNT(*),COALESCE(SUM(size),0) FROM file_inventory WHERE root_path IN (SELECT path FROM watched_folders) GROUP BY 1")?;
    for row in age_stmt.query_map(params![now-7*86_400,now-30*86_400,now-90*86_400,now-365*86_400],|r|Ok((r.get::<_,usize>(0)?,r.get::<_,i64>(1)?,r.get::<_,i64>(2)?)))? {
        let (index,files,bytes)=row?; age_buckets[index].files=files;age_buckets[index].bytes=bytes;
    }
    drop(age_stmt);
    let revision=tx.query_row("SELECT revision FROM inventory_state WHERE id=1",[],|r|r.get(0))?;
    let snapshot=DashboardSnapshot{total_files,total_bytes,category_breakdown,largest_files,recent_files,watched_roots:roots,last_scan_at,insights,age_buckets,revision};
    tx.commit()?;
    Ok(snapshot)
}

fn get_snapshot_files_on(conn:&Connection,sort:&str)->SqliteResult<Vec<InventoryFile>> {
    let order=if sort=="mtime" {"mtime DESC,path ASC"} else {"size DESC,path ASC"};
    let mut stmt=conn.prepare(&format!("SELECT path,size,mtime,category,root_path FROM file_inventory WHERE root_path IN (SELECT path FROM watched_folders) ORDER BY {order} LIMIT 50"))?;
    let rows=stmt.query_map([],|r|Ok(InventoryFile{path:r.get(0)?,size:r.get(1)?,mtime:r.get(2)?,category:r.get(3)?,root_path:r.get(4)?}))?.collect::<SqliteResult<Vec<_>>>()?;
    Ok(rows)
}

#[derive(Debug,Clone)]
pub struct ScanToken { pub root: String, root_id:i64, generation:i64, revision:i64 }

#[derive(Debug,Clone)]
pub struct SnapshotInventoryRow { pub row:InventoryRow, pub identity:String, pub mtime_ns:String }

fn recover_interrupted_inventory_on(conn: &mut Connection, error: &str) -> SqliteResult<()> {
    let tx = conn.transaction()?;
    let changed = tx.execute(
        "UPDATE inventory_roots SET status='failed',stale=1,error=?1 WHERE status='scanning'",
        [error],
    )?;
    // Only startup and the unwound, still-exclusive scan worker call this.
    tx.execute("DELETE FROM inventory_scan_stage", [])?;
    if changed > 0 {
        tx.execute("UPDATE inventory_state SET revision=revision+1 WHERE id=1", [])?;
    }
    tx.commit()
}

pub(crate) fn recover_interrupted_inventory(error: &str) -> Result<(), String> {
    recover_interrupted_inventory_on(&mut *lock_db()?, error).map_err(|error| error.to_string())
}

fn require_current_inventory_scan(conn: &Connection, token: &ScanToken) -> Result<(), String> {
    let current: bool = conn.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM watched_folders w JOIN inventory_roots r ON r.root_path=w.path
            WHERE w.id=?1 AND w.path=?2 AND w.enabled=1 AND w.mode<>'paused'
              AND r.generation=?3 AND r.status='scanning'
         ) AND (SELECT mutation_revision FROM inventory_state WHERE id=1)=?4",
        params![token.root_id, token.root, token.generation, token.revision],
        |row| row.get(0),
    ).map_err(|error| error.to_string())?;
    if current {
        Ok(())
    } else {
        Err("Inventory changed during scan; previous snapshot retained. Scan again.".into())
    }
}

pub fn begin_inventory_scan(root:&str)->Result<ScanToken,String> {
    let mut conn=lock_db()?;
    let tx=conn.transaction().map_err(|e|e.to_string())?;
    let root_id=tx.query_row("SELECT id FROM watched_folders WHERE path=?1 AND enabled=1 AND mode<>'paused'",[root],|r|r.get(0)).map_err(|_|format!("Root is no longer active: {root}"))?;
    tx.execute("INSERT INTO inventory_roots(root_path,generation,status,stale) VALUES(?1,1,'scanning',1) ON CONFLICT(root_path) DO UPDATE SET generation=generation+1,status='scanning',stale=1,error=NULL",[root]).map_err(|e|e.to_string())?;
    tx.execute("DELETE FROM inventory_scan_stage WHERE root_path=?1",[root]).map_err(|e|e.to_string())?;
    tx.execute("UPDATE inventory_state SET revision=revision+1 WHERE id=1",[]).map_err(|e|e.to_string())?;
    let generation=tx.query_row("SELECT generation FROM inventory_roots WHERE root_path=?1",[root],|r|r.get(0)).map_err(|e|e.to_string())?;
    let revision=tx.query_row("SELECT mutation_revision FROM inventory_state WHERE id=1",[],|r|r.get(0)).map_err(|e|e.to_string())?;
    tx.commit().map_err(|e|e.to_string())?;
    Ok(ScanToken{root:root.to_string(),root_id,generation,revision})
}

pub fn stage_inventory_batch(token:&ScanToken,rows:&[SnapshotInventoryRow])->Result<(),String> {
    let mut conn=lock_db()?;
    let tx=conn.transaction().map_err(|e|e.to_string())?;
    require_current_inventory_scan(&tx, token)?;
    for item in rows {
        tx.execute("INSERT INTO inventory_scan_stage(root_path,generation,path,size,mtime,category,identity,mtime_ns) VALUES(?1,?2,?3,?4,?5,?6,?7,?8) ON CONFLICT(root_path,generation,path) DO UPDATE SET size=excluded.size,mtime=excluded.mtime,category=excluded.category,identity=excluded.identity,mtime_ns=excluded.mtime_ns",params![token.root,token.generation,item.row.path,item.row.size,item.row.mtime,item.row.category,item.identity,item.mtime_ns]).map_err(|e|e.to_string())?;
    }
    tx.commit().map_err(|e|e.to_string())
}

pub fn publish_inventory_scan(token:&ScanToken)->Result<(),String> {
    let mut conn=lock_db()?;
    let tx=conn.transaction().map_err(|e|e.to_string())?;
    require_current_inventory_scan(&tx, token)?;
    let scanned_at=now_epoch();
    tx.execute("DELETE FROM hash_cache WHERE path IN (SELECT path FROM file_inventory WHERE root_path=?1)",[&token.root]).map_err(|e|e.to_string())?;
    tx.execute("DELETE FROM inventory_fingerprints WHERE path IN (SELECT path FROM file_inventory WHERE root_path=?1)",[&token.root]).map_err(|e|e.to_string())?;
    tx.execute("DELETE FROM file_inventory WHERE root_path=?1 AND path NOT IN (SELECT path FROM inventory_scan_stage WHERE root_path=?1 AND generation=?2)",params![token.root,token.generation]).map_err(|e|e.to_string())?;
    tx.execute("INSERT INTO file_inventory(root_path,path,size,mtime,category,scanned_at) SELECT s.root_path,s.path,s.size,s.mtime,COALESCE(o.category,s.category),?3 FROM inventory_scan_stage s LEFT JOIN category_overrides o ON o.path=s.path AND o.identity=s.identity AND o.size=s.size AND o.mtime_ns=s.mtime_ns WHERE s.root_path=?1 AND s.generation=?2 ON CONFLICT(path) DO UPDATE SET root_path=excluded.root_path,size=excluded.size,mtime=excluded.mtime,category=excluded.category,scanned_at=excluded.scanned_at",params![token.root,token.generation,scanned_at]).map_err(|e|e.to_string())?;
    tx.execute("INSERT INTO inventory_fingerprints(path,identity,size,mtime_ns) SELECT path,identity,size,mtime_ns FROM inventory_scan_stage WHERE root_path=?1 AND generation=?2 ON CONFLICT(path) DO UPDATE SET identity=excluded.identity,size=excluded.size,mtime_ns=excluded.mtime_ns",params![token.root,token.generation]).map_err(|e|e.to_string())?;
    tx.execute("UPDATE inventory_roots SET status='complete',stale=0,last_scan_at=?2,error=NULL WHERE root_path=?1",params![token.root,scanned_at]).map_err(|e|e.to_string())?;
    tx.execute("DELETE FROM inventory_scan_stage WHERE root_path=?1 AND generation=?2",params![token.root,token.generation]).map_err(|e|e.to_string())?;
    tx.execute("UPDATE inventory_state SET revision=revision+1 WHERE id=1",[]).map_err(|e|e.to_string())?;
    // Publishing another root does not invalidate an in-progress scan; only
    // user/file mutations advance this revision.
    tx.commit().map_err(|e|e.to_string())
}

pub fn fail_inventory_scan(token:&ScanToken,error:&str)->Result<(),String> {
    let mut conn=lock_db()?;let tx=conn.transaction().map_err(|e|e.to_string())?;
    let changed = tx.execute(
        "UPDATE inventory_roots SET status='failed',stale=1,error=?3
         WHERE root_path=?1 AND generation=?2 AND status='scanning'
           AND EXISTS(SELECT 1 FROM watched_folders WHERE id=?4 AND path=?1)",
        params![token.root, token.generation, error, token.root_id],
    ).map_err(|e|e.to_string())?;
    if changed > 0 {
        tx.execute("DELETE FROM inventory_scan_stage WHERE root_path=?1 AND generation=?2",params![token.root,token.generation]).map_err(|e|e.to_string())?;
        tx.execute("UPDATE inventory_state SET revision=revision+1 WHERE id=1",[]).map_err(|e|e.to_string())?;
    }
    tx.commit().map_err(|e|e.to_string())
}

// ---------------------------------------------------------------------------
// hash_cache helpers
// ---------------------------------------------------------------------------

pub fn get_cached_hash(path: &str, size: i64, mtime: i64) -> SqliteResult<Option<(String, bool)>> {
    let conn = sqlite_lock()?;
    let mut stmt = conn.prepare(
        "SELECT hash, is_full FROM hash_cache WHERE path = ?1 AND size = ?2 AND mtime = ?3",
    )?;
    let mut rows = stmt.query_map(params![path, size, mtime], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i32>(1)? != 0))
    })?;
    match rows.next() {
        Some(Ok(v)) => Ok(Some(v)),
        Some(Err(e)) => Err(e),
        None => Ok(None),
    }
}

pub fn set_cached_hash(path: &str, size: i64, mtime: i64, hash: &str, is_full: bool) -> SqliteResult<()> {
    let conn = sqlite_lock()?;
    conn.execute(
        "INSERT OR REPLACE INTO hash_cache (path, size, mtime, hash, is_full) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![path, size, mtime, hash, is_full as i32],
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Cleanup query helpers
// ---------------------------------------------------------------------------

pub fn count_inventory_files() -> SqliteResult<i64> {
    let conn = sqlite_lock()?;
    conn.query_row("SELECT COUNT(*) FROM file_inventory", [], |row| row.get(0))
}

/// Return (size, path, mtime) for each size group that has >1 file.
/// Groups are returned unsorted (caller may sort by size desc).
pub type SizeGroup = (i64, Vec<(String, i64)>);
pub fn get_inventory_size_groups() -> SqliteResult<Vec<SizeGroup>> {
    let conn = sqlite_lock()?;
    let mut stmt = conn.prepare(
        "SELECT size, path, mtime FROM file_inventory
         WHERE size IN (SELECT size FROM file_inventory GROUP BY size HAVING COUNT(*) > 1)
         ORDER BY size DESC, path",
    )?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })?
        .collect::<SqliteResult<Vec<_>>>()?;
    let mut groups: std::collections::HashMap<i64, Vec<(String, i64)>> = std::collections::HashMap::new();
    for (size, path, mtime) in rows {
        groups.entry(size).or_default().push((path, mtime));
    }
    Ok(groups.into_iter().collect())
}

pub fn get_large_files_from_inventory(min_bytes: i64) -> SqliteResult<Vec<LargestFile>> {
    let conn = sqlite_lock()?;
    let mut stmt = conn.prepare(
        "SELECT path, size, mtime FROM file_inventory WHERE size >= ?1 ORDER BY size DESC",
    )?;
    let rows = stmt
        .query_map(params![min_bytes], |row| {
            Ok(LargestFile {
                path: row.get(0)?,
                size: row.get(1)?,
                mtime: row.get(2)?,
            })
        })?
        .collect::<SqliteResult<Vec<_>>>()?;
    Ok(rows)
}

pub fn get_stale_files_from_inventory(cutoff_mtime: i64) -> SqliteResult<Vec<LargestFile>> {
    let conn = sqlite_lock()?;
    let mut stmt = conn.prepare(
        "SELECT path, size, mtime FROM file_inventory WHERE mtime < ?1 ORDER BY mtime ASC",
    )?;
    let rows = stmt
        .query_map(params![cutoff_mtime], |row| {
            Ok(LargestFile {
                path: row.get(0)?,
                size: row.get(1)?,
                mtime: row.get(2)?,
            })
        })?
        .collect::<SqliteResult<Vec<_>>>()?;
    Ok(rows)
}

// ---------------------------------------------------------------------------
// Suggestion helpers (dismissed_suggestions + unclassified files)
// ---------------------------------------------------------------------------

pub fn dismiss_suggestion(path: &str) -> SqliteResult<()> {
    let conn = sqlite_lock()?;
    let now = now_epoch();
    conn.execute(
        "INSERT INTO dismissed_suggestions(path,dismissed_at,identity,size,mtime_ns)
         SELECT path,?2,identity,size,mtime_ns FROM inventory_fingerprints WHERE path=?1
         ON CONFLICT(path) DO UPDATE SET dismissed_at=excluded.dismissed_at,identity=excluded.identity,size=excluded.size,mtime_ns=excluded.mtime_ns",
        params![path, now],
    )?;
    Ok(())
}

pub fn clear_dismissed_suggestion(path: &str) -> SqliteResult<()> {
    let conn = sqlite_lock()?;
    conn.execute("DELETE FROM dismissed_suggestions WHERE path = ?1", params![path])?;
    Ok(())
}

pub fn get_dismissed_suggestions() -> SqliteResult<std::collections::HashSet<String>> {
    let conn = sqlite_lock()?;
    let mut stmt = conn.prepare("SELECT d.path FROM dismissed_suggestions d JOIN inventory_fingerprints f ON f.path=d.path AND f.identity=d.identity AND f.size=d.size AND f.mtime_ns=d.mtime_ns")?;
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<SqliteResult<Vec<_>>>()?;
    Ok(rows.into_iter().collect())
}

pub fn reset_dismissed_suggestions()->SqliteResult<()> {
    sqlite_lock()?.execute("DELETE FROM dismissed_suggestions",[])?;Ok(())
}

pub fn get_suggestion_candidates_page(limit:i64,after:Option<&str>)->Result<Vec<InventoryFile>,String> {
    let conn=lock_db()?;
    let mut stmt=conn.prepare("SELECT i.path,i.size,i.mtime,i.category,i.root_path FROM file_inventory i JOIN watched_folders w ON w.path=i.root_path AND w.enabled=1 AND w.mode<>'paused' JOIN inventory_fingerprints f ON f.path=i.path WHERE i.category='Other' AND (?1 IS NULL OR i.path>?1) AND NOT EXISTS(SELECT 1 FROM dismissed_suggestions d WHERE d.path=i.path AND d.identity=f.identity AND d.size=f.size AND d.mtime_ns=f.mtime_ns) ORDER BY i.path LIMIT ?2").map_err(|e|e.to_string())?;
    let rows=stmt.query_map(params![after,limit.max(0)],|r|Ok(InventoryFile{path:r.get(0)?,size:r.get(1)?,mtime:r.get(2)?,category:r.get(3)?,root_path:r.get(4)?})).map_err(|e|e.to_string())?.collect::<SqliteResult<Vec<_>>>().map_err(|e|e.to_string())?;Ok(rows)
}

pub fn record_suggestion_feedback(path:&str,category:&str,action_log_id:Option<i64>)->Result<(),String> {
    if !crate::categories::is_valid_category(category) {return Err("Invalid canonical category".into());}
    let prepared=prepare_move(path)?;
    let name=std::path::Path::new(path).file_name().unwrap_or_default().to_string_lossy().into_owned();
    let mut conn=lock_db()?;let tx=conn.transaction().map_err(|e|e.to_string())?;
    reconcile_move_on(&tx,path,&prepared,Some(category)).map_err(|e|e.to_string())?;
    // No-op explicit acceptances are feedback without fabricated move history.
    tx.execute("INSERT INTO accepted_suggestion_feedback(action_log_id,file_name,category) VALUES(?1,?2,?3)",params![action_log_id.unwrap_or(0),name,category]).map_err(|e|e.to_string())?;
    tx.commit().map_err(|e|e.to_string())
}

// ---------------------------------------------------------------------------
// Test infrastructure — shared across all test modules
// ---------------------------------------------------------------------------

/// Lock that serialises all DB-touching tests so they don't interfere via the
/// global OnceCell DB.  Acquired at the top of every test that touches the DB.
#[cfg(test)]
pub static TEST_DB_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Take `TEST_DB_LOCK` for the whole body of a DB-touching test.
///
/// Every holder of this lock is a test, so poisoning carries no information
/// about production behaviour — it only records that some *other* test failed
/// its assertions while holding it. Unwrapping would turn that one real failure
/// into a cascade of unrelated "poisoned lock" failures in modules that never
/// touched the broken test, which is how a single bad assertion used to hide
/// behind five others.
#[cfg(test)]
pub fn serialise_test_db() -> MutexGuard<'static, ()> {
    TEST_DB_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Initialise the global DB if it has not been set yet.  Idempotent so
/// multiple test modules can call it without panicking.
#[cfg(test)]
pub fn init_test_db() {
    if DB.get().is_none() {
        let dir = std::env::temp_dir().join(format!("mouzi-db-{}", std::process::id()));
        // PIDs are reused, and a directory left behind by an earlier test
        // process would hand this one its rows — the suite shares state through
        // this file, so a stale database is a stale fixture.
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);
        let _ = init_db(dir);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn mem_conn_with_inventory() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        create_file_inventory_table(&conn).unwrap();
        conn
    }

    fn remove_action_log(id: i64) -> Result<(), String> {
        lock_db()?
            .execute("DELETE FROM action_logs WHERE id=?1", [id])
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    fn remove_cleanup_log(id: i64) -> Result<(), String> {
        lock_db()?
            .execute("DELETE FROM cleanup_actions WHERE id=?1", [id])
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    /// A row one of these tests inserted, removed when the test ends however it
    /// ends. Every module reads the same database, so a row left behind by a
    /// failed test silently becomes the next module's fixture.
    struct LogRow {
        id: i64,
        table: &'static str,
        remove: fn(i64) -> Result<(), String>,
    }

    impl Drop for LogRow {
        fn drop(&mut self) {
            // Reported rather than unwrapped: this runs while the test may
            // already be panicking, and a second panic would abort the binary.
            if let Err(e) = (self.remove)(self.id) {
                eprintln!(
                    "test fixture: could not remove {} row {}: {e}",
                    self.table, self.id
                );
            }
        }
    }

    #[test]
    fn inventory_replace_and_stats_roundtrip() {
        let mut conn = mem_conn_with_inventory();

        let rows = vec![
            InventoryRow { path: "/a/b.pdf".into(), size: 100, mtime: 1, category: "Documents".into() },
            InventoryRow { path: "/a/c.jpg".into(), size: 200, mtime: 2, category: "Images".into() },
        ];
        replace_inventory_on(&mut conn, "/root1", &rows).unwrap();

        // stats
        let stats = get_inventory_stats_on(&conn).unwrap();
        assert_eq!(stats.total_files, 2);
        assert_eq!(stats.total_bytes, 300);
        assert!(stats.last_scan_at.is_some());

        // category distribution
        let dist = get_category_distribution_on(&conn).unwrap();
        assert_eq!(dist.len(), 2);
        let doc = dist.iter().find(|c| c.category == "Documents").unwrap();
        assert_eq!((doc.files, doc.bytes), (1, 100));
        let img = dist.iter().find(|c| c.category == "Images").unwrap();
        assert_eq!((img.files, img.bytes), (1, 200));

        // largest files
        let largest = get_largest_files_on(&conn, 1).unwrap();
        assert_eq!(largest.len(), 1);
        assert_eq!(largest[0].path, "/a/c.jpg");
        assert_eq!(largest[0].size, 200);

        // root summaries
        let roots = get_root_summaries_on(&conn).unwrap();
        assert_eq!(roots.len(), 1);
        assert_eq!((roots[0].files, roots[0].bytes), (2, 300));
        assert_eq!(roots[0].path, "/root1");

        // second root does not mix
        let rows2 = vec![InventoryRow { path: "/b/x.txt".into(), size: 5, mtime: 3, category: "Other".into() }];
        replace_inventory_on(&mut conn, "/root2", &rows2).unwrap();
        let stats2 = get_inventory_stats_on(&conn).unwrap();
        assert_eq!(stats2.total_files, 3);

        // replace same root truncates old rows
        replace_inventory_on(&mut conn, "/root1", &[InventoryRow { path: "/a/d.txt".into(), size: 5, mtime: 3, category: "Other".into() }]).unwrap();
        let stats3 = get_inventory_stats_on(&conn).unwrap();
        assert_eq!(stats3.total_files, 2); // root1:1, root2:1

        // clear root2
        clear_inventory_on(&conn, "/root2").unwrap();
        assert_eq!(get_inventory_stats_on(&conn).unwrap().total_files, 1);
    }

    #[test]
    fn inventory_batch_chunking() {
        // Insert more than INVENTORY_BATCH_SIZE rows to exercise chunking.
        let mut conn = mem_conn_with_inventory();
        let rows: Vec<InventoryRow> = (0..INVENTORY_BATCH_SIZE + 10)
            .map(|i| InventoryRow {
                path: format!("/chunk/{}.txt", i),
                size: i as i64,
                mtime: i as i64,
                category: "Other".into(),
            })
            .collect();
        replace_inventory_on(&mut conn, "/root", &rows).unwrap();
        let stats = get_inventory_stats_on(&conn).unwrap();
        assert_eq!(stats.total_files, (INVENTORY_BATCH_SIZE + 10) as i64);
    }

    #[test]
    fn recent_logs_survives_corrupt_timestamp() {
        let _guard = serialise_test_db();
        init_test_db();
        let id = {
            let conn = lock_db().unwrap();
            conn.execute(
                "INSERT INTO action_logs (timestamp, source_path, destination_path, action, file_name, file_type, undone) VALUES ('not-a-date', '/corrupt-test', '/dst', 'move', 'f.txt', 'text', 0)",
                [],
            )
            .unwrap();
            conn.last_insert_rowid()
        };
        let _row = LogRow { id, table: "action_logs", remove: remove_action_log };
        let logs = get_recent_logs(100).unwrap();
        let corrupt = logs.iter().find(|l| l.source_path == "/corrupt-test");
        assert!(corrupt.is_some(), "corrupt row should be returned with epoch fallback");
        assert_eq!(corrupt.unwrap().timestamp, Utc.timestamp_opt(0, 0).unwrap());
    }

    #[test]
    fn cleanup_logs_survives_corrupt_timestamp() {
        let _guard = serialise_test_db();
        init_test_db();
        let id = {
            let conn = lock_db().unwrap();
            conn.execute(
                "INSERT INTO cleanup_actions (timestamp, path, prev_path, dest, action, status, undoable) VALUES ('not-a-date', '/corrupt-cleanup', '/prev', '/dest', 'delete', 'ok', 0)",
                [],
            )
            .unwrap();
            conn.last_insert_rowid()
        };
        let _row = LogRow { id, table: "cleanup_actions", remove: remove_cleanup_log };
        let logs = get_cleanup_logs(100).unwrap();
        let corrupt = logs.iter().find(|l| l.path == "/corrupt-cleanup");
        assert!(corrupt.is_some(), "corrupt cleanup row should be returned with epoch fallback");
        assert_eq!(corrupt.unwrap().timestamp, Utc.timestamp_opt(0, 0).unwrap());
    }

    #[test]
    fn insights_count_large_stale_same_size_and_other() {
        let mut conn = mem_conn_with_inventory();
        let now = 1_700_000_000;
        let year = 365 * 86_400;
        let rows = vec![
            InventoryRow {
                path: "/a/huge.mp4".into(),
                size: LARGE_FILE_BYTES,
                mtime: now,
                category: "Videos".into(),
            },
            InventoryRow {
                path: "/a/old.txt".into(),
                size: 10,
                mtime: now - year - 10,
                category: "Documents".into(),
            },
            InventoryRow {
                path: "/a/dup1.bin".into(),
                size: 5000,
                mtime: now,
                category: "Other".into(),
            },
            InventoryRow {
                path: "/a/dup2.bin".into(),
                size: 5000,
                mtime: now,
                category: "Other".into(),
            },
        ];
        replace_inventory_on(&mut conn, "/a", &rows).unwrap();

        let insights = get_insights_on(&conn, now).unwrap();
        assert_eq!(insights.large_files, 1);
        assert_eq!(insights.large_bytes, LARGE_FILE_BYTES);
        assert_eq!(insights.stale_files, 1);
        assert_eq!(insights.stale_bytes, 10);
        assert_eq!(insights.same_size_groups, 1);
        assert_eq!(insights.same_size_extra_bytes, 5000);
        assert_eq!(insights.other_files, 2);
    }

    #[test]
    fn age_buckets_are_stable_and_ordered() {
        let mut conn = mem_conn_with_inventory();
        let now = 1_700_000_000;
        let rows = vec![
            InventoryRow { path: "/a/week.txt".into(), size: 1, mtime: now - 2 * 86_400, category: "Other".into() },
            InventoryRow { path: "/a/month.txt".into(), size: 2, mtime: now - 20 * 86_400, category: "Other".into() },
            InventoryRow { path: "/a/quarter.txt".into(), size: 3, mtime: now - 60 * 86_400, category: "Other".into() },
            InventoryRow { path: "/a/year.txt".into(), size: 4, mtime: now - 200 * 86_400, category: "Other".into() },
            InventoryRow { path: "/a/old.txt".into(), size: 5, mtime: now - 400 * 86_400, category: "Other".into() },
        ];
        replace_inventory_on(&mut conn, "/a", &rows).unwrap();
        let buckets = get_age_buckets_on(&conn, now).unwrap();
        let keys: Vec<&str> = buckets.iter().map(|b| b.bucket.as_str()).collect();
        assert_eq!(keys, vec!["7d", "30d", "90d", "365d", "older"]);
        assert_eq!(buckets.iter().map(|b| b.files).collect::<Vec<_>>(), vec![1, 1, 1, 1, 1]);
        assert_eq!(buckets.iter().map(|b| b.bytes).collect::<Vec<_>>(), vec![1, 2, 3, 4, 5]);
    }

    #[test]
    fn inventory_files_filter_sort_and_query() {
        let mut conn = mem_conn_with_inventory();
        conn.execute("INSERT INTO watched_folders(path) VALUES('/docs'),('/pics')", []).unwrap();
        let rows = vec![
            InventoryRow { path: "/docs/a.pdf".into(), size: 30, mtime: 3, category: "Documents".into() },
            InventoryRow { path: "/docs/b.pdf".into(), size: 10, mtime: 9, category: "Documents".into() },
            InventoryRow { path: "/pics/c.jpg".into(), size: 50, mtime: 1, category: "Images".into() },
        ];
        replace_inventory_on(&mut conn, "/docs", &rows[..2]).unwrap();
        replace_inventory_on(&mut conn, "/pics", &rows[2..]).unwrap();

        let by_size = get_inventory_files_on(&conn, "size", None, None, None, 10).unwrap();
        assert_eq!(by_size.iter().map(|f| f.size).collect::<Vec<_>>(), vec![50, 30, 10]);

        let docs = get_inventory_files_on(&conn, "size", Some("Documents"), None, None, 10).unwrap();
        assert_eq!(docs.len(), 2);
        assert!(docs.iter().all(|f| f.category == "Documents"));

        let recent = get_inventory_files_on(&conn, "mtime", None, None, None, 1).unwrap();
        assert_eq!(recent[0].path, "/docs/b.pdf");

        let q = get_inventory_files_on(&conn, "size", None, None, Some("JPG"), 10).unwrap();
        assert_eq!(q.len(), 1);
        assert_eq!(q[0].path, "/pics/c.jpg");

        let root = get_inventory_files_on(&conn, "size", None, Some("/pics"), None, 10).unwrap();
        assert_eq!(root.len(), 1);
        assert_eq!(root[0].root_path, "/pics");
    }

    #[test]
    fn preference_patch_preserves_concurrent_schedule_and_returns_revision() {
        let conn=mem_conn_with_inventory();
        let first=get_settings_on(&conn).unwrap();assert!(!first.autostart);assert!(!first.local_ai_enabled);assert_eq!(first.local_ai_model,"llama3.2");
        let schedule=ScheduleSettings{schedule_enabled:true,schedule_times_per_day:1,schedule_time_1:Some("10:30".into()),schedule_time_2:None,schedule_time_3:None,schedule_time_4:None};
        let scheduled=update_schedule_settings_on(&conn,&schedule).unwrap();
        let committed=update_preferences_on(&conn,&PreferencesPatch{theme:Some("dark".into()),..Default::default()}).unwrap();
        assert_eq!(committed.theme,"dark");assert!(committed.schedule_enabled);assert_eq!(committed.schedule_time_1.as_deref(),Some("10:30"));assert_eq!(committed.revision,scheduled.revision+1);
        let after=update_schedule_settings_on(&conn,&ScheduleSettings{schedule_enabled:false,..schedule}).unwrap();assert_eq!(after.theme,"dark");assert_eq!(after.revision,committed.revision+1);
    }

    #[test]
    fn additive_upgrade_preserves_existing_preferences_and_automation_choice() {
        let conn=Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE settings(id INTEGER PRIMARY KEY,language TEXT NOT NULL,theme TEXT NOT NULL,telemetry_enabled INTEGER NOT NULL,first_run INTEGER NOT NULL,autostart INTEGER NOT NULL);INSERT INTO settings VALUES(42,'hy','light',1,0,1);").unwrap();
        initialize_schema(&conn).unwrap();let settings=get_settings_on(&conn).unwrap();
        assert_eq!(settings.id,Some(42));assert_eq!(settings.language,"hy");assert_eq!(settings.theme,"light");assert!(settings.autostart);assert!(settings.telemetry_enabled);assert!(!settings.first_run);assert!(!settings.local_ai_enabled);
        initialize_schema(&conn).unwrap();assert_eq!(get_settings_on(&conn).unwrap().id,Some(42));
    }

    #[test]
    fn literal_search_keeps_percent_underscore_and_windows_separator() {
        let mut conn=mem_conn_with_inventory();
        conn.execute("INSERT INTO watched_folders(path,mode) VALUES('root','paused')", []).unwrap();
        let paths=[r"C:\docs\100%_ready.txt",r"C:\docs\100Xready.txt",r"C:\other\100%_ready.txt"];
        let rows:Vec<_>=paths.iter().map(|path|InventoryRow{path:path.to_string(),size:1,mtime:1,category:"Other".into()}).collect();
        replace_inventory_on(&mut conn,"root",&rows).unwrap();
        let orphan = InventoryRow {
            path: r"D:\docs\100%_ready.txt".into(),
            size: 100,
            mtime: 1,
            category: "Other".into(),
        };
        replace_inventory_on(&mut conn, "removed-root", &[orphan]).unwrap();
        let result = get_inventory_files_on(&conn, "size", Some("Other"), None, Some(r"\docs\100%_"), 1).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].path, paths[0]);
        assert!(get_inventory_files_on(&conn, "size", None, Some("removed-root"), None, 10).unwrap().is_empty());
        assert_eq!(get_inventory_stats_on(&conn).unwrap().total_files, 4);
    }

    #[test]
    fn database_reopen_discards_abandoned_staging_but_keeps_published_rows() {
        struct TemporaryDatabase(PathBuf);
        impl Drop for TemporaryDatabase {
            fn drop(&mut self) { let _ = std::fs::remove_file(&self.0); }
        }
        let path = TemporaryDatabase(std::env::temp_dir().join(format!(
            "mouzi-interrupted-{}-{}.db",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap(),
        )));
        {
            let conn = open_database(&path.0).unwrap();
            conn.execute_batch("
                INSERT INTO watched_folders(path) VALUES('/work'),('/finished');
                INSERT INTO inventory_roots(root_path,generation,status,stale,last_scan_at)
                    VALUES('/work',7,'scanning',1,100),('/finished',2,'complete',0,200);
                INSERT INTO file_inventory(root_path,path) VALUES('/work','/work/previous.txt');
                INSERT INTO inventory_scan_stage(root_path,generation,path,size,mtime,category,identity,mtime_ns)
                    VALUES('/work',7,'/work/staged.txt',5,1,'Other','staged-identity','1');
            ").unwrap();
        }
        let conn = open_database(&path.0).unwrap();
        let state: (i64, String, bool, i64, String) = conn.query_row(
            "SELECT generation,status,stale,last_scan_at,error FROM inventory_roots WHERE root_path='/work'",
            [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
        ).unwrap();
        assert_eq!((state.0, state.1.as_str(), state.2, state.3), (7, "failed", true, 100));
        assert!(state.4.contains("interrupted"));
        assert_eq!(conn.query_row::<i64, _, _>("SELECT COUNT(*) FROM inventory_scan_stage", [], |row| row.get(0)).unwrap(), 0);
        assert_eq!(conn.query_row::<String, _, _>("SELECT status FROM inventory_roots WHERE root_path='/finished'", [], |row| row.get(0)).unwrap(), "complete");
        let rows = get_inventory_files_on(&conn, "size", None, Some("/work"), None, 10).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].path, "/work/previous.txt");
    }

    #[cfg(windows)]
    #[test]
    fn inventory_removal_keeps_case_distinct_sibling_and_its_metadata() {
        let conn = mem_conn_with_inventory();
        let removed = r"\\?\C:\docs\A.txt";
        let sibling = r"\\?\C:\docs\a.txt";
        for (path, identity) in [(removed, "removed"), (sibling, "sibling")] {
            conn.execute("INSERT INTO file_inventory(root_path,path) VALUES('root',?1)", [path]).unwrap();
            conn.execute("INSERT INTO inventory_fingerprints(path,identity,size,mtime_ns) VALUES(?1,?2,1,'1')", params![path, identity]).unwrap();
            conn.execute("INSERT INTO category_overrides(path,identity,size,mtime_ns,category) VALUES(?1,?2,1,'1','Code')", params![path, identity]).unwrap();
            conn.execute("INSERT INTO dismissed_suggestions(path,identity,size,mtime_ns) VALUES(?1,?2,1,'1')", params![path, identity]).unwrap();
            conn.execute("INSERT INTO hash_cache(path,size,mtime,hash) VALUES(?1,1,1,'hash')", [path]).unwrap();
        }
        remove_inventory_path_on(&conn, r"C:/docs/A.txt").unwrap();
        for table in ["file_inventory", "inventory_fingerprints", "category_overrides", "dismissed_suggestions", "hash_cache"] {
            let path: String = conn.query_row(&format!("SELECT path FROM {table}"), [], |row| row.get(0)).unwrap();
            assert_eq!(path, sibling, "{table}");
            assert_eq!(conn.query_row::<i64, _, _>(&format!("SELECT COUNT(*) FROM {table}"), [], |row| row.get(0)).unwrap(), 1);
        }
    }

    #[test]
    fn replacement_failure_rolls_back_rows_and_preserves_ids() {
        let mut conn=mem_conn_with_inventory();let row=InventoryRow{path:"/root/old".into(),size:3,mtime:1,category:"Other".into()};
        replace_inventory_on(&mut conn,"/root",std::slice::from_ref(&row)).unwrap();
        let id:i64=conn.query_row("SELECT id FROM file_inventory WHERE path='/root/old'",[],|r|r.get(0)).unwrap();
        replace_inventory_on(&mut conn,"/root",std::slice::from_ref(&row)).unwrap();assert_eq!(conn.query_row::<i64,_,_>("SELECT id FROM file_inventory WHERE path='/root/old'",[],|r|r.get(0)).unwrap(),id);
        conn.execute_batch("CREATE TRIGGER fail_inventory_insert BEFORE INSERT ON file_inventory WHEN NEW.path='/root/fail' BEGIN SELECT RAISE(ABORT,'injected failure');END;").unwrap();
        assert!(replace_inventory_on(&mut conn,"/root",&[InventoryRow{path:"/root/fail".into(),..row}]).is_err());assert_eq!(get_inventory_stats_on(&conn).unwrap().total_files,1);assert_eq!(conn.query_row::<i64,_,_>("SELECT id FROM file_inventory WHERE path='/root/old'",[],|r|r.get(0)).unwrap(),id);
    }

    #[test]
    fn replacing_rules_is_one_transaction_on_insert_failure() {
        let mut conn=mem_conn_with_inventory();insert_default_rules_on(&conn).unwrap();
        let before:i64=conn.query_row("SELECT COUNT(*) FROM rules",[],|r|r.get(0)).unwrap();
        let first_id:i64=conn.query_row("SELECT MIN(id) FROM rules",[],|r|r.get(0)).unwrap();
        conn.execute_batch("CREATE TRIGGER fail_rule_insert BEFORE INSERT ON rules WHEN NEW.name='fail' BEGIN SELECT RAISE(ABORT,'injected failure');END;").unwrap();
        let rule=Rule{id:None,name:"fail".into(),priority:0,enabled:true,extensions:vec!["txt".into()],pattern:None,destination:"Documents".into(),action:"move".into(),folder_id:0};
        assert!(import_rules_atomic_on(&mut conn,&[rule],true).is_err());assert_eq!(conn.query_row::<i64,_,_>("SELECT COUNT(*) FROM rules",[],|r|r.get(0)).unwrap(),before);assert_eq!(conn.query_row::<i64,_,_>("SELECT MIN(id) FROM rules",[],|r|r.get(0)).unwrap(),first_id);
    }

    #[test]
    fn existing_root_returns_its_id_after_another_insert() {
        let _guard=serialise_test_db();init_test_db();
        let first=add_watched_folder("/db-id-test-a",FOLDER_MODE_MANUAL).unwrap();let second=add_watched_folder("/db-id-test-b",FOLDER_MODE_MANUAL).unwrap();
        struct Roots(i64,i64);impl Drop for Roots{fn drop(&mut self){let _=remove_watched_folder(self.0);let _=remove_watched_folder(self.1);}}
        let _roots=Roots(first,second);assert_ne!(first,second);assert_eq!(add_watched_folder("/db-id-test-a",FOLDER_MODE_MANUAL).unwrap(),first);
    }

    #[test]
    fn week_cutoff_uses_timestamp_instant_and_history_count_uses_all_rows() {
        let _guard=serialise_test_db();init_test_db();
        let before=history_counts().unwrap();let now=Utc::now();
        let log=|age:chrono::Duration|ActionLog{id:None,timestamp:now-age,source_path:"/week-test".into(),destination_path:Some("/dest".into()),action:"move".into(),file_name:"week-test".into(),file_type:"week-test-category".into(),undone:false};
        let recent=log_action(&log(chrono::Duration::days(6)+chrono::Duration::hours(23))).unwrap();let old=log_action(&log(chrono::Duration::days(7)+chrono::Duration::hours(1))).unwrap();
        let _recent=LogRow{id:recent,table:"action_logs",remove:remove_action_log};let _old=LogRow{id:old,table:"action_logs",remove:remove_action_log};
        let stats=get_weekly_stats().unwrap();assert_eq!(stats.iter().find(|(category,_)|category=="week-test-category").unwrap().1,1);
        let counts=history_counts().unwrap();assert_eq!(counts.total,before.total+2);assert_eq!(counts.undoable,before.undoable+2);
    }

    #[test]
    fn candidate_page_filters_dismissed_and_paused_before_limit() {
        let _guard=serialise_test_db();init_test_db();
        let active=add_watched_folder("/candidate-active",FOLDER_MODE_MANUAL).unwrap();let paused=add_watched_folder("/candidate-paused",FOLDER_MODE_PAUSED).unwrap();
        struct Roots(i64,i64);impl Drop for Roots{fn drop(&mut self){let _=remove_watched_folder(self.0);let _=remove_watched_folder(self.1);}}
        let _roots=Roots(active,paused);
        {
            let conn=lock_db().unwrap();
            for (path,root) in [("/candidate-active/000","/candidate-active"),("/candidate-active/001","/candidate-active"),("/candidate-paused/000","/candidate-paused")] {
                conn.execute("INSERT INTO file_inventory(root_path,path,size,mtime,category) VALUES(?1,?2,1,1,'Other')",params![root,path]).unwrap();
                conn.execute("INSERT INTO inventory_fingerprints(path,identity,size,mtime_ns) VALUES(?1,?2,1,'100')",params![path,path]).unwrap();
            }
        }
        dismiss_suggestion("/candidate-active/000").unwrap();
        let page=get_suggestion_candidates_page(1,Some("/candidate-active/")).unwrap();assert_eq!(page.len(),1);assert_eq!(page[0].path,"/candidate-active/001");
        // Replacing the file under the same name invalidates the dismissal.
        lock_db().unwrap().execute("UPDATE inventory_fingerprints SET identity='replacement' WHERE path='/candidate-active/000'",[]).unwrap();
        let page=get_suggestion_candidates_page(1,Some("/candidate-active/")).unwrap();assert_eq!(page[0].path,"/candidate-active/000");
    }

    #[test]
    fn cleanup_intent_exists_before_outcome_and_reconciles_only_on_removal() {
        let _guard=serialise_test_db();init_test_db();let root=add_watched_folder("/journal-test",FOLDER_MODE_MANUAL).unwrap();
        struct Root(i64);impl Drop for Root{fn drop(&mut self){let _=remove_watched_folder(self.0);}}
        let _root=Root(root);let path="/journal-test/file";
        lock_db().unwrap().execute("INSERT INTO file_inventory(root_path,path,size,mtime,category) VALUES('/journal-test',?1,1,1,'Other')",[path]).unwrap();
        let intent=begin_cleanup_intent("trash_large",path).unwrap();
        let cleanup_id:i64=lock_db().unwrap().query_row("SELECT cleanup_id FROM operation_intents WHERE id=?1 AND status='pending'",[intent],|r|r.get(0)).unwrap();
        finalize_cleanup_intent(intent,"failed",Some("kept source"),false).unwrap();assert_eq!(get_inventory_files("size",None,Some("/journal-test"),None,10).unwrap().len(),1);
        let next=begin_cleanup_intent("trash_large",path).unwrap();let next_cleanup:i64=lock_db().unwrap().query_row("SELECT cleanup_id FROM operation_intents WHERE id=?1",[next],|r|r.get(0)).unwrap();
        finalize_cleanup_intent(next,"deleted",Some("removed"),true).unwrap();assert!(get_inventory_files("size",None,Some("/journal-test"),None,10).unwrap().is_empty());
        let logs=get_cleanup_logs(100).unwrap();assert_eq!(logs.iter().find(|log|log.id==Some(cleanup_id)).unwrap().message.as_deref(),Some("kept source"));
        lock_db().unwrap().execute("DELETE FROM operation_intents WHERE id IN (?1,?2)",params![intent,next]).unwrap();remove_cleanup_log(cleanup_id).unwrap();remove_cleanup_log(next_cleanup).unwrap();
    }
}
