use crate::db::{get_rules, get_settings, get_watched_folders, ActionLog, Rule, ScheduleRunProgress, WatchedFolder};
use crate::ignore::{is_ignored, load_mouziignore};
use crate::safe_fs::{AncestorGuard, FileIdentity, MoveFailure};
use crate::suppress::{CancellationTicket, SuppressionSet, SUPPRESSION_TTL};
use chrono::{DateTime, NaiveDate, Utc};
use regex::Regex;
use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

/// Check if a file is currently locked by another process.
/// On Windows this tries to open with write access; if another process holds
/// the file without FILE_SHARE_WRITE the open will fail.
pub(crate) fn is_file_locked(path: &Path) -> bool {
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
    pub size: u64,
    modified_day: Option<NaiveDate>,
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

pub(crate) fn organization_path_is_protected(path: &Path, root: &Path) -> Result<bool, String> {
    if should_ignore_file(path) || is_file_ignored_by_mouziignore(path)? {
        return Ok(true);
    }
    if path.parent().is_some_and(|parent| !same_path(parent, root)) {
        let patterns = load_mouziignore(&root.to_string_lossy())?;
        return Ok(path.file_name().and_then(|name| name.to_str()).is_some_and(|name| is_ignored(name, &patterns)));
    }
    Ok(false)
}

pub fn scan_file(path: &Path) -> Option<FileInfo> {
    if should_ignore_file(path) {
        return None;
    }
    let metadata = fs::symlink_metadata(path).ok()?;
    if !metadata.is_file() { return None; }
    describe_file(path, &metadata)
}

pub(crate) fn describe_file(path: &Path, metadata: &fs::Metadata) -> Option<FileInfo> {
    let name = path.file_name()?.to_string_lossy().to_string();
    let mut extension = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    if extension.is_empty() {
        extension = extension_from_header(path).unwrap_or_default();
    }
    Some(FileInfo {
        path: path.to_path_buf(),
        name,
        extension,
        size: metadata.len(),
        modified_day: metadata.modified().ok().map(|time| DateTime::<Utc>::from(time).date_naive()),
    })
}

fn extension_from_header(path: &Path) -> Option<String> {
    let (mut file, _ancestors) = crate::safe_fs::open_guarded_regular_file(path).ok()?;
    let mut header = [0u8; 16];
    let count = file.read(&mut header).ok()?;
    let header = &header[..count];
    if header.starts_with(b"MZ") || header.starts_with(b"\x7fELF")
        || [[0xfe,0xed,0xfa,0xce],[0xfe,0xed,0xfa,0xcf],[0xce,0xfa,0xed,0xfe],[0xcf,0xfa,0xed,0xfe]]
            .iter().any(|magic| header.starts_with(magic))
    {
        return Some("exe".into());
    }
    infer::get(header).map(|kind| kind.extension().to_ascii_lowercase())
}

pub(crate) fn matches_rule(file: &FileInfo, rule: &Rule) -> bool {
    if !rule.enabled {
        return false;
    }

    if !rule.extensions.iter().any(|ext| ext == "*" || (!file.extension.is_empty() && ext == &file.extension)) {
        return false;
    }
    let options = &rule.options;
    if options.min_size.is_some_and(|min| file.size < min)
        || options.max_size.is_some_and(|max| file.size > max)
    {
        return false;
    }
    for (date, is_lower_bound) in [
        (options.modified_after.as_deref(), true),
        (options.modified_before.as_deref(), false),
    ] {
        if let Some(date) = date {
            let (Some(day), Ok(bound)) = (file.modified_day, NaiveDate::parse_from_str(date, "%Y-%m-%d")) else {
                return false;
            };
            if (is_lower_bound && day < bound) || (!is_lower_bound && day > bound) {
                return false;
            }
        }
    }

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

    pattern_matches
}

pub(crate) fn output_file_info(file: &FileInfo, rule: &Rule) -> FileInfo {
    let mut output = file.clone();
    let Some(extension) = file.path.extension().and_then(|value| value.to_str()) else {
        return output;
    };
    if !rule.normalize_extensions {
        return output;
    }
    let mapped = rule.extension_mappings.split([',', ';', '\n']).find_map(|entry| {
        let (from, to) = entry.split_once("->").or_else(|| entry.split_once(':'))?;
        let from = from.trim().trim_start_matches('.');
        let to = to.trim().trim_start_matches('.');
        if from.eq_ignore_ascii_case(extension) && !to.is_empty()
            && to.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            Some(to.to_ascii_lowercase())
        } else {
            None
        }
    });
    if let Some(mapped) = mapped {
        let stem = file.path.file_stem().unwrap_or_default().to_string_lossy();
        output.name = format!("{stem}.{mapped}");
        output.extension = mapped;
    }
    output
}

fn resolve_destination(destination: &str, file: &FileInfo) -> String {
    let now = Utc::now();
    destination
        .replace("{year}", &now.format("%Y").to_string())
        .replace("{month}", &now.format("%m").to_string())
        .replace("{day}", &now.format("%d").to_string())
        .replace("{extension}", &file.extension)
        .replace("{filename}", &file.name)
}

fn validate_relative_destination(destination: &str) -> Result<(), String> {
    let path = Path::new(destination);
    if destination.trim().is_empty()
        || destination.starts_with(['/', '\\'])
        || !path.components().all(|component| matches!(component, Component::Normal(_)))
        || destination.split(['/', '\\']).any(|part| {
            part.contains(':') || part.ends_with(['.', ' '])
        })
    {
        return Err("Relative move destinations must use ordinary subfolder names without a root, drive, or dot segments".into());
    }
    Ok(())
}

pub(crate) fn validate_move_destination_template(destination: &str) -> Result<(), String> {
    if Path::new(destination).is_absolute() {
        Ok(())
    } else {
        validate_relative_destination(destination)
    }
}

pub fn find_matching_rule(file: &FileInfo) -> Option<Rule> {
    let rules = get_rules().ok()?;
    rules.into_iter().find(|rule| matches_rule(file, rule))
}

pub(crate) fn matching_rule_in_root<'a>(
    file: &FileInfo, root: Option<&Path>, folders: &[WatchedFolder], rules: &'a [Rule],
) -> Option<&'a Rule> {
    let root_identity = root.filter(|_| rules.iter().any(|rule| rule.folder_id != 0))
        .and_then(|path| crate::safe_fs::file_identity(path).ok());
    rules.iter().find(|rule| {
        (rule.folder_id == 0 || root_identity.as_ref().is_some_and(|expected| {
            folders.iter().any(|folder| folder.id == Some(rule.folder_id) && folder.enabled
                && !crate::db::is_folder_paused_mode(&folder.mode)
                && crate::safe_fs::file_identity(Path::new(&folder.path)).ok().as_ref() == Some(expected))
        })) && matches_rule(file, rule)
    })
}

static RENAME_TOKEN: once_cell::sync::Lazy<Regex> =
    once_cell::sync::Lazy::new(|| Regex::new(r"\{([^{}]*)\}").unwrap());

fn safe_rename_leaf(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or_default().to_ascii_uppercase();
    let device = ["CON", "PRN", "AUX", "NUL"].contains(&stem.as_str())
        || ["COM", "LPT"].iter().any(|prefix| {
            stem.strip_prefix(*prefix).is_some_and(|number| matches!(number, "1"|"2"|"3"|"4"|"5"|"6"|"7"|"8"|"9"|"¹"|"²"|"³"))
        });
    !name.is_empty() && name.len() <= 240 && !name.ends_with(['.', ' '])
        && !name.chars().any(|ch| ch.is_control() || "<>:\"/\\|?*".contains(ch))
        && !device
}

pub(crate) fn validate_rename_template(template: &str) -> Result<(), String> {
    let sample = RENAME_TOKEN.replace_all(template, |capture: &regex::Captures<'_>| {
        match &capture[1] {
            "stem" | "filename" | "extension" | "year" | "month" | "day" => "sample",
            _ => "{}",
        }
    });
    if sample.contains(['{', '}']) || !safe_rename_leaf(&sample) {
        return Err("validation.template".into());
    }
    Ok(())
}

pub(crate) fn planned_destination(root: &Path, file: &FileInfo, rule: &Rule) -> Result<PathBuf, String> {
    if rule.action == "move" {
        let output = output_file_info(file, rule);
        return Ok(resolve_dest_dir(root, &output, rule)?.join(output.name));
    }
    if rule.action != "rename" {
        return Err("validation.action".into());
    }
    let template = &rule.options.rename_template;
    validate_rename_template(template)?;
    let path = Path::new(&file.name);
    let stem = path.file_stem().unwrap_or_default().to_string_lossy();
    let extension = path.extension().unwrap_or_default().to_string_lossy();
    let now = Utc::now();
    let name = RENAME_TOKEN.replace_all(template, |capture: &regex::Captures<'_>| {
        match &capture[1] {
            "stem" => stem.to_string(),
            "filename" => file.name.clone(),
            "extension" => extension.to_string(),
            "year" => now.format("%Y").to_string(),
            "month" => now.format("%m").to_string(),
            "day" => now.format("%d").to_string(),
            _ => unreachable!("validated rename token"),
        }
    });
    let name = if extension.is_empty() { name.trim_end_matches('.') } else { &name };
    if !safe_rename_leaf(name) {
        return Err("validation.template".into());
    }
    Ok(file.path.parent().ok_or("Missing parent folder")?.join(name))
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
pub fn resolve_dest_dir(root: &Path, file: &FileInfo, rule: &Rule) -> Result<PathBuf, String> {
    validate_move_destination_template(&rule.destination)?;
    let resolved = resolve_destination(&rule.destination, file);
    if Path::new(&rule.destination).is_absolute() {
        if !Path::new(&resolved).is_absolute() {
            return Err("The expanded move destination must remain absolute".into());
        }
        Ok(PathBuf::from(resolved))
    } else {
        validate_relative_destination(&resolved)?;
        Ok(root.join(resolved))
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
    resolve_dest_dir(root, file, rule).map_or(true, |destination| path_is_already_filed(path, &destination))
}

fn path_is_already_filed(path: &Path, dest_dir: &Path) -> bool {
    let Some(base) = path.parent() else {
        return false;
    };
    if base.exists() && dest_dir.exists() {
        crate::safe_fs::directory_contains(base, dest_dir).unwrap_or(false)
    } else {
        // Pure destination planning when the output directory does not exist.
        // This branch is a skip hint, never filesystem authorization.
        crate::safe_fs::is_within_any_root(base, &[dest_dir.to_path_buf()])
    }
}

#[cfg(test)]
fn execute_rule(file_info: &FileInfo, rule: &Rule, root: &Path) -> Result<String, String> {
    match rule.action.as_str() {
        "move" => {
            let dest = resolve_dest_dir(root, file_info, rule)?;
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
    Retained { message: String },
    Skipped,
    Retry { reason: String },
}

pub(crate) struct WatchedRoot {
    pub(crate) id: i64,
    pub(crate) path: PathBuf,
    identity: FileIdentity,
}

pub(crate) type WatchedManualRoot = WatchedRoot;

impl WatchedRoot {
    pub(crate) fn capture(folder: &WatchedFolder) -> Result<Self, String> {
        let id = folder.id.ok_or("Watched folder has no identifier")?;
        if !folder.enabled || crate::db::is_folder_paused_mode(&folder.mode) {
            return Err("Folder is no longer an eligible watched root".into());
        }
        let path = PathBuf::from(&folder.path);
        crate::safe_fs::validate_root(&path)?;
        let _guard = crate::safe_fs::guard_directory(&path)?;
        let identity = crate::safe_fs::file_identity(&path)?;
        Ok(Self { id, path, identity })
    }

    pub(crate) fn matches_registration(&self, folder: &WatchedFolder) -> bool {
        folder.id == Some(self.id) && Path::new(&folder.path) == self.path.as_path()
    }

    pub(crate) fn guard_physical(&self) -> Result<AncestorGuard, String> {
        let guard = crate::safe_fs::guard_directory(&self.path)?;
        if crate::safe_fs::file_identity(&self.path)? != self.identity {
            return Err(format!("Watched folder changed identity: {}", self.path.display()));
        }
        Ok(guard)
    }

    fn guard_current(&self, folders: &[WatchedFolder]) -> Result<AncestorGuard, String> {
        if !folders.iter().any(|folder| self.matches_registration(folder)
            && folder.enabled && !crate::db::is_folder_paused_mode(&folder.mode)) {
            return Err(format!("Watched folder is no longer eligible: {}", self.path.display()));
        }
        self.guard_physical()
    }
}

#[derive(Clone, Copy)]
enum FolderAuthority<'a> {
    Automatic,
    RegisteredWatch(&'a WatchedRoot),
    Scheduled {
        root: &'a WatchedRoot,
        run_id: i64,
        is_current: &'a dyn Fn() -> bool,
        revoked: &'a std::cell::Cell<bool>,
    },
    WatchedManual(&'a WatchedManualRoot),
    ArchiveStaging,
}

impl FolderAuthority<'_> {
    fn uses_folder_protection(self) -> bool {
        // Imported payloads cannot supply the organizer's folder policy.
        !matches!(self, Self::ArchiveStaging)
    }
}

pub fn process_file_with_suppression(
    path: &Path,
    bypass_grace: bool,
    root: &Path,
    suppression: &SuppressionSet,
    ticket: &CancellationTicket,
    expected_identity: Option<&FileIdentity>,
    require_auto_root: bool,
) -> Result<ProcessOutcome, String> {
    let authority = if require_auto_root { FolderAuthority::Automatic } else { FolderAuthority::ArchiveStaging };
    process_file(path, bypass_grace, root, suppression, ticket, expected_identity, authority)
}

pub(crate) fn process_watched_file(
    path: &Path,
    root: &WatchedRoot,
    suppression: &SuppressionSet,
    ticket: &CancellationTicket,
    expected_identity: Option<&FileIdentity>,
) -> Result<ProcessOutcome, String> {
    process_file(path, false, &root.path, suppression, ticket, expected_identity, FolderAuthority::RegisteredWatch(root))
}

fn process_file(
    path: &Path,
    bypass_grace: bool,
    root: &Path,
    suppression: &SuppressionSet,
    ticket: &CancellationTicket,
    expected_identity: Option<&FileIdentity>,
    authority: FolderAuthority<'_>,
) -> Result<ProcessOutcome, String> {
    let _operation = suppression.with_operation();
    if let FolderAuthority::Scheduled { is_current, revoked, .. } = authority {
        if !is_current() {
            revoked.set(true);
            return Ok(ProcessOutcome::Skipped);
        }
    }
    if !ticket.is_current() || suppression.is_suppressed(path, Instant::now()) {
        return Ok(ProcessOutcome::Skipped);
    }
    let settings = get_settings().map_err(|e| format!("Cannot read organizer settings: {e}"))?;
    let folders = get_watched_folders().map_err(|e| format!("Cannot read watched folders: {e}"))?;
    if matches!(authority, FolderAuthority::Automatic) && !folders.iter().any(|folder| {
        folder.enabled && crate::db::is_folder_auto_mode(&folder.mode)
            && same_path(Path::new(&folder.path), root)
    }) {
        return Ok(ProcessOutcome::Skipped);
    }
    let _root_guard = match authority {
        FolderAuthority::RegisteredWatch(watched) | FolderAuthority::Scheduled { root: watched, .. } => {
            if !folders.iter().any(|folder| watched.matches_registration(folder)
                && folder.enabled && crate::db::is_folder_auto_mode(&folder.mode)) {
                return Ok(ProcessOutcome::Skipped);
            }
            Some(watched.guard_physical()?)
        }
        FolderAuthority::WatchedManual(watched) => Some(watched.guard_current(&folders)?),
        _ => {
            crate::safe_fs::validate_root(root)?;
            None
        }
    };
    if !path.parent().is_some_and(|parent| crate::safe_fs::directory_contains(parent, root).unwrap_or(false)) {
        return Err(format!("File is outside organizer root: {}", path.display()));
    }
    let use_folder_protection = authority.uses_folder_protection();
    if if use_folder_protection { organization_path_is_protected(path, root)? } else { should_ignore_file(path) } {
        return Ok(ProcessOutcome::Skipped);
    }
    if !bypass_grace && !check_grace_period(path, settings.grace_period_seconds)? {
        return Ok(ProcessOutcome::Retry { reason: "File is still settling".into() });
    }
    if settings.lock_check_enabled && is_file_locked(path) {
        return Ok(ProcessOutcome::Retry { reason: "File is locked".into() });
    }
    if crate::operations::suspicious_name(path) {
        return Ok(ProcessOutcome::Skipped);
    }
    let (mut source_file, _source_ancestors) = match crate::safe_fs::open_guarded_regular_file(path) {
        Ok(file) => file,
        Err(error) => return Ok(ProcessOutcome::Retry { reason: error }),
    };
    let identity = crate::safe_fs::identity_from_handle(&source_file)?;
    if expected_identity.is_some_and(|expected| expected != &identity) {
        return Ok(ProcessOutcome::Skipped);
    }
    let metadata = source_file.metadata().map_err(|error| error.to_string())?;
    let file_info = describe_file(path, &metadata).ok_or_else(|| format!("Cannot read file metadata: {}", path.display()))?;
    let canonical = std::fs::canonicalize(path).map_err(|error| error.to_string())?;
    let source = path.to_string_lossy().into_owned();
    if use_folder_protection && crate::db::is_organization_baseline(&source, &canonical.to_string_lossy())
        .map_err(|error| error.to_string())? {
        return Ok(ProcessOutcome::Skipped);
    }
    let rules = get_rules().map_err(|e| format!("Cannot read organizer rules: {e}"))?;
    let Some(rule) = matching_rule_in_root(&file_info, use_folder_protection.then_some(root), &folders, &rules).cloned() else {
        return Ok(ProcessOutcome::Skipped);
    };
    if rule.action == "ignore" { return Ok(ProcessOutcome::Skipped); }
    if rule.action == "delete" { return Err("File removal requires a verified recovery method".into()); }
    let requested = planned_destination(root, &file_info, &rule)?;
    let same_leaf = requested.file_name() == path.file_name();
    if same_path(path, &requested) || (rule.action == "move" && same_leaf && (
        requested.parent().is_some_and(|parent| path_is_already_filed(path, parent))
        || folders.iter().filter(|folder| folder.enabled).any(|folder| {
            is_already_filed(Path::new(&folder.path), path, &file_info, &rule)
        })))
    {
        return Ok(ProcessOutcome::Skipped);
    }
    let modified = metadata.modified().map_err(|error| error.to_string())?;
    let digest = crate::safe_fs::hash_handle(&mut source_file)?.to_hex();
    let fingerprints = crate::db::organization_fingerprints(&source, &canonical.to_string_lossy(), true)
        .map_err(|error| error.to_string())?;
    if crate::operations::fingerprints_match(&mut source_file, &fingerprints, digest.as_str())? {
        return Ok(ProcessOutcome::Skipped);
    }
    drop(source_file);
    let requested_string = requested.to_string_lossy().to_string();
    // If intent cannot be recorded, no filesystem mutation takes place.
    let intent = match authority {
        FolderAuthority::Scheduled { run_id, .. } => crate::db::begin_scheduled_move_intent(run_id, &source, &requested_string),
        _ => crate::db::begin_operation_intent("move", &source, Some(&requested_string)),
    }.map_err(|e| format!("Cannot record move intent for {source}: {e}"))?;
    suppression.arm(std::slice::from_ref(&requested), Instant::now(), SUPPRESSION_TTL);
    let moved = match crate::safe_fs::move_organization_file(path, &requested, &identity, digest.as_str(), modified, false) {
        Ok(moved) => moved,
        Err(error) => {
            if let Some(message) = record_retained_move(intent, "move", path, &error, suppression) {
                return Ok(ProcessOutcome::Retained { message });
            }
            let retry_source = matches!(authority, FolderAuthority::RegisteredWatch(_))
                && matches!(&error, MoveFailure::SourceSharingViolation(_));
            let error = error.to_string();
            return match crate::db::finalize_operation_intent(intent, "failed", None, Some(&error)) {
                Ok(()) if retry_source => Ok(ProcessOutcome::Retry { reason: error }),
                Ok(()) => Err(error),
                Err(record_error) => Err(format!("{error}; recording the failed move also failed: {record_error}")),
            };
        }
    };
    let destination = moved.destination.to_string_lossy().to_string();
    // Renew for the actual collision-selected destination, measured after the
    // operation so a slow copy does not consume the delivery lease.
    suppression.arm(&[requested, moved.destination.clone()], Instant::now(), SUPPRESSION_TTL);
    let log = ActionLog {
        id: None, timestamp: Utc::now(), source_path: source,
        destination_path: Some(destination.clone()), action: rule.action.clone(),
        file_name: file_info.name, file_type: rule.name.clone(), undone: false,
    };
    let (run_id, trigger) = match authority {
        FolderAuthority::Scheduled { run_id, .. } => (format!("schedule-{run_id}"), "scheduled"),
        FolderAuthority::RegisteredWatch(_) | FolderAuthority::Automatic => (crate::operations::new_run_id(), "automatic"),
        _ => (crate::operations::new_run_id(), "manual"),
    };
    let history = crate::db::OrganizationHistory {
        run_id: &run_id, trigger, extension: &file_info.extension, size: file_info.size, digest: digest.as_str(),
    };
    let warning = crate::db::finalize_organization_move(intent, &log, &moved, history)
        .err().map(|error| format!("Moved to {destination}, but history/inventory recording failed: {error}"));
    Ok(ProcessOutcome::Organized { rule, destination, warning })
}

pub(crate) fn record_retained_move(
    intent: i64, action: &str, source: &Path, error: &MoveFailure, suppression: &SuppressionSet,
) -> Option<String> {
    let MoveFailure::RetainedOutput { path, source_preserved, .. } = error else { return None; };
    suppression.arm(&[source.to_path_buf(), path.clone()], Instant::now(), SUPPRESSION_TTL);
    let source_state = if *source_preserved { "The source was preserved." }
        else { "The source state could not be confirmed." };
    let mut message = format!("{error}. {source_state} Recovery record {intent} remains pending.");
    if let Err(error) = crate::db::observe_retained_move_output(intent, action, &path.to_string_lossy(), &message) {
        message.push_str(&format!(" Could not save the retained path: {error}."));
    }
    if let Err(error) = crate::db::mark_retained_move_inventory_stale(source, path) {
        message.push_str(&format!(" Could not mark inventory stale: {error}."));
    }
    Some(message)
}

pub fn same_path(left: &Path, right: &Path) -> bool {
    crate::safe_fs::normalize_lexically(left).to_string_lossy().to_lowercase()
        == crate::safe_fs::normalize_lexically(right).to_string_lossy().to_lowercase()
}

pub fn manual_scan_folder_with_suppression(folder: &str, suppression: &SuppressionSet)
    -> Result<Vec<(String, String, String)>, String>
{
    let folders = get_watched_folders().map_err(|error| error.to_string())?;
    let folder = folders.iter().find(|candidate| candidate.path == folder)
        .ok_or("Folder is no longer an eligible watched root")?;
    scan_watched_manual_folder(&WatchedManualRoot::capture(folder)?, suppression)
}

pub(crate) fn scan_watched_manual_folder(root: &WatchedManualRoot, suppression: &SuppressionSet)
    -> Result<Vec<(String, String, String)>, String>
{
    scan_folder(&root.path, suppression, FolderAuthority::WatchedManual(root), None)
        .map_err(|error| error.message)
}

pub(crate) type SortedFiles = Vec<(String, String, String)>;

#[derive(Debug)]
pub(crate) struct FolderScanError {
    pub results: SortedFiles,
    pub message: String,
}

pub(crate) fn manual_scan_folder_cancelable(root: &Path, suppression: &SuppressionSet, cancelled: &std::sync::atomic::AtomicBool)
    -> Result<SortedFiles, FolderScanError>
{
    scan_folder(root, suppression, FolderAuthority::ArchiveStaging, Some(cancelled))
}

pub fn scheduled_scan_folder(folder: &str, suppression: &SuppressionSet)
    -> Result<Vec<(String, String, String)>, String>
{
    scan_folder(Path::new(folder), suppression, FolderAuthority::Automatic, None)
        .map_err(|error| error.message)
}

pub(crate) fn scheduled_scan_folder_if_current(
    root: &WatchedRoot, suppression: &SuppressionSet, run_id: i64, is_current: &dyn Fn() -> bool,
    progress: &mut ScheduleRunProgress,
) {
    if let Err(error) = scan_scheduled_folder(root, suppression, run_id, is_current, progress, |_| {}) {
        progress.record_error(format!("{}: {error}", root.path.display()));
    }
}

fn scan_scheduled_folder(
    root: &WatchedRoot, suppression: &SuppressionSet, run_id: i64, is_current: &dyn Fn() -> bool,
    progress: &mut ScheduleRunProgress, before_file: impl FnMut(&Path),
) -> Result<(), String> {
    let entries = {
        let _guard = root.guard_physical()?;
        load_mouziignore(&root.path.to_string_lossy())?;
        fs::read_dir(&root.path).map_err(|error| error.to_string())?
    };
    let entries = entries.map(|entry| entry.map(|entry| entry.path()).map_err(|error| error.to_string()));
    scan_scheduled_entries(root, suppression, run_id, is_current, progress, entries, before_file);
    Ok(())
}

fn scan_scheduled_entries(
    root: &WatchedRoot, suppression: &SuppressionSet, run_id: i64, is_current: &dyn Fn() -> bool,
    progress: &mut ScheduleRunProgress, entries: impl IntoIterator<Item = Result<PathBuf, String>>,
    mut before_file: impl FnMut(&Path),
) {
    let revoked = std::cell::Cell::new(false);
    let authority = FolderAuthority::Scheduled { root, run_id, is_current, revoked: &revoked };
    for entry in entries {
        let path = match entry {
            Ok(path) => path,
            Err(error) => { progress.record_error(format!("{}: {error}", root.path.display())); continue; }
        };
        if !path.is_file() { continue; }
        let ticket = suppression.ticket(&path);
        before_file(&path);
        match process_file(&path, false, &root.path, suppression, &ticket, None, authority) {
            Ok(ProcessOutcome::Organized { warning, .. }) => {
                progress.organized += 1;
                if let Some(warning) = warning { progress.record_error(warning); }
            }
            Ok(ProcessOutcome::Retry { .. }) => progress.deferred += 1,
            Ok(ProcessOutcome::Skipped) => {}
            Ok(ProcessOutcome::Retained { message }) | Err(message) => {
                progress.record_error(format!("{}: {message}", path.display()));
            }
        }
        if revoked.get() {
            progress.revoked = true;
            break;
        }
    }
}

fn scan_folder(root: &Path, suppression: &SuppressionSet, authority: FolderAuthority<'_>, cancelled: Option<&std::sync::atomic::AtomicBool>)
    -> Result<SortedFiles, FolderScanError>
{
    let entries = (|| {
        crate::safe_fs::validate_root(root)?;
        if authority.uses_folder_protection() {
            load_mouziignore(&root.to_string_lossy())?;
        }
        fs::read_dir(root).map_err(|error| error.to_string())
    })().map_err(|message| FolderScanError { results: Vec::new(), message })?;
    let bypass_grace = matches!(authority, FolderAuthority::WatchedManual(_) | FolderAuthority::ArchiveStaging);
    scan_folder_entries(
        entries.map(|entry| entry.map(|entry| entry.path()).map_err(|error| error.to_string())),
        cancelled,
        |path| process_file(path, bypass_grace, root, suppression, &suppression.ticket(path), None, authority),
    )
}

fn scan_folder_entries(
    entries: impl IntoIterator<Item = Result<PathBuf, String>>,
    cancelled: Option<&std::sync::atomic::AtomicBool>,
    mut process: impl FnMut(&Path) -> Result<ProcessOutcome, String>,
) -> Result<SortedFiles, FolderScanError> {
    let mut results = Vec::new();
    let mut errors = Vec::new();
    for entry in entries {
        let entry = if cancelled.is_some_and(|flag| flag.load(std::sync::atomic::Ordering::Relaxed)) {
            Err(format!("Import cancelled after organizing {} files", results.len()))
        } else {
            entry
        };
        let path = match entry {
            Ok(path) => path,
            Err(mut message) => {
                if !errors.is_empty() {
                    message.push_str("; ");
                    message.push_str(&errors.join("; "));
                }
                return Err(FolderScanError { results, message });
            }
        };
        if !path.is_file() { continue; }
        let file_name = path.file_name().unwrap_or_default().to_string_lossy().to_string();
        match process(&path) {
            Ok(ProcessOutcome::Organized { rule, destination, warning }) => {
                results.push((file_name, rule.name, destination));
                if let Some(warning) = warning { errors.push(warning); }
            }
            Ok(ProcessOutcome::Skipped | ProcessOutcome::Retry { .. }) => {}
            Ok(ProcessOutcome::Retained { message }) | Err(message) => errors.push(format!("{file_name}: {message}")),
        }
    }
    if errors.is_empty() { Ok(results) }
    else {
        let message = format!("Organized {} files; {}", results.len(), errors.join("; "));
        Err(FolderScanError { results, message })
    }
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
            ..Rule::default()
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
            size: 0,
            modified_day: None,
        }
    }

    #[test]
    fn release_rule_conditions_and_extension_mapping_use_source_metadata() {
        let mut file = file_at(r"C:\Watched\holiday.JPEG");
        file.size = 4096;
        file.modified_day = NaiveDate::from_ymd_opt(2026, 11, 1);
        let mut rule = rule("Photos");
        rule.extensions = vec!["jpeg".into()];
        rule.options.min_size = Some(4096);
        rule.options.max_size = Some(4096);
        rule.options.modified_after = Some("2026-11-01".into());
        rule.options.modified_before = Some("2026-11-01".into());
        rule.normalize_extensions = true;
        rule.extension_mappings = "jpeg:jpg".into();

        assert!(matches_rule(&file, &rule));
        let output = output_file_info(&file, &rule);
        assert_eq!(output.name, "holiday.jpg");
        assert_eq!(output.path, file.path);
        assert_eq!(output.size, file.size);
        file.size -= 1;
        assert!(!matches_rule(&file, &rule));
        file.size += 1;
        rule.options.modified_before = Some("2026-10-31".into());
        assert!(!matches_rule(&file, &rule));
        rule.options.modified_before = Some("invalid".into());
        assert!(!matches_rule(&file, &rule));
    }

    #[test]
    fn relative_destination_cannot_change_the_root_before_or_after_expansion() {
        let root = Path::new(r"C:\W");
        let file = file_at(r"C:\W\README");
        for invalid in [r"\outside", r"C:outside", r"sub\..\outside", r"sub\.. \outside", "sub/./outside"] {
            assert!(validate_move_destination_template(invalid).is_err(), "accepted {invalid:?}");
            assert!(resolve_dest_dir(root, &file, &rule(invalid)).is_err(), "used legacy {invalid:?}");
        }
        assert!(validate_move_destination_template("{extension}/outside").is_ok());
        assert!(resolve_dest_dir(root, &file, &rule("{extension}/outside")).is_err());
        assert_eq!(resolve_dest_dir(root, &file, &rule("Docs/{filename}")).unwrap(), root.join("Docs/README"));
        assert_eq!(resolve_dest_dir(root, &file, &rule(r"D:\Filed\{filename}")).unwrap(), Path::new(r"D:\Filed\README"));
    }

    #[test]
    fn manual_batch_authority_is_revoked_when_paused_or_replaced() {
        let _serial = crate::db::serialise_test_db();
        crate::db::init_test_db();
        let fixture_root = std::env::temp_dir().join(format!(
            "mouzi-manual-authority-{}-{}", std::process::id(), Utc::now().timestamp_nanos_opt().unwrap()
        ));
        fs::create_dir(&fixture_root).unwrap();
        let fixture_root = fs::canonicalize(fixture_root).unwrap();
        let root = fixture_root.join("watched");
        fs::create_dir(&root).unwrap();
        let folder_id = crate::db::add_watched_folder(&root.to_string_lossy(), crate::db::FOLDER_MODE_MANUAL).unwrap();
        let mut custom = rule("Filed");
        custom.extensions = vec!["authorityx".into()];
        custom.priority = i32::MIN;
        let rule_id = crate::db::add_rule(&custom).unwrap();
        struct Fixture { path: PathBuf, folder_id: i64, rule_id: i64 }
        impl Drop for Fixture {
            fn drop(&mut self) {
                if let Ok(conn) = crate::db::lock_db() {
                    for source in [
                        self.path.join("watched").join("first.authorityx"),
                        self.path.join("archive-staging").join("archive.authorityx"),
                    ] {
                        let source = source.to_string_lossy();
                        let _ = conn.execute("DELETE FROM action_logs WHERE source_path=?1", [source.as_ref()]);
                        let _ = conn.execute("DELETE FROM operation_intents WHERE source_path=?1", [source.as_ref()]);
                    }
                }
                let _ = crate::db::delete_rule(self.rule_id);
                let _ = crate::db::remove_watched_folder(self.folder_id);
                let _ = fs::remove_dir_all(&self.path);
            }
        }
        let _fixture = Fixture { path: fixture_root.clone(), folder_id, rule_id };
        let first = root.join("first.authorityx");
        let second = root.join("second.authorityx");
        fs::write(&first, b"first").unwrap();
        fs::write(&second, b"second").unwrap();
        let row = get_watched_folders().unwrap().into_iter().find(|folder| folder.id == Some(folder_id)).unwrap();
        let authority = WatchedManualRoot::capture(&row).unwrap();
        let suppression = SuppressionSet::new();
        let first_outcome = process_file(&first, true, &root, &suppression, &suppression.ticket(&first),
            None, FolderAuthority::WatchedManual(&authority)).unwrap();
        assert!(matches!(first_outcome, ProcessOutcome::Organized { warning: None, .. }));

        {
            let _operation = suppression.with_operation();
            crate::db::update_folder_mode(folder_id, crate::db::FOLDER_MODE_PAUSED).unwrap();
        }
        assert!(scan_watched_manual_folder(&authority, &suppression).unwrap_err().contains("no longer eligible"));
        assert_eq!(fs::read(&second).unwrap(), b"second");

        let staging = fixture_root.join("archive-staging");
        fs::create_dir(&staging).unwrap();
        fs::write(staging.join("archive.authorityx"), b"archive").unwrap();
        let cancelled = std::sync::atomic::AtomicBool::new(false);
        assert_eq!(manual_scan_folder_cancelable(&staging, &suppression, &cancelled).unwrap().len(), 1);
        assert_eq!(fs::read(staging.join("Filed/archive.authorityx")).unwrap(), b"archive");

        crate::db::update_folder_mode(folder_id, crate::db::FOLDER_MODE_MANUAL).unwrap();
        fs::rename(&root, fixture_root.join("old-watched")).unwrap();
        fs::create_dir(&root).unwrap();
        fs::write(&second, b"replacement").unwrap();
        assert!(scan_watched_manual_folder(&authority, &suppression).unwrap_err().contains("changed identity"));
        assert_eq!(fs::read(&second).unwrap(), b"replacement");

        crate::db::update_folder_mode(folder_id, crate::db::FOLDER_MODE_SILENT).unwrap();
        fs::OpenOptions::new().write(true).open(&second).unwrap()
            .set_modified(SystemTime::now() - Duration::from_secs(86_401)).unwrap();
        let checks = std::cell::Cell::new(0);
        let mut scheduled = ScheduleRunProgress::default();
        let row = get_watched_folders().unwrap().into_iter().find(|folder| folder.id == Some(folder_id)).unwrap();
        let scheduled_root = WatchedRoot::capture(&row).unwrap();
        scheduled_scan_folder_if_current(&scheduled_root, &suppression, 0, &|| {
            checks.set(checks.get() + 1);
            false
        }, &mut scheduled);
        assert_eq!(checks.get(), 1);
        assert!(scheduled.revoked);
        assert_eq!(scheduled.organized, 0);
        assert_eq!(fs::read(&second).unwrap(), b"replacement");
    }

    #[test]
    fn archive_sort_preserves_ignore_payload_without_applying_it() {
        const FILE_NAME: &str = "entry.archiveignorex";
        const CASES: [(&str, &[u8]); 2] = [("wildcard", b"*\n"), ("malformed", &[0xff, 0xfe])];

        struct Fixture { root: PathBuf, folder: i64, rule: i64 }
        impl Drop for Fixture {
            fn drop(&mut self) {
                if let Ok(conn) = crate::db::lock_db() {
                    for (case, _) in CASES {
                        let source = self.root.join(case).join(FILE_NAME);
                        let source = source.to_string_lossy();
                        let _ = conn.execute("DELETE FROM action_logs WHERE source_path=?1", [source.as_ref()]);
                        let _ = conn.execute("DELETE FROM operation_intents WHERE source_path=?1", [source.as_ref()]);
                    }
                }
                let _ = crate::db::delete_rule(self.rule);
                let _ = crate::db::remove_watched_folder(self.folder);
                let _ = fs::remove_dir_all(&self.root);
            }
        }

        let _serial = crate::db::serialise_test_db();
        crate::db::init_test_db();
        let root = std::env::temp_dir().join(format!(
            "mouzi-archive-ignore-{}-{}", std::process::id(), Utc::now().timestamp_nanos_opt().unwrap()
        ));
        fs::create_dir(&root).unwrap();
        let root = fs::canonicalize(root).unwrap();
        let watched = root.join("watched");
        fs::create_dir(&watched).unwrap();
        let folder = crate::db::add_watched_folder(&watched.to_string_lossy(), crate::db::FOLDER_MODE_MANUAL).unwrap();
        let mut custom = rule("Filed");
        custom.extensions = vec!["archiveignorex".into()];
        custom.priority = i32::MIN;
        let rule_id = crate::db::add_rule(&custom).unwrap();
        let _fixture = Fixture { root: root.clone(), folder, rule: rule_id };
        let suppression = SuppressionSet::new();
        let cancelled = std::sync::atomic::AtomicBool::new(false);

        for (case, policy) in CASES {
            let staging = root.join(case);
            fs::create_dir(&staging).unwrap();
            let control = staging.join(".mouziignore");
            fs::write(&control, policy).unwrap();
            fs::write(staging.join(FILE_NAME), b"archive payload").unwrap();

            assert_eq!(manual_scan_folder_cancelable(&staging, &suppression, &cancelled).unwrap().len(), 1);
            assert_eq!(fs::read(staging.join("Filed").join(FILE_NAME)).unwrap(), b"archive payload");
            assert_eq!(fs::read(control).unwrap(), policy);
        }

        let row = get_watched_folders().unwrap().into_iter().find(|row| row.id == Some(folder)).unwrap();
        let watched_root = WatchedManualRoot::capture(&row).unwrap();
        let watched_source = watched.join(FILE_NAME);
        let watched_control = watched.join(".mouziignore");
        fs::write(&watched_source, b"watched payload").unwrap();
        fs::write(&watched_control, CASES[0].1).unwrap();
        assert!(scan_watched_manual_folder(&watched_root, &suppression).unwrap().is_empty());
        fs::write(&watched_control, CASES[1].1).unwrap();
        assert!(scan_watched_manual_folder(&watched_root, &suppression).unwrap_err().contains(".mouziignore"));
        assert_eq!(fs::read(watched_source).unwrap(), b"watched payload");
    }

    #[test]
    fn partial_archive_receipts_keep_the_actual_destination() {
        use std::sync::atomic::{AtomicBool, Ordering};

        struct Fixture {
            root: PathBuf,
            first: PathBuf,
            rule: i64,
            intent: i64,
        }
        impl Drop for Fixture {
            fn drop(&mut self) {
                if let Ok(conn) = crate::db::lock_db() {
                    let source = self.first.to_string_lossy();
                    let _ = conn.execute("DELETE FROM action_logs WHERE source_path=?1", [source.as_ref()]);
                    let _ = conn.execute("DELETE FROM operation_intents WHERE source_path=?1", [source.as_ref()]);
                    let _ = conn.execute("DELETE FROM operation_intents WHERE id=?1", [self.intent]);
                }
                let _ = crate::db::delete_rule(self.rule);
                let _ = fs::remove_dir_all(&self.root);
            }
        }

        let _serial = crate::db::serialise_test_db();
        crate::db::init_test_db();
        for stop in ["entry", "cancel", "warning"] {
            let root = std::env::temp_dir().join(format!(
                "mouzi-partial-archive-{stop}-{}-{}", std::process::id(), Utc::now().timestamp_nanos_opt().unwrap()
            ));
            fs::create_dir(&root).unwrap();
            let root = fs::canonicalize(root).unwrap();
            let staging = root.join("staging");
            let filed = root.join("filed");
            fs::create_dir(&staging).unwrap();
            fs::create_dir(&filed).unwrap();
            let first = staging.join("first.archivepartialx");
            let second = staging.join("second.archivepartialx");
            let occupied = filed.join("first.archivepartialx");
            let destination = filed.join("first_0.archivepartialx");
            fs::write(&first, b"first").unwrap();
            fs::write(&second, b"second").unwrap();
            fs::write(&occupied, b"occupied").unwrap();

            let mut custom = rule(&filed.to_string_lossy());
            custom.name = "Archive fixture".into();
            custom.priority = i32::MIN;
            custom.extensions = vec!["archivepartialx".into()];
            let rule_id = crate::db::add_rule(&custom).unwrap();
            let source = root.join("input.zip");
            let intent = crate::db::begin_operation_intent("archive_import", &source.to_string_lossy(), None).unwrap();
            let _fixture = Fixture { root, first: first.clone(), rule: rule_id, intent };
            crate::archive::save_manifest(&staging, Some(&source), "extracted", &[], None).unwrap();
            crate::db::observe_pending_archive_staging(intent, &staging.to_string_lossy()).unwrap();

            let intent_state = || {
                crate::db::lock_db().unwrap().query_row(
                    "SELECT status, actual_destination FROM operation_intents WHERE id=?1",
                    [intent],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                ).unwrap()
            };
            let expected_staging = staging.to_string_lossy().into_owned();
            assert_eq!(intent_state(), ("pending".to_owned(), expected_staging.clone()));

            let cancelled = AtomicBool::new(false);
            let suppression = SuppressionSet::new();
            let entries = [
                Ok(first.clone()),
                if stop == "entry" { Err("forced directory entry failure".into()) } else { Ok(second.clone()) },
            ];
            let sorting = scan_folder_entries(entries, Some(&cancelled), |path| {
                if path == second.as_path() {
                    return Err("forced second-file error".into());
                }
                let mut outcome = process_file(
                    path, true, &staging, &suppression, &suppression.ticket(path), None, FolderAuthority::ArchiveStaging,
                )?;
                let warning = match &mut outcome {
                    ProcessOutcome::Organized { warning, .. } => warning,
                    other => panic!("first file did not organize: {other:?}"),
                };
                assert!(warning.is_none(), "{warning:?}");
                if stop == "warning" {
                    *warning = Some("forced history warning".into());
                } else if stop == "cancel" {
                    cancelled.store(true, Ordering::Relaxed);
                }
                Ok(outcome)
            });
            let error = crate::commands::finish_archive_sort(intent, &staging, 2, sorting).unwrap_err();
            assert!(error.contains(match stop {
                "entry" => "forced directory entry failure",
                "cancel" => "Import cancelled",
                _ => "forced history warning",
            }), "{error}");
            assert_eq!(intent_state(), ("partial".to_owned(), expected_staging));

            let receipt: serde_json::Value = serde_json::from_slice(
                &fs::read(staging.with_extension("manifest.json")).unwrap(),
            ).unwrap();
            assert_eq!(receipt["status"], "partial");
            assert_eq!(receipt["sortedResults"], serde_json::json!([
                ["first.archivepartialx", "Archive fixture", destination.to_string_lossy().as_ref()]
            ]));
            let remaining = receipt["remainingEntries"].as_array().unwrap();
            assert_eq!(remaining.len(), 1);
            assert_eq!(remaining[0]["name"], "second.archivepartialx");
            assert!(!first.exists());
            assert_eq!(fs::read(destination).unwrap(), b"first");
            assert_eq!(fs::read(occupied).unwrap(), b"occupied");
            assert_eq!(fs::read(second).unwrap(), b"second");
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
            PathBuf::from(resolve_destination(&r.destination, file))
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
            let after = resolve_dest_dir(root, &f, &r).expect("default rule destination");
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
        let docs = root.join("Docs");
        fs::create_dir(&docs).unwrap();
        let indexed_source = docs.join("receipt.auditx");
        fs::write(&indexed_source, b"receipt").unwrap();
        let source = if cfg!(windows) {
            root.join("docs").join("receipt.auditx")
        } else {
            indexed_source.clone()
        };
        let root_path = root.to_string_lossy().into_owned();
        let scan = crate::scan::scan_roots(std::slice::from_ref(&root_path), |_| {});
        assert_eq!(scan[0].status, "complete", "{:?}", scan[0].error);
        let before = crate::db::get_inventory_files("size", None, Some(&root_path), None, 10).unwrap();
        assert_eq!(before.len(), 1);
        assert_eq!(Path::new(&before[0].path), indexed_source);
        let suppression = SuppressionSet::new();
        let ticket = suppression.ticket(&source);
        let folder = crate::db::get_watched_folders().unwrap().into_iter()
            .find(|folder| folder.id == Some(folder_id)).unwrap();
        let authority = WatchedManualRoot::capture(&folder).unwrap();
        let outcome = process_file(&source, true, &root, &suppression, &ticket, None,
            FolderAuthority::WatchedManual(&authority)).unwrap();
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
        let remaining: i64 = conn.query_row("SELECT COUNT(*) FROM file_inventory WHERE root_path=?1", [&root_path], |row| row.get(0)).unwrap();
        assert_eq!(remaining, 1, "the move must replace the canonical source inventory row");
        let status: String = conn.query_row("SELECT status FROM operation_intents WHERE source_path=?1", [source.to_string_lossy().as_ref()], |row| row.get(0)).unwrap();
        assert_eq!(status, "completed");
        conn.execute("DELETE FROM action_logs WHERE source_path=?1", [source.to_string_lossy().as_ref()]).unwrap();
    }

    #[test]
    fn scheduled_scan_stops_after_a_committed_mid_batch_disable() {
        use crate::db;
        let _serial = db::serialise_test_db();
        db::init_test_db();
        let original = crate::scheduler::schedule_from_settings(db::get_settings().unwrap());
        let path = std::env::temp_dir().join(format!(
            "mouzi-schedule-{}-{}", std::process::id(), Utc::now().timestamp_nanos_opt().unwrap()
        ));
        fs::create_dir(&path).unwrap();
        let path = fs::canonicalize(path).unwrap();
        let folder = db::add_watched_folder(&path.to_string_lossy(), db::FOLDER_MODE_SILENT).unwrap();
        let mut custom = rule("Filed");
        custom.extensions = vec!["scheduledrevocation".into()];
        custom.priority = i32::MIN;
        let rule_id = db::add_rule(&custom).unwrap();
        let run = {
            let conn = db::lock_db().unwrap();
            conn.execute("INSERT INTO schedule_runs(status,claimed_at,occurrences) VALUES('claimed','fixture',1)", []).unwrap();
            conn.last_insert_rowid()
        };
        struct Fixture { path: PathBuf, folder: i64, rule: i64, run: i64, original: db::ScheduleSettings }
        impl Drop for Fixture {
            fn drop(&mut self) {
                if let Ok(conn) = db::lock_db() {
                    let _ = conn.execute("DELETE FROM action_logs WHERE source_path IN (SELECT source_path FROM operation_intents WHERE schedule_run_id=?1)", [self.run]);
                    let _ = conn.execute("DELETE FROM operation_intents WHERE schedule_run_id=?1", [self.run]);
                    let _ = conn.execute("DELETE FROM schedule_runs WHERE id=?1", [self.run]);
                }
                let _ = db::update_schedule_settings(&self.original);
                let _ = db::delete_rule(self.rule);
                let _ = db::remove_watched_folder(self.folder);
                let _ = fs::remove_dir_all(&self.path);
            }
        }
        let _fixture = Fixture { path: path.clone(), folder, rule: rule_id, run, original };
        for name in ["first.scheduledrevocation", "second.scheduledrevocation"] {
            let file = path.join(name);
            fs::write(&file, name.as_bytes()).unwrap();
            fs::OpenOptions::new().write(true).open(&file).unwrap()
                .set_modified(SystemTime::now() - Duration::from_secs(86_401)).unwrap();
        }
        let scheduler = crate::scheduler::Scheduler::new();
        let suppression = SuppressionSet::new();
        let mut schedule = db::ScheduleSettings {
            schedule_enabled: true, schedule_times_per_day: 1, schedule_time_1: Some("08:00".into()),
            schedule_time_2: None, schedule_time_3: None, schedule_time_4: None,
        };
        scheduler.save_schedule(&suppression, &schedule).unwrap();
        let revision = scheduler.revision();
        db::start_schedule_run(run).unwrap();
        let mut selected = Vec::new();
        let mut progress = ScheduleRunProgress::default();
        let row = get_watched_folders().unwrap().into_iter().find(|row| row.id == Some(folder)).unwrap();
        let root = WatchedRoot::capture(&row).unwrap();
        scan_scheduled_folder(&root, &suppression, run, &|| scheduler.revision() == revision, &mut progress, |file| {
            selected.push(file.to_path_buf());
            if selected.len() == 2 {
                assert!(!selected[0].exists(), "first move must finish before disable");
                schedule.schedule_enabled = false;
                assert!(!scheduler.save_schedule(&suppression, &schedule).unwrap().schedule_enabled);
            }
        }).unwrap();
        assert_eq!(selected.len(), 2);
        assert_eq!((progress.organized, progress.errors, progress.revoked), (1, 0, true));
        let first = selected[0].file_name().unwrap().to_str().unwrap();
        let second = selected[1].file_name().unwrap().to_str().unwrap();
        assert_eq!(fs::read(path.join("Filed").join(first)).unwrap(), first.as_bytes());
        assert_eq!(fs::read(&selected[1]).unwrap(), second.as_bytes());
        assert!(!path.join("Filed").join(second).exists());
        db::finish_schedule_run(run, &progress).unwrap();
        let receipt = db::recent_schedule_runs().unwrap().into_iter().find(|row| row.id == run).unwrap();
        assert_eq!((receipt.status.as_str(), receipt.organized), ("interrupted", 1));
        let conn = db::lock_db().unwrap();
        let linked: i64 = conn.query_row("SELECT COUNT(*) FROM operation_intents WHERE schedule_run_id=?1 AND status='completed'", [run], |row| row.get(0)).unwrap();
        let history: i64 = conn.query_row("SELECT COUNT(*) FROM action_logs WHERE source_path=?1", [selected[0].to_string_lossy().as_ref()], |row| row.get(0)).unwrap();
        let denied: i64 = conn.query_row("SELECT COUNT(*) FROM operation_intents WHERE source_path=?1", [selected[1].to_string_lossy().as_ref()], |row| row.get(0)).unwrap();
        assert_eq!((linked, history, denied), (1, 1, 0));
    }

    #[test]
    fn scheduled_scan_rejects_a_directory_replaced_between_moves() {
        use crate::db;
        let _serial = db::serialise_test_db();
        db::init_test_db();
        let fixture = std::env::temp_dir().join(format!(
            "mouzi-schedule-root-{}-{}", std::process::id(), Utc::now().timestamp_nanos_opt().unwrap()
        ));
        fs::create_dir(&fixture).unwrap();
        let fixture = fs::canonicalize(fixture).unwrap();
        let root = fixture.join("watched");
        let original = fixture.join("original");
        fs::create_dir(&root).unwrap();
        let folder = db::add_watched_folder(&root.to_string_lossy(), db::FOLDER_MODE_SILENT).unwrap();
        let mut custom = rule("Filed");
        custom.extensions = vec!["scheduledroot".into()];
        custom.priority = i32::MIN;
        let rule_id = db::add_rule(&custom).unwrap();
        let run = {
            let conn = db::lock_db().unwrap();
            conn.execute("INSERT INTO schedule_runs(status,claimed_at,occurrences) VALUES('claimed','fixture',1)", []).unwrap();
            conn.last_insert_rowid()
        };
        struct Fixture { path: PathBuf, folder: i64, rule: i64, run: i64 }
        impl Drop for Fixture {
            fn drop(&mut self) {
                if let Ok(conn) = db::lock_db() {
                    let _ = conn.execute("DELETE FROM action_logs WHERE source_path IN (SELECT source_path FROM operation_intents WHERE schedule_run_id=?1)", [self.run]);
                    let _ = conn.execute("DELETE FROM operation_intents WHERE schedule_run_id=?1", [self.run]);
                    let _ = conn.execute("DELETE FROM schedule_runs WHERE id=?1", [self.run]);
                }
                let _ = db::delete_rule(self.rule);
                let _ = db::remove_watched_folder(self.folder);
                let _ = fs::remove_dir_all(&self.path);
            }
        }
        let _fixture = Fixture { path: fixture, folder, rule: rule_id, run };
        let row = get_watched_folders().unwrap().into_iter().find(|row| row.id == Some(folder)).unwrap();
        let captured = WatchedRoot::capture(&row).unwrap();
        let first = root.join("first.scheduledroot");
        let second = root.join("second.scheduledroot");
        let write_aged = |path: &Path, contents: &[u8]| {
            fs::write(path, contents).unwrap();
            fs::OpenOptions::new().write(true).open(path).unwrap()
                .set_modified(SystemTime::now() - Duration::from_secs(86_401)).unwrap();
        };
        write_aged(&first, b"first");
        write_aged(&second, b"original second");
        db::start_schedule_run(run).unwrap();
        let suppression = SuppressionSet::new();
        let mut progress = ScheduleRunProgress::default();
        // Supply the two enumerated paths without keeping a Windows directory
        // iterator open across the injected rename.
        scan_scheduled_entries(&captured, &suppression, run, &|| true, &mut progress,
            [Ok(first.clone()), Ok(second.clone())], |path| {
                if path == second.as_path() {
                    assert!(!first.exists(), "first move must finish before replacement");
                    fs::rename(&root, &original).unwrap();
                    fs::create_dir(&root).unwrap();
                    write_aged(&second, b"replacement second");
                }
            });
        assert_eq!((progress.organized, progress.errors, progress.revoked), (1, 1, false));
        assert!(progress.message.as_deref().is_some_and(|message| message.contains("changed identity")));
        assert_eq!(fs::read(original.join("Filed/first.scheduledroot")).unwrap(), b"first");
        assert_eq!(fs::read(original.join("second.scheduledroot")).unwrap(), b"original second");
        assert_eq!(fs::read(&second).unwrap(), b"replacement second");
        assert!(!root.join("Filed").exists());
        db::finish_schedule_run(run, &progress).unwrap();
        let receipt = db::recent_schedule_runs().unwrap().into_iter().find(|row| row.id == run).unwrap();
        assert_eq!((receipt.status.as_str(), receipt.organized, receipt.errors), ("failed", 1, 1));
        let conn = db::lock_db().unwrap();
        let linked: i64 = conn.query_row("SELECT COUNT(*) FROM operation_intents WHERE schedule_run_id=?1", [run], |row| row.get(0)).unwrap();
        let denied: i64 = conn.query_row("SELECT COUNT(*) FROM operation_intents WHERE source_path=?1", [second.to_string_lossy().as_ref()], |row| row.get(0)).unwrap();
        assert_eq!((linked, denied), (1, 0));
    }
}
