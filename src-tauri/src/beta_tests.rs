use super::{undo_action_with, validate_rule_destination};
use crate::{db, operations as op, rules, suppress::SuppressionSet};
use std::{fs, path::{Path, PathBuf}, sync::{Arc, MutexGuard}};

struct Fixture {
    root: PathBuf,
    folder: i64,
    rules: Vec<i64>,
    suppression: Arc<SuppressionSet>,
    _db: MutexGuard<'static, ()>,
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
    assert!(!f.apply(&plan, 0).success);
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
    assert!(op::apply(&plan.id, &selected, &f.suppression).unwrap().iter().all(|row| row.success));
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
