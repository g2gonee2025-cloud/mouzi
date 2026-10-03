use crate::db::{get_rules, get_settings, get_watched_folders, ActionLog, Rule};
use crate::ignore::{is_ignored, load_mouziignore};
use crate::suppress::{CancellationTicket, SuppressionSet, SUPPRESSION_TTL};
use chrono::Utc;
use regex::Regex;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

/// Check if a file is currently locked by another process.
/// On Windows this tries to open with write access; if another process holds
/// the file without FILE_SHARE_WRITE the open will fail.
fn is_file_locked(path: &Path) -> bool {
    fs::OpenOptions::new().write(true).open(path).is_err()
}

/// Check whether a file has passed its grace period since last modification.
/// Returns true if the file is ready to be moved.
fn check_grace_period(path: &Path, grace_seconds: i64) -> Result<bool, String> {
    let modified = fs::metadata(path).and_then(|metadata| metadata.modified())
        .map_err(|e| format!("Cannot check file settling for {}: {e}", path.display()))?;
    Ok(settled_at(modified, SystemTime::now(), grace_seconds))
}

fn settled_at(modified: SystemTime, now: SystemTime, grace_seconds: i64) -> bool {
    // An unattended move always needs a short quiet interval, including when
    // the optional user grace period is zero. Future timestamps fail closed.
    now.duration_since(modified)
        .map(|elapsed| elapsed >= Duration::from_secs(grace_seconds.max(2) as u64))
        .unwrap_or(false)
}

#[derive(Debug, Clone)]
pub struct FileInfo {
    pub path: PathBuf,
    pub name: String,
    pub extension: String,
}

pub fn should_ignore_file(path: &Path) -> bool {
    let name = path.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_lowercase();

    let ignored_names = [
        "desktop.ini", "thumbs.db", "ntuser.dat", "ntuser.ini",
        "boot.ini", "bootmgr", "pagefile.sys", "hiberfil.sys",
        "swapfile.sys", "autorun.inf", "config.sys", "io.sys",
        "msdos.sys", "command.com", "ntldr", "bootsect.bak",
    ];
    if ignored_names.contains(&name.as_str()) {
        return true;
    }

    // Browser temporary download files
    let temp_extensions = [".crdownload", ".part", ".download", ".tmp"];
    for ext in &temp_extensions {
        if name.ends_with(ext) {
            return true;
        }
    }

    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if let Ok(metadata) = fs::metadata(path) {
            let attrs = metadata.file_attributes();
            const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
            const FILE_ATTRIBUTE_SYSTEM: u32 = 0x4;
            if attrs & (FILE_ATTRIBUTE_HIDDEN | FILE_ATTRIBUTE_SYSTEM) != 0 {
                return true;
            }
        }
    }

    if name.starts_with('.') {
        return true;
    }

    false
}

/// Check whether a file is ignored by the `.mouziignore` in its parent folder.
/// This is the single helper used by both the watcher and the manual Clean Now path
/// so both code paths behave identically.
pub fn is_file_ignored_by_mouziignore(path: &Path) -> Result<bool, String> {
    if let Some(parent) = path.parent() {
        let patterns = load_mouziignore(&parent.to_string_lossy())?;
        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            return Ok(is_ignored(name, &patterns));
        }
    }
    Ok(false)
}

pub fn scan_file(path: &Path) -> Option<FileInfo> {
    if should_ignore_file(path) {
        return None;
    }
    if !fs::metadata(path).ok()?.is_file() { return None; }
    let name = path.file_name()?.to_string_lossy().to_string();
    let extension = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    Some(FileInfo {
        path: path.to_path_buf(),
        name,
        extension,
    })
}

fn matches_rule(file: &FileInfo, rule: &Rule) -> bool {
    if !rule.enabled {
        return false;
    }

    let ext_matches = rule.extensions.contains(&"*".to_string())
        || rule.extensions.contains(&file.extension);

    let pattern_matches = if let Some(ref pattern) = rule.pattern {
        if pattern.is_empty() {
            true
        } else {
            Regex::new(pattern)
                .map(|re| re.is_match(&file.name))
                .unwrap_or(false)
        }
    } else {
        true
    };

    ext_matches && pattern_matches
}

fn resolve_destination(destination: &str, file: &FileInfo) -> PathBuf {
    let now = Utc::now();
    let resolved = destination
        .replace("{year}", &now.format("%Y").to_string())
        .replace("{month}", &now.format("%m").to_string())
        .replace("{day}", &now.format("%d").to_string())
        .replace("{extension}", &file.extension)
        .replace("{filename}", &file.name);
    PathBuf::from(resolved)
}

pub fn find_matching_rule(file: &FileInfo) -> Option<Rule> {
    let rules = get_rules().ok()?;
    rules.into_iter().find(|rule| matches_rule(file, rule))
}

#[cfg(test)]
fn move_file_cross_device(src: &Path, dst: &Path) -> Result<(), std::io::Error> {
    crate::safe_fs::move_file_checked(src, dst)
        .map(|_| ())
        .map_err(std::io::Error::other)
}

/// The directory a rule would file `file` into, resolved against `root` — the
/// watched root the file was found under, NOT the file's own parent.
///
/// Shared by `execute_rule` and `is_already_filed` so the check and the move
/// can never resolve a destination differently. That is the whole anti-divergence
/// measure; without it the two drift the first time someone adds a placeholder
/// to one of them, and the drift is silent.
///
/// `root` is not cosmetic. Resolving against `path.parent()` instead makes a
/// relative destination strictly *below* the containing folder, and a strictly
/// longer path can never be a component-wise prefix of a shorter one — so
/// `is_already_filed` would be `false` for every relative rule, which is every
/// rule this application ships (`db.rs:343-351`). A check that cannot fire is
/// not a check.
///
/// This is nevertheless byte-identical to the old parent-relative behaviour for
/// every file the app can observe today, because the watch, the initial scan
/// and `manual_scan_folder` are all non-recursive, so every such file has
/// `path.parent() == root`. `t1_stage_one_changes_nothing_reachable` is the
/// test that pins that, and it is falsifiable: a function that always returned
/// `false` would fail it.
pub fn resolve_dest_dir(root: &Path, file: &FileInfo, rule: &Rule) -> PathBuf {
    let resolved = resolve_destination(&rule.destination, file);
    if Path::new(&rule.destination).is_absolute() {
        PathBuf::from(resolved)
    } else {
        root.join(resolved)
    }
}

/// True when `path` already sits at or below the folder this rule would file it
/// into for `root`, so acting on it would re-file something already filed.
///
/// This is what stops `Documents\report.pdf` becoming
/// `Documents\Documents\report.pdf` and on down. No time-windowed guard can stop
/// that: each generation lands on a path that was never armed, so there is no
/// entry to suppress and no TTL to wait out. It also covers the absolute-
/// destination case, where the destination can resolve to the file's own
/// containing folder and `fs::rename(x, x)` would succeed and log a row with
/// `source == destination`.
///
/// The containment argument is `path.parent()`, not `path`: the question is
/// whether the file's *containing folder* is the destination folder or below it.
/// Passing `path` would make `C:\W\Documents\report.pdf` fail to match
/// `C:\W\Documents`, which is the row this whole mechanism exists for.
pub fn is_already_filed(root: &Path, path: &Path, file: &FileInfo, rule: &Rule) -> bool {
    let Some(base) = path.parent() else {
        return false;
    };
    let dest_dir = resolve_dest_dir(root, file, rule);
    if base.exists() && dest_dir.exists() {
        crate::safe_fs::directory_contains(base, &dest_dir).unwrap_or(false)
    } else {
        // Pure destination planning when the output directory does not exist.
        // This branch is a skip hint, never filesystem authorization.
        crate::safe_fs::is_within_any_root(base, std::slice::from_ref(&dest_dir))
    }
}

#[cfg(test)]
fn execute_rule(file_info: &FileInfo, rule: &Rule, root: &Path) -> Result<String, String> {
    let dest = resolve_dest_dir(root, file_info, rule);

    match rule.action.as_str() {
        "move" => {
            let moved = crate::safe_fs::move_file_checked(&file_info.path, &dest.join(&file_info.name))?;
            Ok(moved.destination.to_string_lossy().to_string())
        }
        "ignore" => Ok(file_info.path.to_string_lossy().to_string()),
        _ => Err(format!("Unknown action: {}", rule.action)),
    }
}

#[derive(Debug)]
pub enum ProcessOutcome {
    Organized { rule: Rule, destination: String, warning: Option<String> },
    Skipped,
    Retry { reason: String },
}

pub fn process_file_with_suppression(
    path: &Path,
    bypass_grace: bool,
    root: &Path,
    suppression: &SuppressionSet,
    ticket: &CancellationTicket,
    expected_identity: Option<&crate::safe_fs::FileIdentity>,
    require_auto_root: bool,
) -> Result<ProcessOutcome, String> {
    let _operation = suppression.with_operation();
    if !ticket.is_current() || suppression.is_suppressed(path, Instant::now()) {
        return Ok(ProcessOutcome::Skipped);
    }
    let settings = get_settings().map_err(|e| format!("Cannot read organizer settings: {e}"))?;
    let folders = get_watched_folders().map_err(|e| format!("Cannot read watched folders: {e}"))?;
    if require_auto_root && !folders.iter().any(|folder| {
        folder.enabled && crate::db::is_folder_auto_mode(&folder.mode)
            && same_path(Path::new(&folder.path), root)
    }) {
        return Ok(ProcessOutcome::Skipped);
    }
    crate::safe_fs::validate_root(root)?;
    if !path.parent().is_some_and(|parent| crate::safe_fs::directory_contains(parent, root).unwrap_or(false)) {
        return Err(format!("File is outside organizer root: {}", path.display()));
    }
    if should_ignore_file(path) || is_file_ignored_by_mouziignore(path)? {
        return Ok(ProcessOutcome::Skipped);
    }
    // Read the watched-root protection file too if a future recursive caller
    // supplies a descendant. Errors pause organization instead of dropping rules.
    if path.parent().is_some_and(|parent| !same_path(parent, root)) {
        let patterns = load_mouziignore(&root.to_string_lossy())?;
        if path.file_name().and_then(|name| name.to_str()).is_some_and(|name| is_ignored(name, &patterns)) {
            return Ok(ProcessOutcome::Skipped);
        }
    }
    if !bypass_grace && !check_grace_period(path, settings.grace_period_seconds)? {
        return Ok(ProcessOutcome::Retry { reason: "File is still settling".into() });
    }
    if settings.lock_check_enabled && is_file_locked(path) {
        return Ok(ProcessOutcome::Retry { reason: "File is locked".into() });
    }
    let identity = match crate::safe_fs::file_identity(path) {
        Ok(identity) => identity,
        Err(error) => return Ok(ProcessOutcome::Retry { reason: error }),
    };
    if expected_identity.is_some_and(|expected| expected != &identity) {
        return Ok(ProcessOutcome::Skipped);
    }
    let file_info = scan_file(path).ok_or_else(|| format!("Cannot read file metadata: {}", path.display()))?;
    let rules = get_rules().map_err(|e| format!("Cannot read organizer rules: {e}"))?;
    let Some(rule) = rules.into_iter().find(|rule| matches_rule(&file_info, rule)) else {
        return Ok(ProcessOutcome::Skipped);
    };
    if rule.action == "ignore" || is_already_filed(root, path, &file_info, &rule)
        || folders.iter().filter(|folder| folder.enabled).any(|folder| {
            is_already_filed(Path::new(&folder.path), path, &file_info, &rule)
        })
    {
        return Ok(ProcessOutcome::Skipped);
    }
    if rule.action != "move" { return Err(format!("Unknown organizer action: {}", rule.action)); }
    let requested = resolve_dest_dir(root, &file_info, &rule).join(&file_info.name);
    let source = path.to_string_lossy().to_string();
    let requested_string = requested.to_string_lossy().to_string();
    // If intent cannot be recorded, no filesystem mutation takes place.
    let intent = crate::db::begin_operation_intent("move", &source, Some(&requested_string))
        .map_err(|e| format!("Cannot record move intent for {source}: {e}"))?;
    suppression.arm(&[path.to_path_buf(), requested.clone()], Instant::now(), SUPPRESSION_TTL);
    let moved = match crate::safe_fs::move_file_checked_expected(path, &requested, &identity) {
        Ok(moved) => moved,
        Err(error) => {
            let recorded = crate::db::finalize_operation_intent(intent, "failed", None, Some(&error));
            return Err(match recorded {
                Ok(()) => error,
                Err(record_error) => format!("{error}; recording the failed move also failed: {record_error}"),
            });
        }
    };
    let destination = moved.destination.to_string_lossy().to_string();
    // Renew for the actual collision-selected destination, measured after the
    // operation so a slow copy does not consume the delivery lease.
    suppression.arm(&[path.to_path_buf(), requested, moved.destination], Instant::now(), SUPPRESSION_TTL);
    let log = ActionLog {
        id: None, timestamp: Utc::now(), source_path: source,
        destination_path: Some(destination.clone()), action: rule.action.clone(),
        file_name: file_info.name, file_type: rule.name.clone(), undone: false,
    };
    let warning = crate::db::finalize_move_intent(intent, &log, &destination, None, &moved.identity.key())
        .err().map(|error| format!("Moved to {destination}, but history/inventory recording failed: {error}"));
    Ok(ProcessOutcome::Organized { rule, destination, warning })
}

pub fn same_path(left: &Path, right: &Path) -> bool {
    crate::safe_fs::normalize_lexically(left).to_string_lossy().to_lowercase()
        == crate::safe_fs::normalize_lexically(right).to_string_lossy().to_lowercase()
}

pub fn manual_scan_folder_with_suppression(folder: &str, suppression: &SuppressionSet)
    -> Result<Vec<(String, String, String)>, String>
{
    scan_folder(folder, suppression, true, None)
}

pub fn manual_scan_folder_cancelable(folder: &str, suppression: &SuppressionSet, cancelled: &std::sync::atomic::AtomicBool)
    -> Result<Vec<(String, String, String)>, String>
{
    scan_folder(folder, suppression, true, Some(cancelled))
}

pub fn scheduled_scan_folder(folder: &str, suppression: &SuppressionSet)
    -> Result<Vec<(String, String, String)>, String>
{
    scan_folder(folder, suppression, false, None)
}

fn scan_folder(folder: &str, suppression: &SuppressionSet, manual: bool, cancelled: Option<&std::sync::atomic::AtomicBool>)
    -> Result<Vec<(String, String, String)>, String>
{
    let root = Path::new(folder);
    crate::safe_fs::validate_root(root)?;
    load_mouziignore(folder)?;
    let mut results = Vec::new();
    let mut errors = Vec::new();
    for entry in fs::read_dir(root).map_err(|e| e.to_string())? {
        if cancelled.is_some_and(|flag| flag.load(std::sync::atomic::Ordering::Relaxed)) { return Err(format!("Import cancelled after organizing {} files", results.len())); }
        let path = entry.map_err(|e| e.to_string())?.path();
        if !path.is_file() { continue; }
        let file_name = path.file_name().unwrap_or_default().to_string_lossy().to_string();
        let ticket = suppression.ticket(&path);
        match process_file_with_suppression(&path, manual, root, suppression, &ticket, None, !manual) {
            Ok(ProcessOutcome::Organized { rule, destination, warning }) => {
                results.push((file_name, rule.name, destination));
                if let Some(warning) = warning { errors.push(warning); }
            }
            Ok(ProcessOutcome::Skipped | ProcessOutcome::Retry { .. }) => {}
            Err(error) => errors.push(format!("{file_name}: {error}")),
        }
    }
    if errors.is_empty() { Ok(results) }
    else { Err(format!("Organized {} files; {}", results.len(), errors.join("; "))) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Rule;
    use std::fs;

    fn rule(destination: &str) -> Rule {
        Rule {
            id: None,
            name: destination.to_string(),
            priority: 0,
            enabled: true,
            extensions: vec!["*".to_string()],
            pattern: None,
            destination: destination.to_string(),
            action: "move".to_string(),
            folder_id: 0,
        }
    }

    /// Never touches the filesystem: every test below is pure, so there are no
    /// fixtures to race over and nothing to clean up.
    fn file_at(path: &str) -> FileInfo {
        let path = PathBuf::from(path);
        FileInfo {
            extension: path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_lowercase(),
            name: path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            path,
        }
    }

    /// The destination expression exactly as it stood before `root` was
    /// threaded through — verbatim, not reimplemented from memory. Every
    /// byte-identity assertion below is a comparison against *this* line, which
    /// is what makes the inertness claim falsifiable: change `resolve_dest_dir`
    /// and these fail.
    fn destination_before_this_change(file: &FileInfo, r: &Rule) -> PathBuf {
        let base_folder = file
            .path
            .parent()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| ".".to_string());
        if Path::new(&r.destination).is_absolute() {
            resolve_destination(&r.destination, file)
        } else {
            PathBuf::from(&base_folder).join(resolve_destination(&r.destination, file))
        }
    }

    /// T1. The path-shape policy, as a truth table.
    ///
    /// Rows 2, 3, 6, 7, 8 and 12 are the ones that matter: they are the
    /// generation-2 nesting case, the lexical-`..` case, the casing case, the
    /// absolute-destination-same-as-source case, and the case where the root is
    /// carried rather than derived. Rows that resolve against the file's parent
    /// instead of the watched root return `false` for all of them, and a Layer 1
    /// that is inert looks exactly like a Layer 1 that is working.
    #[test]
    fn t1_path_shape_policy() {
        let w = Path::new(r"C:\W");
        let other = Path::new(r"C:\Other");

        let cases: Vec<(&str, &Path, &str, &str, bool)> = vec![
            ("1 ordinary, not yet filed", w, r"C:\W\report.pdf", "Documents", false),
            ("2 generation 2 of the nesting loop", w, r"C:\W\Documents\report.pdf", "Documents", true),
            ("3 already below the destination", w, r"C:\W\Documents\notes\a.pdf", "Documents", true),
            ("4 another rule's folder is not this rule's folder", w, r"C:\W\Documents\report.pdf", "Images", false),
            ("5 sibling sharing a string prefix", w, r"C:\W\DocumentsArchive\a.pdf", "Documents", false),
            ("6 lexical normalisation", w, r"C:\W\Documents\sub\..\report.pdf", "Documents", true),
            ("7 case-insensitive containment", w, r"C:\W\documents\report.pdf", "Documents", true),
            ("8 absolute destination equal to the root", w, r"C:\W\report.pdf", r"C:\W", true),
            ("9 absolute destination is ordinary filing", w, r"C:\W\report.pdf", r"C:\W\Documents", false),
            ("10 lexical normalisation, genuinely unfiled", w, r"C:\W\a\b\..\Documents\report.pdf", "Documents", false),
            ("11 a placeholder destination degrades to the lease", w, r"C:\W\Documents\report.pdf", "Documents/{filename}", false),
            ("12 the root is carried, not derived", other, r"C:\Other\Documents\a.pdf", "Documents", true),
            ("13 a different root's file is not already filed", other, r"C:\Other\a.pdf", "Documents", false),
            ("14 a top-level file is not already filed", w, r"C:\W\a.pdf", "Documents", false),
        ];

        for (what, root, path, destination, expected) in cases {
            let r = rule(destination);
            let f = file_at(path);
            assert_eq!(
                is_already_filed(root, Path::new(path), &f, &r),
                expected,
                "row {what}: root={root:?} file={path} destination={destination:?}"
            );
        }
    }

    /// T1's falsifiability check, stated as a test.
    ///
    /// A Layer 1 that always returned `false` would pass every "expected false"
    /// row above and is exactly what the pre-fix implementation did. What rules
    /// that out is that the same table contains rows expecting `true`. If this
    /// assertion ever needs loosening, the thing it is protecting — that
    /// `is_already_filed` has a reachable `true` — has already been lost.
    #[test]
    fn t1_the_policy_is_not_a_constant_false() {
        let w = Path::new(r"C:\W");
        let r = rule("Documents");
        let unfiled = file_at(r"C:\W\report.pdf");
        let filed = file_at(r"C:\W\Documents\report.pdf");

        assert!(
            !is_already_filed(w, Path::new(r"C:\W\report.pdf"), &unfiled, &r),
            "an unfiled top-level file must not be skipped"
        );
        assert!(
            is_already_filed(w, Path::new(r"C:\W\Documents\report.pdf"), &filed, &r),
            "an already-filed file must be skipped; if this fails, the loop guard is inert"
        );
    }

    /// T1's real subject: stage 1 must be a no-op for every file the app can
    /// observe today.
    ///
    /// Today the watch, the initial scan and `manual_scan_folder` are all
    /// non-recursive, so every file the app can see satisfies
    /// `path.parent() == root`. The seven destinations are the real default
    /// rules from `db.rs:343-351`, unmodified, and the file names are the
    /// extensions those rules claim — the shape of a real Downloads folder.
    #[test]
    fn t1_stage_one_changes_nothing_reachable() {
        let root = Path::new(r"C:\W");

        let default_rules = [
            ("Images", "holiday.jpg"),
            ("Documents", "report.pdf"),
            ("Archives", "backup.zip"),
            ("Installers", "setup.exe"),
            ("Music", "track.mp3"),
            ("Videos", "clip.mp4"),
            ("Others", "mystery.bin"),
        ];

        for (destination, file_name) in default_rules {
            let r = rule(destination);
            let path = format!(r"C:\W\{file_name}");
            let f = file_at(&path);

            assert_eq!(
                root,
                Path::new(&path).parent().expect("a top-level file has a parent"),
                "the premise: a non-recursively-visible file's parent IS the root"
            );

            let before = destination_before_this_change(&f, &r);
            let after = resolve_dest_dir(root, &f, &r);
            assert_eq!(
                after.to_string_lossy(),
                before.to_string_lossy(),
                "{destination}: the destination string changed for a file the app can see today"
            );

            assert!(
                !is_already_filed(root, Path::new(&path), &f, &r),
                "{destination}: Layer 1 fired on a file the app can see today, so stage 1 is not inert"
            );
        }
    }

    /// The same claim, end to end: run the real `execute_rule` against a real
    /// fixture and assert the path it actually moved the file to. This is the
    /// check that would catch a change to `execute_rule` itself, not just to the
    /// destination expression.
    #[test]
    fn t1_execute_rule_moves_the_file_exactly_where_it_used_to() {
        let root = std::env::temp_dir().join(format!("mouzi-t1-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("create fixture root");
        let root = fs::canonicalize(root).expect("resolve fixture root");

        for (destination, file_name) in [
            ("Documents", "report.pdf"),
            ("Images", "holiday.jpg"),
            ("Others", "mystery.bin"),
        ] {
            let r = rule(destination);
            let path = root.join(file_name);
            fs::write(&path, b"payload").expect("write fixture file");
            let f = file_at(&path.to_string_lossy());

            assert!(
                !is_already_filed(&root, &path, &f, &r),
                "{destination}: Layer 1 fired on a top-level file in its own root"
            );

            let moved = execute_rule(&f, &r, &root).expect("execute_rule");

            let expected = destination_before_this_change(&f, &r).join(file_name);
            assert_eq!(
                moved,
                expected.to_string_lossy(),
                "{destination}: execute_rule moved the file somewhere it used to not move it"
            );
            assert!(
                Path::new(&moved).is_file(),
                "{destination}: the reported destination does not exist: {moved}"
            );
            assert!(!path.exists(), "{destination}: the source survived the move");

            // One level of nesting, never two. `Documents\Documents` appearing
            // here is the whole failure this design exists to prevent.
            let nested = Path::new(&moved)
                .parent()
                .and_then(|d| d.parent())
                .map(|grandparent| grandparent.join(destination).join(destination));
            if let Some(grandparent) = nested {
                assert!(
                    !grandparent.exists(),
                    "{destination}: the file nested one level deeper than it should have"
                );
            }
        }

        let _ = fs::remove_dir_all(&root);
    }

    /// The degenerate destination the design flags: a rule whose destination is
    /// `.` or empty resolves to the root itself.
    ///
    /// Before the root-resolution fix that produced a rename *in place* —
    /// `report.pdf` became `report_0.pdf` in the very same folder, which the
    /// watcher then read as new user activity, forever. Layer 1 now makes the
    /// containment trivially true and the file is left alone, which is the
    /// better of the two behaviours for a rule that cannot work. The rule edit
    /// is rejected in `commands.rs::update_rule_cmd`; this test pins the
    /// defence that does not depend on the edit being rejected.
    #[test]
    fn t1_a_dot_destination_cannot_re_file_a_top_level_file() {
        let root = Path::new(r"C:\W");
        for destination in [".", "", "./", "sub/.."] {
            let r = rule(destination);
            let path = r"C:\W\report.pdf";
            let f = file_at(path);
            assert!(
                is_already_filed(root, Path::new(path), &f, &r),
                "destination {destination:?} must be treated as unfileable, not acted on"
            );
        }
    }

    #[test]
    fn test_move_file_between_directories_on_the_same_volume() {
        let test_root = std::env::temp_dir().join(format!(
            "mouzi-move-test-{}",
            std::process::id()
        ));
        let src_dir = test_root.join("src");
        let dst_dir = test_root.join("dst");
        fs::create_dir_all(&src_dir).unwrap();
        fs::create_dir_all(&dst_dir).unwrap();

        let src = src_dir.join("cross_device_test.txt");
        let dst = dst_dir.join("cross_device_test.txt");

        fs::write(&src, "hello cross-device").unwrap();
        if dst.exists() {
            fs::remove_file(&dst).unwrap();
        }

        move_file_cross_device(&src, &dst).unwrap();

        assert!(dst.exists(), "destination file should exist after cross-device move");
        assert!(!src.exists(), "source file should be removed after cross-device move");

        // cleanup
        let _ = fs::remove_file(&dst);
        let _ = fs::remove_file(&src);
        let _ = fs::remove_dir_all(&test_root);
    }

    #[test]
    fn custom_rule_labels_finalize_moves_without_becoming_categories() {
        let _serial = crate::db::serialise_test_db();
        crate::db::init_test_db();
        let root = std::env::temp_dir().join(format!("mouzi-custom-rule-{}-{}", std::process::id(), chrono::Utc::now().timestamp_nanos_opt().unwrap()));
        fs::create_dir(&root).unwrap();
        let root = fs::canonicalize(root).unwrap();
        let folder_id = crate::db::add_watched_folder(&root.to_string_lossy(), crate::db::FOLDER_MODE_MANUAL).unwrap();
        let mut custom = rule("Documents");
        custom.name = "My receipts".into();
        custom.extensions = vec!["auditx".into()];
        custom.priority = i32::MAX;
        custom.folder_id = folder_id;
        let rule_id = crate::db::add_rule(&custom).unwrap();
        struct Fixture { root: PathBuf, folder_id: i64, rule_id: i64 }
        impl Drop for Fixture {
            fn drop(&mut self) {
                let _ = crate::db::delete_rule(self.rule_id);
                let _ = crate::db::remove_watched_folder(self.folder_id);
                let _ = fs::remove_dir_all(&self.root);
            }
        }
        let _fixture = Fixture { root: root.clone(), folder_id, rule_id };
        let source = root.join("receipt.auditx");
        fs::write(&source, b"receipt").unwrap();
        let suppression = SuppressionSet::new();
        let ticket = suppression.ticket(&source);
        let outcome = process_file_with_suppression(&source, true, &root, &suppression, &ticket, None, false).unwrap();
        let ProcessOutcome::Organized { destination, warning, .. } = outcome else { panic!("custom rule did not move the file"); };
        assert!(warning.is_none(), "successful custom rules must finalize history: {warning:?}");
        assert_eq!(fs::read(&destination).unwrap(), b"receipt");
        assert!(!source.exists());
        let conn = crate::db::lock_db().unwrap();
        let (label, identity): (String, Option<String>) = conn.query_row("SELECT file_type,file_identity FROM action_logs WHERE source_path=?1", [source.to_string_lossy().as_ref()], |row| Ok((row.get(0)?, row.get(1)?))).unwrap();
        assert_eq!(label, "My receipts");
        assert!(identity.is_some());
        let category: String = conn.query_row("SELECT category FROM file_inventory WHERE path=?1", [&destination], |row| row.get(0)).unwrap();
        assert_eq!(category, "Other", "rule labels must not become canonical categories");
        let status: String = conn.query_row("SELECT status FROM operation_intents WHERE source_path=?1", [source.to_string_lossy().as_ref()], |row| row.get(0)).unwrap();
        assert_eq!(status, "completed");
        conn.execute("DELETE FROM action_logs WHERE source_path=?1", [source.to_string_lossy().as_ref()]).unwrap();
    }
}
