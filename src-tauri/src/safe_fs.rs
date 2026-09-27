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
/// Uses the `trash` crate. On Windows this sends to the Recycle Bin.
/// Returns an error if trashing is not supported on the current platform
/// or if the operation fails.
pub fn delete_to_trash(path: &Path) -> Result<(), String> {
    let path_str = path.to_string_lossy();
    trash::delete(path).map_err(|e| format!("Failed to send {} to trash: {}", path_str, e))
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

    #[test]
    fn test_delete_to_trash_temp_file() {
        let root = test_dir("trash_test");
        let file = root.join("to-trash.txt");
        fs::write(&file, "delete me").unwrap();
        assert!(file.exists());

        // Trash support varies by platform/environment (e.g. headless Linux).
        // If unsupported here, log and skip rather than fail.
        match delete_to_trash(&file) {
            Ok(()) => {
                assert!(
                    !file.exists(),
                    "file should no longer exist at original path"
                );
            }
            Err(e) => {
                eprintln!("Note: trash test skipped — {}", e);
            }
        }

        fs::remove_dir_all(&root).ok();
    }
}