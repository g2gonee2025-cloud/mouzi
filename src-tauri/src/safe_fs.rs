use std::path::{Component, Path, PathBuf};

/// The outcome of a safe file move operation.
#[derive(Debug, Clone, PartialEq)]
pub enum MoveOutcome {
    /// File was moved to the exact destination requested.
    Moved,
    /// File was moved but renamed to avoid collision (new name provided).
    MovedWithNewName(String),
}

/// Collapse `.` and `..` without touching the filesystem. `canonicalize` is
/// deliberately not used: it fails on paths that do not exist and would resolve
/// a link to its target, which can sit outside the root the caller checked.
fn normalize_lexically(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            // pop() does nothing once the prefix and root are consumed, so a
            // run of `..` cannot climb above the drive.
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

/// Whether `path` lies inside one of `roots`.
///
/// Compares components rather than testing a string prefix: a prefix test would
/// let `C:\Users\Me` match `C:\Users\Melissa`, and a `..` segment would let a
/// path leave its root while still beginning with it. Comparison is
/// case-insensitive because Windows paths are.
pub fn is_within_any_root(path: &Path, roots: &[PathBuf]) -> bool {
    let target = normalize_lexically(path);
    roots.iter().any(|root| {
        let root = normalize_lexically(root);
        // An empty root would vacuously contain everything.
        if root.as_os_str().is_empty() {
            return false;
        }
        let mut root_components = root.components();
        let mut target_components = target.components();
        loop {
            match (root_components.next(), target_components.next()) {
                (None, _) => return true,
                (Some(_), None) => return false,
                (Some(root_component), Some(target_component)) => {
                    if !root_component
                        .as_os_str()
                        .eq_ignore_ascii_case(target_component.as_os_str())
                    {
                        return false;
                    }
                }
            }
        }
    })
}

/// Resolve the destination path, appending a timestamp suffix if `dest` exists.
/// Pick a name inside `dir` that is not already taken.
///
/// A second-resolution timestamp is not enough of a suffix: two files whose
/// stems and extensions collide in the same folder within one second produce
/// the same candidate, and the rename then replaces the first file outright
/// rather than failing. Counting upwards until the name is free makes the
/// result unique by construction rather than by luck.
pub fn unique_destination(dir: &Path, file_name: &str) -> PathBuf {
    let candidate = dir.join(file_name);
    if !candidate.exists() {
        return candidate;
    }
    let source = Path::new(file_name);
    let stem = source
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    let extension = source
        .extension()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    // An extensionless file must not gain a trailing dot, which Windows rejects.
    for attempt in 0..10_000u32 {
        let name = if extension.is_empty() {
            format!("{}_{}", stem, attempt)
        } else {
            format!("{}_{}.{}", stem, attempt, extension)
        };
        let candidate = dir.join(name);
        if !candidate.exists() {
            return candidate;
        }
    }
    candidate
}

fn resolve_destination(dest: &Path) -> (PathBuf, MoveOutcome) {
    if !dest.exists() {
        return (dest.to_path_buf(), MoveOutcome::Moved);
    }
    let parent = dest.parent().unwrap_or(dest);
    let name = dest.file_name().unwrap_or_default().to_string_lossy().to_string();
    let new_path = unique_destination(parent, &name);
    let new_name = new_path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    (new_path, MoveOutcome::MovedWithNewName(new_name))
}

/// Copy `src` to `dest` and delete `src`.
///
/// Used as a fallback when `std::fs::rename` fails across devices
/// (EXDEV on Linux, ERROR_NOT_SAME_DEVICE on Windows).
fn copy_delete_fallback(src: &Path, dest: &Path) -> Result<MoveOutcome, String> {
    let (actual_dest, outcome) = resolve_destination(dest);
    std::fs::copy(src, &actual_dest).map_err(|e| {
        format!(
            "Failed to copy {} to {}: {}",
            src.to_string_lossy(),
            actual_dest.to_string_lossy(),
            e
        )
    })?;
    std::fs::remove_file(src).map_err(|e| {
        format!(
            "Copied {} to {} but failed to remove source: {}",
            src.to_string_lossy(),
            actual_dest.to_string_lossy(),
            e
        )
    })?;
    Ok(outcome)
}

/// Move a file from `src` to `dest` with safe handling:
///
/// - Creates parent directories of `dest` if they do not exist.
/// - If `dest` already exists, appends a timestamp suffix to the filename.
/// - Falls back to copy + delete if `std::fs::rename` crosses devices.
/// - Never silently swallows errors.
pub fn move_file(src: &Path, dest: &Path) -> Result<MoveOutcome, String> {
    let src_str = src.to_string_lossy();
    let dest_str = dest.to_string_lossy();

    // Ensure parent directory exists
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create parent dirs for {}: {}", dest_str, e))?;
    }

    let (actual_dest, outcome) = resolve_destination(dest);

    // Try atomic rename first; fall back to copy+delete for cross-device moves.
    match std::fs::rename(src, &actual_dest) {
        Ok(()) => Ok(outcome),
        Err(rename_err) => {
            if rename_err.raw_os_error() == Some(18)
                || rename_err.kind() == std::io::ErrorKind::CrossesDevices
            {
                copy_delete_fallback(src, &actual_dest)
            } else {
                Err(format!(
                    "Failed to move {} to {}: {}",
                    src_str,
                    actual_dest.to_string_lossy(),
                    rename_err
                ))
            }
        }
    }
}

/// Delete a file by moving it to the platform's trash / Recycle Bin.
///
/// Uses the `trash` crate. On Windows this is the Recycle Bin, but only while
/// the bin can accept the item: when the bin is full, the item is too large, or
/// the folder is on a network drive, Windows deletes it permanently instead.
/// `trash::delete` returns `Ok(())` in that case, so the caller cannot tell
/// which of the two happened and must not promise a restore.
/// Returns an error if trashing is not supported on the current platform
/// or if the operation fails.
pub fn delete_to_trash(path: &Path) -> Result<(), String> {
    let path_str = path.to_string_lossy();
    trash::delete(path).map_err(|e| format!("Failed to send {} to trash: {}", path_str, e))
}

/// What actually became of a file whose `trash::delete` call returned `Ok`.
///
/// `delete()` returns `Result<(), Error>` — a unit type, so there is no flag in
/// it to inspect. On Windows the underlying `IFileOperation::PerformOperations`
/// also returns `S_OK` when the Shell silently deleted the item outright
/// because the bin could not take it (full, item too large, network volume, bin
/// disabled), and `trash` never calls `GetAnyOperationsAborted`. The return
/// value therefore cannot distinguish "recoverable" from "gone forever".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TrashVerdict {
    /// Seen in the Recycle Bin, so the user can restore it.
    InRecycleBin,
    /// Absent from the Recycle Bin even though the call succeeded: Windows
    /// deleted it permanently.
    PermanentlyDeleted,
    /// The evidence is missing, so the fate is genuinely unknown.
    Unverified,
}

/// A reading of the Recycle Bin at one instant.
///
/// This is DETECTION, NOT PREVENTION. A path only becomes visible here after
/// the file has already left its original location, and a file the bin refused
/// has already been destroyed by the time anyone could look. There is no
/// pre-flight check that would make the permanent deletion stop happening; the
/// only thing this buys is an honest report. Do not "optimise" it into a check
/// that runs before `delete_to_trash` and conclude the outcome is then safe.
#[derive(Debug, Clone)]
pub(crate) struct TrashSnapshot {
    /// One key per item currently in the bin. A `Vec` and not a set: `report.txt`
    /// and `report.docx` share a key once the extension is dropped, and letting
    /// one entry vouch for both files would repeat the original defect.
    items: Vec<Vec<Vec<u8>>>,
    /// Whether the bin could actually be read at all. When false, `items` means
    /// "unknown", never "empty".
    known: bool,
}

impl TrashSnapshot {
    /// A reading that failed.
    ///
    /// Deliberately distinct from a reading that found nothing. Conflating the
    /// two is the worst bug available here: one transient Shell error would
    /// turn a whole successful cleanup into a page of "deleted permanently".
    fn unavailable() -> Self {
        Self {
            items: Vec::new(),
            known: false,
        }
    }
}

/// The key two paths are compared by, one entry per path component.
///
/// Deliberately lossy, because what the Recycle Bin reports about an item's
/// original path is lossy too — `TrashItem::original_path()` is NOT the path the
/// file had. Two things are lost on the way in:
///
/// * The Recycle Bin exposes a shell item's name through `SIGDN_PARENTRELATIVE`,
///   which for a binned file omits the extension: a file `report.txt` comes back
///   as the name `report`. Comparing full paths would therefore fail to match
///   essentially every real file.
/// * The original folder arrives in its long form even when the app holds the
///   8.3 short form, and the app may hold a path reached either way.
///
/// Both are absorbed by the key rather than by the comparison: the last
/// extension is dropped and every component is ASCII-folded. Folding only ASCII
/// keeps this the same notion of "same path" that `is_within_any_root` uses via
/// `eq_ignore_ascii_case`, so a snapshot lookup and a root check cannot disagree
/// about a path's identity. Components are bytes, not `str`, because a Windows
/// path need not be valid Unicode and two distinct paths must not collapse onto
/// one key.
fn trash_key(path: &Path) -> Vec<Vec<u8>> {
    let mut components: Vec<Vec<u8>> = normalize_lexically(path)
        .components()
        .map(|component| {
            component
                .as_os_str()
                .as_encoded_bytes()
                .to_ascii_lowercase()
        })
        .collect();
    // `Path::file_stem` semantics applied to the last component: cut at the
    // final dot, except for a name that is nothing but a leading dot, which
    // Rust also treats as having no extension.
    if let Some(last) = components.last_mut() {
        if let Some(dot) = last.iter().rposition(|byte| *byte == b'.') {
            if dot > 0 {
                last.truncate(dot);
            }
        }
    }
    components
}

/// Read the Recycle Bin once.
///
/// Take exactly one reading before a batch and one after. Listing walks every
/// trashed item on the machine, so a reading per file makes a 500-item cleanup
/// 500 full listings and hangs the app.
pub(crate) fn trash_snapshot() -> TrashSnapshot {
    match trash::os_limited::list() {
        Ok(items) => TrashSnapshot {
            items: items
                .iter()
                // `original_path()` is `original_parent.join(name)`, and `name`
                // is already extension-stripped, so the same key rule applies to
                // both sides of the comparison.
                .map(|item| trash_key(&item.original_path()))
                .collect(),
            known: true,
        },
        Err(e) => {
            log::warn!("Could not read the Recycle Bin: {}", e);
            TrashSnapshot::unavailable()
        }
    }
}

/// Decide, per attempted path, whether it reached the Recycle Bin.
///
/// `attempted` are the paths whose `trash::delete` call returned `Ok`; verdicts
/// come back in the same order. Pure so the policy can be tested without a
/// Recycle Bin.
///
/// A path that does not match a new bin entry is NOT on its own evidence of a
/// permanent deletion: `trash_key` is deliberately lossy, and any further
/// mismatch we have not thought of would condemn files the bin in fact accepted.
/// A deletion is therefore only reported when the bin's own item count
/// independently shows that items went missing, and only when that shortfall
/// exactly accounts for the unmatched paths. Reporting a recoverable file as
/// destroyed would be a different lie, and a more corrosive one.
pub(crate) fn classify_trash_outcomes(
    before: &TrashSnapshot,
    after: &TrashSnapshot,
    attempted: &[PathBuf],
) -> Vec<TrashVerdict> {
    // Without both readings there is no way to tell a new entry from a stale
    // one, and no count to corroborate an absence with, so nothing is claimed.
    if !before.known || !after.known {
        return vec![TrashVerdict::Unverified; attempted.len()];
    }

    // Attribute bin entries to paths one-for-one. Greedy, in the order the batch
    // ran, so a key shared by two items cannot be spent on the same path twice.
    let mut verdicts = vec![TrashVerdict::Unverified; attempted.len()];
    let mut spent = vec![false; after.items.len()];
    for (index, path) in attempted.iter().enumerate() {
        let key = trash_key(path);
        let is_new = !before.items.contains(&key);
        let landed = after.items.iter().enumerate().position(|(i, item)| {
            !spent[i] && is_new && *item == key
        });
        if let Some(found) = landed {
            spent[found] = true;
            verdicts[index] = TrashVerdict::InRecycleBin;
        }
    }

    let unmatched = attempted.len() - verdicts.iter().filter(|v| **v == TrashVerdict::InRecycleBin).count();
    if unmatched == 0 {
        return verdicts;
    }
    // Signed: the user emptying the bin mid-batch makes this negative, which
    // must not be read as a shortfall.
    let growth = after.items.len() as i64 - before.items.len() as i64;
    let missing = attempted.len() as i64 - growth;
    if missing == unmatched as i64 {
        for verdict in verdicts.iter_mut() {
            if *verdict == TrashVerdict::Unverified {
                *verdict = TrashVerdict::PermanentlyDeleted;
            }
        }
    }
    // Otherwise the bin accounted for the batch (the shortfall is zero or
    // negative) or the shortfall is smaller than the number of unmatched paths,
    // so the unmatched ones are unproven either way and stay Unverified.
    verdicts
}

#[cfg(test)]
impl TrashSnapshot {
    /// Build a reading from a known set of binned paths, through the same key
    /// builder the real snapshot uses.
    fn from_paths(paths: &[&Path]) -> Self {
        Self {
            items: paths.iter().map(|path| trash_key(path)).collect(),
            known: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn root() -> Vec<PathBuf> {
        vec![PathBuf::from("C:/Users/Me/Documents")]
    }

    #[test]
    fn accepts_a_file_directly_inside_a_root() {
        assert!(is_within_any_root(
            Path::new("C:/Users/Me/Documents/a.txt"),
            &root()
        ));
    }

    #[test]
    fn accepts_a_nested_file_inside_a_root() {
        assert!(is_within_any_root(
            Path::new("C:/Users/Me/Documents/deep/nested/a.txt"),
            &root()
        ));
    }

    #[test]
    fn accepts_the_root_itself() {
        assert!(is_within_any_root(
            Path::new("C:/Users/Me/Documents"),
            &root()
        ));
    }

    #[test]
    fn rejects_a_sibling_whose_name_shares_the_root_prefix() {
        assert!(!is_within_any_root(
            Path::new("C:/Users/Me/DocumentsArchive/a.txt"),
            &root()
        ));
    }

    #[test]
    fn rejects_parent_traversal_that_leaves_the_root() {
        assert!(!is_within_any_root(
            Path::new("C:/Users/Me/Documents/../Secrets/a.txt"),
            &root()
        ));
    }

    #[test]
    fn accepts_parent_traversal_that_stays_inside_the_root() {
        assert!(is_within_any_root(
            Path::new("C:/Users/Me/Documents/sub/../a.txt"),
            &root()
        ));
    }

    #[test]
    fn rejects_parent_traversal_that_climbs_above_the_drive() {
        assert!(!is_within_any_root(
            Path::new("C:/Users/Me/Documents/../../../../a.txt"),
            &root()
        ));
    }

    #[test]
    fn compares_case_insensitively() {
        assert!(is_within_any_root(
            Path::new("c:/users/me/DOCUMENTS/a.txt"),
            &root()
        ));
    }

    #[test]
    fn accepts_when_any_one_root_contains_the_path() {
        let roots = vec![
            PathBuf::from("C:/Users/Me/Pictures"),
            PathBuf::from("C:/Users/Me/Documents"),
        ];
        assert!(is_within_any_root(
            Path::new("C:/Users/Me/Documents/a.txt"),
            &roots
        ));
    }

    #[test]
    fn rejects_a_path_outside_every_root() {
        assert!(!is_within_any_root(Path::new("C:/Windows/System32/a.dll"), &root()));
    }

    #[test]
    fn rejects_everything_when_there_are_no_roots() {
        assert!(!is_within_any_root(
            Path::new("C:/Users/Me/Documents/a.txt"),
            &[]
        ));
    }

    #[test]
    fn rejects_everything_when_a_root_is_empty() {
        assert!(!is_within_any_root(
            Path::new("C:/Users/Me/Documents/a.txt"),
            &[PathBuf::new()]
        ));
    }

    #[test]
    fn unique_destination_keeps_a_free_name_unchanged() {
        let dir = test_dir("unique_free");
        let picked = unique_destination(&dir, "report.txt");
        assert_eq!(picked, dir.join("report.txt"));
    }

    #[test]
    fn unique_destination_suffixes_a_taken_name() {
        let dir = test_dir("unique_taken");
        fs::write(dir.join("report.txt"), b"original").unwrap();
        let picked = unique_destination(&dir, "report.txt");
        assert_ne!(picked, dir.join("report.txt"));
        assert!(picked.starts_with(&dir));
    }

    /// The regression: a second collision inside the same call sequence used to
    /// produce a name identical to the first, and the rename replaced the file
    /// that had already been moved there.
    #[test]
    fn unique_destination_never_repeats_a_name_it_already_issued() {
        let dir = test_dir("unique_repeat");
        fs::write(dir.join("report.txt"), b"original").unwrap();
        let first = unique_destination(&dir, "report.txt");
        fs::write(&first, b"first move").unwrap();
        let second = unique_destination(&dir, "report.txt");
        assert_ne!(first, second, "two collisions produced the same name");
        assert_eq!(fs::read(dir.join("report.txt")).unwrap(), b"original");
        assert_eq!(fs::read(&first).unwrap(), b"first move");
    }

    #[test]
    fn unique_destination_does_not_add_a_trailing_dot_to_an_extensionless_file() {
        let dir = test_dir("unique_noext");
        fs::write(dir.join("README"), b"original").unwrap();
        let picked = unique_destination(&dir, "README");
        let name = picked.file_name().unwrap().to_string_lossy().to_string();
        assert!(!name.ends_with('.'), "got {:?}", name);
    }

    #[test]
    fn unique_destination_finds_a_gap_in_an_existing_run_of_suffixes() {
        let dir = test_dir("unique_gap");
        fs::write(dir.join("a.txt"), b"x").unwrap();
        fs::write(dir.join("a_0.txt"), b"x").unwrap();
        fs::write(dir.join("a_2.txt"), b"x").unwrap();
        let picked = unique_destination(&dir, "a.txt");
        assert_eq!(picked, dir.join("a_1.txt"));
    }

    /// Helper: create a temporary test directory unique to this process.
    fn test_dir(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "mouzi-safe-fs-{}-{}",
            name,
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn test_move_file_normal() {
        let root = test_dir("normal");
        let src = root.join("hello.txt");
        let dest = root.join("target").join("hello.txt");

        fs::write(&src, "world").unwrap();

        let outcome = move_file(&src, &dest).unwrap();
        assert_eq!(outcome, MoveOutcome::Moved);
        assert!(!src.exists(), "source should be removed");
        assert!(dest.exists(), "destination should exist");
        assert_eq!(fs::read_to_string(&dest).unwrap(), "world");

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn test_move_file_collision_creates_suffixed_name() {
        let root = test_dir("collision");
        let src = root.join("doc.txt");
        let dest = root.join("dest").join("doc.txt");

        // Create an existing file at the destination
        fs::create_dir_all(dest.parent().unwrap()).unwrap();
        fs::write(&dest, "original").unwrap();
        // Create the source
        fs::write(&src, "new-version").unwrap();

        let outcome = move_file(&src, &dest).unwrap();
        match outcome {
            MoveOutcome::MovedWithNewName(ref name) => {
                assert!(
                    name.starts_with("doc_"),
                    "expected doc_ prefix, got {}",
                    name
                );
                assert!(name.ends_with(".txt"), "expected .txt suffix, got {}", name);
                // The original dest file should still have its content
                assert_eq!(fs::read_to_string(&dest).unwrap(), "original");
                // The renamed file should have the new content
                assert!(
                    dest.with_file_name(name).exists(),
                    "renamed file should exist"
                );
                assert_eq!(
                    fs::read_to_string(dest.with_file_name(name)).unwrap(),
                    "new-version"
                );
            }
            _ => panic!("Expected MovedWithNewName on collision"),
        }

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn test_move_file_creates_missing_parent_dirs() {
        let root = test_dir("missing_parent");
        let src = root.join("file.txt");
        let dest = root.join("a").join("b").join("c").join("file.txt");

        fs::write(&src, "deep").unwrap();

        // Parent dirs a/b/c do not exist yet
        assert!(!dest.parent().unwrap().exists());

        let outcome = move_file(&src, &dest).unwrap();
        assert_eq!(outcome, MoveOutcome::Moved);
        assert!(dest.exists(), "destination should exist in recreated dirs");
        assert_eq!(fs::read_to_string(&dest).unwrap(), "deep");

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn test_move_file_copy_delete_fallback() {
        // The copy+delete path is what move_file uses when std::fs::rename fails
        // (cross-device moves). We cannot reliably provoke a cross-device rename
        // failure in a unit test, so we exercise the fallback directly.
        let root = test_dir("copy_delete");
        let src = root.join("important.bin");
        let dest = root.join("backup").join("important.bin");

        fs::write(&src, "content").unwrap();
        fs::create_dir_all(dest.parent().unwrap()).unwrap();

        let outcome = copy_delete_fallback(&src, &dest).unwrap();
        assert_eq!(outcome, MoveOutcome::Moved);
        assert!(!src.exists(), "source should be deleted after copy");
        assert!(dest.exists(), "dest should exist after copy");
        assert_eq!(fs::read_to_string(&dest).unwrap(), "content");

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn test_move_file_returns_error_for_missing_source() {
        let root = test_dir("missing_source");
        let src = root.join("does-not-exist.txt");
        let dest = root.join("somewhere").join("does-not-exist.txt");

        let err = move_file(&src, &dest).unwrap_err();
        assert!(!err.is_empty());

        fs::remove_dir_all(&root).ok();
    }

    /// A long-form scratch directory under the user profile.
    ///
    /// `std::env::temp_dir()` hands back the 8.3 short form on some Windows
    /// installs (`C:\Users\HAGOPG~1\...`) while the Recycle Bin always reports
    /// the long form, so a test using it would be measuring the 8.3 mismatch
    /// rather than the trash path.
    fn long_form_dir(name: &str) -> PathBuf {
        let base = std::env::var("USERPROFILE")
            .map(PathBuf::from)
            .unwrap_or_else(|_| std::env::temp_dir());
        let dir = base.join(format!("mouzi-safe-fs-{}-{}", name, std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// The one test that touches the real Recycle Bin. Set
    /// `MOUZI_SKIP_TRASH_INTEGRATION_TEST=1` to opt out explicitly; there is no
    /// silent path where a broken trash implementation still passes.
    #[test]
    fn delete_to_trash_places_the_file_in_the_recycle_bin() {
        if std::env::var("MOUZI_SKIP_TRASH_INTEGRATION_TEST").as_deref() == Ok("1") {
            eprintln!("skipped: MOUZI_SKIP_TRASH_INTEGRATION_TEST=1");
            return;
        }

        let root = long_form_dir("trash_test");
        let file = root.join("to-trash.txt");
        fs::write(&file, "delete me").unwrap();
        assert!(file.exists());

        let before = trash_snapshot();
        // Previously the Err arm printed a note and the test passed anyway, so a
        // totally broken trash path was invisible to CI. A failure here is a
        // failure.
        delete_to_trash(&file).unwrap_or_else(|e| panic!("delete_to_trash failed: {}", e));
        let after = trash_snapshot();

        assert!(!file.exists(), "file is still at its original path");
        assert!(
            before.known && after.known,
            "the Recycle Bin could not be read; set MOUZI_SKIP_TRASH_INTEGRATION_TEST=1 \
             to skip this test on a machine where the Shell will not enumerate it"
        );
        assert!(
            after.items.len() > before.items.len(),
            "delete_to_trash returned Ok but the Recycle Bin did not grow — Windows \
             deleted the file permanently (bin full, item too large, or bin disabled)"
        );
        // The strongest form of the assertion: the snapshot has to positively
        // find the item and attribute it, not merely notice a larger bin.
        assert_eq!(
            classify_trash_outcomes(&before, &after, std::slice::from_ref(&file)),
            vec![TrashVerdict::InRecycleBin],
            "the item landed but the snapshot could not match it back to its path"
        );

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn delete_to_trash_reports_an_error_for_a_path_that_does_not_exist() {
        let root = test_dir("trash_missing");
        let missing = root.join("never-existed.txt");

        let err = delete_to_trash(&missing)
            .expect_err("trashing a path that does not exist should not report success");
        assert!(!err.is_empty());

        fs::remove_dir_all(&root).ok();
    }

    // ── Recycle Bin classification ─────────────────────────────

    fn classify(before: &TrashSnapshot, after: &TrashSnapshot, paths: &[&str]) -> Vec<TrashVerdict> {
        let attempted: Vec<PathBuf> = paths.iter().map(PathBuf::from).collect();
        let verdicts = classify_trash_outcomes(before, after, &attempted);
        assert_eq!(verdicts.len(), paths.len());
        verdicts
    }

    #[test]
    fn a_newly_appearing_path_is_reported_as_in_the_bin() {
        let before = TrashSnapshot::from_paths(&[Path::new("C:/Users/Me/old.txt")]);
        let after = TrashSnapshot::from_paths(&[
            Path::new("C:/Users/Me/old.txt"),
            Path::new("C:/Users/Me/new.txt"),
        ]);
        assert_eq!(
            classify(&before, &after, &["C:/Users/Me/new.txt"]),
            vec![TrashVerdict::InRecycleBin]
        );
    }

    #[test]
    fn a_path_that_never_appears_is_reported_as_permanently_deleted() {
        let before = TrashSnapshot::from_paths(&[Path::new("C:/Users/Me/other.txt")]);
        let after = TrashSnapshot::from_paths(&[Path::new("C:/Users/Me/other.txt")]);
        assert_eq!(
            classify(&before, &after, &["C:/Users/Me/victim.txt"]),
            vec![TrashVerdict::PermanentlyDeleted]
        );
    }

    /// The regression this whole module exists for: a failed reading must not be
    /// read as an empty bin, which would condemn every file in the batch.
    #[test]
    fn a_failed_reading_is_unknown_not_empty() {
        let before = TrashSnapshot::from_paths(&[Path::new("C:/Users/Me/other.txt")]);
        let after = TrashSnapshot::unavailable();
        assert_eq!(
            classify(&before, &after, &["C:/Users/Me/other.txt", "C:/Users/Me/victim.txt"]),
            vec![TrashVerdict::Unverified, TrashVerdict::Unverified]
        );
    }

    /// A failed *before* reading claims nothing at all, even for a path plainly
    /// absent from a good after-reading: without the before count there is
    /// nothing to corroborate an absence against, and an uncorroborated absence
    /// is how a recoverable file gets reported as destroyed.
    #[test]
    fn a_failed_before_reading_leaves_everything_unproven() {
        let before = TrashSnapshot::unavailable();
        let after = TrashSnapshot::from_paths(&[Path::new("C:/Users/Me/landed.txt")]);
        assert_eq!(
            classify(
                &before,
                &after,
                &["C:/Users/Me/landed.txt", "C:/Users/Me/victim.txt"]
            ),
            vec![TrashVerdict::Unverified, TrashVerdict::Unverified]
        );
    }

    /// A pre-existing entry does not vouch for this attempt — and because the
    /// bin did not grow, the count says this one really was destroyed.
    #[test]
    fn a_pre_existing_entry_does_not_vouch_for_this_attempt() {
        let before = TrashSnapshot::from_paths(&[Path::new("C:/Users/Me/recycled.txt")]);
        let after = TrashSnapshot::from_paths(&[Path::new("C:/Users/Me/recycled.txt")]);
        assert_eq!(
            classify(&before, &after, &["C:/Users/Me/recycled.txt"]),
            vec![TrashVerdict::PermanentlyDeleted]
        );
    }

    /// The other half of the coin: when the bin's count accounts for the item,
    /// a failed match is a matching artefact and must never be reported as a
    /// deletion. This is the case an unforeseen path mismatch would land in.
    #[test]
    fn a_failed_match_is_not_a_deletion_when_the_bin_grew_by_one() {
        let before = TrashSnapshot::from_paths(&[]);
        // The entry landed but under a key our matcher cannot reproduce.
        let after = TrashSnapshot::from_paths(&[Path::new("Z:/elsewhere/something.txt")]);
        assert_eq!(
            classify(&before, &after, &["C:/Users/Me/victim.txt"]),
            vec![TrashVerdict::Unverified]
        );
    }

    #[test]
    fn a_shortfall_smaller_than_the_unmatched_count_is_not_attributed() {
        let before = TrashSnapshot::from_paths(&[]);
        let after = TrashSnapshot::from_paths(&[Path::new("Z:/elsewhere/something.txt")]);
        // One item landed, so at most one of the two was destroyed — not both,
        // and there is no way to say which.
        assert_eq!(
            classify(
                &before,
                &after,
                &["C:/Users/Me/a.txt", "C:/Users/Me/b.txt"]
            ),
            vec![TrashVerdict::Unverified, TrashVerdict::Unverified]
        );
    }

    #[test]
    fn a_bin_emptied_mid_batch_is_not_read_as_a_shortfall() {
        let before = TrashSnapshot::from_paths(&[
            Path::new("C:/Users/Me/a.txt"),
            Path::new("C:/Users/Me/b.txt"),
        ]);
        let after = TrashSnapshot::from_paths(&[]);
        // Growth is -2, so the shortfall would be 4 for a batch of 2. Claiming
        // deletions on that arithmetic would be nonsense.
        assert_eq!(
            classify(&before, &after, &["C:/Users/Me/a.txt"]),
            vec![TrashVerdict::Unverified]
        );
    }

    #[test]
    fn two_items_sharing_a_stem_are_not_spent_on_one_path() {
        // `report.txt` and `report.docx` both land under the key `report`.
        let before = TrashSnapshot::from_paths(&[]);
        let after = TrashSnapshot::from_paths(&[
            Path::new("C:/Users/Me/report"),
            Path::new("C:/Users/Me/report"),
        ]);
        assert_eq!(
            classify(
                &before,
                &after,
                &["C:/Users/Me/report.txt", "C:/Users/Me/report.docx"]
            ),
            vec![TrashVerdict::InRecycleBin, TrashVerdict::InRecycleBin]
        );
    }

    /// The Recycle Bin reports a binned `report.txt` under the name `report`,
    /// so the key has to drop the extension or nothing would ever match.
    #[test]
    fn matching_ignores_the_extension_the_bin_drops() {
        let before = TrashSnapshot::from_paths(&[]);
        let after = TrashSnapshot::from_paths(&[Path::new("C:/Users/Me/report")]);
        assert_eq!(
            classify(&before, &after, &["C:/Users/Me/report.txt"]),
            vec![TrashVerdict::InRecycleBin]
        );
    }

    #[test]
    fn matching_is_case_insensitive_like_the_root_check() {
        let before = TrashSnapshot::from_paths(&[]);
        let after = TrashSnapshot::from_paths(&[Path::new("c:/users/me/report")]);
        assert_eq!(
            classify(&before, &after, &["C:\\Users\\Me\\Report.TXT"]),
            vec![TrashVerdict::InRecycleBin]
        );
    }

    #[test]
    fn matching_normalises_dot_segments_like_the_root_check() {
        let before = TrashSnapshot::from_paths(&[]);
        let after = TrashSnapshot::from_paths(&[Path::new("C:/Users/Me/a")]);
        assert_eq!(
            classify(&before, &after, &["C:/Users/Me/sub/../a.txt"]),
            vec![TrashVerdict::InRecycleBin]
        );
    }

    #[test]
    fn verdicts_come_back_in_the_order_the_paths_were_attempted() {
        let before = TrashSnapshot::from_paths(&[]);
        let after = TrashSnapshot::from_paths(&[Path::new("C:/Users/Me/b")]);
        assert_eq!(
            classify(
                &before,
                &after,
                &["C:/Users/Me/a.txt", "C:/Users/Me/b.txt"]
            ),
            vec![TrashVerdict::PermanentlyDeleted, TrashVerdict::InRecycleBin]
        );
    }
}