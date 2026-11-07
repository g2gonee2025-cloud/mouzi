use crate::db::{self, ActionLog, OrganizationHistory, OrganizationLog, Rule, WatchedFolder};
use crate::rules::{self, FileInfo};
use crate::safe_fs::{self, AncestorGuard, FileIdentity, RootScope, ValidatedFile};
use crate::suppress::{SuppressionSet, SUPPRESSION_TTL};
use chrono::Utc;
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{Read, Seek};
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Mutex,
};
use std::time::{Duration, Instant, SystemTime};

static NEXT_ID: AtomicU64 = AtomicU64::new(0);
static PLANS: Lazy<Mutex<HashMap<String, CachedPlan>>> = Lazy::new(|| Mutex::new(HashMap::new()));

pub fn new_run_id() -> String {
    format!(
        "{}-{}-{}",
        Utc::now().timestamp_micros(),
        std::process::id(),
        NEXT_ID.fetch_add(1, Ordering::Relaxed)
    )
}

pub fn suspicious_name(path: &Path) -> bool {
    let name = path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_lowercase();
    let parts: Vec<_> = name.split('.').collect();
    parts.len() >= 3
        && ["exe", "scr", "com", "bat", "cmd", "ps1", "vbs", "js", "msi"]
            .contains(parts.last().unwrap())
        && parts[..parts.len() - 1].iter().any(|p| {
            [
                "pdf", "doc", "docx", "xls", "xlsx", "jpg", "jpeg", "png", "gif", "zip", "rar",
                "txt",
            ]
            .contains(p)
        })
}


fn path_key(path: &Path) -> String {
    let text = path.to_string_lossy().to_string();
    if cfg!(target_os = "windows") {
        text.to_lowercase()
    } else {
        text
    }
}

#[cfg(test)]
pub(crate) fn move_without_overwrite(source: &Path, destination: &Path) -> Result<(), String> {
    let (mut file, _ancestors) = safe_fs::open_guarded_regular_file(source)?;
    let identity = safe_fs::identity_from_handle(&file)?;
    let digest = safe_fs::hash_handle(&mut file)?.to_hex();
    drop(file);
    safe_fs::move_reviewed_file(source, destination, &identity, digest.as_str()).map(|_| ())
}

pub fn already_processed(path: &Path) -> Result<bool, String> {
    let canonical = fs::canonicalize(path).map_err(|error| error.to_string())?;
    let fingerprints = db::organization_fingerprints(&path.to_string_lossy(), &canonical.to_string_lossy(), true)
        .map_err(|error| error.to_string())?;
    if fingerprints.is_empty() {
        return Ok(false);
    }
    let (mut file, _ancestors) = safe_fs::open_guarded_regular_file(path)?;
    let digest = safe_fs::hash_handle(&mut file)?.to_hex();
    fingerprints_match(&mut file, &fingerprints, digest.as_str())
}

#[derive(Clone, Serialize)]
pub struct PreviewEntry {
    pub id: String,
    pub source: String,
    pub destination: Option<String>,
    pub rule: String,
    pub action: String,
    pub size: u64,
    pub warning: bool,
    pub error: Option<String>,
}
#[derive(Serialize)]
pub struct Preview {
    pub id: String,
    pub entries: Vec<PreviewEntry>,
}
struct Planned {
    entry: PreviewEntry,
    rule: Rule,
    source_identity: FileIdentity,
    root_identity: FileIdentity,
    digest: String,
    modified: SystemTime,
    requested_destination: PathBuf,
    destination_ancestor: PathBuf,
    destination_identity: FileIdentity,
}
struct CachedPlan {
    created: Instant,
    items: Vec<Planned>,
}
#[derive(Serialize)]
pub struct OperationResult {
    pub id: String,
    pub source: String,
    pub success: bool,
    pub error: Option<String>,
}

const PLAN_LIFETIME: Duration = Duration::from_secs(1800);

fn preview_paths(paths: Option<Vec<String>>, scope: &RootScope) -> Result<Vec<PathBuf>, String> {
    if let Some(paths) = paths {
        return Ok(paths.into_iter().map(PathBuf::from).collect());
    }
    let mut paths = Vec::new();
    for root in scope.active_roots() {
        let entries = fs::read_dir(&root).map_err(|error| format!("{}: {error}", root.display()))?;
        for entry in entries {
            let entry = entry.map_err(|error| error.to_string())?;
            if entry.file_type().map_err(|error| error.to_string())?.is_file() {
                paths.push(entry.path());
            }
        }
    }
    Ok(paths)
}

fn eligible_file(scope: &RootScope, path: &Path) -> Result<Option<ValidatedFile>, String> {
    let file = match scope.validate_file(path) {
        Ok(file) => file,
        Err(_) => return Ok(None),
    };
    if rules::organization_path_is_protected(path, &file.owning_root)?
        || db::is_organization_baseline(&path.to_string_lossy(), &file.canonical_path.to_string_lossy())
            .map_err(|error| error.to_string())?
    {
        return Ok(None);
    }
    Ok(Some(file))
}

fn sha256_handle(file: &mut fs::File) -> Result<String, String> {
    file.rewind().map_err(|error| error.to_string())?;
    let mut hasher = Sha256::new();
    let mut buffer = [0; 65536];
    loop {
        let count = file.read(&mut buffer).map_err(|error| error.to_string())?;
        if count == 0 {
            return Ok(format!("{:x}", hasher.finalize()));
        }
        hasher.update(&buffer[..count]);
    }
}

fn recorded_as_processed(path: &Path, file: &mut ValidatedFile, digest: &str) -> Result<bool, String> {
    let fingerprints = db::organization_fingerprints(
        &path.to_string_lossy(), &file.canonical_path.to_string_lossy(), false,
    ).map_err(|error| error.to_string())?;
    fingerprints_match(&mut file.file, &fingerprints, digest)
}

pub(crate) fn fingerprints_match(file: &mut fs::File, fingerprints: &[String], digest: &str) -> Result<bool, String> {
    if fingerprints.iter().any(|value| value.strip_prefix("blake3:") == Some(digest)) {
        return Ok(true);
    }
    if fingerprints.iter().any(|value| !value.contains(':')) {
        let legacy_digest = sha256_handle(file)?;
        return Ok(fingerprints.contains(&legacy_digest));
    }
    Ok(false)
}

fn destination_guard(path: &Path) -> Result<(PathBuf, FileIdentity, AncestorGuard), String> {
    let mut ancestor = path.parent().ok_or("Missing destination parent")?;
    loop {
        match fs::symlink_metadata(ancestor) {
            Ok(_) => {
                let guard = safe_fs::guard_directory(ancestor)?;
                let identity = safe_fs::file_identity(ancestor)?;
                return Ok((ancestor.to_path_buf(), identity, guard));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                ancestor = ancestor.parent().ok_or("No existing destination parent")?;
            }
            Err(error) => return Err(error.to_string()),
        }
    }
}

fn checked_destination(root: &Path, file: &FileInfo, rule: &Rule) -> Result<PathBuf, String> {
    let requested = rules::planned_destination(root, file, rule)?;
    let unchanged = path_key(&requested) == path_key(&file.path);
    let already_filed = rule.action == "move" && requested.file_name() == file.path.file_name()
        && rules::is_already_filed(root, &file.path, file, rule);
    if unchanged || already_filed {
        return Err("preview.unchanged".into());
    }
    Ok(requested)
}

pub(crate) fn preview(paths: Option<Vec<String>>, suppression: &SuppressionSet) -> Result<Preview, String> {
    let _operation = suppression.with_operation();
    let folders = db::get_watched_folders().map_err(|error| error.to_string())?;
    let scope = safe_fs::eligible_scope(&folders)?;
    let rules = db::get_rules().map_err(|error| error.to_string())?;
    let check_locks = db::get_settings().map_err(|error| error.to_string())?.lock_check_enabled;
    let mut paths = preview_paths(paths, &scope)?;
    paths.sort();
    paths.dedup();

    let id = new_run_id();
    let mut seen = HashSet::new();
    let mut reserved = HashSet::new();
    let mut items = Vec::new();
    let mut entries = Vec::new();
    for path in paths {
        // Probe before taking the write-denying file handle.
        let locked = check_locks && rules::is_file_locked(&path);
        let Some(mut file) = eligible_file(&scope, &path)? else { continue; };
        if !seen.insert(file.identity.clone()) { continue; }
        let info = rules::describe_file(&path, &file.metadata).ok_or("Invalid organization file name")?;
        let Some(rule) = rules::matching_rule_in_root(&info, Some(&file.owning_root), &folders, &rules) else {
            continue;
        };
        if rule.action == "ignore" { continue; }
        let mut entry = PreviewEntry {
            id: new_run_id(),
            source: path.to_string_lossy().into_owned(),
            destination: None,
            rule: rule.name.clone(),
            action: rule.action.clone(),
            size: info.size,
            warning: suspicious_name(&path),
            error: None,
        };
        let planned = (|| -> Result<Option<Planned>, String> {
            if rule.action == "delete" {
                return Err("preview.recoveryUnavailable".into());
            }
            if locked {
                return Err("preview.locked".into());
            }
            let digest = safe_fs::hash_handle(&mut file.file)?.to_hex().to_string();
            if recorded_as_processed(&path, &mut file, &digest)? {
                return Ok(None);
            }
            let modified = file.metadata.modified().map_err(|error| error.to_string())?;
            let requested = checked_destination(&file.owning_root, &info, rule)?;
            let parent = requested.parent().ok_or("Missing destination parent")?;
            let name = requested.file_name().and_then(|name| name.to_str()).ok_or("Invalid destination filename")?;
            let destination = safe_fs::try_unique_destination_with(parent, name, |candidate| {
                reserved.contains(&path_key(candidate))
            })?;
            let (ancestor, ancestor_identity, _destination_guard) = destination_guard(&destination)?;
            reserved.insert(path_key(&destination));
            entry.destination = Some(destination.to_string_lossy().into_owned());
            Ok(Some(Planned {
                entry: entry.clone(),
                rule: rule.clone(),
                source_identity: file.identity.clone(),
                root_identity: safe_fs::file_identity(&file.owning_root)?,
                digest,
                modified,
                requested_destination: requested,
                destination_ancestor: ancestor,
                destination_identity: ancestor_identity,
            }))
        })();
        match planned {
            Ok(Some(item)) => items.push(item),
            Ok(None) => continue,
            Err(error) => entry.error = Some(error),
        }
        entries.push(entry);
    }

    let mut plans = PLANS.lock().map_err(|_| "Organization preview cache is unavailable")?;
    plans.retain(|_, plan| plan.created.elapsed() < PLAN_LIFETIME);
    if plans.len() >= 32 {
        return Err("Too many open previews. Close an older preview.".into());
    }
    plans.insert(id.clone(), CachedPlan { created: Instant::now(), items });
    Ok(Preview { id, entries })
}

pub fn discard(id: &str) {
    if let Ok(mut plans) = PLANS.lock() {
        plans.remove(id);
    }
}

pub(crate) fn apply(
    id: &str, selected: &[String], suppression: &SuppressionSet,
) -> Result<Vec<OperationResult>, String> {
    let _operation = suppression.with_operation();
    let plan = PLANS.lock().map_err(|_| "Organization preview cache is unavailable")?
        .remove(id).ok_or("preview.expired")?;
    if plan.created.elapsed() >= PLAN_LIFETIME {
        return Err("preview.expired".into());
    }
    let selected: HashSet<_> = selected.iter().map(String::as_str).collect();
    let available: HashSet<_> = plan.items.iter().map(|item| item.entry.id.as_str()).collect();
    if !selected.is_subset(&available) {
        return Err("Selection contains an item outside the reviewed plan".into());
    }

    let folders = db::get_watched_folders().map_err(|error| error.to_string())?;
    let scope = safe_fs::eligible_scope(&folders)?;
    let rules = db::get_rules().map_err(|error| error.to_string())?;
    let run_id = new_run_id();
    let mut results = Vec::new();
    for item in plan.items.into_iter().filter(|item| selected.contains(item.entry.id.as_str())) {
        let result = apply_item(&item, &scope, &folders, &rules, &run_id, suppression);
        results.push(OperationResult {
            id: item.entry.id,
            source: item.entry.source,
            success: result.is_ok(),
            error: result.err(),
        });
    }
    Ok(results)
}

fn apply_item(
    item: &Planned, scope: &RootScope, folders: &[WatchedFolder], rules: &[Rule],
    run_id: &str, suppression: &SuppressionSet,
) -> Result<(), String> {
    let path = Path::new(&item.entry.source);
    let destination = Path::new(item.entry.destination.as_deref().ok_or("Missing reviewed destination")?);
    let file = eligible_file(scope, path)?.ok_or("preview.changed")?;
    if file.identity != item.source_identity
        || safe_fs::file_identity(&file.owning_root)? != item.root_identity
        || file.metadata.modified().ok() != Some(item.modified)
    {
        return Err("preview.changed".into());
    }
    let info = rules::describe_file(path, &file.metadata).ok_or("preview.changed")?;
    let rule = rules::matching_rule_in_root(&info, Some(&file.owning_root), folders, rules).ok_or("preview.changed")?;
    if *rule != item.rule || checked_destination(&file.owning_root, &info, rule)? != item.requested_destination {
        return Err("preview.changed".into());
    }
    // The reviewed ancestor was the deepest directory that existed at preview.
    // The mover validates any new descendants without following links. A prior
    // item in this same batch may already have created those directories.
    let _destination_guard = safe_fs::guard_directory(&item.destination_ancestor)?;
    if safe_fs::file_identity(&item.destination_ancestor)? != item.destination_identity {
        return Err("Reviewed destination folder changed; preview again".into());
    }

    let _source_guard = file.into_parent_guard();
    let intent = db::begin_operation_intent("move", &item.entry.source, item.entry.destination.as_deref())
        .map_err(|error| format!("Cannot record reviewed move intent: {error}"))?;
    suppression.arm(&[destination.to_path_buf()], Instant::now(), SUPPRESSION_TTL);
    let moved = match safe_fs::move_organization_file(
        path, destination, &item.source_identity, &item.digest, item.modified, true,
    ) {
        Ok(moved) => moved,
        Err(error) => {
            if let Some(message) = rules::record_retained_move(intent, "move", path, &error, suppression) {
                return Err(message);
            }
            let message = error.to_string();
            db::finalize_operation_intent(intent, "failed", None, Some(&message))
                .map_err(|error| format!("{message}; could not record failed move: {error}"))?;
            return Err(message);
        }
    };
    suppression.arm(std::slice::from_ref(&moved.destination), Instant::now(), SUPPRESSION_TTL);
    let log = ActionLog {
        id: None,
        timestamp: Utc::now(),
        source_path: item.entry.source.clone(),
        destination_path: Some(moved.destination.to_string_lossy().into_owned()),
        action: rule.action.clone(),
        file_name: info.name,
        file_type: rule.name.clone(),
        undone: false,
    };
    let history = OrganizationHistory {
        run_id, trigger: "approved", extension: &info.extension, size: info.size, digest: &item.digest,
    };
    db::finalize_organization_move(intent, &log, &moved, history).map_err(|error| {
        format!("Moved to {}, but recovery record {intent} is pending: {error}", moved.destination.display())
    })
}

#[derive(Default, Deserialize)]
#[serde(default)]
pub struct HistoryFilter {
    pub query: String,
    pub rule: String,
    pub extension: String,
    pub from: String,
    pub to: String,
    pub run_id: String,
}
pub fn history(filter: &HistoryFilter) -> Result<Vec<OrganizationLog>, String> {
    let from = if filter.from.is_empty() {
        None
    } else {
        Some(
            chrono::DateTime::parse_from_rfc3339(&filter.from)
                .map_err(|_| "validation.date")?
                .with_timezone(&Utc),
        )
    };
    let to = if filter.to.is_empty() {
        None
    } else {
        Some(
            chrono::DateTime::parse_from_rfc3339(&filter.to)
                .map_err(|_| "validation.date")?
                .with_timezone(&Utc),
        )
    };
    Ok(db::get_organization_logs()
        .map_err(|e| e.to_string())?
        .into_iter()
        .filter(|log| {
            log.action.file_name
                .to_lowercase()
                .contains(&filter.query.to_lowercase())
                && log
                    .action.file_type
                    .to_lowercase()
                    .contains(&filter.rule.to_lowercase())
                && (filter.extension.is_empty()
                    || log
                        .file_extension
                        .eq_ignore_ascii_case(filter.extension.trim_start_matches('.')))
                && from.is_none_or(|date| log.action.timestamp >= date)
                && to.is_none_or(|date| log.action.timestamp <= date)
                && (filter.run_id.is_empty()
                    || log.run_id.as_deref() == Some(filter.run_id.as_str()))
        })
        .collect())
}
