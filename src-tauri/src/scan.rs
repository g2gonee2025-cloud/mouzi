//! Recursive scanner that builds the `file_inventory` table for the dashboard.
//!
//! Zero Tauri dependencies: the Tauri command layer (commands.rs) calls
//! `scan_roots` from a spawned std::thread and forwards progress events.

use crate::db::{append_inventory_batch, clear_inventory_for_root, InventoryRow, INVENTORY_BATCH_SIZE};
use crate::ignore::{is_ignored, load_mouziignore};
use serde::Serialize;
use std::fs;
use std::path::Path;
use std::time::UNIX_EPOCH;

/// Emit a progress event roughly every this many files to avoid event storms.
const PROGRESS_EVERY: u64 = 200;

/// Do not descend deeper than this to guard against pathological trees.
const MAX_DEPTH: usize = 128;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanProgress {
    pub root: String,
    pub processed: u64,
    pub total: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanSummary {
    pub root: String,
    pub files: u64,
    pub bytes: u64,
}

#[derive(Debug, Clone)]
pub enum ScanEvent {
    Progress(ScanProgress),
    Complete(ScanSummary),
}

/// Classify a file path into one of the dashboard categories by extension.
/// Extensions are matched case-insensitively; files without an extension
/// (or with an unknown one) fall into "Other".
pub fn categorize(path: &str) -> &'static str {
    let ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_lowercase);
    match ext.as_deref() {
        Some("pdf" | "doc" | "docx" | "txt" | "md" | "xls" | "xlsx" | "ppt" | "pptx" | "csv") => {
            "Documents"
        }
        Some("jpg" | "jpeg" | "png" | "gif" | "webp" | "svg" | "heic" | "bmp") => "Images",
        Some("mp4" | "mkv" | "avi" | "mov" | "webm" | "wmv") => "Videos",
        Some("mp3" | "wav" | "flac" | "aac" | "ogg" | "m4a") => "Audio",
        Some("zip" | "rar" | "7z" | "tar" | "gz" | "bz2") => "Archives",
        Some("rs" | "ts" | "tsx" | "js" | "py" | "java" | "c" | "cpp" | "go" | "rb") => "Code",
        _ => "Other",
    }
}

/// True for symlinks on all platforms, plus NTFS junctions on Windows
/// (junctions report as directory reparse points and would otherwise
/// create cycles during recursion).
fn is_symlink(ft: &fs::FileType) -> bool {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::fs::FileTypeExt;
        ft.is_symlink() || ft.is_symlink_dir()
    }
    #[cfg(not(target_os = "windows"))]
    {
        ft.is_symlink()
    }
}

fn to_epoch_secs(meta: &fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

struct RootWalk<'a> {
    root: &'a str,
    patterns: &'a [String],
    processed: u64,
    bytes: u64,
    batch: Vec<InventoryRow>,
    progress: &'a mut dyn FnMut(ScanEvent),
}

impl<'a> RootWalk<'a> {
    fn push_file(&mut self, path: &Path, meta: &fs::Metadata) {
        self.processed += 1;
        self.bytes += meta.len();
        self.batch.push(InventoryRow {
            path: path.to_string_lossy().into_owned(),
            size: meta.len() as i64,
            mtime: to_epoch_secs(meta),
            category: categorize(&path.to_string_lossy()).to_string(),
        });
        if self.batch.len() >= INVENTORY_BATCH_SIZE {
            self.flush_batch();
        }
        if self.processed.is_multiple_of(PROGRESS_EVERY) {
            (self.progress)(ScanEvent::Progress(ScanProgress {
                root: self.root.to_string(),
                processed: self.processed,
                total: None,
            }));
        }
    }

    fn flush_batch(&mut self) {
        if self.batch.is_empty() {
            return;
        }
        if let Err(e) = append_inventory_batch(self.root, &self.batch) {
            eprintln!("[scan] failed to persist batch for {}: {}", self.root, e);
        }
        self.batch.clear();
    }

    fn walk_dir(&mut self, dir: &Path, depth: usize) {
        if depth > MAX_DEPTH {
            return;
        }
        let entries = match fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(_) => return,
        };
        for entry in entries.flatten() {
            let file_type = match entry.file_type() {
                Ok(ft) => ft,
                Err(_) => continue,
            };
            if is_symlink(&file_type) {
                continue;
            }
            let name = entry.file_name();
            if is_ignored(&name.to_string_lossy(), self.patterns) {
                continue;
            }
            if file_type.is_dir() {
                self.walk_dir(&entry.path(), depth + 1);
            } else if file_type.is_file() {
                if let Ok(meta) = entry.metadata() {
                    self.push_file(&entry.path(), &meta);
                }
            }
        }
    }
}

/// Recursively scan each root, replacing its previous inventory.
///
/// Persists in batches of `INVENTORY_BATCH_SIZE` rows (one transaction /
/// one DB lock per batch) and never holds the DB mutex during the walk.
/// Skips symlinks/junctions and `.mouziignore`-matching entries.
/// Unreadable directories are skipped; the scan is best-effort by design.
pub fn scan_roots(paths: &[String], mut progress: impl FnMut(ScanEvent)) -> Vec<ScanSummary> {
    let mut summaries = Vec::with_capacity(paths.len());
    for root in paths {
        let patterns = load_mouziignore(root);
        if let Err(e) = clear_inventory_for_root(root) {
            eprintln!("[scan] failed to clear inventory for {}: {} — skipping root", root, e);
            continue;
        }

        let mut walk = RootWalk {
            root: root.as_str(),
            patterns: &patterns,
            processed: 0,
            bytes: 0,
            batch: Vec::with_capacity(INVENTORY_BATCH_SIZE),
            progress: &mut progress,
        };
        (walk.progress)(ScanEvent::Progress(ScanProgress {
            root: root.clone(),
            processed: 0,
            total: None,
        }));
        walk.walk_dir(Path::new(root), 0);
        walk.flush_batch();

        summaries.push(ScanSummary {
            root: root.clone(),
            files: walk.processed,
            bytes: walk.bytes,
        });
        (walk.progress)(ScanEvent::Complete(ScanSummary {
            root: root.clone(),
            files: walk.processed,
            bytes: walk.bytes,
        }));
    }
    summaries
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use std::fs;
    use std::path::PathBuf;

    fn temp_root(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mouzi-scan-{}-{}", name, std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Drops the scanned tree and the inventory rows it produced, however the
    /// test ends. The inventory assertions below read global totals, so rows
    /// another module's failed test left behind would be counted as this
    /// fixture's.
    struct ScanFixture {
        root: PathBuf,
    }

    impl ScanFixture {
        fn new(name: &str) -> Self {
            ScanFixture { root: temp_root(name) }
        }
    }

    impl Drop for ScanFixture {
        fn drop(&mut self) {
            let root = self.root.to_string_lossy().into_owned();
            // Reported, not unwrapped: a test that is already panicking must
            // not turn a cleanup problem into a second panic.
            if let Err(e) = db::clear_inventory_for_root(&root) {
                eprintln!("test fixture: inventory for {root} not cleared: {e}");
            }
            if let Err(e) = fs::remove_dir_all(&self.root) {
                if e.kind() != std::io::ErrorKind::NotFound {
                    eprintln!("test fixture: {} not removed: {e}", self.root.display());
                }
            }
        }
    }

    #[test]
    fn categorize_all_categories_case_insensitive() {
        assert_eq!(categorize("report.pdf"), "Documents");
        assert_eq!(categorize("notes.DOCX"), "Documents");
        assert_eq!(categorize("/some/dir/data.csv"), "Documents");
        assert_eq!(categorize("photo.JPEG"), "Images");
        assert_eq!(categorize("pic.webp"), "Images");
        assert_eq!(categorize("movie.mp4"), "Videos");
        assert_eq!(categorize("clip.mov"), "Videos");
        assert_eq!(categorize("song.mp3"), "Audio");
        assert_eq!(categorize("sound.flac"), "Audio");
        assert_eq!(categorize("backup.zip"), "Archives");
        assert_eq!(categorize("src.tar.gz"), "Archives");
        assert_eq!(categorize("main.rs"), "Code");
        assert_eq!(categorize("component.tsx"), "Code");
        assert_eq!(categorize("app.py"), "Code");
    }

    #[test]
    fn categorize_other_and_no_extension() {
        assert_eq!(categorize("junk.xyz"), "Other");
        assert_eq!(categorize("README"), "Other");
        assert_eq!(categorize(".gitignore"), "Other");
        assert_eq!(categorize("archive"), "Other");
        assert_eq!(categorize("/path/with.noext"), "Other");
    }

    #[test]
    fn scan_roots_fixture_populates_inventory() {
        let _guard = db::serialise_test_db();
        let fixture = ScanFixture::new("fixture");
        let root = fixture.root.clone();
        fs::create_dir_all(root.join("sub").join("deep")).unwrap();
        fs::write(root.join("a.txt"), "hello").unwrap();
        fs::write(root.join("sub").join("b.jpg"), "imgdata").unwrap();
        fs::write(root.join("sub").join("deep").join("c.mp4"), "videodata").unwrap();
        fs::write(root.join("noext"), "noextdata").unwrap();
        fs::write(root.join("secret.tmp"), "secret").unwrap();
        fs::write(root.join("sub").join("deep").join("too.tmp"), "x").unwrap();
        fs::write(root.join(".mouziignore"), "*.tmp\n").unwrap();

        // Symlink cycle guard: creates a dir symlink back to root. Needs
        // privilege/developer mode on Windows; skipped with a note if not.
        #[cfg(target_os = "windows")]
        let symlink_created =
            std::os::windows::fs::symlink_dir(&root, root.join("loop")).is_ok();
        #[cfg(target_family = "unix")]
        let symlink_created = std::os::unix::fs::symlink(&root, root.join("loop")).is_ok();
        #[cfg(not(any(target_os = "windows", target_family = "unix")))]
        let symlink_created = false;

        let root_str = root.to_string_lossy().into_owned();

        // Idempotent init — shared with other test modules.
        db::init_test_db();

        // Every assertion below reads a global total, so the fixture starts
        // from a known-empty table. Several modules scan into it, and a module
        // that failed part way through leaves its own roots behind.
        db::lock_db()
            .unwrap()
            .execute("DELETE FROM file_inventory", [])
            .unwrap();

        let summaries = scan_roots(&[root_str.clone()], |_| {});
        if !symlink_created {
            eprintln!("Note: symlink cycle guard not exercised (creating symlink needs privilege)");
        }

        assert_eq!(summaries.len(), 1);
        // a.txt, b.jpg, c.mp4, noext, .mouziignore (not matched by *.tmp)
        // .tmp files and symlink dir are skipped
        assert_eq!(summaries[0].files, 5);
        assert_eq!(summaries[0].bytes, 5 + 7 + 9 + 9 + 6);
        assert_eq!(summaries[0].root, root_str);

        let stats = db::get_inventory_stats().unwrap();
        assert_eq!(stats.total_files, 5);
        assert_eq!(stats.total_bytes, 5 + 7 + 9 + 9 + 6);
        assert!(stats.last_scan_at.is_some());

        let dist = db::get_category_distribution().unwrap();
        assert_eq!(dist.len(), 4); // Documents, Images, Videos, Other
        let doc = dist.iter().find(|c| c.category == "Documents").unwrap();
        assert_eq!((doc.files, doc.bytes), (1, 5));
        let img = dist.iter().find(|c| c.category == "Images").unwrap();
        assert_eq!((img.files, img.bytes), (1, 7));
        let other = dist.iter().find(|c| c.category == "Other").unwrap();
        assert_eq!((other.files, other.bytes), (2, 15)); // noext (9) + .mouziignore (6)

        let largest = db::get_largest_files(2).unwrap();
        assert_eq!(largest.len(), 2);
        assert_eq!(largest[0].size, 9);

        let root_stats = db::get_root_summaries().unwrap();
        assert_eq!(root_stats.len(), 1);
        assert_eq!((root_stats[0].files, root_stats[0].bytes), (5, 36));

        db::clear_inventory_for_root(&root_str).unwrap();
        assert_eq!(db::get_inventory_stats().unwrap().total_files, 0);
    }
}