use crate::{db, safe_fs};
use chrono::{Duration as ChronoDuration, Utc};
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const PLAN_LIFETIME: Duration = Duration::from_secs(10 * 60);
const MAX_PENDING_PLANS: usize = 128;
const UNVERIFIED_MESSAGE: &str = "Source removed; Recycle Bin recovery could not be verified";
const PERMANENTLY_DELETED_STATUS: &str = "permanently_deleted";
const PERMANENTLY_DELETED_MESSAGE: &str = "Deleted permanently; this item cannot be restored from the Recycle Bin";

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
    pub files: Vec<DuplicateFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CleanupRequest {
    pub kind: String,
    pub path: String,
    pub keep_path: Option<String>,
    /// Finder criteria are captured with the selection, before confirmation.
    #[serde(default)]
    pub min_bytes: Option<i64>,
    #[serde(default)]
    pub days: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanupOutcome {
    pub path: String,
    pub status: String,
    pub message: Option<String>,
}

impl CleanupOutcome {
    fn apply_trash_verdict(&mut self, verdict: safe_fs::TrashVerdict) {
        match verdict {
            safe_fs::TrashVerdict::InRecycleBin => {}
            safe_fs::TrashVerdict::Unverified => self.message = Some(UNVERIFIED_MESSAGE.into()),
            safe_fs::TrashVerdict::PermanentlyDeleted => {
                // Only an explicit, proven verdict may use this historical wire
                // status. A bin-count shortfall is not such proof.
                self.status = PERMANENTLY_DELETED_STATUS.into();
                self.message = Some(PERMANENTLY_DELETED_MESSAGE.into());
            }
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanupPlanPreview {
    pub plan_id: String,
    pub expires_at: String,
    pub actions: Vec<CleanupRequest>,
    pub total_bytes: i64,
}

pub use db::LargestFile as CleanupFile;

pub fn has_inventory() -> bool {
    db::count_inventory_files().map(|count| count > 0).unwrap_or(false)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FileFingerprint {
    identity: safe_fs::FileIdentity,
    size: u64,
    modified: SystemTime,
    links: u64,
    hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RootConfig {
    id: Option<i64>,
    path: String,
    enabled: bool,
    mode: String,
}

fn root_configuration() -> Result<Vec<RootConfig>, String> {
    let mut configuration: Vec<_> = db::get_watched_folders()
        .map_err(|error| format!("Cannot read watched folders: {error}"))?
        .into_iter()
        .map(|folder| RootConfig {
            id: folder.id,
            path: folder.path,
            enabled: folder.enabled,
            mode: folder.mode,
        })
        .collect();
    configuration.sort_by(|left, right| left.id.cmp(&right.id).then(left.path.cmp(&right.path)));
    Ok(configuration)
}

fn current_scope() -> Result<safe_fs::RootScope, String> {
    safe_fs::eligible_scope(
        &db::get_watched_folders().map_err(|error| format!("Cannot read watched folders: {error}"))?,
    )
}

fn modified_epoch(modified: SystemTime) -> i64 {
    modified.duration_since(UNIX_EPOCH).map(|duration| duration.as_secs() as i64).unwrap_or(0)
}

/// Persistent hashes and inventory timestamps never authorize a removal.
/// Hash the live handle, then confirm both its metadata and the path identity.
fn fingerprint_guard(scope: &safe_fs::RootScope, path: &Path) -> Result<(FileFingerprint, safe_fs::ValidatedFile), String> {
    let mut target = scope.validate_file(path)?;
    let before = target.file.metadata().map_err(|error| format!("Cannot inspect file: {error}"))?;
    let modified = before.modified().map_err(|error| format!("Cannot read file time: {error}"))?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let count = target.file.read(&mut buffer).map_err(|error| format!("Cannot hash file: {error}"))?;
        if count == 0 { break; }
        hasher.update(&buffer[..count]);
    }
    let after = target.file.metadata().map_err(|error| format!("Cannot recheck file: {error}"))?;
    if before.len() != after.len() || after.modified().ok() != Some(modified)
        || safe_fs::identity_from_handle(&target.file)? != target.identity
    {
        return Err("File changed while its contents were being read".into());
    }
    let live = scope.validate_file(path)?;
    if live.identity != target.identity || live.link_count != target.link_count {
        return Err("File was replaced while its contents were being read".into());
    }
    Ok((FileFingerprint {
        identity: target.identity.clone(),
        size: after.len(),
        modified,
        links: target.link_count,
        hash: hasher.finalize().to_hex().to_string(),
    }, target))
}

fn fingerprint(scope: &safe_fs::RootScope, path: &Path) -> Result<FileFingerprint, String> {
    fingerprint_guard(scope, path).map(|(fingerprint, _guard)| fingerprint)
}

fn inventory_paths() -> Result<Vec<CleanupFile>, String> {
    db::get_large_files_from_inventory(0).map_err(|error| format!("Cannot read file inventory: {error}"))
}

fn duplicate_candidates(scope: &safe_fs::RootScope) -> Result<Vec<(String, FileFingerprint)>, String> {
    let mut seen = HashSet::new();
    let mut by_size: HashMap<u64, Vec<String>> = HashMap::new();
    for row in inventory_paths()? {
        if let Ok(current) = scope.validate_file(Path::new(&row.path)) {
            // A hard-link entry does not independently reclaim a file's bytes.
            if current.link_count == 1 && seen.insert(current.identity) {
                by_size.entry(current.metadata.len()).or_default().push(row.path);
            }
        }
    }
    let mut files = Vec::new();
    let mut seen = HashSet::new();
    // Size is a live, cheap candidate filter; every reported match still uses
    // one full-content digest domain and no persisted hash evidence.
    for paths in by_size.into_values().filter(|paths| paths.len() > 1) {
        for path in paths {
            if let Ok(current) = fingerprint(scope, Path::new(&path)) {
                if current.links == 1 && seen.insert(current.identity.clone()) {
                    files.push((path, current));
                }
            }
        }
    }
    Ok(files)
}

pub fn find_duplicates() -> Result<Vec<DuplicateGroup>, String> {
    let scope = current_scope()?;
    let mut grouped: HashMap<(u64, String), Vec<DuplicateFile>> = HashMap::new();
    for (path, current) in duplicate_candidates(&scope)? {
        grouped.entry((current.size, current.hash)).or_default().push(DuplicateFile {
            path,
            size: current.size as i64,
            mtime: modified_epoch(current.modified),
        });
    }
    let mut groups: Vec<_> = grouped.into_iter().filter_map(|((_, hash), mut files)| {
        if files.len() < 2 { return None; }
        files.sort_by(|left, right| left.path.cmp(&right.path));
        Some(DuplicateGroup { hash, files })
    }).collect();
    groups.sort_by(|left, right| {
        let reclaim = |group: &DuplicateGroup| group.files[0].size.saturating_mul(group.files.len() as i64 - 1);
        reclaim(right).cmp(&reclaim(left)).then(left.hash.cmp(&right.hash))
    });
    Ok(groups)
}

fn current_inventory_files(scope: &safe_fs::RootScope) -> Result<Vec<CleanupFile>, String> {
    let mut seen = HashSet::new();
    let mut files = Vec::new();
    for row in inventory_paths()? {
        if let Ok(target) = scope.validate_file(Path::new(&row.path)) {
            if target.link_count != 1 { continue; }
            if !seen.insert(target.identity) { continue; }
            if let Ok(metadata) = target.file.metadata() {
                if let Ok(modified) = metadata.modified() {
                    files.push(CleanupFile { path: row.path, size: metadata.len() as i64, mtime: modified_epoch(modified) });
                }
            }
        }
    }
    Ok(files)
}

pub fn find_large_files(min_bytes: i64) -> Result<Vec<CleanupFile>, String> {
    if min_bytes < 1 { return Err("The minimum file size must be positive".into()); }
    let mut files = current_inventory_files(&current_scope()?)?;
    files.retain(|file| file.size >= min_bytes);
    files.sort_by(|left, right| right.size.cmp(&left.size).then(left.path.cmp(&right.path)));
    Ok(files)
}

fn stale_cutoff(days: i64, now: SystemTime) -> Result<i64, String> {
    if days < 1 { return Err("The file age must be at least one day".into()); }
    let seconds = days.checked_mul(86_400).ok_or("The file age is too large")?;
    Ok(modified_epoch(now).saturating_sub(seconds))
}

pub fn find_stale_files(days: i64) -> Result<Vec<CleanupFile>, String> {
    let cutoff = stale_cutoff(days, SystemTime::now())?;
    let mut files = current_inventory_files(&current_scope()?)?;
    files.retain(|file| file.mtime < cutoff);
    files.sort_by(|left, right| left.mtime.cmp(&right.mtime).then(left.path.cmp(&right.path)));
    Ok(files)
}

fn empty_leaves(scope: &safe_fs::RootScope, directory: &Path, result: &mut Vec<String>) {
    let entries = match fs::read_dir(directory) { Ok(entries) => entries, Err(_) => return };
    let mut has_entries = false;
    for entry in entries {
        has_entries = true;
        let entry = match entry { Ok(entry) => entry, Err(_) => continue };
        // Validation rejects links, configured roots and inactive nested scope.
        if scope.validate_dir(&entry.path()).is_ok() {
            empty_leaves(scope, &entry.path(), result);
        }
    }
    if !has_entries && scope.validate_dir(directory).is_ok() {
        result.push(directory.to_string_lossy().into_owned());
    }
}

/// Uses eligible registered roots directly, including an entirely empty scan.
pub fn find_empty_dirs() -> Result<Vec<String>, String> {
    let scope = current_scope()?;
    let mut result = Vec::new();
    for root in scope.active_roots() {
        empty_leaves(&scope, &root, &mut result);
    }
    result.sort();
    result.dedup();
    Ok(result)
}

#[derive(Debug, Clone)]
struct PlannedAction {
    request: CleanupRequest,
    identity: safe_fs::FileIdentity,
    /// All members of the reviewed duplicate group, including the keeper and
    /// members the user did not select, remain required evidence.
    group_members: Vec<String>,
}

#[derive(Debug, Clone)]
struct ReviewedPlan {
    actions: Vec<PlannedAction>,
    files: HashMap<String, FileFingerprint>,
    roots: Vec<RootConfig>,
    root_identities: Vec<safe_fs::FileIdentity>,
    created: Instant,
}

static PLANS: Lazy<Mutex<HashMap<String, ReviewedPlan>>> = Lazy::new(|| Mutex::new(HashMap::new()));

fn new_plan_id() -> Result<String, String> {
    let mut bytes = [0u8; 32];
    #[cfg(windows)]
    {
        #[link(name = "bcrypt")]
        extern "system" { fn BCryptGenRandom(algorithm: *mut std::ffi::c_void, buffer: *mut u8, length: u32, flags: u32) -> i32; }
        // BCRYPT_USE_SYSTEM_PREFERRED_RNG; no algorithm handle is required.
        let status = unsafe { BCryptGenRandom(std::ptr::null_mut(), bytes.as_mut_ptr(), bytes.len() as u32, 2) };
        if status < 0 { return Err("Cannot create a secure cleanup plan token".into()); }
    }
    #[cfg(not(windows))]
    fs::File::open("/dev/urandom").and_then(|mut file| file.read_exact(&mut bytes))
        .map_err(|error| format!("Cannot create cleanup plan token: {error}"))?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn directory_is_empty(path: &Path) -> Result<(), String> {
    let mut entries = fs::read_dir(path).map_err(|error| format!("Cannot read directory: {error}"))?;
    match entries.next() {
        None => Ok(()),
        Some(Ok(_)) => Err("Directory is no longer empty".into()),
        Some(Err(error)) => Err(format!("Cannot confirm directory is empty: {error}")),
    }
}

pub fn prepare_cleanup(actions: &[CleanupRequest]) -> Result<CleanupPlanPreview, String> {
    if actions.is_empty() { return Err("Select at least one cleanup action".into()); }
    if actions.len() > 10_000 { return Err("Too many cleanup actions in one review".into()); }
    let roots = root_configuration()?;
    let scope = current_scope()?;
    let root_identities = scope.root_identities();
    let inventory: HashSet<String> = inventory_paths()?.into_iter().map(|file| file.path).collect();
    let candidates = if actions.iter().any(|action| action.kind == "trash_duplicate") {
        duplicate_candidates(&scope)?
    } else { Vec::new() };
    let mut by_content: HashMap<(u64, String), Vec<String>> = HashMap::new();
    let mut files: HashMap<String, FileFingerprint> = candidates.into_iter().map(|(path, file)| {
        by_content.entry((file.size, file.hash.clone())).or_default().push(path.clone());
        (path, file)
    }).collect();
    let mut planned = Vec::new();
    let mut victims = HashSet::new();
    let mut keepers = HashSet::new();
    let mut total_bytes = 0i64;
    for request in actions {
        let path = Path::new(&request.path);
        let (identity, group_members) = match request.kind.as_str() {
            "trash_duplicate" => {
                if request.min_bytes.is_some() || request.days.is_some() {
                    return Err("Duplicate actions cannot include size or age criteria".into());
                }
                let keep = request.keep_path.as_ref().filter(|keep| !keep.is_empty())
                    .ok_or("A duplicate action requires its reviewed keeper")?;
                let victim = files.get(&request.path).ok_or("The selected file is no longer an eligible duplicate")?;
                let keeper = files.get(keep).ok_or("The keeper is no longer an eligible duplicate")?;
                if victim.identity == keeper.identity { return Err("The victim and keeper are the same physical file".into()); }
                if victim.size != keeper.size || victim.hash != keeper.hash { return Err("The victim and keeper no longer have identical contents".into()); }
                keepers.insert(keeper.identity.clone());
                total_bytes = total_bytes.saturating_add(victim.size as i64);
                (victim.identity.clone(), by_content[&(victim.size, victim.hash.clone())].clone())
            }
            "trash_large" | "trash_stale" => {
                if request.keep_path.is_some() { return Err("Only duplicate actions may name a keeper".into()); }
                if !inventory.contains(&request.path) { return Err("The selected file is not in the current inventory".into()); }
                let current = fingerprint(&scope, path)?;
                if current.links != 1 { return Err("Hard-linked files are not eligible for ordinary cleanup".into()); }
                if request.kind == "trash_large" {
                    if request.days.is_some() { return Err("A large-file action cannot include an age criterion".into()); }
                    let minimum = request.min_bytes.filter(|minimum| *minimum > 0).ok_or("A large-file action requires its reviewed minimum size")?;
                    if current.size < minimum as u64 { return Err("The file is no longer large enough for the reviewed search".into()); }
                } else {
                    if request.min_bytes.is_some() { return Err("A stale-file action cannot include a size criterion".into()); }
                    let days = request.days.ok_or("A stale-file action requires its reviewed age")?;
                    if modified_epoch(current.modified) >= stale_cutoff(days, SystemTime::now())? {
                        return Err("The file is no longer old enough for the reviewed search".into());
                    }
                }
                total_bytes = total_bytes.saturating_add(current.size as i64);
                let identity = current.identity.clone();
                files.insert(request.path.clone(), current);
                (identity, Vec::new())
            }
            "remove_empty_dir" => {
                if request.keep_path.is_some() || request.min_bytes.is_some() || request.days.is_some() {
                    return Err("An empty-directory action cannot include file criteria".into());
                }
                let current = scope.validate_dir(path)?;
                directory_is_empty(path)?;
                (current.identity, Vec::new())
            }
            _ => return Err(format!("Unknown cleanup action kind: {}", request.kind)),
        };
        if !victims.insert(identity.clone()) { return Err("The same physical target occurs more than once in the cleanup plan".into()); }
        planned.push(PlannedAction { request: request.clone(), identity, group_members });
    }
    if !victims.is_disjoint(&keepers) {
        return Err("A retained duplicate is also targeted for cleanup in this plan".into());
    }
    // Only evidence belonging to selected groups or actions needs revalidation.
    let required: HashSet<_> = planned.iter().flat_map(|action| {
        action.group_members.iter().cloned().chain(std::iter::once(action.request.path.clone()))
    }).collect();
    files.retain(|path, _| required.contains(path));
    if roots != root_configuration()? || root_identities != current_scope()?.root_identities() {
        return Err("Watched folders changed while the cleanup review was being prepared".into());
    }
    let mut plans = PLANS.lock().map_err(|_| "Cleanup plan storage is unavailable")?;
    plans.retain(|_, plan| plan.created.elapsed() < PLAN_LIFETIME);
    if plans.len() >= MAX_PENDING_PLANS { return Err("Too many cleanup reviews are pending; close an older review and try again".into()); }
    let plan_id = new_plan_id()?;
    plans.insert(plan_id.clone(), ReviewedPlan { actions: planned, files, roots, root_identities, created: Instant::now() });
    Ok(CleanupPlanPreview {
        plan_id,
        expires_at: (Utc::now() + ChronoDuration::minutes(10)).to_rfc3339(),
        actions: actions.to_vec(),
        total_bytes,
    })
}

trait CleanupIo {
    fn begin_intent(&mut self, kind: &str, path: &str) -> Result<i64, String>;
    fn finish_intent(&mut self, id: i64, outcome: &CleanupOutcome, removed: bool) -> Result<(), String>;
    fn prepare_trash(&mut self) {}
    fn trash(&mut self, path: &Path, identity: &safe_fs::FileIdentity) -> Result<safe_fs::TrashVerdict, String>;
    fn remove_empty_dir(&mut self, path: &Path, identity: &safe_fs::FileIdentity) -> Result<(), String>;
}

#[derive(Default)]
struct NativeCleanupIo { before: Option<safe_fs::TrashSnapshot> }

impl CleanupIo for NativeCleanupIo {
    fn begin_intent(&mut self, kind: &str, path: &str) -> Result<i64, String> {
        db::begin_cleanup_intent(kind, path).map_err(|error| error.to_string())
    }
    fn finish_intent(&mut self, id: i64, outcome: &CleanupOutcome, removed: bool) -> Result<(), String> {
        db::finalize_cleanup_intent(id, &outcome.status, outcome.message.as_deref(), removed).map_err(|error| error.to_string())
    }
    fn prepare_trash(&mut self) {
        self.before = Some(safe_fs::trash_snapshot());
    }
    fn trash(&mut self, path: &Path, identity: &safe_fs::FileIdentity) -> Result<safe_fs::TrashVerdict, String> {
        let before = self.before.take().ok_or("The trash observation was not prepared")?;
        safe_fs::delete_to_trash(path)?;
        let after = safe_fs::trash_snapshot();
        Ok(safe_fs::classify_trash_outcomes_with_identity(&before, &after, &[(path.to_path_buf(), identity.clone())])
            .into_iter().next().unwrap_or(safe_fs::TrashVerdict::Unverified))
    }
    fn remove_empty_dir(&mut self, path: &Path, identity: &safe_fs::FileIdentity) -> Result<(), String> {
        safe_fs::remove_empty_directory_expected(path, identity)
    }
}

fn plan_scope(plan: &ReviewedPlan) -> Result<safe_fs::RootScope, String> {
    if plan.roots != root_configuration()? { return Err("Watched folders changed; review cleanup again".into()); }
    let scope = current_scope()?;
    if plan.root_identities != scope.root_identities() { return Err("A watched folder was replaced; review cleanup again".into()); }
    Ok(scope)
}

fn check_file(plan: &ReviewedPlan, scope: &safe_fs::RootScope, path: &str) -> Result<(), String> {
    let reviewed = plan.files.get(path).ok_or("Cleanup plan is missing file evidence")?;
    if &fingerprint(scope, Path::new(path))? != reviewed {
        return Err(format!("File changed since cleanup review: {path}"));
    }
    Ok(())
}

fn validate_action(plan: &ReviewedPlan, scope: &safe_fs::RootScope, action: &PlannedAction, removed: &HashSet<String>) -> Result<(), String> {
    if action.request.kind == "remove_empty_dir" {
        if scope.validate_dir(Path::new(&action.request.path))?.identity != action.identity {
            return Err("Directory changed since cleanup review".into());
        }
        return directory_is_empty(Path::new(&action.request.path));
    }
    check_file(plan, scope, &action.request.path)?;
    for member in &action.group_members {
        if !removed.contains(member) { check_file(plan, scope, member)?; }
    }
    Ok(())
}

fn source_absent(path: &Path) -> Result<bool, String> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(false),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(true),
        Err(error) => Err(format!("Cannot confirm source removal: {error}")),
    }
}

fn add_message(outcome: &mut CleanupOutcome, message: String) {
    outcome.message = Some(match outcome.message.take() {
        Some(existing) => format!("{existing}. {message}"),
        None => message,
    });
}

struct MutationGuards {
    path: PathBuf,
    _parents: safe_fs::AncestorGuard,
    _keeper: Option<safe_fs::ValidatedFile>,
}

fn mutation_guards(plan: &ReviewedPlan, scope: &safe_fs::RootScope, action: &PlannedAction) -> Result<MutationGuards, String> {
    let request = &action.request;
    if request.kind == "remove_empty_dir" {
        let target = scope.validate_dir(Path::new(&request.path))?;
        if target.identity != action.identity { return Err("Directory changed since cleanup review".into()); }
        directory_is_empty(&target.canonical_path)?;
        return Ok(MutationGuards {
            path: target.canonical_path.clone(),
            _parents: target.into_parent_guard(),
            _keeper: None,
        });
    }
    let (current, target) = fingerprint_guard(scope, Path::new(&request.path))?;
    if plan.files.get(&request.path) != Some(&current) { return Err("File changed since cleanup review".into()); }
    let keeper = if let Some(path) = &request.keep_path {
        let (current, target) = fingerprint_guard(scope, Path::new(path))?;
        if plan.files.get(path) != Some(&current) { return Err("Keeper changed since cleanup review".into()); }
        Some(target)
    } else { None };
    Ok(MutationGuards {
        path: target.canonical_path.clone(),
        // Windows Shell needs permission to delete the victim. Keep every
        // ancestor locked against replacement while releasing that one handle.
        // Shell deletion is still path based: the release-to-Shell interval is
        // a residual victim-entry race, not an identity-bound guarantee.
        _parents: target.into_parent_guard(),
        _keeper: keeper,
    })
}

fn execute_cleanup_plan_with(plan_id: &str, io: &mut impl CleanupIo) -> Result<Vec<CleanupOutcome>, String> {
    // Consume before validation. Expired/stale/failed plans are never reusable.
    let plan = PLANS.lock().map_err(|_| "Cleanup plan storage is unavailable")?
        .remove(plan_id).ok_or("Cleanup review expired or has already been used; review cleanup again")?;
    if plan.created.elapsed() >= PLAN_LIFETIME { return Err("Cleanup review expired; review cleanup again".into()); }
    let scope = plan_scope(&plan)?;
    let mut removed = HashSet::new();
    // All reviewed members are checked before the first mutation, including
    // members in another group or retained copies not selected for removal.
    for action in &plan.actions { validate_action(&plan, &scope, action, &removed)?; }
    let mut outcomes = Vec::with_capacity(plan.actions.len());
    for action in &plan.actions {
        let request = &action.request;
        let mut outcome = CleanupOutcome { path: request.path.clone(), status: "skipped".into(), message: None };
        let validation = plan_scope(&plan).and_then(|scope| validate_action(&plan, &scope, action, &removed));
        if let Err(error) = validation {
            outcome.message = Some(error);
            outcomes.push(outcome);
            continue;
        }
        let intent = match io.begin_intent(&request.kind, &request.path) {
            Ok(intent) => intent,
            Err(error) => {
                outcome.status = "failed".into();
                outcome.message = Some(format!("Cleanup did not start because its recovery record could not be saved: {error}"));
                outcomes.push(outcome);
                continue;
            }
        };
        // Reading a large bin can take time. Do it before the final live hash
        // guards, so it does not lengthen the victim-handle release interval.
        if request.kind != "remove_empty_dir" { io.prepare_trash(); }
        // Intent writes can take time. Validate again immediately before the
        // path-based platform operation, and hold keeper guards while it runs.
        let mutation_scope = plan_scope(&plan);
        let validation = mutation_scope.as_ref().map_err(|error| error.clone())
            .and_then(|scope| validate_action(&plan, scope, action, &removed));
        let mut source_removed = false;
        match validation {
            Err(error) => outcome.message = Some(error),
            Ok(()) => {
                let guards = mutation_guards(&plan, mutation_scope.as_ref().unwrap(), action);
                match guards {
                    Err(error) => outcome.message = Some(error),
                    Ok(guards) => {
                        let result = if request.kind == "remove_empty_dir" {
                            io.remove_empty_dir(&guards.path, &action.identity).map(|_| None)
                        } else { io.trash(&guards.path, &action.identity).map(Some) };
                        match result {
                            Err(error) => {
                                outcome.status = "failed".into();
                                outcome.message = Some(error);
                                if source_absent(&guards.path) == Ok(true) {
                                    source_removed = true;
                                    removed.insert(request.path.clone());
                                    add_message(&mut outcome, "The source is absent despite the reported error; recovery remains unverified".into());
                                }
                            }
                            Ok(verdict) => match source_absent(&guards.path) {
                                Ok(true) => {
                                    source_removed = true;
                                    removed.insert(request.path.clone());
                                    outcome.status = "ok".into();
                                    if let Some(verdict) = verdict { outcome.apply_trash_verdict(verdict); }
                                }
                                Ok(false) => { outcome.status = "failed".into(); outcome.message = Some("The operation returned success but the source still exists; removal was not confirmed".into()); }
                                Err(error) => { outcome.status = "failed".into(); outcome.message = Some(error); }
                            },
                        }
                    }
                }
            }
        }
        if let Err(error) = io.finish_intent(intent, &outcome, source_removed) {
            // Retain the actual filesystem status. The pending intent remains
            // recoverable, and callers see a separate persistence warning.
            add_message(&mut outcome, format!("Cleanup history could not be finalized; its recovery record remains pending: {error}"));
        }
        outcomes.push(outcome);
    }
    Ok(outcomes)
}

/// This is the sole destructive cleanup entry point. Paths/kinds/keepers are
/// obtained from the immutable backend review, never from execution IPC.
pub fn execute_cleanup_plan(plan_id: &str) -> Result<Vec<CleanupOutcome>, String> {
    execute_cleanup_plan_with(plan_id, &mut NativeCleanupIo::default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

    struct TestTree(PathBuf);

    impl TestTree {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "mouzi-cleanup-{}-{}-{}",
                std::process::id(),
                SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos(),
                NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&root).unwrap();
            Self(fs::canonicalize(root).unwrap())
        }

        fn register(&self) {
            db::add_watched_folder(&self.0.to_string_lossy(), "manual").unwrap();
        }

        fn file(&self, name: &str, contents: &[u8]) -> String {
            let path = self.0.join(name);
            fs::write(&path, contents).unwrap();
            path.to_string_lossy().into_owned()
        }

        fn inventory(&self, paths: &[&str]) {
            let rows: Vec<_> = paths.iter().map(|path| {
                let metadata = fs::metadata(path).unwrap();
                db::InventoryRow {
                    path: (*path).to_string(),
                    size: metadata.len() as i64,
                    mtime: modified_epoch(metadata.modified().unwrap()),
                    category: "Other".into(),
                }
            }).collect();
            db::append_inventory_batch(&self.0.to_string_lossy(), &rows).unwrap();
        }
    }

    impl Drop for TestTree {
        fn drop(&mut self) {
            // Only the unique test-owned tree, never an existing user path.
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn init() {
        db::init_test_db();
        // The test database is shared across modules. A deleted root from a
        // previous fixture must not become configuration for the next test.
        db::lock_db().unwrap().execute_batch("DELETE FROM watched_folders; DELETE FROM file_inventory; DELETE FROM hash_cache;").unwrap();
        PLANS.lock().unwrap().clear();
    }

    fn duplicate(path: &str, keeper: &str) -> CleanupRequest {
        CleanupRequest { kind: "trash_duplicate".into(), path: path.into(), keep_path: Some(keeper.into()), min_bytes: None, days: None }
    }

    fn large(path: &str) -> CleanupRequest {
        CleanupRequest { kind: "trash_large".into(), path: path.into(), keep_path: None, min_bytes: Some(1), days: None }
    }

    fn directory(path: &str) -> CleanupRequest {
        CleanupRequest { kind: "remove_empty_dir".into(), path: path.into(), keep_path: None, min_bytes: None, days: None }
    }

    #[derive(Default)]
    struct FakeIo {
        trash_calls: usize,
        directory_calls: usize,
        intent_calls: usize,
        finalized: Vec<(String, bool)>,
        fail_intent: bool,
        fail_finalize: bool,
        leave_source: bool,
        fail_after_removal: bool,
        change_keeper_after_first: Option<String>,
    }

    impl CleanupIo for FakeIo {
        fn begin_intent(&mut self, _kind: &str, _path: &str) -> Result<i64, String> {
            self.intent_calls += 1;
            if self.fail_intent { Err("injected intent failure".into()) } else { Ok(self.intent_calls as i64) }
        }
        fn finish_intent(&mut self, _id: i64, outcome: &CleanupOutcome, removed: bool) -> Result<(), String> {
            self.finalized.push((outcome.status.clone(), removed));
            if self.finalized.len() == 1 {
                if let Some(keeper) = &self.change_keeper_after_first {
                    fs::write(keeper, b"changed keeper").unwrap();
                }
            }
            if self.fail_finalize { Err("injected history failure".into()) } else { Ok(()) }
        }
        fn trash(&mut self, path: &Path, _identity: &safe_fs::FileIdentity) -> Result<safe_fs::TrashVerdict, String> {
            self.trash_calls += 1;
            if !self.leave_source {
                // Model a removal by moving to another test-owned path. This
                // adapter never calls the native trash implementation.
                let destination = path.with_file_name(format!("removed-{}", self.trash_calls));
                fs::rename(path, destination).map_err(|error| error.to_string())?;
            }
            if self.fail_after_removal { return Err("injected failure after source removal".into()); }
            Ok(safe_fs::TrashVerdict::Unverified)
        }
        fn remove_empty_dir(&mut self, path: &Path, _identity: &safe_fs::FileIdentity) -> Result<(), String> {
            self.directory_calls += 1;
            let destination = path.with_file_name(format!("removed-dir-{}", self.directory_calls));
            fs::rename(path, destination).map_err(|error| error.to_string())
        }
    }

    #[test]
    fn duplicate_discovery_uses_live_contents_and_ignores_poisoned_cache() {
        let _guard = db::serialise_test_db();
        init();
        let tree = TestTree::new();
        tree.register();
        let a = tree.file("a.txt", b"AAAA");
        let b = tree.file("b.txt", b"BBBB");
        tree.inventory(&[&a, &b]);
        let metadata = fs::metadata(&a).unwrap();
        db::set_cached_hash(&a, 4, modified_epoch(metadata.modified().unwrap()), &blake3::hash(b"BBBB").to_hex().to_string(), true).unwrap();
        assert!(find_duplicates().unwrap().is_empty(), "a cached digest cannot turn different live contents into duplicates");
        fs::write(&a, b"BBBB").unwrap();
        let groups = find_duplicates().unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].files.len(), 2);
    }

    #[test]
    fn duplicate_groups_exclude_hard_links_and_physical_aliases() {
        let _guard = db::serialise_test_db();
        init();
        let tree = TestTree::new();
        tree.register();
        let original = tree.file("a.txt", b"same");
        let linked = tree.0.join("linked.txt");
        fs::hard_link(&original, &linked).unwrap();
        let other = tree.file("b.txt", b"same");
        let linked = linked.to_string_lossy().into_owned();
        tree.inventory(&[&original, &linked, &other]);
        assert!(find_duplicates().unwrap().is_empty());
        assert!(prepare_cleanup(&[duplicate(&linked, &original)]).is_err());
    }

    #[test]
    fn finder_returns_current_sizes_and_times_in_eligible_roots() {
        let _guard = db::serialise_test_db();
        init();
        let tree = TestTree::new();
        tree.register();
        let old = tree.file("old.txt", b"old");
        let fresh = tree.file("fresh.txt", b"fresh");
        let old_time = SystemTime::now() - Duration::from_secs(2 * 86_400);
        fs::OpenOptions::new().write(true).open(&old).unwrap().set_times(fs::FileTimes::new().set_modified(old_time)).unwrap();
        tree.inventory(&[&old, &fresh]);
        fs::write(&fresh, vec![b'x'; 200]).unwrap();
        let large = find_large_files(100).unwrap();
        assert_eq!(large.len(), 1);
        assert_eq!(large[0].path, fresh);
        assert_eq!(large[0].size, 200, "inventory size is not live evidence");
        let stale = find_stale_files(1).unwrap();
        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0].path, old);
    }

    #[test]
    fn empty_discovery_excludes_roots_without_requiring_inventory() {
        let _guard = db::serialise_test_db();
        init();
        let tree = TestTree::new();
        tree.register();
        assert!(!has_inventory());
        assert!(find_empty_dirs().unwrap().is_empty(), "registered root itself is protected");
        fs::create_dir_all(tree.0.join("a").join("deep")).unwrap();
        fs::create_dir(tree.0.join("b")).unwrap();
        let leaves = find_empty_dirs().unwrap();
        assert_eq!(leaves.len(), 2);
        assert!(leaves.iter().any(|path| Path::new(path) == tree.0.join("a").join("deep")));
        assert!(leaves.iter().any(|path| Path::new(path) == tree.0.join("b")));
        assert!(prepare_cleanup(&[directory(&tree.0.to_string_lossy())]).is_err());
    }

    #[test]
    fn invalid_payloads_and_cross_kind_keeper_intersections_never_mutate() {
        let _guard = db::serialise_test_db();
        init();
        let tree = TestTree::new();
        tree.register();
        let a = tree.file("a.txt", b"same");
        let b = tree.file("b.txt", b"same");
        tree.inventory(&[&a, &b]);
        let outside = TestTree::new();
        let outside_file = outside.file("outside.txt", b"same");
        let mut no_keeper = duplicate(&a, &b);
        no_keeper.keep_path = None;
        let mut unknown = large(&a);
        unknown.kind = "recursive_delete".into();
        for actions in [
            vec![no_keeper],
            vec![unknown],
            vec![large(&outside_file)],
            vec![large(&tree.0.to_string_lossy())],
            vec![duplicate(&a, &b), duplicate(&b, &a)],
            vec![duplicate(&a, &b), large(&b)],
            vec![large(&a), large(&a)],
        ] {
            assert!(prepare_cleanup(&actions).is_err());
        }
        let mut io = FakeIo::default();
        assert!(execute_cleanup_plan_with("client-created-token", &mut io).is_err());
        assert_eq!(io.trash_calls, 0);
        assert_eq!(io.directory_calls, 0);
    }

    #[test]
    fn paused_nested_root_cannot_borrow_active_parent_authority() {
        let _guard = db::serialise_test_db();
        init();
        let tree = TestTree::new();
        tree.register();
        let nested = tree.0.join("nested");
        fs::create_dir(&nested).unwrap();
        db::add_watched_folder(&nested.to_string_lossy(), "paused").unwrap();
        let path = tree.file("nested/a.txt", b"same");
        tree.inventory(&[&path]);
        assert!(find_large_files(1).unwrap().is_empty());
        assert!(prepare_cleanup(&[large(&path)]).is_err());
        assert!(prepare_cleanup(&[directory(&nested.to_string_lossy())]).is_err());
    }

    #[test]
    fn same_size_preserved_time_victim_rewrite_rejects_whole_batch() {
        let _guard = db::serialise_test_db();
        init();
        let tree = TestTree::new();
        tree.register();
        let a = tree.file("a.txt", b"same");
        let b = tree.file("b.txt", b"same");
        tree.inventory(&[&a, &b]);
        let preview = prepare_cleanup(&[duplicate(&b, &a)]).unwrap();
        let modified = fs::metadata(&b).unwrap().modified().unwrap();
        fs::write(&b, b"diff").unwrap();
        fs::OpenOptions::new().write(true).open(&b).unwrap().set_times(fs::FileTimes::new().set_modified(modified)).unwrap();
        let mut io = FakeIo::default();
        assert!(execute_cleanup_plan_with(&preview.plan_id, &mut io).is_err());
        assert_eq!(io.trash_calls, 0);
        assert_eq!(io.intent_calls, 0);
        fs::write(&b, b"same").unwrap();
        let preview = prepare_cleanup(&[duplicate(&b, &a)]).unwrap();
        let modified = fs::metadata(&a).unwrap().modified().unwrap();
        fs::write(&a, b"diff").unwrap();
        fs::OpenOptions::new().write(true).open(&a).unwrap().set_times(fs::FileTimes::new().set_modified(modified)).unwrap();
        assert!(execute_cleanup_plan_with(&preview.plan_id, &mut io).is_err());
        assert_eq!(io.trash_calls, 0, "a same-size, preserved-time keeper rewrite must also be detected");
    }

    #[test]
    fn missing_keeper_and_changed_unselected_group_member_prevent_every_removal() {
        let _guard = db::serialise_test_db();
        init();
        let tree = TestTree::new();
        tree.register();
        let keeper = tree.file("keeper.txt", b"same");
        let victim = tree.file("victim.txt", b"same");
        let other = tree.file("unselected.txt", b"same");
        tree.inventory(&[&keeper, &victim, &other]);
        let changed_plan = prepare_cleanup(&[duplicate(&victim, &keeper)]).unwrap();
        fs::write(&other, b"diff").unwrap();
        let mut io = FakeIo::default();
        assert!(execute_cleanup_plan_with(&changed_plan.plan_id, &mut io).is_err());
        assert_eq!(io.trash_calls, 0);
        fs::write(&other, b"same").unwrap();
        let missing_plan = prepare_cleanup(&[duplicate(&victim, &keeper)]).unwrap();
        fs::rename(&keeper, tree.0.join("keeper-moved.txt")).unwrap();
        assert!(execute_cleanup_plan_with(&missing_plan.plan_id, &mut io).is_err());
        assert_eq!(io.trash_calls, 0);
    }

    #[test]
    fn root_configuration_change_and_expired_plan_are_single_use_rejections() {
        let _guard = db::serialise_test_db();
        init();
        let tree = TestTree::new();
        tree.register();
        let file = tree.file("a.txt", b"one");
        tree.inventory(&[&file]);
        let expired = prepare_cleanup(&[large(&file)]).unwrap();
        PLANS.lock().unwrap().get_mut(&expired.plan_id).unwrap().created = Instant::now() - PLAN_LIFETIME;
        let mut io = FakeIo::default();
        assert!(execute_cleanup_plan_with(&expired.plan_id, &mut io).is_err());
        assert!(execute_cleanup_plan_with(&expired.plan_id, &mut io).is_err());
        let changed = prepare_cleanup(&[large(&file)]).unwrap();
        let id = db::get_watched_folders().unwrap()[0].id.unwrap();
        db::update_folder_mode(id, "paused").unwrap();
        assert!(execute_cleanup_plan_with(&changed.plan_id, &mut io).is_err());
        assert_eq!(io.trash_calls, 0);
    }

    #[test]
    fn directory_gaining_a_child_is_rejected_before_fake_remover() {
        let _guard = db::serialise_test_db();
        init();
        let tree = TestTree::new();
        tree.register();
        let empty = tree.0.join("empty");
        fs::create_dir(&empty).unwrap();
        let preview = prepare_cleanup(&[directory(&empty.to_string_lossy())]).unwrap();
        fs::write(empty.join("new.txt"), b"keep").unwrap();
        let mut io = FakeIo::default();
        assert!(execute_cleanup_plan_with(&preview.plan_id, &mut io).is_err());
        assert_eq!(io.directory_calls, 0);
    }

    #[test]
    fn file_and_root_identity_replacements_are_rejected() {
        let _guard = db::serialise_test_db();
        init();
        let tree = TestTree::new();
        let watched = tree.0.join("watched");
        fs::create_dir(&watched).unwrap();
        db::add_watched_folder(&watched.to_string_lossy(), "manual").unwrap();
        let path = watched.join("a.txt").to_string_lossy().into_owned();
        fs::write(&path, b"same").unwrap();
        let row = db::InventoryRow { path: path.clone(), size: 4, mtime: modified_epoch(fs::metadata(&path).unwrap().modified().unwrap()), category: "Other".into() };
        db::append_inventory_batch(&watched.to_string_lossy(), &[row]).unwrap();
        let changed_file = prepare_cleanup(&[large(&path)]).unwrap();
        fs::rename(&path, watched.join("original.txt")).unwrap();
        fs::write(&path, b"same").unwrap();
        let mut io = FakeIo::default();
        assert!(execute_cleanup_plan_with(&changed_file.plan_id, &mut io).is_err());
        let changed_type = prepare_cleanup(&[large(&path)]).unwrap();
        fs::rename(&path, watched.join("second-original.txt")).unwrap();
        fs::create_dir(&path).unwrap();
        assert!(execute_cleanup_plan_with(&changed_type.plan_id, &mut io).is_err());
        // A newly created watched root at the same spelling is a new scope.
        let another = watched.join("b.txt").to_string_lossy().into_owned();
        fs::write(&another, b"same").unwrap();
        let row = db::InventoryRow { path: another.clone(), size: 4, mtime: modified_epoch(fs::metadata(&another).unwrap().modified().unwrap()), category: "Other".into() };
        db::append_inventory_batch(&watched.to_string_lossy(), &[row]).unwrap();
        let changed_root = prepare_cleanup(&[large(&another)]).unwrap();
        fs::rename(&watched, tree.0.join("old-watched")).unwrap();
        fs::create_dir(&watched).unwrap();
        fs::write(&another, b"same").unwrap();
        assert!(execute_cleanup_plan_with(&changed_root.plan_id, &mut io).is_err());
        assert_eq!(io.trash_calls, 0);
    }

    #[test]
    fn successful_duplicate_retains_keeper_and_consumes_exact_plan() {
        let _guard = db::serialise_test_db();
        init();
        let tree = TestTree::new();
        tree.register();
        let keeper = tree.file("keeper.txt", b"same");
        let victim = tree.file("victim.txt", b"same");
        tree.inventory(&[&keeper, &victim]);
        let preview = prepare_cleanup(&[duplicate(&victim, &keeper)]).unwrap();
        let mut io = FakeIo::default();
        let result = execute_cleanup_plan_with(&preview.plan_id, &mut io).unwrap();
        assert_eq!(io.trash_calls, 1);
        assert_eq!(result[0].path, victim);
        assert_eq!(result[0].status, "ok");
        assert_eq!(result[0].message.as_deref(), Some(UNVERIFIED_MESSAGE));
        assert_eq!(fs::read(&keeper).unwrap(), b"same");
        assert!(execute_cleanup_plan_with(&preview.plan_id, &mut io).is_err());
        assert_eq!(io.trash_calls, 1);
    }

    #[test]
    fn keeper_change_after_one_success_stops_remaining_group_members() {
        let _guard = db::serialise_test_db();
        init();
        let tree = TestTree::new();
        tree.register();
        let keeper = tree.file("keeper.txt", b"same");
        let first = tree.file("first.txt", b"same");
        let second = tree.file("second.txt", b"same");
        tree.inventory(&[&keeper, &first, &second]);
        let preview = prepare_cleanup(&[duplicate(&first, &keeper), duplicate(&second, &keeper)]).unwrap();
        let mut io = FakeIo { change_keeper_after_first: Some(keeper.clone()), ..FakeIo::default() };
        let results = execute_cleanup_plan_with(&preview.plan_id, &mut io).unwrap();
        assert_eq!(io.trash_calls, 1);
        assert_eq!(results[0].status, "ok");
        assert_eq!(results[1].status, "skipped");
        assert!(Path::new(&second).exists());
    }

    #[test]
    fn intent_failure_prevents_mutation_and_finalize_failure_keeps_actual_success() {
        let _guard = db::serialise_test_db();
        init();
        let tree = TestTree::new();
        tree.register();
        let file = tree.file("a.txt", b"one");
        tree.inventory(&[&file]);
        let rejected = prepare_cleanup(&[large(&file)]).unwrap();
        let mut failed = FakeIo { fail_intent: true, ..FakeIo::default() };
        let outcomes = execute_cleanup_plan_with(&rejected.plan_id, &mut failed).unwrap();
        assert_eq!(failed.trash_calls, 0);
        assert_eq!(outcomes[0].status, "failed");
        let accepted = prepare_cleanup(&[large(&file)]).unwrap();
        let mut partial = FakeIo { fail_finalize: true, ..FakeIo::default() };
        let outcomes = execute_cleanup_plan_with(&accepted.plan_id, &mut partial).unwrap();
        assert_eq!(partial.trash_calls, 1);
        assert_eq!(outcomes[0].status, "ok");
        assert!(outcomes[0].message.as_deref().unwrap().contains("recovery record remains pending"));
        assert_eq!(partial.finalized, vec![("ok".into(), true)]);
    }

    #[test]
    fn trash_success_with_source_still_present_is_not_reported_as_removed() {
        let _guard = db::serialise_test_db();
        init();
        let tree = TestTree::new();
        tree.register();
        let file = tree.file("a.txt", b"one");
        tree.inventory(&[&file]);
        let preview = prepare_cleanup(&[large(&file)]).unwrap();
        let mut io = FakeIo { leave_source: true, ..FakeIo::default() };
        let outcomes = execute_cleanup_plan_with(&preview.plan_id, &mut io).unwrap();
        assert_eq!(outcomes[0].status, "failed");
        assert_eq!(io.finalized, vec![("failed".into(), false)]);
        assert!(Path::new(&file).exists());
    }

    #[test]
    fn reported_trash_error_with_missing_source_records_actual_removal() {
        let _guard = db::serialise_test_db();
        init();
        let tree = TestTree::new();
        tree.register();
        let file = tree.file("a.txt", b"one");
        tree.inventory(&[&file]);
        let preview = prepare_cleanup(&[large(&file)]).unwrap();
        let mut io = FakeIo { fail_after_removal: true, ..FakeIo::default() };
        let outcomes = execute_cleanup_plan_with(&preview.plan_id, &mut io).unwrap();
        assert_eq!(outcomes[0].status, "failed");
        assert!(outcomes[0].message.as_deref().unwrap().contains("source is absent"));
        assert_eq!(io.finalized, vec![("failed".into(), true)]);
    }

    #[test]
    fn explicit_trash_verdicts_keep_the_frontend_status_vocabulary() {
        for (verdict, expected) in [
            (safe_fs::TrashVerdict::InRecycleBin, "ok"),
            (safe_fs::TrashVerdict::Unverified, "ok"),
            (safe_fs::TrashVerdict::PermanentlyDeleted, "permanently_deleted"),
        ] {
            let mut outcome = CleanupOutcome { path: "victim".into(), status: "ok".into(), message: None };
            outcome.apply_trash_verdict(verdict);
            assert_eq!(outcome.status, expected);
            assert!(matches!(outcome.status.as_str(), "ok" | "failed" | "skipped" | "permanently_deleted"));
        }
    }
}
