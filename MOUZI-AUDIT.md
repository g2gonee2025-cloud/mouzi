# mouzi-elevate — audit

Read-only audit. **Nothing in this repository was modified to produce this document.**
One exception, taken deliberately and before any finding was acted on: the uncommitted work
was committed to a new branch as protection. See §10.

- Audited: 2026-09-27
- Method: 21 parallel read-only agents, one per concern, then every Critical and High claim
  re-checked against source by the orchestrator before it was written down here
- Result: **5 Critical, 9 High**, plus 14 Medium and a long tail of Low
- Coverage: 7 of 21 reports synthesised. These 7 cover the entire safety-critical surface —
  duplicate detection, delete/undo, the filesystem walker, the database, and the rules engine.
  §9 lists what is not yet in.

---

## 1. Executive summary

The elevate work is real, substantial and mostly good. The fork took a working MIT tray
organiser and added a scanner, a dashboard, duplicate detection, a cleanup pipeline and an
optional AI path — roughly 9,400 lines across 14 commits plus 43 uncommitted paths. Several
things that were outright broken upstream are now properly fixed, and a few design decisions
are better than they needed to be.

**The problem is not the work. The problem is that the safety layer has holes, and they are
concentrated in exactly the place that destroys data.**

Duplicate detection is the correct fix for the defect that cost 6.8 GB in the sibling
`file-dashboard` project. Grouping is by full-content blake3 digest, full stop — size is used
only to decide which files are worth reading, never as evidence of identity. That part is
right, and it is right for the right reason.

One layer above it, the code that turns those groups into actions trashes every copy of every
group, including the one the interface badges green "Kept", and a passing test asserts that
this is correct.

The pattern across the whole safety layer is the same: **the identification logic is careful,
and the execution logic assumes the identification was right.** Nothing re-checks at the point
of destruction.

Four other things share this shape:

- `execute_cleanup_cmd` accepts any path on the machine and deletes it.
- The Recycle Bin, which is presented as the safety net, can silently permanently delete.
- The scanner's root boundary — the one thing standing between this app and a whole-drive
  walk — is enforced nowhere.
- A partial scan is indistinguishable from a complete one, and reports success over an empty
  inventory.

### What the numbers say

| | |
|---|---|
| Commits ahead of upstream | 14 (unpushed) |
| Uncommitted paths | 43 (32 M, 4 D, 7 untracked, ~9,438 lines) |
| Tests | 57 Rust (from 11), 12 frontend assertions (from 1 asserting `1 + 1 === 2`) |
| `commands.rs` tests | **0** |
| SQLite pragmas | **0** (only `PRAGMA table_info`) |
| Transactions in the crate | **1** |
| SQL injection vectors | **0 found** |
| Reparse-point escapes / infinite loops | **0 found** |

---

## 2. Critical

### C1 — Duplicates trashes every copy, including the one badged "Kept"

**Verified from source by the orchestrator.**

| Location | Code |
|---|---|
| `src/components/cleanup/DuplicatesTab.tsx:20` | `const [selected, setSelected] = useState<Record<string, string>>({})` |
| `src/utils/cleanup.ts:54` | `const keepPath = keptByGroup[g.hash]` |
| `src/utils/cleanup.ts:56` | `if (f.path !== keepPath) { actions.push(...) }` |
| `src/components/cleanup/DuplicatesTab.tsx:119` | `keepPath={selected[g.hash] ?? g.files[0]?.path}` |
| `src/__tests__/cleanup.test.ts:61-64` | `buildDuplicateActions([a], {})` → `expect(actions).toHaveLength(2)` |

`selected` starts empty. `handleKeep` (`:31-33`) only runs on an explicit click. Until then
`keptByGroup[g.hash]` is `undefined`, and `f.path !== undefined` is true for every file, so
every file in every group is queued.

The interface renders `g.files[0]` as the kept copy and badges it green. The action list
includes it. The count at `DuplicatesTab.tsx:106` is honest about the total but contradicts
the badge sitting next to it.

The backend cannot compensate. `keepPath: undefined` is dropped by `JSON.stringify`, so Rust
receives `None` and the guard at `cleanup.rs:314` never fires.

`cleanup.test.ts:61-64` passes `{}` and asserts that all files come back. **A green test
certifies the destructive default.** Any future change to this function will be reviewed
against a test that says this is what it should do.

A second trigger path: `handleResultsDone` (`DuplicatesTab.tsx:42-46`) re-runs
`findDuplicates()` without rescanning and **never resets `selected`**. A stale
`selected[hash]` pointing at a path no longer in a re-found group re-enters the same branch.

**Fix.** Default `keepPath` to `g.files[0]?.path` inside the loop, not at the render site:

```ts
for (const g of groups) {
  const keepPath = keptByGroup[g.hash] ?? g.files[0]?.path;
  for (const f of g.files) {
    if (f.path !== keepPath) actions.push({ kind: "trash_duplicate", path: f.path, keepPath });
  }
}
```

Then invert the test: assert that the default state returns `n - 1` actions and that the
first file is absent from the result. Also reset `selected` in `handleResultsDone`, and
`execute_one` should verify the kept file still exists (see H6).

### C2 — `execute_cleanup_cmd` deletes any path you give it

**Verified from source by the orchestrator.**

The only path-validation function in the crate is `archive.rs:188 ensure_safe_relative_path`,
which is zip-slip defence for archive entry names. Nothing anywhere checks that a path is
inside a watched root.

`cleanup.rs:307-330 execute_one`:

```rust
if let Some(ref keep) = action.keep_path { if keep == path { return skipped } }   // :314
if !path_obj.exists() { return skipped }                                          // :323
match safe_fs::delete_to_trash(path_obj) { ... }                                 // :330
```

The only guard is a self-reference supplied by the same caller. `undo_action_cmd`,
`undo_all_cmd` and `accept_suggestion_cmd` are in the same position.

Amplifiers:
- `tauri.conf.json:25` is `"csp": null` — no Content Security Policy.
- `"withGlobalTauri": true`.
- In Tauri v2, capabilities gate **only** `core:` plugin permissions. All ~50 app-defined
  commands are callable from all 6 windows regardless of the capability file.

So any script that reaches the webview, and any malicious dependency in the bundle, can call
`invoke("execute_cleanup_cmd", { actions: [{ kind: "trash_large", path: "…\\taxes.pdf" }] })`.

**Fix.** A `require_within_roots(path) -> Result<(), String>` in `safe_fs.rs`, called at the
top of every destructive command, resolving the path first and rejecting anything not inside
an enabled watched folder. Set a real CSP.

### C3 — Window consolidation breaks the Tauri capability ACL

The uncommitted work replaces the `dashboard` / `cleanup` / `suggestions` / `settings`
windows with one window labelled `"app"` (`tray.rs:170`). But
`src-tauri/capabilities/default.json:5` still lists:

```json
["main", "popup", "settings", "dashboard", "cleanup", "suggestions"]
```

No `"app"`. Tauri 2's `authority.rs:439-470 resolve_access()` denies any window label that
matches no capability. `Settings.tsx:5` imports `save` and `open` from
`@tauri-apps/plugin-dialog` for Import/Export Rules — those will be denied in the
consolidated window.

App-defined `#[tauri::command]`s bypass the ACL, which is exactly why the app will appear to
work. The plugin dialogs are the visible casualty.

**Fix.** Add `"app"` to the array. One line.

### C4 — The Recycle Bin is not the safety net it appears to be

`trash` 3.3.1 (per `Cargo.lock`) does **not** set `FOFX_RECYCLEONDELETE` (0x00080000).
`windows.rs:59` sets only `FOF_NO_UI | FOF_ALLOWUNDO | FOF_WANTNUKEWARNING`.

- `FOF_NO_UI` is 0x0614, which includes `FOF_NOCONFIRMATION` — "respond Yes to All". It
  auto-answers yes to the permanent-delete prompt.
- When the Recycle Bin cannot accept an item, Windows **silently permanently deletes it**, and
  the crate still returns `Ok(())`. `delete()` returns `Result<(), Error>` — a unit type, so
  there is no flag to inspect.
- `FOF_WANTNUKEWARNING` does nothing when there is no bin for the volume.
- trash 3.3.1 has no `GetAnyOperationsAborted` (added in 5.1.0) and passes `None` for
  `IFileOperationProgressSink`, so per-item outcomes are unobservable.
- If **any** file in a batch cannot be recycled, `IFileOperation` may permanently delete all
  of them. `execute_cleanup` submits everything in one `PerformOperations` batch, so one
  oversized file can escalate the entire batch.

**Mitigating, and worth stating:** the i18n string for a successful cleanup is `"done"`, not
"moved to the Recycle Bin". The UI does not overclaim. It also never explains the semantics,
and the permanent-delete fallback is undocumented in-app.

**Fix.** Upgrade to trash ≥ 5.1 for `GetAnyOperationsAborted`, submit per-item rather than
one batch, and either detect or stop calling the operation "reversible". At minimum, surface
the real contract in the UI.

### C5 — The scanner's root boundary is enforced nowhere

**Verified from source by the orchestrator.**

`commands.rs:58-60`:

```rust
pub fn add_folder_cmd(app: tauri::AppHandle, path: String, mode: String) -> Result<i64, String> {
    let _ = std::fs::create_dir_all(&path);          // :59  creates it, swallows the error
    let id = add_watched_folder(&path, &mode)...     // :60  inserts it
```

Zero validation. It also **creates the directory** and discards the error, so a typo becomes
a real folder on disk and a permanently-broken root that reports zero files forever. `C:\` is
accepted silently.

`update_folder_mode_cmd` three lines below (`:79-81`) *does* validate via
`is_valid_folder_mode`. The omission is specific to the command that establishes the trust
boundary.

Downstream, `start_scan_cmd` (`commands.rs:523-535`) takes roots from `get_watched_folders()`
and filters only on `!is_folder_paused_mode`. `scan_roots` (`scan.rs:163-200`) performs zero
containment checking. `Settings.tsx:153`'s `<Input>` is not `readOnly`, so typing `C:\` is
the path of least resistance.

**Net: add `C:\` and you get a multi-hour whole-drive walk with no way to stop it** (H5).

D4 — "recursive within watched roots only, never whole drives" — holds as a convention, not
as an invariant.

**Fix.** Validate in `add_folder_cmd`: reject a filesystem root, require the path to exist and
be a directory, drop the `create_dir_all`. Then re-check containment in `start_scan_cmd`.

---

## 3. High

### H1 — A partial scan is indistinguishable from a complete one

`scan_roots` (`scan.rs:165-198`) per root: load ignore file → `clear_inventory_for_root(root)`
(**autocommit, immediately durable**, `db.rs:992-997`) → walk, appending 500-row batches.
No staging table, no transaction spanning the scan, no generation or epoch column.

- At the `clear` step the previous good inventory is already gone.
- `get_dashboard_stats_cmd` (`commands.rs:582`) is **not gated on `is_scanning`**.
  `DashboardStats` (`commands.rs:566-578`) carries no "scan in progress / data partial" flag,
  so a half-populated inventory is presented as authoritative.
- `last_scan_at` is `MAX(scanned_at)` (`db.rs:790`), set by the **first successful batch**. A
  truncated inventory gets a **fresh** timestamp.
- `flush_batch` (`scan.rs:116-124`) logs the error at `:121` and **still calls
  `self.batch.clear()`** at `:123`. Rows are destroyed, the walk continues, `processed` keeps
  incrementing, and `scan-complete` (`:193-197`) reports the count **walked**, not **persisted**.
  A scan where every INSERT failed emits a successful completion over an empty inventory.
- Quitting from the tray mid-scan leaves a partial inventory with a fresh `last_scan_at`, no
  recovery path, and no rescan-on-startup.

### H2 — Removing a watched folder never prunes its files

`remove_folder_cmd` (`commands.rs:69-76`) deletes the `watched_folders` row. Nothing anywhere
deletes `file_inventory` rows — `clear_inventory_for_root` has exactly one non-test caller,
`scan.rs:167`. So `total_files`, `total_bytes` and `watched_roots` permanently include any
removed folder. **The dashboard's headline numbers are wrong after any removal.**

### H3 — Overlapping roots corrupt per-root attribution

`file_inventory.path` is `UNIQUE` **globally**, not per root (`db.rs:193`), and writes use
`INSERT OR REPLACE` (`db.rs:765`). Overlapping roots are trivially creatable through the free
text field. Add `C:\Users\Me` and `C:\Users\Me\Downloads`; the second insert conflicts on the
global `path` and **REPLACE overwrites `root_path`**. After a scan every file under the outer
root is attributed to the outer root and the inner root reports **zero files** despite having
them. The outer tree is also walked twice.

`COUNT(*)` stays correct, so this is confined to the per-root breakdown — which is what the
dashboard's folder view is built on.

### H4 — The undo reports success for files it never restored

`undo_all_cmd` computes `count` as `results.filter(|r| r.status != "failed")`, which
**includes `"missing"`**. `perform_undo` marks `undone = 1` on the `missing` path and returns
`Ok`, so a file that was missing because a drive is unmounted can never be retried.

The UI discards the return value entirely — `Settings.tsx:749` and `Popup.tsx:219` drop the
promise and re-render from the DB — so a `missing` undo shows a **green "Undone" badge**.
`UndoResult.status` is a bare `String` in both Rust and TypeScript, so no type-level
distinction is possible.

Only four statuses exist; the plan's `cross-device` is **never constructed**.

### H5 — A scan cannot be cancelled

`start_scan_cmd` spawns a thread (`commands.rs:543`) and **drops the `JoinHandle`
immediately**. There is no cancel or stop command in `lib.rs:165-217` and no pause. The
window is hidden rather than destroyed (`tray.rs:178`), so `listen('scan-progress')` handlers
survive and events keep arriving — but the user has no way to stop a run and no completion
signal once the window is closed. Combined with C5, a `C:\` scan is unstoppable.

### H6 — Nothing re-verifies identity at the point of destruction

`execute_one` (`cleanup.rs:307-342`) checks only `path_obj.exists()`. It does not re-verify
that the file is still a duplicate, and it **never checks that the kept copy still exists**.
Delete your keeper, then confirm, and every remaining copy goes.

### H7 — `undo_all_cmd` holds the global DB mutex across real file copies

`db.rs` is a single `Arc<Mutex<Connection>>` (`db.rs:117-121`). `undo_all_cmd`
(`commands.rs:227`) holds `db.lock()` for the whole move loop, and `safe_fs.rs:39` performs a
full `std::fs::copy`. A large cross-device restore blocks every other database operation
app-wide, including the scanner's batch flush.

It also holds `state.ignored_files.lock()` for the whole batch, and the watcher's notify
callback takes that same lock on **every** event. The callback stalls, the OS notification
buffer backs up, and events arrive **late** — past the 30 s suppression window. The
freshly-restored file is then treated as new user activity, queued, and after the 300 s grace
**moved right back**, with its log row already `undone = 1`. No confirmed deadlock; the lock
order is acyclic.

### H8 — The two log tables have no indexes

`action_logs` and `cleanup_actions` have **no indexes at all**, despite
`ORDER BY timestamp DESC LIMIT ?` on both, an unbounded `WHERE undone = 0 ORDER BY timestamp
DESC` that full-scans, sorts and loads every undoable row into memory, and
`timestamp > datetime('now', '-7 days')`. Both are per-dashboard-refresh queries.

### H9 — The parameterised `OR` guard defeats every index

`get_inventory_files_on` builds:

```sql
WHERE (?1='' OR category=?1) AND (?2='' OR root_path=?2) AND (?3='' OR LOWER(path) LIKE ?3)
```

SQLite cannot use an index for a term wrapped in `OR` against a non-indexable operand, so
**all four `file_inventory` indexes are unusable** and this always full-scans.
`LOWER(path) LIKE '%q%'` is a leading wildcard over a function wrapper, so the UNIQUE index on
`path` is dead for search as well.

### H10 — The AI path and duplicate detection block the Tauri main thread

`commands.rs:673-678 get_suggestions_cmd` and `commands.rs:625-631 find_duplicates_cmd` are
both **sync** `#[tauri::command]`s with no `(async)` modifier. Per the Tauri v2 docs: *"Commands
without the async keyword are executed on the main thread unless defined with
`#[tauri::command(async)]`."* **No `#[tauri::command(async)]` exists anywhere in the crate.**

- `get_suggestions_cmd`: `detect_provider()` → a blocking `ureq` GET `/api/tags`
  (`timeout_connect` 2 s, overall 4 s) then up to **5 sequential** POSTs to `/api/generate`,
  each with a 15 s overall timeout. **Worst case ≈ 75 s of frozen UI from one window open.**
  Reachable whenever Ollama is installed and slow — which is exactly when you use it.
- `find_duplicates_cmd`: full-content blake3 of potentially gigabytes, synchronously, with no
  progress and no cancel.

Two independent agents, different scopes, both read the Tauri docs, both reached this
conclusion. D6 mandated a spawned std thread; the only `std::thread::spawn` calls in the
crate are in `watcher.rs`, `scheduler.rs` and the scanner.

The scanner got this right. The AI and dedup paths did not.

---

## 4. Medium

| | Finding |
|---|---|
| M1 | `replace_inventory_for_root` (`db.rs:980`), the **atomic** replace primitive, has **zero callers**. An atomic implementation exists and was replaced by non-atomic clear-then-append. That is the regression, visible in the tree. |
| M2 | No `PRAGMA journal_mode=WAL`, no `busy_timeout`, no `foreign_keys`, no `user_version`. Masked by the single mutex-guarded connection — which is also why D10 is unimplemented *and would be unsafe to add* without `busy_timeout` first. |
| M3 | D10 is partial: only `settings` has a migration path (9 conditional ALTERs). The four tables the fork added have no column-migration path, and there is no `user_version`, so a downgrade is undetectable — it fails at query time, not startup. |
| M4 | `init_db` is **not idempotent in-process**: `OnceCell::set` returns `Err` on the second call. The plan-mandated "init_db twice" test cannot be written as `assert!(is_ok())` and **no idempotency test exists anywhere**. |
| M5 | Test DDL is a `#[cfg(test)]` **duplicate** of the production DDL (`db.rs:731-750` vs `:189-216`). All 7 query tests bypass `init_db`, so they validate a schema the app never creates. |
| M6 | One dashboard refresh = 11 statements, ~10 full scans, 7 separate lock acquisitions, and **no transaction** — so `total_files` and `category_breakdown` can disagree if a scan commits in between. |
| M7 | Unbounded table growth. There is no `DELETE FROM hash_cache` anywhere; `INSERT OR REPLACE` only replaces the exact `(path, size, mtime)` key, so every mtime a file ever has leaves a permanent orphan. `action_logs`, `cleanup_actions` and `dismissed_suggestions` likewise. |
| M8 | `get_logs_cmd` and `get_cleanup_logs_cmd` pass `limit` straight from the frontend. `LIMIT -1` means unlimited in SQLite, so a negative value loads the whole table. `get_inventory_files_cmd` correctly clamps — the inconsistency is the finding. |
| M9 | `scan.rs` does not skip hidden or system files, unlike `rules::should_ignore_file`, and `execute_one` never applies that filter. Hidden and system files can be offered for trashing. |
| M10 | `walk_empty_dirs` (`cleanup.rs:250-286`) is a second, independent walk sharing nothing with the scanner: no `.mouziignore`, no depth bound (the scanner has `MAX_DEPTH = 128`), and it reports a directory as empty when its only children are reparse points. `execute_one`'s re-check runs the same buggy predicate, passes, then `remove_dir` fails `ENOTEMPTY` — a permanently-failing cleanup item. **No data loss**, since `remove_dir` refuses a non-empty directory. |
| M11 | `categorize()` (`scan.rs:44`) has no `Installers` arm although `db.rs:333` defines the category and the default rules rely on it. `.exe`/`.msi` fall through to `Other`, inflating the unclassified insight. |
| M12 | `is_scanning` leaks on panic. Set at `commands.rs:537`, cleared at `:552` only after `scan_roots` returns, with no `catch_unwind`. A poisoned mutex leaves it `true` forever and the app refuses to scan again until restart. |
| M13 | Empty catch blocks — `HistoryPanel.tsx:19-21`, `useCleanupStore.ts:118-120`, plus five new ones in the WIP — violating the plan's explicit hard constraint. Two swallow user-initiated `open_folder_cmd` failures, so the user clicks and nothing happens. |
| M14 | `loadHistory` never sets `error`, so a failed read renders "No cleanup history yet" — an audit surface reporting that it is empty when it actually failed. |
| M15 | `cleanup.rs:395 let _ = db::insert_cleanup_action(...)` swallows the audit write **after** the file is already trashed. `accept_suggestion_cmd` has three more `let _` swallows on the same move-then-log sequence. This is the exact `let _` anti-pattern M1 was meant to eliminate, and it survives in the commit titled "harden production paths against panics and silent data corruption". |
| M16 | Two divergent move implementations: `safe_fs::move_file` (undo) and `rules.rs:154-165 move_file_cross_device` (rules). The rules version catches `Err(_)` for **any** reason and falls back to copy + `fs::remove_file(src)`, so a permission error silently becomes a permanent delete after copy. |
| M17 | D2's "2 s timeout" is only `timeout_connect`; the overall timeouts are 4 s and 15 s. This deviation is what turns a slow model into a frozen app. |
| M18 | `"llama3.2"` is hardcoded (`classify.rs:481`) and `is_available()` only checks that `/api/tags` returns 200, never that the model exists. With a different model, detection succeeds and all five calls 404, swallowed at `classify.rs:645`. Silent misconfiguration. |
| M19 | The "co-occurrence" category is a **user-defined rule name**, not a category. `rules.rs:239` writes `file_type: rule.name.clone()`; `learned_hints()` (`classify.rs:338`) uses it as the category proxy with **no validation** (unlike the Ollama path's allowlist). `accept_suggestion_cmd` then does `parent.join(&suggested_category).join(file_name)`, so Accept moves the file into a folder named after the user's own rule. |
| M20 | Classification is non-deterministic on ties. `classify.rs:284` uses `max_by` over a `HashMap`, whose iteration order is randomised per process. Ties are reachable by construction — `"RECORDING"` is in both `vid_tokens` and `audio_tokens`, `"PACKAGE"` in both `archive_tokens` and `code_tokens`. |
| M21 | Rule patterns are case-sensitive regex on a case-insensitive filesystem. `Regex::new` defaults to case-sensitive and the pattern is stored raw, so `^Invoice` never matches `invoice.pdf` on NTFS. Extensions are fine — exact match, lowercased at both ends; the `evil.txt.exe` worry is not real. |
| M22 | `import_rules_cmd` accepts arbitrary JSON and `add_rule` validates nothing, so a rules file with `"action": "delete"` persists. Bounded — `execute_rule` errors and `process_file` propagates — but it is a permanently broken rule with no UI feedback. |
| M23 | An invalid regex returns `Err` → `.unwrap_or(false)` (`rules.rs:129`), so the rule silently never fires while looking correctly configured. `get_rules()` and `Regex::new` also run per rule, per file event, uncached. |
| M24 | `get_weekly_stats` compares RFC3339 text against `datetime('now')` — a cross-format lexicographic comparison that works by luck (`'T'` 0x54 > `' '` 0x20 at index 10), not by construction. |
| M25 | `store.findDuplicates` swallows its error, so a zero-folder configuration fails with a generic toast and no state. `subscribeScanProgress` is registered on every `syncScanState()` and never unsubscribed — a listener leak producing duplicate progress bars. |
| M26 | `ScanProgress.total` is `Option<u64>` and **always `None`** (`scan.rs:111,183`), so the progress bar can never be determinate. `processed` also resets per root, so a multi-root scan jumps back to zero. |

---

## 5. Low

- `mtime = 0` files bucket as "7d": `to_epoch_secs` returns 0 for unstattable mtimes and
  `get_age_buckets_on` (`db.rs:892-902`) tests `mtime >= now-7d`, which 0 satisfies — while
  `get_insights_on` (`db.rs:858`) explicitly excludes them from stale. The two consumers
  disagree.
- `.mouziignore` is read only from the root (`scan.rs:166`). No per-directory files, no
  `node_modules` / `.git` / venv defaults, and the ignore file itself gets inventoried.
- Walk errors are swallowed entirely: `read_dir` failure → bare `return` (`scan.rs:130-133`),
  `file_type()` failure → bare `continue` (`:135-138`). An offline drive reports
  `files=0, Complete` — indistinguishable from an empty folder.
- `undoable` at `cleanup.rs:391-394` is a `match` with two identical `false` arms, so it is a
  constant dead field and `mark_cleanup_undone` is dead code. The cleanup audit trail is
  strictly write-only. Consistent with D5, but it means the Recycle Bin is the only net.
- No NTFS hard-link or file-ID detection, so two links to one file are reported as duplicates.
  Data survives; the label is wrong.
- `size as u64` on a possibly-negative `i64` (`cleanup.rs:133,162`) flips a corrupt negative
  size into the large/sample path.
- `rules.rs:192` builds `{stem}_{timestamp}.{extension}`, yielding a trailing dot for
  extensionless files — invalid on Windows.
- `rules.rs` + `import_rules_cmd`: a rule can move files to any absolute location.
- Equal rule priorities have no deterministic tiebreak, and no ordering test exists.
- `get_settings` uses `LIMIT 1` with no `ORDER BY`; `mode` has no CHECK constraint.
- `migrate_rules_to_relative` (`db.rs:296-317`) is a textbook N+1 — but it is dead code, so
  Low. No N+1 in any live query path.
- Magic numbers throughout the classifier (0.80, 0.35, 0.30, the 0.6/0.85 gates, the 5-call
  budget) and `"IMG_"/"DSC_"/"DSCN"` duplicated in `img_tokens`.

---

## 6. What the elevate work got right

Recorded so the finding count is not read as a verdict on the whole effort.

**The duplicate-detection fix is correct for the right reason.** `db.rs:1093-1097` uses size
only to select candidates; identity is established at `cleanup.rs:166-180` by a `HashMap`
keyed on a full-content blake3 digest. The 64 KB sample for files over 100 MB is demoted to a
prefilter and always recomputed as a full hash before anything is reported.
`file_inventory.path` is UNIQUE, so a file cannot be grouped with itself. **No code path calls
two files identical without comparing their bytes.** This is the defect that cost 6.8 GB in
`file-dashboard`, properly fixed.

**Reparse points are handled correctly.** `scan.rs:139-141` tests
`ft.is_symlink() || ft.is_symlink_dir()`, and on Windows `FileType::is_symlink()` is true for
**any** `FILE_ATTRIBUTE_REPARSE_POINT` — junctions, mount points, symlinks, OneDrive
placeholders and cloud files are all excluded. No junction escape, no infinite loop.
`DirEntry::metadata()` does not follow symlinks, and `file_type()` on Windows comes from
`FindFirstFileExW` attributes without opening a handle.

**M2's recursive-walk requirement is met.** The scanner does not use `notify` at all — it
imports only `std::fs`, `std::path`, `serde`, `crate::db` and `crate::ignore` — so the
upstream `RecursiveMode::NonRecursive` defect has no bearing on it.

**The upstream broken undo is genuinely fixed.** `commands.rs:174` now routes through
`safe_fs::move_file`, which propagates every error, creates missing parent directories and
suffix-resolves collisions. No `let _`, no bare `fs::rename`.

**D8 is fully compliant, and more strongly than planned.** `execute_rule` still implements
only `move`/`ignore`. `delete` is not merely hidden — the rules form has **no action selector
at all**; `Settings.tsx:491`, `db.rs:343` and `commands.rs:803` all hardcode `"move"`. And
`cleanup.rs` never imports `rules::execute_rule`, so cleanup provably bypasses the rules
engine. Every one of the three mandated mitigations is present.

**The AI seam is genuinely injectable and AI is never a hard dependency.** `AiProvider`
(`classify.rs:405-409`) with two implementations, `detect_provider()` returning
`Box<dyn AiProvider>`, and `get_suggestions` taking `&dyn AiProvider` — proven by
`get_suggestions(10, &provider)` at `classify.rs:1008`. Seven distinct failure paths all
degrade gracefully (transport, read, outer parse, missing `response`, inner parse, missing
`category`, out-of-vocabulary category), and `get_suggestions_cmd` never returns `Err`.
Everything is `serde_json::Value` with `.get().and_then(as_str)`, so a model returning
`{"category": 42}` yields `Err`, not a panic.

**Preview-then-confirm holds for suggestions.** `Suggestions.tsx:239-299` renders each
suggestion as a card with filename, current → suggested category, confidence and source badge
*before* any action button. Accept **moves**, never deletes, and logs for undo.

**No SQL injection.** The only `format!` interpolated into SQL is `db.rs:955-962`, and the
single `{order}` hole is filled from a hardcoded two-arm whitelist never derived from the
`sort` parameter. Every other value is bound. The search term has LIKE metacharacters
stripped rather than escaped (`db.rs:946-949`). All five raw statements in `commands.rs` are
parameterised. **Verified, and worth stating so nobody re-raises it.**

**No catastrophic regex backtracking.** Rust's `regex` crate is a finite-automaton engine
with no backtracking, and `process_file` matches one file per event rather than walking a
tree. Neither worry applies.

**The WIP did not erode the safety posture.** `safe_fs.rs`, `rules.rs`, `cleanup.rs`,
`scan.rs`, `classify.rs` and `watcher.rs` are all **byte-identical** to HEAD. Every Rust change
is read-only or additive.

**The WIP is coherent, not scattered.** All four new components are wired, not orphaned
(`StorageRibbon`, `InsightCards`, `AgeHistogram`, `FileBrowser`). Both new utils are fully
consumed — every export has a consumer. All three deletions are genuinely superseded:
`LargestFiles` by `FileBrowser` (201 lines, strictly more capable), `App.css` already dead
upstream, and `smoke.test.ts` — which asserted `1 + 1 === 2` — by `dashboard.test.ts` with 12
real assertions including a regression test for the doubled-hash bug. All 10 locales have the
49 dashboard keys. Sort and limit whitelists are closed in **both** Rust and TypeScript.

**Tests grew from 11 to 57 Rust, and from 1 vacuous assertion to 12 real ones.**

---

## 7. Plan compliance

| Decision | Status | Evidence |
|---|---|---|
| D1 fork hygiene, personal-use bar | Compliant | Attribution, licence, app id preserved |
| D2 heuristic-first, Ollama-optional | **Partial** | Heuristic works offline; but the timeout is 4 s/15 s not 2 s (M17), the model name is hardcoded and unchecked (M18), and detection succeeding does not mean the model exists |
| D3 1024×768 dashboard window | **Partial** | The window is built correctly at `tray.rs:163-227` via runtime `WebviewWindowBuilder`. `Settings.tsx` is untouched by all 14 commits, so there is no settings link. C3 breaks the capability ACL |
| D4 recursive within watched roots only | **Non-compliant in enforcement** | The walk is recursive and reparse-safe. The *boundary* is enforced nowhere (C5) |
| D5 two-tier reversibility | **Partial** | Undo genuinely fixed. But `cross-device` status is never constructed, and the Recycle Bin tier is weaker than claimed (C4) |
| D6 Ollama in a spawned std thread | **Violated** | No `thread::spawn` in the classify path; blocks the main thread up to ~75 s (H10) |
| D7 size-prefilter → hash → cache | Compliant | Correctly implemented. The cache has a second-granularity-mtime weakness and grows unbounded |
| D8 never route cleanup through `execute_rule` | **Compliant** | `delete` has no UI selector at all; `cleanup.rs` never imports `execute_rule` |
| D9 vitest for pure-logic helpers only | Compliant | `dashboard.test.ts` respects this. Some of the new tests violate the spirit |
| D10 additive-only migrations, scanner own connection | **Partial** | `settings` has a migration path; the four new tables do not. No `user_version`. No separate read connection — and it would be unsafe to add without `busy_timeout` |

Milestone coverage: M0 and M1 substantially met. M2 is the flagship and is **not** complete —
the scanner runs, but H1 through H3 and C5 are all in M2's scope. M3 is functionally present
with C1 unresolved. M4 is present with H10 and M18. M5 not assessed (the window/tray agent
has not been synthesised).

**Upstream defect remediation: 6 fixed, 3 untouched, 2 partial.**
Untouched: `watcher.rs:317` is still `RecursiveMode::NonRecursive` — the file's entire diff
is one line, a timeout constant. `watcher.rs:110`/`:120` still emit `file-organized` globally
and `App.tsx:56` still listens globally. The sync initial scan block `watcher.rs:206-252` is
byte-identical. **All three are in the live file-ORGANISATION path** — the scanner got
recursion, the organizer did not.

---

## 8. Refuted / non-issues

Checked and cleared. Recorded so nobody re-raises them.

| Claim | Verdict |
|---|---|
| SQL injection via the `sort` parameter | **Not possible.** Hardcoded two-arm whitelist, never derived from the parameter |
| SQL injection via `commands.rs` raw statements | **Not possible.** All five are `?1`-parameterised |
| Catastrophic regex backtracking | **Not possible.** Rust `regex` is a finite automaton with no backtracking |
| Junction escape / infinite scan loop | **Not possible.** `is_symlink()` covers all Windows reparse points; the scanner does not use `notify` at all |
| `evil.txt.exe` matching a `*.txt` rule | **Not possible.** Extension matching is exact, never glob, lowercased both ends |
| Ollama can inject `..` traversal into a category | **Not possible.** Allowlist of 7 names at `classify.rs:522-529` |
| AI failure can fail a command | **Not possible.** Seven degradation paths; `get_suggestions_cmd` never returns `Err` |
| A hallucinated model field can panic | **Not possible.** `serde_json::Value` with `and_then`/`ok_or_else` throughout |
| `batch_walk` recursion could blow the stack | **Not possible.** `MAX_DEPTH = 128` bounds it |
| MAX_PATH needs mitigation | **Not needed.** Rust std uses the `\\?\` verbatim form for absolute paths |
| Deadlock between the DB mutex and the watcher lock | **Not found.** Lock order is acyclic; `perform_undo` takes `&conn` and never re-locks |
| Schema drift between commits | **Not present.** Verified byte-identical via `git show c4eac33:...` and `9d0171c` |
| `watched_folders.mode` needs a migration | **Not needed.** The column predates the fork |
| N+1 in any live query path | **Not present.** `migrate_rules_to_relative` is N+1 but dead |
| The 4 new components are orphaned | **False.** All four are wired into `Dashboard.tsx` |
| Tests got worse in the WIP | **False.** 1 vacuous assertion became 12 real ones plus 3 Rust tests |
| The `1d6d27e` "harden" commit is pure vapour | **Mostly false.** It fixed 3 real panics and 1 corruption site — but left M15 on the same defect class |

---

## 9. Not yet synthesised

14 of 21 agent reports are collected but not yet read into this document. Nothing in this
section is a claim; it is a list of what the audit has not yet covered.

| Area | Agent | Priority |
|---|---|---|
| Crate-wide panics, swallowed errors, clippy readiness | `bg_cb6d33e8` | High |
| Product safety UX, armed-by-default actions, first-run states | `bg_aa9be9b5` | High |
| Fork hygiene, milestone exit gates, deferred-scope discipline | `bg_28a8a61e` | Medium |
| Test trustworthiness — would the suite survive a deliberate break | `bg_7221e102` | Medium |
| TypeScript strictness, IPC typing, the 2^53 integer risk on file sizes | `bg_1d0a4974` | Medium |
| README false claims, cross-language action-type and status-string integrity | `bg_a23008a4` | Medium |
| `lib.rs` / `tray.rs` window lifecycle and capabilities | `bg_b38148fa` | Low |
| Watcher, event storm, shutdown | `bg_9823cb75` | Low |
| Cargo dependencies, `tauri.conf.json` CSP, build reproducibility | `bg_25aff191` | Low |
| `App.tsx` routing, `invoke` name cross-check | `bg_8ebf7be3` | Low |
| Zustand store divergence and listener leaks | `bg_d9d26174` | Low |
| Component a11y, list keys, hardcoded strings | `bg_84f7eca3` | Low |
| i18n key parity across 10 locales | `bg_96e8d08b` | Low |
| Performance, SQLite pragma verdict, idle cost | `bg_f8e99ed6` | Low |

**One known disagreement, deliberately unresolved.** An agent flagged `safe_fs.rs:80-81`
`raw_os_error() == Some(18)` as a cross-device-detection bug: 18 is `EXDEV` on Linux, whereas
Windows `ERROR_NOT_SAME_DEVICE` is **17** and 18 is `ERROR_NO_MORE_FILES`. A second agent
reported that the adjacent `|| rename_err.kind() == ErrorKind::CrossesDevices` already covers
Windows, making the `18` harmless. **Settled by direct inspection of `safe_fs.rs:80-81`: both
conditions are present and OR'd together, so the `ErrorKind` check is the live one and the
`18` is dead weight — not a bug.** The branch still has no test; `safe_fs.rs:199-210` calls
`copy_delete_fallback` directly and its own comment concedes a cross-device rename failure
cannot be provoked in a unit test. That gap is real.

---

## 10. Verification appendix

**Method.** 21 read-only agents, one per concern, each forbidden from writing to the project,
running cargo/npm/tauri, touching the real SQLite database or the user's files, or writing a
report file into the repo. Scratch files were confined to `%TEMP%\opencode\` and deleted. Each
brief instructed agents to mark unverifiable claims UNVERIFIED rather than assert them, and
several explicitly warned that cross-agent agreement is not evidence — a warning earned by the
sibling `file-dashboard` audit, where roughly 40% of auditor claims were false.

Every Critical finding was then re-checked against source by the orchestrator before being
written here. C1, C2 and C5 were confirmed by direct file reads; C3 and C4 rest on agent
findings that were checked for internal consistency but not re-derived from the Windows SDK
headers.

**Repository state.** Branch `main`, 14 commits ahead of `origin/main`, 0 behind, plus 43
uncommitted paths. `origin` is `https://github.com/hsr88/mouzi.git` — the **public 828-star
upstream**, which the user does not own. The 14 commits are unpushed. The baseline for
comparison is `C:\Users\Hagop Ghazarian\Desktop\mouzi`, a pristine checkout at merge-base
`c4eac33a50194932d4103668005c131875f005f2` with 0 uncommitted changes.

**Work protection applied before any remediation.** Roughly 9,438 lines existed in one folder
on one disk with no remote of their own. Before acting on anything:

- `git switch -c wip/workspace-window`
- `git add -A` → 42 files, +1,968 / −612
- commit `26735ca` "wip: consolidate windows and rebuild dashboard", whose body records the
  consolidation, the dashboard rebuild, the test improvement, the three known defects, and the
  fact that `origin` is the public upstream
- result: **tree clean, 15 commits ahead**
- `git bundle create %TEMP%\opencode\mouzi-elevate-backup.bundle --all` → **16,468,717 bytes**

A secret scan over all committed files returned zero hits. **Nothing was pushed** — pushing to
the public upstream would be publishing the user's work to a repository they do not control.
Fully reversible with `git checkout main`.

The blast radius this closed: `git clean -fd` would have destroyed 8 untracked files (608
lines) irrecoverably, and `git checkout .` would have reverted 34 tracked files.

**Environment.** Node v24.19.0, npm 11.17.0, Rust 1.98, Tauri 2, SQLite via rusqlite 0.32
(bundled), Windows-only in practice.

**Repository hygiene noted, not fixed.** `src-tauri/gen` (4 files, 5,664 lines, generated) is
tracked and should be ignored. `package.json` still names the package `mouzi` at v0.1.6,
which is correct under D8's "keep mouzi branding" but worth a conscious decision. The debug
binary `mouzi.exe` (27.3 MB) sits in `src-tauri/target/debug` and is untracked. An untracked
`start-mouzi.bat` runs `npm run tauri -- dev`.

---

## 11. Addendum - sixth pass: panic reachability and destructive-surface UX

This pass is additive. Nothing above it is edited, including section 9, whose junction claim is
corrected rather than rewritten (11.11). Findings are grouped by the surface that makes them
reachable, not by severity alone, because in most cases the severity comes from the surface.

---

### 11.1 CRITICAL C6 - there is no confirmation dialog anywhere in the frontend

A grep across all of `src/` returns **zero** `confirm()` and **zero** `alert()` calls. This is the
finding. The individual call sites below are its consequences, not five separate bugs, and they
should be fixed as one change: a single confirmation primitive, applied to every destructive
control.

- `src/components/cleanup/DuplicatesTab.tsx:96` - the red destructive button is wired straight to
  the trash action. The label says "confirm"; there is no confirm step. Two clicks from
  destruction after one "Find duplicates".
- `src/pages/Settings.tsx:687` - `undoAll()`, one click, no confirmation, no result display.
  Reverses every un-undone move in the entire history, potentially thousands of files, creating
  timestamp-suffixed duplicates. It is styled as a **neutral bordered button**, visually
  indistinguishable from a preferences toggle. This is the mirror image of the armed-by-default
  cleanup defect and is arguably more dangerous, because users open Settings expecting only
  preferences.
- `src/pages/Settings.tsx:696` - `clearLogs()`, one click, no confirmation. Destroys the only
  record of what the app did to the user's files, which is the audit trail decision D5 depends
  on. Irreversible.
- `src/pages/Settings.tsx:669` - `deleteRule(r.id)`, one click, no confirmation, no undo. It is
  the third of three identical-looking icon buttons; only a red tint on a 14px icon separates it
  from the harmless enable/disable toggle immediately to its left.
- `src/pages/Suggestions.tsx:218-225` - "Accept all" bulk-moves every suggested file on one
  click, with no preview and no count.

---

### 11.2 CRITICAL C7 - "Clean now" moves real files on one click, with no preview

`src/components/Popup.tsx:103-119` (`handleClean`) and `:161-168` (the button, primary-coloured
and the most prominent element in the primary window). It loops every active watched folder
calling `scanFolder(path)`, which invokes `scan_folder_cmd` -> `manual_scan_folder` ->
`process_file` -> rule match -> `fs::rename`. No confirmation, no preview of which files or
where, no count. The outcome is reported only afterwards, as a green toast.

Two amplifiers:

- If no folder is configured, `handleClean` falls back to `get_downloads_folder()`. A fresh
  install's first click of the primary button starts operating on the user's real Downloads.
- `manual_scan_folder` passes `bypass_grace: true` (`src-tauri/src/rules.rs:266`), so files that
  are seconds old, and possibly still being written or still downloading, are moved.
  `is_file_locked` catches genuinely locked files, but an in-progress write that is not locked,
  or a partial `.crdownload`, is fair game.

Separately: `src-tauri/src/commands.rs` `scan_folder_cmd` performs no containment check on the
supplied path, so a path that is not a registered watched root is still scanned and organised.

---

### 11.3 HIGH H11 - second-resolution collision suffix silently overwrites, in BOTH move implementations

Two independent implementations build a suffixed name on collision, and both use whole-second
resolution while checking only whether the **original** destination exists, never whether the
**suffixed** name already exists.

- `src-tauri/src/safe_fs.rs` `resolve_destination` - `SystemTime::now().as_secs()`.
- `src-tauri/src/rules.rs:186-199` - `Utc::now().timestamp()`, and this one checks nothing at
  all on the suffixed path.

`std::fs::rename` on Windows uses `MoveFileExW` with `MOVEFILE_REPLACE_EXISTING`, so it
silently overwrites. Two files mapping to the same destination within the same second means the
second replaces the first.

Reachable from the most prominent button in the app: `manual_scan_folder` processes a whole
folder in one pass, so two `image.jpg` from different subdirectories entering the same category
folder in the same batch overwrite each other. **This is a data-loss path independent of every
other finding in this document.**

---

### 11.4 HIGH H12 - integer overflow in `find_stale_files`, reachable from the frontend

`src-tauri/src/cleanup.rs:237` computes `now - days * 86_400`, where `days: i64` arrives from the
frontend and is clamped only with `.max(1)`. At `days = 1e15` the multiplication exceeds
`i64::MAX` (9.22e18). A debug build panics. **A Tauri release build has `overflow-checks` off by
default, so it wraps** to a negative cutoff, and every file in the inventory is reported as
stale. The user is shown a preview in which everything is stale, and confirming it trashes
everything. Any negative value below roughly -1e14 reaches the same state from the other
direction.

---

### 11.5 HIGH H13 - reachable panic in the watcher from a negative grace period

`src-tauri/src/watcher.rs:240-242` and `:295-297` compute `settings.grace_period_seconds as u64`.
The value is set by `update_settings_cmd` from a whole frontend settings object with **no clamp
and no validation**. `-1 as u64` is 18,446,744,073,709,551,615, and
`Instant::now() + Duration::from_secs(that)` panics on overflow.

The panic occurs on the notify event thread, so silent-mode auto-organisation dies with no
user-visible error and no recovery short of restarting the app. Note the asymmetry: the sibling
check in `rules.rs:24` is guarded by an early `grace_seconds <= 0` return, so only the watcher
path is exposed.

---

### 11.6 HIGH H14 - the audit write is discarded on the move path, so a move can have no undo record

`src-tauri/src/rules.rs:242` `let _ = log_action(&log);` runs **after** the file has already been
moved. If the insert fails, the error is discarded, the file is moved, and no undo row exists.

The same pattern appears four times in `src-tauri/src/commands.rs` at `:747` (`log_action` after
`accept_suggestion_cmd`'s move), `:762` (`update_inventory_category`), `:763`
(`clear_dismissed_suggestion`) and `:795` (`add_rule`), and once at
`src-tauri/src/cleanup.rs:395` (`insert_cleanup_action`, after the file is already trashed).

This is the exact `let _` anti-pattern that commit `1d6d27e` claims to have eliminated, and it
survives in the code that commit touched. Concretely: when a user checks "create rule" on a
suggestion and the rule insert fails, the UI shows a green accepted state and no rule exists.

---

### 11.7 HIGH H15 - the "auto-applied AI rules" deferral is deployed as the default behaviour

The work plan's deferred-scope list explicitly excludes auto-applied AI rules. The mechanism is
live and pre-armed:

- `src/store/useSuggestionsStore.ts:43` `createRuleDefault: true`
- `src/pages/Suggestions.tsx:210` the checkbox is rendered pre-checked
- `src-tauri/src/commands.rs:769+` `create_suggestion_rule` inserts a rule with `enabled: true`,
  `action: "move"`, priority 10

So one click on a green check mark both moves the file and creates a standing, enabled,
unattended rule for that extension.

Two further problems. The AI-created rule is invisible on the Suggestions page, so the user has
no way to know one was created except by hunting through Settings -> Rules. And `folder_id: 0` is
written, whose semantics are not established anywhere in the crate. **UNVERIFIED**: the agent
that raised the `folder_id` point did not trace how `folder_id` is consumed by rule matching.

---

### 11.8 HIGH H16 - the `undoAll` enablement gate reads 50 rows while the command applies to all of them

`src/store/useAppStore.ts` `loadLogs` requests `limit: 50`, and `Settings.tsx` computes the
button's `disabled` state from that 50-row array via `logs.every(log => log.undone)`. The backing
query `get_undoable_logs()` in `src-tauri/src/db.rs` has **no limit**.

So if the most recent 50 actions are all undone but older ones are not, the button is disabled
and those files can never be reverted. The converse also holds: enabled when it should be
disabled. A destructive control whose availability is computed from a subset of the data it acts
on.

---

### 11.9 HIGH H17 - command injection via PowerShell string interpolation in `open_folder_cmd`

`src-tauri/src/commands.rs:334` builds `format!("explorer '{}'", win_path)` and passes it to a
PowerShell invocation. A filename containing a single quote breaks out of the quoting. The path
originates from the inventory or `action_logs`, so a file named `foo'; <command>; '.txt` created
in any watched folder is stored, then interpolated when the user clicks "open folder" on it.

The same function's `http://` branch means a file named `http://example.com` would be handed to
`start` and opened in the default browser. Local-only and low practical severity, but it is a
genuine CWE-78 and it is one interpolation away from being a remote one.

**UNVERIFIED**: the exact quoting behaviour of the PowerShell invocation. The agent read the
format string and did not execute it.

---

### 11.10 HIGH H18 - the plan's clippy gate catches none of the panics, and would likely fail on style lints instead

The work plan's verification gate is `cargo clippy -- -D warnings`. That runs the default lint
groups only. Every panic path found in this pass - roughly 44 `db.lock().unwrap()` calls,
`get_db().expect("Database not initialized")`, the `Instant` overflow - lives in
`clippy::restriction` or `clippy::pedantic` lints (`unwrap_used`, `expect_used`, `panic`), which
are **not enabled by default**. The gate that is supposed to catch panics catches none of them.

Meanwhile several default-on lints would probably fire on this codebase: `manual_unwrap_or`
(`cleanup.rs:62`, `rules.rs:129`), `manual_strip` (`db.rs:301`), `manual_unwrap_or_default`
(`commands.rs:112`), `uninlined_format_args` (version-dependent; it has moved between the `style`
and `pedantic` groups across releases, so no verdict is asserted here), and possibly
`collapsible_else_if`, `needless_return` and `doc_comment_continuation`.

**Present this as an asymmetry in the gate, not as a list of confirmed lint failures: the agent
did not run clippy.** `db.rs` `undo_action` also appears to be dead code, since `commands.rs`
calls `perform_undo` directly, which would trip `dead_code`.

---

### 11.11 CORRECTION to section 9 - junctions and volume mount points are NOT skipped

Section 9 records, on the strength of the scanner audit, that reparse points are correctly skipped
and that there is no junction escape. **That is too strong and must be corrected.**
`src-tauri/src/scan.rs:139-141` tests `ft.is_symlink() || ft.is_symlink_dir()`. On Windows
`FileTypeExt::is_symlink()` is true for any `FILE_ATTRIBUTE_REPARSE_POINT`, but
`is_symlink_dir()` is true only for `IO_REPARSE_TAG_SYMLINK` carrying the DIRECTORY attribute. A
**junction or a volume mount point is `IO_REPARSE_TAG_MOUNT_POINT`**, so it is not matched by
`is_symlink_dir()`. The doc comment at `scan.rs:62-64` claims the check covers "NTFS junctions";
that claim is wrong.

Consequences, bounded but real. `MAX_DEPTH = 128` prevents an infinite loop, so this is not a
hang. But a junction to another volume is descended into and its files are attributed to the
wrong `root_path`, and a junction pointing at an ancestor causes the same subtree to be re-walked
up to the depth limit.

**UNVERIFIED**: the exact Windows reparse-tag semantics. The reasoning is from documented tag
values; no junction was created to test it.

This is an error in an earlier section of this document, not a new defect, and it is recorded as
such.

---

### 11.12 Also newly recorded

- **Silent partial failure reported as success.** `src-tauri/src/rules.rs:274-277` catches
  per-file errors with `eprintln!` and returns `Ok(results)` containing only the successes.
  `Popup.handleClean` then reports "cleaned N" in a green toast. Fifty failures out of two hundred
  is indistinguishable from complete success. This directly violates the plan's hard constraint
  that every destructive command return per-item status and never a bare `Result<()>` for a
  multi-file operation.
- **A silently skipped rule.** `src-tauri/src/rules.rs:127-129` turns an invalid `Regex::new`
  into `.unwrap_or(false)`, so a malformed pattern means the rule silently never matches while
  still appearing correctly configured in the UI. `find_matching_rule` at `:149-152` additionally
  re-queries all rules per file event and recompiles every pattern per rule per event, with no
  cache.
- **Unreadable metadata is treated as "old enough".** `src-tauri/src/rules.rs:17-29`
  `check_grace_period` returns `true` on any metadata error, so a file whose mtime cannot be read
  is organised immediately. The unsafe direction.
- **`is_file_locked` conflates three conditions.** `src-tauri/src/rules.rs:11-13` treats a
  permission-denied file, a read-only file and a nonexistent file all as "locked", and skips each
  silently with no log entry.
- **A mode switch can report success while doing nothing.**
  `src-tauri/src/commands.rs:108` `let _ = watcher.refresh(app.clone())` inside
  `update_folder_mode_cmd` discards the error and the command still returns `Ok(())`, so switching
  a folder to silent mode can silently fail to register the watch.
- **A disconnected drive is skipped silently.** `src-tauri/src/watcher.rs:194-197` logs to
  stderr and continues. The dashboard continues to present that root's stale inventory totals.
  This is the same "silent success" class as the `oldDownloads` bug in the file-dashboard audit.
- **The multi-file notification is not actionable.** The Windows toast for a batch of more than
  one file says only "Organized N files", with no filenames, destinations or rule names, and
  clicking it opens the destination folder of the **last** file only. A forty-file batch spread
  across categories tells the user nothing they can act on or undo.
- **A latent catastrophic fallback, currently unreachable.**
  `src-tauri/src/commands.rs:361-367` `get_downloads_folder` falls back to the literal string
  `"C:/Users"` when `UserDirs` has no download directory. `initialize_defaults_cmd` would then
  register the entire user profile tree as a **silent** watched root with default rules, and the
  watcher would move files out of every profile on the machine. It is unreachable today only
  because `initialize_defaults_cmd` is registered at `lib.rs:190` and never called from the
  frontend. Both halves are recorded: the fallback is real, the reason it is currently harmless
  is an unreferenced function.
- **No first-run onboarding exists.** Because that command is never called, a fresh install has
  no watched folder and no rules, and "Clean now" defaults to the Downloads folder with zero
  rules, so nothing matches. The most prominent button in the app does nothing on a fresh
  install, and there is no path forward except hunting through Settings. This is a product gap,
  and it is the only reason the previous item is currently harmless.
- **The empty-directory sweep only removes leaves.** For `root/a/b/c` all empty, only `c` is
  removed and `a/b` remain, so the UI's "empty folders" promise needs repeated sweeps. The doc
  comment's stated rationale, avoiding cascades, is accurate; the functional gap is the finding.
- **Deadlock-adjacent lock hold across real filesystem moves.**
  `src-tauri/src/commands.rs:227` `undo_all_cmd` holds the global database mutex **and** the
  `ignored_files` mutex across N real filesystem moves, while the watcher's notify callback takes
  that same `ignored_files` lock on every event. The callback stalls, the OS notification buffer
  backs up, events arrive after the 30-second suppression window expires, and a freshly
  restored file is treated as new user activity and moved straight back with its log row already
  marked `undone = 1`. Confidence on the mechanism is high from the code; confidence that the
  race is being hit in practice is **medium**, because it needs a large batch.
- **`db.rs` `undo_action` appears to have no callers.**

---

### 11.13 Recorded as sound, so the counts are not inflated

- The Ollama integration cannot inject a traversal: `src-tauri/src/classify.rs:525` validates
  model output against an allowlist of known category names. A model returning
  `{"category": 42}` yields an `Err`, not a panic, because the parse path is entirely
  `serde_json::Value` with `.get().and_then(as_str)` and `ok_or_else`, with no `unwrap`, no
  indexing and no `derive(Deserialize)`.
- AI is never a hard dependency. Seven distinct failure paths in the classify flow all degrade to
  the heuristic result, and `get_suggestions_cmd` never returns `Err`.
- The zip-slip defence in `src-tauri/src/archive.rs:188` `ensure_safe_relative_path` is correct,
  and the flattening `collision_safe_path` is tested.
- `find_empty_dirs`'s read-error path returns "not empty", so a permission-denied subtree errs
  toward **not** deleting. That is the safe direction and is recorded as a positive.
- `commands.rs` contains five raw SQL statements, all `?1`-parameterised, and `db.rs` has exactly
  one `format!`-interpolated SQL string whose single hole is filled from a two-arm hardcoded
  whitelist. **No SQL injection anywhere in the crate.**

---

**Scope of this pass.** 9 of the 21 audit reports. No executive summary is added or amended: the
summary in section 1 predates these findings, and a later pass should reconcile it.
