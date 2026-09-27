use crate::db;
use crate::safe_fs;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Files larger than this are hashed on a 64 KB sample first (if the 64 KB
/// samples of two same-size files differ there is no need to hash the full
/// file).  Files at or below this size are always hashed in full.
const FULL_HASH_MAX_BYTES: u64 = 100 * 1024 * 1024; // 100 MB
const SAMPLE_BYTES: u64 = 64 * 1024; // 64 KB

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DuplicateFile {
    pub path: String,
    pub size: i64,
    pub mtime: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DuplicateGroup {
    pub hash: String,
    /// Total reclaimable bytes if one copy of this group is kept.
    pub files: Vec<DuplicateFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanupRequest {
    pub kind: String,
    pub path: String,
    #[serde(rename = "keepPath")]
    pub keep_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanupOutcome {
    pub path: String,
    pub status: String,
    pub message: Option<String>,
}

/// A fourth status, and the reason it is a named constant: this exact string is
/// a contract, not an internal detail. `src/utils/cleanup.ts` declares the
/// result status as a closed union and `ResultsPanel` counts and colours on
/// each member, and `cleanup_actions.status` in the database is read back
/// verbatim by `HistoryPanel`. Change or rename the string and the frontend
/// stops compiling or silently draws the result in no bucket — the half of this
/// change that lives in TypeScript has to be made at the same time.
const PERMANENTLY_DELETED_STATUS: &str = "permanently_deleted";

const PERMANENTLY_DELETED_MESSAGE: &str = "Deleted permanently — Windows did not put this in the \
                                         Recycle Bin, so it cannot be restored";
const UNVERIFIED_MESSAGE: &str = "The Recycle Bin could not be read to confirm this one, so \
                                  whether it can be undone is unknown";

impl CleanupOutcome {
    /// Fold an observed Recycle Bin verdict into this outcome.
    ///
    /// The fourth status, `PERMANENTLY_DELETED_STATUS`, exists because
    /// "Windows shredded this and I cannot get it back" and "the move failed
    /// and nothing was destroyed" are different facts and the user needs to be
    /// able to tell them apart. Folding the first into "failed" hid the one
    /// case in this function where data is genuinely gone.
    ///
    /// `status` is still a closed union shared with `src/utils/cleanup.ts`, so
    /// that file declares this string too; the two are edited together. The
    /// `Unverified` arm below is deliberately *not* a fifth value — it is a
    /// caveat on a success, and inventing a status for "we could not check"
    /// would report uncertainty as an outcome.
    fn apply_trash_verdict(&mut self, verdict: safe_fs::TrashVerdict) {
        match verdict {
            safe_fs::TrashVerdict::InRecycleBin => {}
            safe_fs::TrashVerdict::PermanentlyDeleted => {
                self.status = PERMANENTLY_DELETED_STATUS.to_string();
                self.message = Some(PERMANENTLY_DELETED_MESSAGE.to_string());
            }
            safe_fs::TrashVerdict::Unverified => {
                // Left as ok, with the caveat spelled out. The call succeeded and
                // there is no evidence it did not; reporting a missing reading as
                // a failure would cry wolf on a cleanup that in all likelihood
                // worked. The un-recoverable case above is the one that must
                // never be counted as a success.
                self.message = Some(UNVERIFIED_MESSAGE.to_string());
            }
        }
    }
}

/// Re-export the DB LargestFile type so callers don't need to import db.
pub use db::LargestFile as CleanupFile;

// ---------------------------------------------------------------------------
// Inventory guard
// ---------------------------------------------------------------------------

pub fn has_inventory() -> bool {
    db::count_inventory_files().map(|c| c > 0).unwrap_or(false)
}

// ---------------------------------------------------------------------------
// Duplicate detection
// ---------------------------------------------------------------------------

/// Compute a full-content blake3 hash of the file at `path`.
/// Returns a hex string or an error message.
fn hash_full(path: &Path) -> Result<String, String> {
    let mut file = fs::File::open(path).map_err(|e| {
        format!("Failed to open {}: {}", path.to_string_lossy(), e)
    })?;
    let mut hasher = blake3::Hasher::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf).map_err(|e| {
            format!("Failed to read {}: {}", path.to_string_lossy(), e)
        })?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

/// Compute a hash of the first `SAMPLE_BYTES` of the file.
/// Returns a hex string or an error message.
fn hash_sample(path: &Path) -> Result<String, String> {
    let mut file = fs::File::open(path).map_err(|e| {
        format!("Failed to open {}: {}", path.to_string_lossy(), e)
    })?;
    let mut hasher = blake3::Hasher::new();
    let mut buf = vec![0u8; SAMPLE_BYTES as usize];
    let mut total = 0usize;
    while total < SAMPLE_BYTES as usize {
        let n = file.read(&mut buf[total..]).map_err(|e| {
            format!("Failed to read {}: {}", path.to_string_lossy(), e)
        })?;
        if n == 0 {
            break;
        }
        total += n;
    }
    hasher.update(&buf[..total]);
    Ok(hasher.finalize().to_hex().to_string())
}

/// Check the cache for a matching (path, size, mtime) entry.
/// Returns Some((hash, is_full)) on cache hit, None on miss.
fn cache_lookup(path: &str, size: i64, mtime: i64) -> Option<(String, bool)> {
    db::get_cached_hash(path, size, mtime).ok().flatten()
}

/// Store a hash in the cache.
fn cache_store(path: &str, size: i64, mtime: i64, hash: &str, is_full: bool) {
    let _ = db::set_cached_hash(path, size, mtime, hash, is_full);
}

/// Hash a file, using the cache when possible.  For small files (≤100 MB)
/// the returned hash is always a full-file hash.  For large files the
/// returned hash is a sample hash unless the caller requests a full hash.
fn hash_path(path: &str, size: i64, mtime: i64, want_full: bool) -> Result<String, String> {
    if let Some((hash, is_full)) = cache_lookup(path, size, mtime) {
        if is_full || !want_full {
            return Ok(hash);
        }
        // Cached sample only but we need full — fall through to compute.
    }
    let path_obj = Path::new(path);
    if size as u64 <= FULL_HASH_MAX_BYTES || want_full {
        let hash = hash_full(path_obj)?;
        cache_store(path, size, mtime, &hash, true);
        Ok(hash)
    } else {
        let hash = hash_sample(path_obj)?;
        cache_store(path, size, mtime, &hash, false);
        Ok(hash)
    }
}

/// Find duplicate files across the scanned inventory.
///
/// 1.  Query `file_inventory` for size groups with >1 file.
/// 2.  Hash each file in every size group (cache-aware).
/// 3.  For large files (≥100 MB): first gather sample (64 KB) hashes; if
///     two files share the same sample hash, a full hash is computed to
///     confirm.
/// 4.  Return groups of files sharing the same (full) hash, sorted by
///     potential reclaimable bytes descending.
pub fn find_duplicates() -> Result<Vec<DuplicateGroup>, String> {
    let size_groups = db::get_inventory_size_groups().map_err(|e| e.to_string())?;
    if size_groups.is_empty() {
        return Ok(Vec::new());
    }

    let mut all_groups: Vec<DuplicateGroup> = Vec::new();

    for (size, paths_and_mtimes) in size_groups {
        let is_large = (size as u64) > FULL_HASH_MAX_BYTES;

        if !is_large {
            // Small files: hash everything in full, group by hash.
            let mut by_hash: HashMap<String, Vec<DuplicateFile>> = HashMap::new();
            for (path, mtime) in &paths_and_mtimes {
                if let Ok(hash) = hash_path(path, size, *mtime, true) {
                    by_hash.entry(hash).or_default().push(DuplicateFile {
                        path: path.clone(),
                        size,
                        mtime: *mtime,
                    });
                }
            }
            for (hash, files) in by_hash {
                if files.len() > 1 {
                    all_groups.push(DuplicateGroup { hash, files });
                }
            }
        } else {
            // Large files: phase 1 — sample hashes only.
            let mut sample_map: HashMap<String, Vec<(String, i64)>> = HashMap::new();
            for (path, mtime) in &paths_and_mtimes {
                if let Ok(sample) = hash_path(path, size, *mtime, false) {
                    sample_map.entry(sample).or_default().push((path.clone(), *mtime));
                }
            }
            // Phase 2 — for sample groups with >1 file, compute full hash.
            for (_sample, candidates) in sample_map {
                if candidates.len() < 2 {
                    continue;
                }
                let mut full_map: HashMap<String, Vec<DuplicateFile>> = HashMap::new();
                for (path, mtime) in &candidates {
                    if let Ok(full) = hash_path(path, size, *mtime, true) {
                        full_map.entry(full).or_default().push(DuplicateFile {
                            path: path.clone(),
                            size,
                            mtime: *mtime,
                        });
                    }
                }
                for (hash, files) in full_map {
                    if files.len() > 1 {
                        all_groups.push(DuplicateGroup { hash, files });
                    }
                }
            }
        }
    }

    // Sort groups by reclaimable bytes descending.
    all_groups.sort_by(|a, b| {
        let reclaim_a = a.files[0].size * (a.files.len() as i64 - 1);
        let reclaim_b = b.files[0].size * (b.files.len() as i64 - 1);
        reclaim_b.cmp(&reclaim_a)
    });

    Ok(all_groups)
}

// ---------------------------------------------------------------------------
// Large / stale file finders
// ---------------------------------------------------------------------------

pub fn find_large_files(min_bytes: i64) -> Result<Vec<CleanupFile>, String> {
    db::get_large_files_from_inventory(min_bytes).map_err(|e| e.to_string())
}

pub fn find_stale_files(days: i64) -> Result<Vec<CleanupFile>, String> {
    let days = days.max(1);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let cutoff = now - days * 86_400;
    db::get_stale_files_from_inventory(cutoff).map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// Empty directory finder
// ---------------------------------------------------------------------------

/// Recursively walk `dir` and return `(has_files_below, deepest_empty_leaf_dirs)`.
/// A directory is a "deepest empty leaf" if it contains no files at any depth
/// and has no subdirectories (or all subdirectories are themselves empty
/// leaves).  Only the deepest empty dirs are returned so that deleting them
/// does not cascade duplicates.
fn walk_empty_dirs(dir: &Path) -> (bool, Vec<String>) {
    let mut has_files = false;
    let mut leaves = Vec::new();
    let mut has_subdirs = false;

    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return (false, Vec::new()),
    };

    for entry in entries.flatten() {
        let ft = match entry.file_type() {
            Ok(t) => t,
            Err(_) => continue,
        };
        // Skip symlinks (safety / cycle guard).
        if ft.is_symlink() {
            continue;
        }
        if ft.is_dir() {
            has_subdirs = true;
            let (sub_has_files, mut sub_leaves) = walk_empty_dirs(&entry.path());
            has_files |= sub_has_files;
            leaves.append(&mut sub_leaves);
        } else if ft.is_file() {
            has_files = true;
        }
    }

    // If we have no files below and no subdirs, this dir is a deepest empty leaf.
    if !has_files && !has_subdirs {
        leaves.push(dir.to_string_lossy().into_owned());
    }
    // If we have no files below but have subdirs, those subdirs are already in
    // `leaves` — we do NOT push this dir (it is not deepest).
    (has_files, leaves)
}

pub fn find_empty_dirs() -> Result<Vec<String>, String> {
    let folders = db::get_watched_folders().map_err(|e| e.to_string())?;
    let mut result = Vec::new();
    for folder in folders {
        if !folder.enabled || db::is_folder_paused_mode(&folder.mode) {
            continue;
        }
        let (_, mut leaves) = walk_empty_dirs(Path::new(&folder.path));
        result.append(&mut leaves);
    }
    result.sort();
    result.dedup();
    Ok(result)
}

// ---------------------------------------------------------------------------
// Cleanup execution
// ---------------------------------------------------------------------------

/// The action kinds whose payload ends up in the Recycle Bin, and so whose
/// outcome the bin snapshot gets a vote on. Must list every kind `execute_one`
/// trashes: a kind missing here gets no snapshot and every file in its batch is
/// reported unverified.
fn is_trash_kind(kind: &str) -> bool {
    matches!(kind, "trash_duplicate" | "trash_large" | "trash_stale")
}

/// One action's result, plus whether the Recycle Bin has to arbitrate it.
struct ExecutedOne {
    outcome: CleanupOutcome,
    /// True only when `trash::delete` returned `Ok`. Its unit return value says
    /// nothing about whether Windows used the bin, so these are exactly the
    /// items a before/after snapshot has to rule on.
    trash_call_succeeded: bool,
}

fn execute_one(action: &CleanupRequest) -> ExecutedOne {
    let path = &action.path;
    let path_obj = Path::new(path);

    match action.kind.as_str() {
        "trash_duplicate" | "trash_large" | "trash_stale" => {
            // Guard: if this file is marked as the keeper, skip it.
            if let Some(ref keep) = action.keep_path {
                if keep == path {
                    return ExecutedOne {
                        outcome: CleanupOutcome {
                            path: path.clone(),
                            status: "skipped".to_string(),
                            message: Some("Kept file".to_string()),
                        },
                        trash_call_succeeded: false,
                    };
                }
            }
            if !path_obj.exists() {
                return ExecutedOne {
                    outcome: CleanupOutcome {
                        path: path.clone(),
                        status: "skipped".to_string(),
                        message: Some("File not found".to_string()),
                    },
                    trash_call_succeeded: false,
                };
            }
            return match safe_fs::delete_to_trash(path_obj) {
                Ok(()) => ExecutedOne {
                    outcome: CleanupOutcome {
                        path: path.clone(),
                        status: "ok".to_string(),
                        message: None,
                    },
                    trash_call_succeeded: true,
                },
                Err(e) => ExecutedOne {
                    outcome: CleanupOutcome {
                        path: path.clone(),
                        status: "failed".to_string(),
                        message: Some(e),
                    },
                    trash_call_succeeded: false,
                },
            };
        }
        "remove_empty_dir" => {
            if !path_obj.exists() {
                return ExecutedOne {
                    outcome: CleanupOutcome {
                        path: path.clone(),
                        status: "skipped".to_string(),
                        message: Some("Directory not found".to_string()),
                    },
                    trash_call_succeeded: false,
                };
            }
            // Recheck: the dir must still be empty (no files at any depth).
            let (has_files, _) = walk_empty_dirs(path_obj);
            if has_files {
                return ExecutedOne {
                    outcome: CleanupOutcome {
                        path: path.clone(),
                        status: "skipped".to_string(),
                        message: Some("Directory not empty".to_string()),
                    },
                    trash_call_succeeded: false,
                };
            }
            return match fs::remove_dir(path_obj) {
                Ok(()) => ExecutedOne {
                    outcome: CleanupOutcome {
                        path: path.clone(),
                        status: "ok".to_string(),
                        message: None,
                    },
                    trash_call_succeeded: false,
                },
                Err(e) => ExecutedOne {
                    outcome: CleanupOutcome {
                        path: path.clone(),
                        status: "failed".to_string(),
                        message: Some(format!("Failed to remove directory: {}", e)),
                    },
                    trash_call_succeeded: false,
                },
            };
        }
        other => ExecutedOne {
            outcome: CleanupOutcome {
                path: path.clone(),
                status: "failed".to_string(),
                message: Some(format!("Unknown cleanup action kind: {}", other)),
            },
            trash_call_succeeded: false,
        },
    }
}

/// Execute a batch of cleanup actions.  Each action is processed
/// individually; failures do not abort the batch.
///
/// For every action an audit row is inserted into `cleanup_actions`.
/// Empty-dir removals are marked as not undoable (trash deletions are not
/// app-undoable either — the Recycle Bin provides native restore).
pub fn execute_cleanup(actions: &[CleanupRequest]) -> Vec<CleanupOutcome> {
    let roots: Vec<PathBuf> = db::get_watched_folders()
        .unwrap_or_default()
        .into_iter()
        .map(|folder| PathBuf::from(folder.path))
        .collect();

    // One reading either side of the whole batch, never one per file: listing
    // the bin walks every trashed item on the machine, so a per-file reading
    // makes a large cleanup quadratic. A batch with no trash action in it never
    // touches the bin at all.
    let before = if actions.iter().any(|a| is_trash_kind(&a.kind)) {
        Some(safe_fs::trash_snapshot())
    } else {
        None
    };

    let mut results = Vec::with_capacity(actions.len());
    // Indices into `results` whose `trash::delete` call succeeded, so the bin
    // still has to say whether the file is recoverable.
    let mut awaiting_verdict: Vec<usize> = Vec::new();
    for (index, action) in actions.iter().enumerate() {
        // Nothing outside a watched folder may be destroyed. These paths arrive
        // from the frontend, and app-defined commands are callable from every
        // window, so without this check the command can name any path on the
        // machine. An empty or unreadable folder list authorises nothing.
        let executed = if safe_fs::is_within_any_root(Path::new(&action.path), &roots) {
            execute_one(action)
        } else {
            ExecutedOne {
                outcome: CleanupOutcome {
                    path: action.path.clone(),
                    status: "skipped".to_string(),
                    message: Some("Outside every watched folder".to_string()),
                },
                trash_call_succeeded: false,
            }
        };
        if executed.trash_call_succeeded {
            awaiting_verdict.push(index);
        }
        results.push(executed.outcome);
    }

    match before {
        Some(before) if !awaiting_verdict.is_empty() => {
            let after = safe_fs::trash_snapshot();
            let attempted: Vec<PathBuf> = awaiting_verdict
                .iter()
                .map(|&i| PathBuf::from(&actions[i].path))
                .collect();
            for (position, verdict) in awaiting_verdict
                .iter()
                .zip(safe_fs::classify_trash_outcomes(&before, &after, &attempted))
            {
                results[*position].apply_trash_verdict(verdict);
            }
        }
        _ => {}
    }

    // The audit rows are written only once the verdict is known, so a file that
    // Windows deleted permanently is not recorded in the history as a success.
    let mut outcomes = Vec::with_capacity(results.len());
    for (action, outcome) in actions.iter().zip(results) {
        let undoable = false;
        let _ = db::insert_cleanup_action(&db::CleanupAction {
            id: None,
            timestamp: Utc::now(),
            path: action.path.clone(),
            prev_path: None,
            dest: None,
            action: action.kind.clone(),
            status: outcome.status.clone(),
            undoable,
        });
        outcomes.push(outcome);
    }
    outcomes
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn temp_root(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mouzi-cleanup-{}-{}", name, std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn insert_inventory(root: &str, rows: &[db::InventoryRow]) {
        db::append_inventory_batch(root, rows).unwrap();
    }

    fn blake3_hex(data: &[u8]) -> String {
        let mut hasher = blake3::Hasher::new();
        hasher.update(data);
        hasher.finalize().to_hex().to_string()
    }

    // ── Duplicate detection ─────────────────────────────────────

    #[test]
    fn duplicates_grouped_by_content() {
        let _guard = db::TEST_DB_LOCK.lock().unwrap();
        db::init_test_db();

        let root = temp_root("dup_grouped");
        let a = root.join("a.txt");
        let b = root.join("b.txt");
        let c = root.join("c.txt");
        fs::write(&a, "hello").unwrap();
        fs::write(&b, "hello").unwrap();
        fs::write(&c, "world").unwrap();

        let meta_a = a.metadata().unwrap();
        let meta_b = b.metadata().unwrap();
        let meta_c = c.metadata().unwrap();

        let root_str = root.to_string_lossy().into_owned();
        insert_inventory(
            &root_str,
            &[
                db::InventoryRow {
                    path: a.to_string_lossy().into_owned(),
                    size: meta_a.len() as i64,
                    mtime: meta_a.modified().unwrap().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64,
                    category: "Other".into(),
                },
                db::InventoryRow {
                    path: b.to_string_lossy().into_owned(),
                    size: meta_b.len() as i64,
                    mtime: meta_b.modified().unwrap().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64,
                    category: "Other".into(),
                },
                db::InventoryRow {
                    path: c.to_string_lossy().into_owned(),
                    size: meta_c.len() as i64,
                    mtime: meta_c.modified().unwrap().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64,
                    category: "Other".into(),
                },
            ],
        );

        let groups = find_duplicates().unwrap();
        assert_eq!(groups.len(), 1, "expected 1 duplicate group");
        assert_eq!(groups[0].files.len(), 2, "expected 2 files in group");
        let paths: Vec<&str> = groups[0].files.iter().map(|f| {
            Path::new(&f.path).file_name().unwrap().to_str().unwrap()
        }).collect();
        assert!(paths.contains(&"a.txt"));
        assert!(paths.contains(&"b.txt"));

        db::clear_inventory_for_root(&root_str).unwrap();
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn different_sizes_not_grouped() {
        let _guard = db::TEST_DB_LOCK.lock().unwrap();
        db::init_test_db();

        let root = temp_root("diff_sizes");
        let small = root.join("small.txt");
        let large = root.join("large.txt");
        fs::write(&small, "hello").unwrap();
        let large_data = vec![b'x'; 500];
        fs::write(&large, &large_data).unwrap();

        let meta_s = small.metadata().unwrap();
        let meta_l = large.metadata().unwrap();

        let root_str = root.to_string_lossy().into_owned();
        insert_inventory(
            &root_str,
            &[
                db::InventoryRow {
                    path: small.to_string_lossy().into_owned(),
                    size: meta_s.len() as i64,
                    mtime: 1,
                    category: "Other".into(),
                },
                db::InventoryRow {
                    path: large.to_string_lossy().into_owned(),
                    size: meta_l.len() as i64,
                    mtime: 1,
                    category: "Other".into(),
                },
            ],
        );

        let groups = find_duplicates().unwrap();
        assert_eq!(groups.len(), 0, "different sizes should not form groups");

        db::clear_inventory_for_root(&root_str).unwrap();
        fs::remove_dir_all(&root).ok();
    }

    // ── Hash cache ──────────────────────────────────────────────

    #[test]
    fn hash_cache_reused_when_unchanged() {
        let _guard = db::TEST_DB_LOCK.lock().unwrap();
        db::init_test_db();

        let root = temp_root("cache_reuse");
        let f1 = root.join("f1.txt");
        let f2 = root.join("f2.txt");
        fs::write(&f1, "AAAA").unwrap();
        fs::write(&f2, "BBBB").unwrap();
        let meta1 = f1.metadata().unwrap();
        let meta2 = f2.metadata().unwrap();
        let mtime1 = meta1.modified().unwrap().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64;
        let mtime2 = meta2.modified().unwrap().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64;
        let size = meta1.len() as i64;

        // Pre-populate cache for f1 with a deliberately wrong hash (f2's hash).
        // If find_duplicates reuses the cache, f1's hash will match f2's real
        // hash and the group will form.
        let f2_real_hash = blake3_hex(b"BBBB");
        let f1_path = f1.to_string_lossy().into_owned();
        let f2_path = f2.to_string_lossy().into_owned();
        db::set_cached_hash(&f1_path, size, mtime1, &f2_real_hash, true).unwrap();

        let root_str = root.to_string_lossy().into_owned();
        insert_inventory(
            &root_str,
            &[
                db::InventoryRow {
                    path: f1_path.clone(),
                    size,
                    mtime: mtime1,
                    category: "Other".into(),
                },
                db::InventoryRow {
                    path: f2_path.clone(),
                    size,
                    mtime: mtime2,
                    category: "Other".into(),
                },
            ],
        );

        let groups = find_duplicates().unwrap();
        // f1's cached hash matches f2's real hash → they appear as duplicates.
        assert_eq!(groups.len(), 1, "expected group from cache reuse");
        assert_eq!(groups[0].hash, f2_real_hash);

        db::clear_inventory_for_root(&root_str).unwrap();
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn hash_cache_invalidated_on_mtime_change() {
        let _guard = db::TEST_DB_LOCK.lock().unwrap();
        db::init_test_db();

        let root = temp_root("cache_inval");
        let f1 = root.join("f1.txt");
        let f2 = root.join("f2.txt");
        fs::write(&f1, "AAAA").unwrap();
        fs::write(&f2, "BBBB").unwrap();
        let meta1 = f1.metadata().unwrap();
        let meta2 = f2.metadata().unwrap();
        let mtime1 = meta1.modified().unwrap().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64;
        let mtime2 = meta2.modified().unwrap().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64;
        let size = meta1.len() as i64;

        let f1_path = f1.to_string_lossy().into_owned();
        let f2_path = f2.to_string_lossy().into_owned();
        let root_str = root.to_string_lossy().into_owned();

        // Insert inventory with a fake mtime for f1 (different from the file's
        // real mtime).  The cache key uses the inventory mtime, so this will
        // cause a cache miss.
        insert_inventory(
            &root_str,
            &[
                db::InventoryRow {
                    path: f1_path.clone(),
                    size,
                    mtime: mtime1 + 9999, // altered mtime
                    category: "Other".into(),
                },
                db::InventoryRow {
                    path: f2_path.clone(),
                    size,
                    mtime: mtime2,
                    category: "Other".into(),
                },
            ],
        );

        // Pre-populate cache for f1's original (mtime1) key — won't match
        // because the inventory now has mtime1+9999.
        db::set_cached_hash(&f1_path, size, mtime1, &blake3_hex(b"BBBB"), true).unwrap();

        let groups = find_duplicates().unwrap();
        // The cache miss means f1 gets freshly hashed → blake3("AAAA") which
        // differs from f2's blake3("BBBB") → no group.
        assert_eq!(groups.len(), 0, "mtime change should invalidate cache");

        db::clear_inventory_for_root(&root_str).unwrap();
        fs::remove_dir_all(&root).ok();
    }

    // ── Empty directory detection ───────────────────────────────

    #[test]
    fn empty_dirs_deepest_only() {
        // Structure:
        //   root/
        //     a/          ← empty leaf → reported
        //     b/
        //       c/        ← empty leaf → reported
        //       file.txt  ← file → b is NOT empty
        //     d/          ← has file.txt → NOT empty
        //       file.txt
        //     e/          ← empty leaf → reported
        let root = temp_root("empty_deepest");
        fs::create_dir_all(root.join("a")).unwrap();
        fs::create_dir_all(root.join("b").join("c")).unwrap();
        fs::write(root.join("b").join("file.txt"), "hello").unwrap();
        fs::create_dir_all(root.join("d")).unwrap();
        fs::write(root.join("d").join("file.txt"), "world").unwrap();
        fs::create_dir_all(root.join("e")).unwrap();

        let (has_files, leaves) = walk_empty_dirs(&root);
        assert!(has_files, "root has files below");
        let mut leaf_names: Vec<String> = leaves
            .iter()
            .map(|p| Path::new(p).file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        leaf_names.sort();
        assert_eq!(leaf_names, vec!["a", "c", "e"], "deepest empty dirs: a, c, e");

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn empty_dirs_entirely_empty_root() {
        let root = temp_root("empty_root");
        fs::create_dir_all(root.join("sub1")).unwrap();
        fs::create_dir_all(root.join("sub1").join("sub2")).unwrap();

        // The entire tree has no files.  Deepest leaves: sub2.
        let (has_files, leaves) = walk_empty_dirs(&root);
        assert!(!has_files);
        let leaves: Vec<String> = leaves
            .iter()
            .map(|p| {
                let rel = Path::new(p)
                    .strip_prefix(&root)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned();
                rel
            })
            .collect();
        assert_eq!(leaves, vec!["sub1\\sub2"], "deepest empty dir: sub2");

        fs::remove_dir_all(&root).ok();
    }

    // ── Large file finder (integration-light) ───────────────────

    #[test]
    fn find_large_files_returns_only_over_threshold() {
        let _guard = db::TEST_DB_LOCK.lock().unwrap();
        db::init_test_db();

        let root = temp_root("large");
        let small = root.join("small.txt");
        let big = root.join("big.txt");
        fs::write(&small, "small").unwrap();
        fs::write(&big, &vec![b'x'; 200]).unwrap();

        let root_str = root.to_string_lossy().into_owned();
        insert_inventory(
            &root_str,
            &[
                db::InventoryRow {
                    path: small.to_string_lossy().into_owned(),
                    size: 5,
                    mtime: 1,
                    category: "Other".into(),
                },
                db::InventoryRow {
                    path: big.to_string_lossy().into_owned(),
                    size: 200,
                    mtime: 1,
                    category: "Other".into(),
                },
            ],
        );

        let large = find_large_files(100).unwrap();
        assert_eq!(large.len(), 1, "only the 200-byte file should appear");
        assert!(large[0].path.contains("big.txt"));

        db::clear_inventory_for_root(&root_str).unwrap();
        fs::remove_dir_all(&root).ok();
    }

    // ── Stale file finder ───────────────────────────────────────

    #[test]
    fn find_stale_files_returns_old_files() {
        let _guard = db::TEST_DB_LOCK.lock().unwrap();
        db::init_test_db();

        let root = temp_root("stale");
        let old = root.join("old.txt");
        let fresh = root.join("fresh.txt");
        fs::write(&old, "old").unwrap();
        fs::write(&fresh, "fresh").unwrap();

        let root_str = root.to_string_lossy().into_owned();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        insert_inventory(
            &root_str,
            &[
                db::InventoryRow {
                    path: old.to_string_lossy().into_owned(),
                    size: 3,
                    mtime: now - 200_000, // ~2.3 days ago
                    category: "Other".into(),
                },
                db::InventoryRow {
                    path: fresh.to_string_lossy().into_owned(),
                    size: 5,
                    mtime: now - 10_000, // ~2.8 hours ago
                    category: "Other".into(),
                },
            ],
        );

        // Find files older than 1 day (86400 seconds).
        let stale = find_stale_files(1).unwrap();
        assert_eq!(stale.len(), 1, "only the old file should be stale");
        assert!(stale[0].path.contains("old.txt"));

        db::clear_inventory_for_root(&root_str).unwrap();
        fs::remove_dir_all(&root).ok();
    }

    // ── Recycle Bin verdict → outcome mapping ──────────────────
    //
    // Pure: no DB, no filesystem, so these cannot interfere with the shared
    // test database the other modules in this crate share.

    fn outcome_with_verdict(verdict: safe_fs::TrashVerdict) -> CleanupOutcome {
        let mut outcome = CleanupOutcome {
            path: "C:/Users/Me/victim.txt".to_string(),
            status: "ok".to_string(),
            message: None,
        };
        outcome.apply_trash_verdict(verdict);
        outcome
    }

    /// The invariant the frontend depends on: a status outside this set is
    /// counted in no `ResultsPanel` bucket and, read back out of
    /// `cleanup_actions`, mislabelled in `HistoryPanel`. The fourth member is
    /// the constant, not a second literal, so the guard cannot be widened here
    /// while the TypeScript union is left behind.
    fn assert_status_is_in_the_frontend_vocabulary(outcome: &CleanupOutcome) {
        assert!(
            matches!(
                outcome.status.as_str(),
                "ok" | "failed" | "skipped" | PERMANENTLY_DELETED_STATUS
            ),
            "status {:?} is outside the union declared in src/utils/cleanup.ts",
            outcome.status
        );
    }

    #[test]
    fn a_file_found_in_the_bin_stays_a_plain_success() {
        let outcome = outcome_with_verdict(safe_fs::TrashVerdict::InRecycleBin);
        assert_status_is_in_the_frontend_vocabulary(&outcome);
        assert_eq!(outcome.status, "ok");
        assert_eq!(outcome.message, None);
    }

    /// The defect: this file is gone forever, so it must not be counted as a
    /// success anywhere the frontend reads status — and it must not be
    /// indistinguishable from a move that failed and destroyed nothing.
    #[test]
    fn a_permanently_deleted_file_gets_its_own_status() {
        let outcome = outcome_with_verdict(safe_fs::TrashVerdict::PermanentlyDeleted);
        assert_status_is_in_the_frontend_vocabulary(&outcome);
        // Spelled as a literal on purpose: this is the cross-language contract
        // with the union in src/utils/cleanup.ts, so renaming the string has to
        // break this test rather than ride along on the constant.
        assert_eq!(outcome.status, "permanently_deleted");
        assert_ne!(outcome.status, "ok");
        assert_ne!(outcome.status, "failed");
        assert_eq!(outcome.message.as_deref(), Some(PERMANENTLY_DELETED_MESSAGE));
    }

    #[test]
    fn an_unverified_file_keeps_its_success_but_carries_the_caveat() {
        let outcome = outcome_with_verdict(safe_fs::TrashVerdict::Unverified);
        assert_status_is_in_the_frontend_vocabulary(&outcome);
        assert_eq!(outcome.status, "ok");
        assert_eq!(outcome.message.as_deref(), Some(UNVERIFIED_MESSAGE));
    }

    /// A guard that accepts everything is worse than no guard. Pinned against a
    /// value the union does not declare.
    #[test]
    fn the_vocabulary_guard_still_rejects_an_unknown_status() {
        let outcome = CleanupOutcome {
            path: "C:/Users/Me/victim.txt".to_string(),
            status: "moved_to_recycle_bin".to_string(),
            message: None,
        };
        let verdict = std::panic::catch_unwind(|| assert_status_is_in_the_frontend_vocabulary(&outcome));
        assert!(
            verdict.is_err(),
            "a status the TypeScript union does not declare must be rejected"
        );
    }

    /// The spelling is agreed with the frontend agent; `PERMANENTLY_DELETED_STATUS`
    /// and the union in `src/utils/cleanup.ts` are two halves of one contract and
    /// a typo on either side compiles fine and renders nothing.
    #[test]
    fn the_permanent_status_is_the_string_the_frontend_declares() {
        assert_eq!(PERMANENTLY_DELETED_STATUS, "permanently_deleted");
    }

    #[test]
    fn every_trash_kind_is_covered_by_the_snapshot_gate() {
        for kind in ["trash_duplicate", "trash_large", "trash_stale"] {
            assert!(is_trash_kind(kind), "{} must take a bin reading", kind);
        }
        assert!(!is_trash_kind("remove_empty_dir"));
    }
}