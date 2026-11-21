use super::{rule_for_editor, rule_from_editor, undo_action_with, validate_rule_destination};
use crate::{db, operations as op, rules, suppress::SuppressionSet};
use std::{fs, path::{Path, PathBuf}, sync::{Arc, MutexGuard}};

#[test]
fn rule_editor_wire_preserves_exact_u64_sizes_without_altering_file_json() {
    let mut rule = db::Rule {
        name: "Exact byte limits".into(), extensions: vec!["*".into()], action: "ignore".into(),
        options: db::RuleOptions {
            min_size: Some(u64::MAX - 1), max_size: Some(u64::MAX),
            ..Default::default()
        },
        ..Default::default()
    };
    let stored = serde_json::to_value(&rule).unwrap();
    assert_eq!(stored["options"]["max_size"].as_u64(), Some(u64::MAX));
    let mut wire = rule_for_editor(rule.clone()).unwrap();
    assert_eq!(wire["options"]["min_size"], "18446744073709551614");
    assert_eq!(wire["options"]["max_size"], "18446744073709551615");
    assert_eq!(rule_from_editor(wire.clone()).unwrap(), rule);
    assert_eq!(rule_from_editor(stored).unwrap(), rule);

    for invalid in ["1e3", "-1", "1.5", "", "18446744073709551616"] {
        wire["options"]["min_size"] = serde_json::Value::String(invalid.into());
        assert_eq!(rule_from_editor(wire.clone()).unwrap_err(), "validation.size");
    }
    wire["options"]["min_size"] = serde_json::Value::String(u64::MAX.to_string());
    assert!(validate_rule_destination(&rule_from_editor(wire.clone()).unwrap()).is_ok());
    wire["options"]["max_size"] = serde_json::Value::String((u64::MAX - 1).to_string());
    assert_eq!(validate_rule_destination(&rule_from_editor(wire).unwrap()).unwrap_err(), "validation.size");

    rule.options.min_size = Some(0);
    rule.options.max_size = None;
    let ordinary = rule_for_editor(rule.clone()).unwrap();
    assert_eq!(ordinary["options"]["min_size"].as_u64(), Some(0));
    assert!(ordinary["options"]["max_size"].is_null());
    assert_eq!(rule_from_editor(ordinary).unwrap(), rule);
}

struct Fixture {
    root: PathBuf,
    folder: i64,
    rules: Vec<i64>,
    suppression: Arc<SuppressionSet>,
    _db: MutexGuard<'static, ()>,
}

struct RegisteredFolder(i64);

impl RegisteredFolder {
    fn manual(path: &Path) -> Self {
        Self(db::add_watched_folder(path.to_str().unwrap(), "manual").unwrap())
    }
}

impl Drop for RegisteredFolder {
    fn drop(&mut self) {
        db::remove_watched_folder(self.0).unwrap();
    }
}

impl Fixture {
    fn new() -> Self {
        let guard = db::serialise_test_db();
        db::init_test_db();
        let root = std::env::temp_dir().join(format!("mouzi-organization-test-{}", op::new_run_id()));
        fs::create_dir(&root).unwrap();
        let folder = db::add_watched_folder(root.to_str().unwrap(), "manual").unwrap();
        Self { root, folder, rules: Vec::new(), suppression: Arc::new(SuppressionSet::new()), _db: guard }
    }

    fn write(&self, name: &str, contents: &str) -> PathBuf {
        let path = self.root.join(name);
        fs::write(&path, contents).unwrap();
        path
    }

    fn rule(&self) -> db::Rule {
        db::Rule {
            name: "Reviewed documents".into(),
            priority: i32::MIN,
            enabled: true,
            extensions: vec!["txt".into()],
            destination: "Sorted".into(),
            action: "move".into(),
            folder_id: self.folder,
            ..Default::default()
        }
    }

    fn add(&mut self, rule: &db::Rule) -> i64 {
        validate_rule_destination(rule).unwrap();
        let id = db::add_rule(rule).unwrap();
        self.rules.push(id);
        id
    }

    fn preview(&self, paths: &[&Path]) -> op::Preview {
        op::preview(Some(paths.iter().map(|path| path.to_string_lossy().into_owned()).collect()), &self.suppression).unwrap()
    }

    fn apply(&self, plan: &op::Preview, index: usize) -> op::OperationResult {
        op::apply(&plan.id, &[plan.entries[index].id.clone()], &self.suppression).unwrap().remove(0)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        for id in &self.rules {
            db::delete_rule(*id).unwrap();
        }
        db::remove_watched_folder(self.folder).unwrap();
        assert_eq!(self.root.parent(), Some(std::env::temp_dir().as_path()));
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn review_selection_evidence_matches_only_unchanged_proposals() {
    let mut fixture = Fixture::new();
    let path = fixture.write("ordinary.txt", "first");
    let mut rule = fixture.rule();
    rule.id = Some(fixture.add(&rule));
    let snapshot = || {
        let preview = fixture.preview(&[&path]);
        assert_eq!(preview.entries.len(), 1);
        let entry = serde_json::to_value(&preview.entries[0]).unwrap();
        op::discard(&preview.id);
        assert!(entry["error"].is_null());
        assert!(entry["selectionKey"].as_str().is_some());
        (preview.id, entry)
    };

    let (first_plan, first) = snapshot();
    let (next_plan, next) = snapshot();
    assert_ne!(first_plan, next_plan);
    assert_ne!(first["id"], next["id"]);
    assert_eq!(first["selectionKey"], next["selectionKey"]);

    let modified = fs::metadata(&path).unwrap().modified().unwrap();
    let identity = crate::safe_fs::file_identity(&path).unwrap();
    fs::write(&path, b"other").unwrap();
    fs::OpenOptions::new().write(true).open(&path).unwrap().set_modified(modified).unwrap();
    let (_, changed) = snapshot();
    assert_eq!(fs::metadata(&path).unwrap().modified().unwrap(), modified);
    assert_eq!(crate::safe_fs::file_identity(&path).unwrap(), identity);
    assert_eq!(first["size"], changed["size"]);
    assert_eq!(first["source"], changed["source"]);
    assert_eq!(first["destination"], changed["destination"]);
    assert_ne!(first["selectionKey"], changed["selectionKey"]);

    rule.notification_message = Some("Updated receipt".into());
    db::update_rule(&rule).unwrap();
    let (_, edited_rule) = snapshot();
    assert_eq!(changed["rule"], edited_rule["rule"]);
    assert_eq!(changed["destination"], edited_rule["destination"]);
    assert_ne!(changed["selectionKey"], edited_rule["selectionKey"]);
    assert_eq!(fs::read(&path).unwrap(), b"other");
}

#[test]
fn scoped_rule_conditions_and_normalized_extensions_survive_storage() {
    let mut f = Fixture::new();
    let path = f.write("invoice.txt", "12345");
    let info = rules::scan_file(&path).unwrap();
    let date = chrono::DateTime::<chrono::Utc>::from(fs::metadata(&path).unwrap().modified().unwrap())
        .format("%Y-%m-%d").to_string();
    let mut rule = f.rule();
    rule.extensions = vec![" TXT, msi, ,".into()];
    rule.pattern = Some("^invoice".into());
    rule.options.min_size = Some(5);
    rule.options.max_size = Some(5);
    rule.options.modified_after = Some(date.clone());
    rule.options.modified_before = Some(date);
    let id = f.add(&rule);
    let stored = db::get_rules().unwrap().into_iter().find(|rule| rule.id == Some(id)).unwrap();
    assert_eq!(stored.extensions, ["txt", "msi"]);
    assert!(rules::matches_rule(&info, &stored));
    let extensionless = f.write("invoice", "12345");
    assert!(!rules::matches_rule(&rules::scan_file(&extensionless).unwrap(), &stored));
    rule.options.min_size = Some(6);
    assert!(!rules::matches_rule(&info, &rule));
    assert!(validate_rule_destination(&rule).is_err());
    rule.options.min_size = Some(5);
    rule.pattern = Some("[".into());
    assert!(validate_rule_destination(&rule).is_err());
    rule.pattern = None;
    rule.options.modified_after = Some("invalid".into());
    assert!(validate_rule_destination(&rule).is_err());

    let other = f.root.join("Other");
    fs::create_dir(&other).unwrap();
    let other_id = db::add_watched_folder(other.to_str().unwrap(), "manual").unwrap();
    let folders = db::get_watched_folders().unwrap();
    assert!(rules::matching_rule_in_root(&info, Some(&other), &folders, &[stored]).is_none());
    db::remove_watched_folder(other_id).unwrap();
}

#[cfg(windows)]
#[test]
fn review_distinguishes_read_only_files_from_sharing_locks() {
    use std::os::windows::fs::OpenOptionsExt;

    let mut fixture = Fixture::new();
    fixture.add(&fixture.rule());
    assert!(db::get_settings().unwrap().lock_check_enabled);
    let read_only = fixture.write("read-only.txt", "read-only payload");
    let locked = fixture.write("locked.txt", "open payload");
    let expected = fixture.root.join("Sorted").join("read-only.txt");
    let mut permissions = fs::metadata(&read_only).unwrap().permissions();
    permissions.set_readonly(true);
    fs::set_permissions(&read_only, permissions).unwrap();
    let held = fs::OpenOptions::new().read(true).share_mode(1 | 4).open(&locked).unwrap();

    let preview = op::preview(
        Some([&read_only, &locked].into_iter().map(|path| path.to_string_lossy().into_owned()).collect()),
        &fixture.suppression,
    );
    drop(held);
    let result = match &preview {
        Ok(plan) => match plan.entries.iter().find(|entry| Path::new(&entry.source).ends_with("read-only.txt")) {
            Some(entry) if entry.error.is_none() => Some(op::apply(&plan.id, &[entry.id.clone()], &fixture.suppression)),
            _ => { op::discard(&plan.id); None }
        },
        Err(_) => None,
    };

    let mut preserved = false;
    for path in [&read_only, &expected] {
        if let Ok(metadata) = fs::metadata(path) {
            let mut permissions = metadata.permissions();
            if path == &expected { preserved = permissions.readonly(); }
            permissions.set_readonly(false);
            fs::set_permissions(path, permissions).unwrap();
        }
    }

    let entries = preview.unwrap().entries;
    let retained = entries.iter().find(|entry| Path::new(&entry.source).ends_with("locked.txt")).unwrap();
    assert_eq!(retained.error.as_deref(), Some("preview.locked"));
    assert_eq!(fs::read_to_string(&locked).unwrap(), "open payload");
    let entry = entries.iter().find(|entry| Path::new(&entry.source).ends_with("read-only.txt")).unwrap();
    assert_eq!(entry.error, None);
    let result = result.unwrap().unwrap();
    assert!(result[0].success, "{:?}", result[0].error);
    assert!(preserved);
    assert_eq!(fs::read_to_string(&expected).unwrap(), "read-only payload");
    assert!(!read_only.exists());
}

#[cfg(windows)]
#[test]
fn review_reports_a_writer_held_file_until_it_can_be_previewed() {
    use std::os::windows::fs::OpenOptionsExt;

    let mut fixture = Fixture::new();
    fixture.add(&fixture.rule());
    let held = fixture.write("writer-held.txt", "writer payload");
    let healthy = fixture.write("healthy.txt", "healthy payload");
    let excluded = fixture.write("excluded.txt", "excluded payload");
    let unmatched = fixture.write("unmatched.bin", "unmatched payload");
    let canonical_root = fs::canonicalize(&fixture.root).unwrap();
    let history_before = db::get_organization_logs().unwrap().len();
    crate::ignore::save_mouziignore(fixture.root.to_str().unwrap(), &["excluded.txt".into()]).unwrap();
    let writers: Vec<_> = [&held, &excluded, &unmatched].into_iter().map(|path| {
        fs::OpenOptions::new().write(true).share_mode(1).open(path).unwrap()
    }).collect();
    let sharing_denial = fs::OpenOptions::new().read(true).share_mode(1).open(&held).unwrap_err();
    assert_eq!(sharing_denial.raw_os_error(), Some(32));

    let all = op::preview(None, &fixture.suppression).unwrap();
    op::discard(&all.id);
    let sole = fixture.preview(&[&held]);
    op::discard(&sole.id);
    let empty = fixture.preview(&[&excluded, &unmatched]);
    op::discard(&empty.id);
    drop(writers);
    let refreshed = op::preview(None, &fixture.suppression).unwrap();
    op::discard(&refreshed.id);

    assert_eq!(all.entries.len(), 1);
    assert_eq!(Path::new(&all.entries[0].source), canonical_root.join("healthy.txt"));
    assert!(all.entries[0].error.is_none());
    assert!(all.entries[0].selection_key.is_some());
    assert!(sole.entries.is_empty());
    assert!(empty.entries.is_empty());
    let wire = serde_json::to_value(&all).unwrap();
    assert_eq!(wire["deferredFiles"], serde_json::json!([{
        "source": canonical_root.join("writer-held.txt"),
        "error": "preview.locked",
    }]), "the writer-held path must be reported beside the healthy proposal");
    assert_eq!(serde_json::to_value(&sole).unwrap()["deferredFiles"], serde_json::json!([{
        "source": held,
        "error": "preview.locked",
    }]));
    assert!(serde_json::to_value(&empty).unwrap().get("deferredFiles").is_none());
    assert!(serde_json::to_value(&refreshed).unwrap().get("deferredFiles").is_none());
    assert_eq!(refreshed.entries.len(), 2);
    let recovered = refreshed.entries.iter().find(|entry| Path::new(&entry.source) == canonical_root.join("writer-held.txt")).unwrap();
    assert!(recovered.error.is_none());
    assert!(recovered.selection_key.is_some());
    assert_eq!(Path::new(recovered.destination.as_deref().unwrap()), canonical_root.join("Sorted/writer-held.txt"));
    assert_eq!(fs::read_to_string(&held).unwrap(), "writer payload");
    assert_eq!(fs::read_to_string(&healthy).unwrap(), "healthy payload");
    assert_eq!(fs::read_to_string(&excluded).unwrap(), "excluded payload");
    assert_eq!(fs::read_to_string(&unmatched).unwrap(), "unmatched payload");
    assert_eq!(db::get_organization_logs().unwrap().len(), history_before);
}

#[test]
fn apply_uses_only_the_selected_entries_and_rechecks_the_review() {
    let mut f = Fixture::new();
    let mut rule = f.rule();
    rule.id = Some(f.add(&rule));
    let a = f.write("a.txt", "alpha");
    let b = f.write("b.txt", "beta");
    let plan = f.preview(&[&a, &b]);
    assert_eq!(plan.entries.len(), 2);
    assert!(a.exists() && b.exists());
    assert!(!f.root.join("Sorted").exists());
    let result = f.apply(&plan, 0);
    assert!(result.success, "{:?}", result.error);
    assert!(!a.exists() && b.exists());
    assert!(op::apply(&plan.id, &[], &f.suppression).is_err());

    let plan = f.preview(&[&b]);
    fs::write(&b, "edit").unwrap();
    let failed = f.apply(&plan, 0);
    assert!(!failed.success);
    assert_eq!(failed.source, b.to_string_lossy());
    assert!(serde_json::to_value(failed).unwrap().get("actualDestination").is_none());
    let plan = f.preview(&[&b]);
    let destination = plan.entries[0].destination.as_ref().unwrap();
    fs::write(destination, "occupied").unwrap();
    assert!(!f.apply(&plan, 0).success);
    assert_eq!(fs::read_to_string(destination).unwrap(), "occupied");

    let plan = f.preview(&[&b]);
    assert!(plan.entries[0].destination.as_ref().unwrap().ends_with("b_0.txt"));
    rule.destination = "Elsewhere".into();
    db::update_rule(&rule).unwrap();
    assert!(!f.apply(&plan, 0).success);
    assert!(b.exists());
}

#[test]
fn reviewed_move_receipt_matches_the_collision_destination_and_history() {
    let mut fixture = Fixture::new();
    fixture.add(&fixture.rule());
    let source = fixture.write("sample.txt", "selected payload");
    let sibling = fixture.write("sibling.txt", "unselected payload");
    fs::create_dir(fixture.root.join("Sorted")).unwrap();
    let occupied = fixture.write("Sorted/sample.txt", "occupied payload");
    let plan = fixture.preview(&[&source, &sibling]);
    let index = plan.entries.iter().position(|item| item.source == source.to_string_lossy()).unwrap();
    let reviewed = plan.entries[index].destination.as_deref().unwrap();
    assert!(reviewed.ends_with("sample_0.txt"));

    let result = fixture.apply(&plan, index);
    assert!(result.success, "{:?}", result.error);
    let wire = serde_json::to_value(&result).unwrap();
    let committed = fs::canonicalize(reviewed).unwrap();
    assert_eq!(wire["actualDestination"].as_str(), committed.to_str());
    let history = op::history(&op::HistoryFilter::default()).unwrap();
    let recorded = history.iter().find(|row| row.action.source_path == result.source).unwrap();
    assert_eq!(recorded.action.destination_path.as_deref(), wire["actualDestination"].as_str());
    assert!(!source.exists());
    assert_eq!(fs::read_to_string(reviewed).unwrap(), "selected payload");
    assert_eq!(fs::read_to_string(occupied).unwrap(), "occupied payload");
    assert_eq!(fs::read_to_string(sibling).unwrap(), "unselected payload");
}

#[test]
fn reviewed_move_receipt_keeps_the_known_destination_if_history_fails() {
    let mut fixture = Fixture::new();
    fixture.add(&fixture.rule());
    let source = fixture.write("history-failure.txt", "reviewed payload");
    let plan = fixture.preview(&[&source]);
    let destination = plan.entries[0].destination.as_deref().unwrap();
    let source_path = source.to_string_lossy().replace('\'', "''");
    {
        let conn = db::lock_db().unwrap();
        conn.execute_batch(&format!(
            "CREATE TEMP TRIGGER fail_review_history BEFORE INSERT ON action_logs
             WHEN NEW.source_path='{source_path}'
             BEGIN SELECT RAISE(ABORT,'injected review history failure'); END;"
        )).unwrap();
    }
    struct TriggerGuard;
    impl Drop for TriggerGuard {
        fn drop(&mut self) {
            let _ = db::lock_db().unwrap().execute("DROP TRIGGER IF EXISTS fail_review_history", []);
        }
    }
    let trigger = TriggerGuard;
    let result = fixture.apply(&plan, 0);
    drop(trigger);

    let wire = serde_json::to_value(&result).unwrap();
    let conn = db::lock_db().unwrap();
    let pending: String = conn.query_row(
        "SELECT status FROM operation_intents WHERE source_path=?1",
        [result.source.as_str()], |row| row.get(0),
    ).unwrap();
    conn.execute("DELETE FROM operation_intents WHERE source_path=?1", [result.source.as_str()]).unwrap();
    let recorded: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM action_logs WHERE source_path=?1)",
        [result.source.as_str()], |row| row.get(0),
    ).unwrap();
    drop(conn);

    assert!(!result.success);
    assert!(!source.exists());
    assert_eq!(fs::read_to_string(destination).unwrap(), "reviewed payload");
    assert_eq!(wire["actualDestination"].as_str(), fs::canonicalize(destination).unwrap().to_str());
    assert!(result.error.unwrap().contains("injected review history failure"));
    assert_eq!(pending, "pending");
    assert!(!recorded);
}

#[test]
fn unavailable_sibling_is_reported_and_healthy_operations_work() {
    let mut fixture = Fixture::new();
    let rule = fixture.rule();
    fixture.add(&rule);
    let source = fixture.write("healthy.txt", "healthy file");
    let sibling = fixture.root.with_extension("unavailable");
    let _unavailable = RegisteredFolder::manual(&sibling);

    let preview = op::preview(None, &fixture.suppression).unwrap();
    assert!(preview.unavailable_roots.iter().any(|path| Path::new(path) == sibling.as_path()));
    let index = preview.entries.iter().position(|entry| Path::new(&entry.source).file_name() == source.file_name()).unwrap();
    let result = fixture.apply(&preview, index);
    assert!(result.success, "{:?}", result.error);
    assert_eq!(fs::read(fixture.root.join("Sorted/healthy.txt")).unwrap(), b"healthy file");
    assert!(!source.exists());
    assert!(!sibling.exists());
}

#[test]
fn unreadable_review_exclusions_leave_healthy_roots_and_explicit_scope_intact() {
    let mut fixture = Fixture::new();
    fixture.add(&fixture.rule());
    let affected = fixture.write("blocked.txt", "blocked data");
    let healthy_dir = fixture.root.join("healthy");
    fs::create_dir(&healthy_dir).unwrap();
    let healthy_root = RegisteredFolder::manual(&healthy_dir);
    fixture.add(&db::Rule { folder_id: healthy_root.0, ..fixture.rule() });
    let healthy = fixture.write("healthy/healthy.txt", "healthy data");
    let bad_policy = fixture.root.join(".mouziignore");
    fs::write(&bad_policy, [0xff]).unwrap();

    let exercise = |paths, shown: &[&Path], blocked: &[&Path]| {
        let preview = op::preview(paths, &fixture.suppression).unwrap();
        let canonical = |path: &Path| fs::canonicalize(path).unwrap();
        let mut actual = preview.entries.iter().map(|entry| canonical(Path::new(&entry.source))).collect::<Vec<_>>();
        let mut expected = shown.iter().map(|path| canonical(path)).collect::<Vec<_>>();
        actual.sort();
        expected.sort();
        assert_eq!(actual, expected);
        assert!(preview.entries.iter().all(|entry| entry.error.is_none()));
        let json = serde_json::to_value(&preview).unwrap();
        let rows = json.get("blockedRoots").and_then(serde_json::Value::as_array).map(Vec::as_slice).unwrap_or_default();
        assert_eq!(rows.len(), blocked.len());
        for root in blocked {
            let row = rows.iter().find(|row| row["root"].as_str().is_some_and(|path| canonical(Path::new(path)) == canonical(root))).unwrap();
            let detail = row["error"].as_str().unwrap();
            assert!(detail.contains(".mouziignore"));
            assert!(detail.contains("Unable to read"));
        }
        op::discard(&preview.id);
    };

    exercise(None, &[&healthy], &[&fixture.root]);
    exercise(Some(vec![healthy.to_string_lossy().into_owned()]), &[&healthy], &[]);
    exercise(Some(vec![affected.to_string_lossy().into_owned()]), &[], &[&fixture.root]);

    // A healthy preview does not authorize a file after its policy changes.
    let verified = fixture.preview(&[&healthy]);
    let healthy_policy = healthy_dir.join(".mouziignore");
    fs::write(&healthy_policy, [0xff]).unwrap();
    let failed = fixture.apply(&verified, 0);
    assert!(!failed.success);
    assert!(failed.error.unwrap().contains(".mouziignore"));
    exercise(None, &[], &[&fixture.root, &healthy_dir]);
    fs::remove_file(&healthy_policy).unwrap();

    // A missing source remains ineligible at execution.
    let verified = fixture.preview(&[&healthy]);
    let stored = fs::read(&healthy).unwrap();
    fs::remove_file(&healthy).unwrap();
    let failed = fixture.apply(&verified, 0);
    assert!(!failed.success);
    assert_eq!(failed.error.as_deref(), Some("preview.changed"));
    fs::write(&healthy, stored).unwrap();

    fs::remove_file(&bad_policy).unwrap();
    exercise(None, &[&affected, &healthy], &[]);
    assert_eq!(fs::read(&affected).unwrap(), b"blocked data");
    assert_eq!(fs::read(&healthy).unwrap(), b"healthy data");
    assert!(!fixture.root.join("Sorted").exists());
    assert!(!healthy_dir.join("Sorted").exists());
}

#[test]
fn unreadable_review_exclusions_do_not_create_a_warning_for_an_empty_candidate_scope() {
    let fixture = Fixture::new();
    let policy = fixture.root.join(".mouziignore");
    fs::write(&policy, [0xff]).unwrap();

    let preview = fixture.preview(&[&policy]);
    assert!(preview.entries.is_empty());
    let json = serde_json::to_value(&preview).unwrap();
    assert!(json.get("blockedRoots").is_none());
    op::discard(&preview.id);
}

#[test]
fn unreadable_nested_review_exclusions_block_the_whole_owning_root_without_reserving_a_name() {
    let mut fixture = Fixture::new();
    let healthy_dir = fixture.root.join("zz-healthy");
    fs::create_dir(&healthy_dir).unwrap();
    let healthy_root = RegisteredFolder::manual(&healthy_dir);
    let bad_parent = fixture.root.join("zz-exclusions");
    fs::create_dir(&bad_parent).unwrap();
    let shared_destination = fixture.root.join("Sorted");
    let original_rule = db::Rule {
        destination: shared_destination.to_string_lossy().into_owned(),
        ..fixture.rule()
    };
    fixture.add(&original_rule);
    fixture.add(&db::Rule { folder_id: healthy_root.0, ..original_rule });
    let earlier = fixture.write("same.txt", "excluded root data");
    let nested = fixture.write("zz-exclusions/report.txt", "nested data");
    let healthy = fixture.write("zz-healthy/same.txt", "healthy data");
    fs::write(bad_parent.join(".mouziignore"), [0xff]).unwrap();

    let preview = fixture.preview(&[&earlier, &nested, &healthy]);
    assert_eq!(preview.entries.len(), 1);
    assert_eq!(Path::new(&preview.entries[0].source), &healthy);
    assert!(preview.entries[0].error.is_none());
    let destination = preview.entries[0].destination.as_deref().unwrap();
    assert_eq!(Path::new(destination).file_name(), healthy.file_name());
    let json = serde_json::to_value(&preview).unwrap();
    assert_eq!(json["blockedRoots"].as_array().unwrap().len(), 1);
    assert_eq!(fs::canonicalize(json["blockedRoots"][0]["root"].as_str().unwrap()).unwrap(), fs::canonicalize(&fixture.root).unwrap());
    assert!(json["blockedRoots"][0]["error"].as_str().unwrap().contains("zz-exclusions"));
    op::discard(&preview.id);
    assert!(!shared_destination.exists());
}

#[test]
fn unavailable_reviewed_root_fails_independently_at_apply() {
    let mut fixture = Fixture::new();
    let healthy_rule = fixture.rule();
    fixture.add(&healthy_rule);
    let healthy = fixture.write("healthy.txt", "healthy data");
    let transient = fixture.root.join("transient");
    fs::create_dir(&transient).unwrap();
    let folder = RegisteredFolder::manual(&transient);
    let mut transient_rule = fixture.rule();
    transient_rule.folder_id = folder.0;
    fixture.add(&transient_rule);
    let dropped = fixture.write("transient/offline.txt", "offline data");

    let preview = fixture.preview(&[&healthy, &dropped]);
    assert_eq!(preview.entries.len(), 2);
    assert!(preview.unavailable_roots.is_empty());
    let entries = preview.entries.iter().map(|entry| entry.id.clone()).collect::<Vec<_>>();
    // Renaming the disposable folder models the originals becoming unavailable
    // after review, with their bytes still present and untouched.
    let disconnected = fixture.root.join("disconnected");
    fs::rename(&transient, &disconnected).unwrap();
    let results = op::apply(&preview.id, &entries, &fixture.suppression).unwrap();
    assert_eq!(results.len(), 2);
    assert!(results.iter().any(|item| item.source.ends_with("healthy.txt") && item.success));
    assert!(results.iter().any(|item| item.source.ends_with("offline.txt") && !item.success));
    assert_eq!(fs::read(fixture.root.join("Sorted/healthy.txt")).unwrap(), b"healthy data");
    assert_eq!(fs::read(disconnected.join("offline.txt")).unwrap(), b"offline data");
    assert!(!transient.exists());
}

#[test]
fn history_extension_filter_includes_rows_without_extension_metadata() {
    let fixture = Fixture::new();
    let rule = fixture.root.to_string_lossy().into_owned();
    let mut ids = Vec::new();
    for (file_name, action, stored_extension) in [
        ("legacy.pdf", "move", None),
        ("accepted.PDF", "accept", None),
        ("normalized.txt", "move", Some("pdf")),
        ("extensionless", "move", None),
        ("different.pdf", "move", Some("txt")),
    ] {
        let path = fixture.write(file_name, "sample");
        let id = db::log_action(&db::ActionLog {
            id: None,
            timestamp: chrono::Utc::now(),
            source_path: path.to_string_lossy().into_owned(),
            destination_path: None,
            action: action.into(),
            file_name: file_name.into(),
            file_type: rule.clone(),
            undone: false,
        }).unwrap();
        if let Some(extension) = stored_extension {
            db::lock_db().unwrap().execute(
                "UPDATE action_logs SET file_extension=?1 WHERE id=?2",
                rusqlite::params![extension, id],
            ).unwrap();
        }
        ids.push(id);
    }

    let matches = |extension: &str| {
        let filter = op::HistoryFilter { rule: rule.clone(), extension: extension.into(), ..Default::default() };
        let mut names = op::history(&filter).unwrap().into_iter().map(|row| row.action.file_name).collect::<Vec<_>>();
        names.sort();
        names
    };
    let pdf = matches("pdf");
    let dotted_pdf = matches(".PDF");
    let all = matches("");
    {
        let conn = db::lock_db().unwrap();
        for id in ids {
            conn.execute("DELETE FROM action_logs WHERE id=?1", [id]).unwrap();
        }
    }
    assert_eq!(pdf, ["accepted.PDF", "legacy.pdf", "normalized.txt"]);
    assert_eq!(dotted_pdf, pdf);
    assert_eq!(all.len(), 5);
}

#[test]
fn rename_run_history_and_undo_verify_content_and_preserve_collisions() {
    let mut f = Fixture::new();
    let mut rule = f.rule();
    rule.action = "rename".into();
    rule.options.rename_template = "renamed-{stem}.{extension}".into();
    f.add(&rule);
    let a = f.write("note.txt", "original");
    let b = f.write("second.txt", "second");
    let occupied = f.write("renamed-note.txt", "keep");
    let plan = f.preview(&[&a, &b]);
    let renamed = PathBuf::from(plan.entries[0].destination.as_ref().unwrap());
    assert!(renamed.ends_with("renamed-note_0.txt"));
    let selected = plan.entries.iter().map(|entry| entry.id.clone()).collect::<Vec<_>>();
    let results = op::apply(&plan.id, &selected, &f.suppression).unwrap();
    assert!(results.iter().all(|row| row.success));
    let receipt = results.iter().find(|row| row.source == a.to_string_lossy()).unwrap();
    assert_eq!(serde_json::to_value(receipt).unwrap()["actualDestination"].as_str(), fs::canonicalize(&renamed).unwrap().to_str());
    assert!(op::already_processed(&renamed).unwrap());
    let repeat = f.preview(&[&renamed]);
    assert!(repeat.entries.is_empty());
    op::discard(&repeat.id);

    let filter = op::HistoryFilter {
        rule: rule.name.clone(),
        extension: ".TXT".into(),
        ..Default::default()
    };
    let logs: Vec<_> = op::history(&filter).unwrap().into_iter().filter(|row| {
        row.action.source_path == a.to_string_lossy() || row.action.source_path == b.to_string_lossy()
    }).collect();
    assert_eq!(logs.len(), 2);
    assert!(logs[0].run_id.is_some());
    assert_eq!(logs[0].run_id, logs[1].run_id);
    let id = logs.iter().find(|row| row.action.source_path == a.to_string_lossy()).unwrap().action.id.unwrap();

    fs::write(&renamed, "replaced").unwrap();
    assert_eq!(undo_action_with(id, &f.suppression).unwrap().status, "failed");
    assert_eq!(fs::read_to_string(&renamed).unwrap(), "replaced");
    assert!(!a.exists());
    fs::write(&renamed, "original").unwrap();
    fs::write(&a, "new file").unwrap();
    let result = undo_action_with(id, &f.suppression).unwrap();
    assert_eq!(result.status, "collision", "{result:?}");
    let restored = PathBuf::from(result.restored_to.unwrap());
    assert_eq!(fs::read_to_string(&restored).unwrap(), "original");
    assert!(op::already_processed(&restored).unwrap());
    assert_eq!(fs::read_to_string(&a).unwrap(), "new file");
    assert_eq!(fs::read_to_string(occupied).unwrap(), "keep");
    for template in ["../{stem}", "CON.txt", "{unknown}", "a:b", "foo."] {
        rule.options.rename_template = template.into();
        assert!(validate_rule_destination(&rule).is_err(), "{template}");
    }
}

#[test]
fn only_new_and_suggest_respect_baseline_and_keep_suspicious_files_reviewable() {
    let mut f = Fixture::new();
    assert_eq!(db::register_watched_folder(f.root.to_str().unwrap(), db::FOLDER_MODE_MANUAL, false).unwrap(), f.folder);
    let mut rule = f.rule();
    rule.extensions = vec!["*".into()];
    f.add(&rule);
    let old = f.write("old.txt", "old");
    db::set_only_new(f.folder, true).unwrap();
    let new = f.write("new.txt", "new");
    let suspicious = f.write("invoice.pdf.exe", "unexpected extension");
    db::update_folder_mode(f.folder, "suggest").unwrap();

    let plan = f.preview(&[&old, &new, &suspicious]);
    assert_eq!(plan.entries.len(), 2);
    assert!(plan.entries.iter().any(|entry| entry.source.ends_with("new.txt") && !entry.warning));
    assert!(plan.entries.iter().any(|entry| entry.source.ends_with("invoice.pdf.exe") && entry.warning));
    op::discard(&plan.id);

    db::update_folder_mode(f.folder, "paused").unwrap();
    let plan = f.preview(&[&old, &new, &suspicious]);
    assert!(plan.entries.is_empty());
    op::discard(&plan.id);

    db::update_folder_mode(f.folder, "manual").unwrap();
    fs::remove_file(&old).unwrap();
    db::forget_removed_baseline(f.folder, &old, &f.root).unwrap();
    f.write("old.txt", "new file at old path");
    let plan = f.preview(&[&old]);
    assert_eq!(plan.entries.len(), 1);
    op::discard(&plan.id);
}

#[test]
fn delete_rule_cannot_be_applied_without_a_verified_recovery_method() {
    let mut f = Fixture::new();
    let mut rule = f.rule();
    rule.action = "delete".into();
    f.add(&rule);
    let source = f.write("retain.txt", "keep");
    let plan = f.preview(&[&source]);
    assert_eq!(plan.entries[0].error.as_deref(), Some("preview.recoveryUnavailable"));
    assert!(op::apply(&plan.id, &[plan.entries[0].id.clone()], &f.suppression).is_err());
    assert_eq!(fs::read_to_string(source).unwrap(), "keep");
}

#[cfg(windows)]
#[test]
fn moving_preserves_mark_of_the_web_and_never_overwrites() {
    let f = Fixture::new();
    let source = f.write("download.txt", "content");
    let destination = f.root.join("moved.txt");
    let mark = "[ZoneTransfer]\r\nZoneId=3\r\n";
    fs::write(format!("{}:Zone.Identifier", source.display()), mark).unwrap();
    op::move_without_overwrite(&source, &destination).unwrap();
    assert_eq!(fs::read_to_string(format!("{}:Zone.Identifier", destination.display())).unwrap(), mark);
    let source = f.write("download.txt", "another");
    assert!(op::move_without_overwrite(&source, &destination).is_err());
    assert!(source.exists());
}
