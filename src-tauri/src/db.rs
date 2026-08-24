use chrono::{DateTime, TimeZone, Utc};
use rusqlite::{params, Connection, Result as SqliteResult};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
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
    let db_path = app_dir.join("mouzi.db");
    let conn = Connection::open(db_path)?;

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
            autostart INTEGER NOT NULL DEFAULT 1
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

    // Migration: add missing columns
    let cols: Vec<String> = conn.prepare("PRAGMA table_info(settings)")?
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?;
    if !cols.iter().any(|c| c == "autostart") {
        conn.execute("ALTER TABLE settings ADD COLUMN autostart INTEGER NOT NULL DEFAULT 1", [])?;
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
    // Insert default settings if empty
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM settings",
        [],
        |row| row.get(0),
    )?;

    if count == 0 {
        conn.execute(
            "INSERT INTO settings (language, theme, telemetry_enabled, first_run, autostart) VALUES ('en', 'system', 0, 1, 1)",
            [],
        )?;
    }

    DB.set(Arc::new(Mutex::new(conn)))
        .map_err(|_| rusqlite::Error::ExecuteReturnedResults)?;

    Ok(())
}

pub fn get_db() -> Arc<Mutex<Connection>> {
    DB.get().expect("Database not initialized").clone()
}

pub fn migrate_rules_to_relative() -> SqliteResult<()> {
    let folders = get_watched_folders()?;
    let db = get_db();
    let conn = db.lock().unwrap();
    for folder in folders {
        let folder_norm = folder.path.trim_end_matches('/').trim_end_matches('\\');
        if folder_norm.is_empty() { continue; }
        let mut stmt = conn.prepare("SELECT id, destination FROM rules WHERE destination LIKE ?1")?;
        let rows: Vec<(i64, String)> = stmt
            .query_map([format!("{}%", folder_norm)], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<SqliteResult<Vec<_>>>()?;
        for (id, dest) in rows {
            let relative = dest.strip_prefix(folder_norm)
                    .map(|s| s.trim_start_matches('/').trim_start_matches('\\').to_string())
                    .unwrap_or_else(|| dest.clone());
            if !relative.is_empty() && relative != dest {
                conn.execute("UPDATE rules SET destination = ?1 WHERE id = ?2", params![relative, id])?;
            }
        }
    }
    Ok(())
}

pub fn insert_default_rules(_folder_path: &str) -> SqliteResult<()> {
    let db = get_db();
    let conn = db.lock().unwrap();

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

pub fn get_rules() -> SqliteResult<Vec<Rule>> {
    let db = get_db();
    let conn = db.lock().unwrap();
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
    let db = get_db();
    let conn = db.lock().unwrap();
    let exts = rule.extensions.join(",");
    conn.execute(
        "INSERT INTO rules (name, priority, enabled, extensions, pattern, destination, action, folder_id) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![rule.name, rule.priority, rule.enabled as i32, exts, rule.pattern, rule.destination, rule.action, rule.folder_id],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn update_rule(rule: &Rule) -> SqliteResult<()> {
    let db = get_db();
    let conn = db.lock().unwrap();
    let exts = rule.extensions.join(",");
    conn.execute(
        "UPDATE rules SET name=?1, priority=?2, enabled=?3, extensions=?4, pattern=?5, destination=?6, action=?7, folder_id=?8 WHERE id=?9",
        params![rule.name, rule.priority, rule.enabled as i32, exts, rule.pattern, rule.destination, rule.action, rule.folder_id, rule.id],
    )?;
    Ok(())
}

pub fn delete_rule(id: i64) -> SqliteResult<()> {
    let db = get_db();
    let conn = db.lock().unwrap();
    conn.execute("DELETE FROM rules WHERE id=?1", params![id])?;
    Ok(())
}

pub fn delete_all_rules() -> SqliteResult<()> {
    let db = get_db();
    let conn = db.lock().unwrap();
    conn.execute("DELETE FROM rules", [])?;
    Ok(())
}

pub fn get_watched_folders() -> SqliteResult<Vec<WatchedFolder>> {
    let db = get_db();
    let conn = db.lock().unwrap();
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

pub fn add_watched_folder(path: &str, mode: &str) -> SqliteResult<i64> {
    let db = get_db();
    let conn = db.lock().unwrap();
    conn.execute(
        "INSERT OR IGNORE INTO watched_folders (path, enabled, mode) VALUES (?1, 1, ?2)",
        params![path, mode],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn remove_watched_folder(id: i64) -> SqliteResult<()> {
    let db = get_db();
    let conn = db.lock().unwrap();
    conn.execute("DELETE FROM watched_folders WHERE id=?1", params![id])?;
    Ok(())
}

pub fn update_folder_mode(id: i64, mode: &str) -> SqliteResult<()> {
    let db = get_db();
    let conn = db.lock().unwrap();
    conn.execute("UPDATE watched_folders SET mode=?1 WHERE id=?2", params![mode, id])?;
    Ok(())
}

pub fn log_action(log: &ActionLog) -> SqliteResult<i64> {
    let db = get_db();
    let conn = db.lock().unwrap();
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
    let db = get_db();
    let conn = db.lock().unwrap();
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

pub fn get_undoable_logs() -> SqliteResult<Vec<(i64, String, Option<String>)>> {
    let db = get_db();
    let conn = db.lock().unwrap();
    let mut stmt = conn.prepare(
        "SELECT id, source_path, destination_path FROM action_logs WHERE undone = 0 ORDER BY timestamp DESC"
    )?;
    let logs = stmt.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<String>>(2)?,
        ))
    })?
    .collect::<SqliteResult<Vec<_>>>()?;
    Ok(logs)
}

pub fn get_weekly_stats() -> SqliteResult<Vec<(String, i64)>> {
    let db = get_db();
    let conn = db.lock().unwrap();
    let mut stmt = conn.prepare(
        "SELECT file_type, COUNT(*) FROM action_logs WHERE timestamp > datetime('now', '-7 days') AND undone = 0 GROUP BY file_type"
    )?;
    let stats = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
    })?
    .collect::<SqliteResult<Vec<_>>>()?;
    Ok(stats)
}

pub fn undo_action(id: i64) -> SqliteResult<Option<(String, String)>> {
    let db = get_db();
    let conn = db.lock().unwrap();
    let log: Option<(String, String)> = conn.query_row(
        "SELECT source_path, destination_path FROM action_logs WHERE id=?1 AND undone=0",
        params![id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    ).ok();

    if log.is_some() {
        conn.execute("UPDATE action_logs SET undone=1 WHERE id=?1", params![id])?;
    }
    Ok(log)
}

pub fn get_settings() -> SqliteResult<AppSettings> {
    let db = get_db();
    let conn = db.lock().unwrap();
    conn.query_row(
        "SELECT id, language, theme, telemetry_enabled, first_run, autostart, grace_period_seconds, lock_check_enabled, schedule_enabled, schedule_times_per_day, schedule_time_1, schedule_time_2, schedule_time_3, schedule_time_4 FROM settings LIMIT 1",
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
            })
        },
    )
}

pub fn update_settings(settings: &AppSettings) -> SqliteResult<()> {
    let db = get_db();
    let conn = db.lock().unwrap();
    conn.execute(
        "UPDATE settings SET language=?1, theme=?2, telemetry_enabled=?3, first_run=?4, autostart=?5, grace_period_seconds=?6, lock_check_enabled=?7, schedule_enabled=?8, schedule_times_per_day=?9, schedule_time_1=?10, schedule_time_2=?11, schedule_time_3=?12, schedule_time_4=?13 WHERE id=?14",
        params![
            settings.language,
            settings.theme,
            settings.telemetry_enabled as i32,
            settings.first_run as i32,
            settings.autostart as i32,
            settings.grace_period_seconds,
            settings.lock_check_enabled as i32,
            settings.schedule_enabled as i32,
            settings.schedule_times_per_day,
            settings.schedule_time_1,
            settings.schedule_time_2,
            settings.schedule_time_3,
            settings.schedule_time_4,
            settings.id
        ],
    )?;
    Ok(())
}

pub fn clear_logs() -> SqliteResult<()> {
    let db = get_db();
    let conn = db.lock().unwrap();
    conn.execute("DELETE FROM action_logs", [])?;
    Ok(())
}

// ---------------------------------------------------------------------------
// cleanup_actions helpers
// ---------------------------------------------------------------------------

pub fn insert_cleanup_action(action: &CleanupAction) -> SqliteResult<i64> {
    let db = get_db();
    let conn = db.lock().unwrap();
    conn.execute(
        "INSERT INTO cleanup_actions (timestamp, path, prev_path, dest, action, status, undoable)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            action.timestamp.to_rfc3339(),
            action.path,
            action.prev_path,
            action.dest,
            action.action,
            action.status,
            action.undoable as i32,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn get_cleanup_logs(limit: i64) -> SqliteResult<Vec<CleanupAction>> {
    let db = get_db();
    let conn = db.lock().unwrap();
    let mut stmt = conn.prepare(
        "SELECT id, timestamp, path, prev_path, dest, action, status, undoable
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
            })
        })?
        .collect::<SqliteResult<Vec<_>>>()?;
    Ok(logs)
}

pub fn mark_cleanup_undone(id: i64) -> SqliteResult<()> {
    let db = get_db();
    let conn = db.lock().unwrap();
    conn.execute(
        "UPDATE cleanup_actions SET undoable = 0, status = 'undone' WHERE id = ?1",
        params![id],
    )?;
    Ok(())
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
pub struct RootStat {
    pub path: String,
    pub files: i64,
    pub bytes: i64,
}

#[cfg(test)]
fn create_file_inventory_table(conn: &Connection) -> SqliteResult<()> {
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
            "INSERT OR REPLACE INTO file_inventory (root_path, path, size, mtime, category, scanned_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )?;
        for row in rows {
            stmt.execute(params![root, row.path, row.size, row.mtime, row.category, scanned_at])?;
        }
    }
    tx.commit()
}

fn replace_inventory_on(conn: &mut Connection, root: &str, rows: &[InventoryRow]) -> SqliteResult<()> {
    conn.execute("DELETE FROM file_inventory WHERE root_path = ?1", params![root])?;
    for chunk in rows.chunks(INVENTORY_BATCH_SIZE) {
        append_inventory_batch_on(conn, root, chunk)?;
    }
    Ok(())
}

#[cfg(test)]
fn clear_inventory_on(conn: &Connection, root: &str) -> SqliteResult<()> {
    conn.execute("DELETE FROM file_inventory WHERE root_path = ?1", params![root]).map(|_| ())
}

fn get_inventory_stats_on(conn: &Connection) -> SqliteResult<InventoryStats> {
    conn.query_row(
        "SELECT COUNT(*), COALESCE(SUM(size), 0), MAX(scanned_at) FROM file_inventory",
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
        "SELECT root_path, COUNT(*), COALESCE(SUM(size), 0) FROM file_inventory GROUP BY root_path ORDER BY 3 DESC",
    )?;
    let rows = stmt
        .query_map([], |row| {
            Ok(RootStat {
                path: row.get(0)?,
                files: row.get(1)?,
                bytes: row.get(2)?,
            })
        })?
        .collect::<SqliteResult<Vec<_>>>()?;
    Ok(rows)
}

// Public wrappers that use the global DB.

pub fn replace_inventory_for_root(root: &str, rows: &[InventoryRow]) -> SqliteResult<()> {
    let db = get_db();
    let mut conn = db.lock().unwrap();
    replace_inventory_on(&mut conn, root, rows)
}

pub fn append_inventory_batch(root: &str, rows: &[InventoryRow]) -> SqliteResult<()> {
    let db = get_db();
    let mut conn = db.lock().unwrap();
    append_inventory_batch_on(&mut conn, root, rows)
}

pub fn clear_inventory_for_root(root: &str) -> SqliteResult<()> {
    let db = get_db();
    let conn = db.lock().unwrap();
    conn.execute("DELETE FROM file_inventory WHERE root_path = ?1", params![root])
        .map(|_| ())
}

pub fn get_inventory_stats() -> SqliteResult<InventoryStats> {
    let db = get_db();
    let conn = db.lock().unwrap();
    get_inventory_stats_on(&conn)
}

pub fn get_largest_files(limit: i64) -> SqliteResult<Vec<LargestFile>> {
    let db = get_db();
    let conn = db.lock().unwrap();
    get_largest_files_on(&conn, limit)
}

pub fn get_category_distribution() -> SqliteResult<Vec<CategoryStat>> {
    let db = get_db();
    let conn = db.lock().unwrap();
    get_category_distribution_on(&conn)
}

pub fn get_root_summaries() -> SqliteResult<Vec<RootStat>> {
    let db = get_db();
    let conn = db.lock().unwrap();
    get_root_summaries_on(&conn)
}

// ---------------------------------------------------------------------------
// hash_cache helpers
// ---------------------------------------------------------------------------

pub fn get_cached_hash(path: &str, size: i64, mtime: i64) -> SqliteResult<Option<(String, bool)>> {
    let db = get_db();
    let conn = db.lock().unwrap();
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
    let db = get_db();
    let conn = db.lock().unwrap();
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
    let db = get_db();
    let conn = db.lock().unwrap();
    conn.query_row("SELECT COUNT(*) FROM file_inventory", [], |row| row.get(0))
}

/// Return (size, path, mtime) for each size group that has >1 file.
/// Groups are returned unsorted (caller may sort by size desc).
pub type SizeGroup = (i64, Vec<(String, i64)>);
pub fn get_inventory_size_groups() -> SqliteResult<Vec<SizeGroup>> {
    let db = get_db();
    let conn = db.lock().unwrap();
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
    let db = get_db();
    let conn = db.lock().unwrap();
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
    let db = get_db();
    let conn = db.lock().unwrap();
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
    let db = get_db();
    let conn = db.lock().unwrap();
    let now = now_epoch();
    conn.execute(
        "INSERT OR IGNORE INTO dismissed_suggestions (path, dismissed_at) VALUES (?1, ?2)",
        params![path, now],
    )?;
    Ok(())
}

pub fn clear_dismissed_suggestion(path: &str) -> SqliteResult<()> {
    let db = get_db();
    let conn = db.lock().unwrap();
    conn.execute("DELETE FROM dismissed_suggestions WHERE path = ?1", params![path])?;
    Ok(())
}

pub fn get_dismissed_suggestions() -> SqliteResult<std::collections::HashSet<String>> {
    let db = get_db();
    let conn = db.lock().unwrap();
    let mut stmt = conn.prepare("SELECT path FROM dismissed_suggestions")?;
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<SqliteResult<Vec<_>>>()?;
    Ok(rows.into_iter().collect())
}

pub fn get_unclassified_files(limit: i64) -> SqliteResult<Vec<InventoryRow>> {
    let db = get_db();
    let conn = db.lock().unwrap();
    let mut stmt = conn.prepare(
        "SELECT path, size, mtime, category FROM file_inventory WHERE category = 'Other' ORDER BY size DESC LIMIT ?1",
    )?;
    let rows = stmt
        .query_map(params![limit], |row| {
            Ok(InventoryRow {
                path: row.get(0)?,
                size: row.get(1)?,
                mtime: row.get(2)?,
                category: row.get(3)?,
            })
        })?
        .collect::<SqliteResult<Vec<_>>>()?;
    Ok(rows)
}

pub fn update_inventory_category(path: &str, category: &str) -> SqliteResult<()> {
    let db = get_db();
    let conn = db.lock().unwrap();
    conn.execute(
        "UPDATE file_inventory SET category = ?1 WHERE path = ?2",
        params![category, path],
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Test infrastructure — shared across all test modules
// ---------------------------------------------------------------------------

/// Lock that serialises all DB-touching tests so they don't interfere via the
/// global OnceCell DB.  Acquired at the top of every test that touches the DB.
#[cfg(test)]
pub static TEST_DB_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Initialise the global DB if it has not been set yet.  Idempotent so
/// multiple test modules can call it without panicking.
#[cfg(test)]
pub fn init_test_db() {
    if DB.get().is_none() {
        let dir = std::env::temp_dir().join(format!("mouzi-db-{}", std::process::id()));
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
        let _guard = TEST_DB_LOCK.lock().unwrap();
        init_test_db();
        let db = get_db();
        let conn = db.lock().unwrap();
        conn.execute(
            "INSERT INTO action_logs (timestamp, source_path, destination_path, action, file_name, file_type, undone) VALUES ('not-a-date', '/corrupt-test', '/dst', 'move', 'f.txt', 'text', 0)",
            [],
        ).unwrap();
        drop(conn);
        let logs = get_recent_logs(100).unwrap();
        let corrupt = logs.iter().find(|l| l.source_path == "/corrupt-test");
        assert!(corrupt.is_some(), "corrupt row should be returned with epoch fallback");
        assert_eq!(corrupt.unwrap().timestamp, Utc.timestamp_opt(0, 0).unwrap());
    }

    #[test]
    fn cleanup_logs_survives_corrupt_timestamp() {
        let _guard = TEST_DB_LOCK.lock().unwrap();
        init_test_db();
        let db = get_db();
        let conn = db.lock().unwrap();
        conn.execute(
            "INSERT INTO cleanup_actions (timestamp, path, prev_path, dest, action, status, undoable) VALUES ('not-a-date', '/corrupt-cleanup', '/prev', '/dest', 'delete', 'ok', 0)",
            [],
        ).unwrap();
        drop(conn);
        let logs = get_cleanup_logs(100).unwrap();
        let corrupt = logs.iter().find(|l| l.path == "/corrupt-cleanup");
        assert!(corrupt.is_some(), "corrupt cleanup row should be returned with epoch fallback");
        assert_eq!(corrupt.unwrap().timestamp, Utc.timestamp_opt(0, 0).unwrap());
    }
}
