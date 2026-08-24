use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// The outcome of a safe file move operation.
#[derive(Debug, Clone, PartialEq)]
pub enum MoveOutcome {
    /// File was moved to the exact destination requested.
    Moved,
    /// File was moved but renamed to avoid collision (new name provided).
    MovedWithNewName(String),
}

/// Resolve the destination path, appending a timestamp suffix if `dest` exists.
fn resolve_destination(dest: &Path) -> (PathBuf, MoveOutcome) {
    if dest.exists() {
        let stem = dest.file_stem().unwrap_or_default().to_string_lossy();
        let ext = dest
            .extension()
            .map(|e| format!(".{}", e.to_string_lossy()))
            .unwrap_or_default();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let new_name = format!("{}_{}{}", stem, now, ext);
        let new_path = dest.with_file_name(&new_name);
        (new_path, MoveOutcome::MovedWithNewName(new_name))
    } else {
        (dest.to_path_buf(), MoveOutcome::Moved)
    }
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