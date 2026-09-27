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

---

## 12. Addendum - seventh pass: test trustworthiness

The previous pass asked whether the code can panic. This pass asks a narrower and more uncomfortable
question: whether the tests would notice. It read every test in the repository, both the 32 vitest
cases and the 57 `#[test]` functions, and traced each one to the production symbol it claims to
cover. The answer is that the suite is broad and shallow. One new Critical and six new Highs follow.

### 12.1 CRITICAL C8 - C1 is worse than recorded: the frontend guard AND the backend guard both fail, and a passing test locks the behaviour in

Section 2's C1 records that `buildDuplicateActions` with an empty keeper map trashes every copy
including the one the UI badges as kept. This pass traced the full chain and it is worse in two
specific ways that section 2 does not state.

**The display default and the action builder read different values.**
`src/components/cleanup/DuplicatesTab.tsx:119` renders the keeper as
`selected[g.hash] ?? g.files[0]?.path`, so the UI always shows the first file badged green as "kept"
even when the user has selected nothing. But `buildDuplicateActions(groups, selected)` at
`DuplicatesTab.tsx:36` is handed the **raw** `selected` map, which never receives that `??` default.
So `keepPath` is `undefined`, `f.path !== undefined` is true for every file, and every file in
every group is queued.

**The backend guard cannot catch it either.** Because `keepPath` is `undefined`, `JSON.stringify`
omits the key entirely, so Rust deserialises `Option<String>` as `None`, and the `keep_path`
self-reference check at `src-tauri/src/cleanup.rs:314-322` is skipped for the same reason. The two
independent safety mechanisms, one in the frontend and one in the backend, are disabled by the *same*
missing value. There is no layer at which the keeper survives.

**The button label is honest; the green badge is the lie.** The confirm-button count at
`DuplicatesTab.tsx:102-110` filters on `f.path !== selected[g.hash]`, which with an empty map counts
**all** files, so the label says the true total. The user therefore sees a red button reading
"Confirm N" and, directly above it, one file per group badged green as kept. The count and the badge
contradict each other, and the badge is the one that produces the false sense of safety.

**A passing test enshrines it.** `src/__tests__/cleanup.test.ts:59-64` calls
`buildDuplicateActions([a], {})` on a two-file group and asserts `toHaveLength(2)`, that is, it
asserts that both copies are queued for trashing when no keeper is chosen. This is not a missing
test. It is a test that would fail if the bug were fixed, so it actively resists the fix. Contrast
the sibling test at `:48-57`, which passes an explicit keeper map and is a correct test of the
intended path.

**The generalisable lesson.** Where a default is applied for *display* but not for *action*, the two
must be shown to be the same value, and a destructive default needs a test that asserts the safe
outcome rather than the observed one.

### 12.2 HIGH H19 - the two cross-device tests both test nothing, and the plan required one

The work plan's M1 exit criteria explicitly require "unit tests for move-back fixtures (collision,
missing-dir, cross-device)". Two tests appear to satisfy the third. Neither does.

- `src-tauri/src/rules.rs:288` `test_move_file_cross_device` creates a source and a destination
  **both under `std::env::temp_dir()`**, on the same filesystem, so `fs::rename` succeeds and the
  fallback branch is never taken. The test name asserts coverage that does not exist. This test is
  inherited unchanged from the upstream baseline, so the plan inherited the false assurance along
  with it.
- `src-tauri/src/safe_fs.rs:199` `test_move_file_copy_delete_fallback` calls `copy_delete_fallback`
  **directly**, bypassing the detection logic that decides when the fallback applies. A comment in
  the test admits a cross-device failure cannot be provoked in a unit test.

Net: the cross-device detection branch in both `rules.rs::move_file_cross_device` and
`safe_fs.rs::move_file` has **zero** coverage, while two green test names imply otherwise. This is
the same shape as C1, a test that creates confidence without creating safety.

### 12.3 HIGH H20 - the five Ollama parser tests test a copy of the parser, not the parser

`src-tauri/src/classify.rs:807-828` defines a **test-local** function `parse_ollama_response` that
re-implements the parsing logic living inline inside `OllamaProvider::classify` at
`classify.rs:504-529`. The five tests at `classify.rs:830-866` exercise the copy.

The two versions have already **drifted**, which is the proof: the copy's error strings are
`"JSON parse failed"` and `"Missing response field"`, while production emits
`"Ollama response parse failed"` and `"Ollama response missing 'response' field"`. The production
parser could be deleted outright and all five tests would stay green. Copy-paste drift between a
function and its test double is invisible to CI by construction.

Two further gaps in the same area:

- `OllamaProvider::is_available()` at `classify.rs:452-455` is `#[cfg(test)] { false }`, so the
  entire ureq detection path is **compiled out** under `cargo test`. No break in the real detection
  code can be caught by the test suite.
- No test calls `detect_provider()`, and **no mock `AiProvider` exists**; the only two
  implementations are `HeuristicProvider` and `OllamaProvider`. The plan's M4 criterion, "Ollama-absent
  path degrades gracefully, simulated via mock provider", is therefore **absent**.
  `test_suggestions_empty_without_inventory` at `classify.rs:1003-1010` is not a substitute: it
  passes `HeuristicProvider` against an empty inventory, so the classify path is never entered and
  the fallback branch is never exercised. It would pass if the fallback were deleted.

### 12.4 HIGH H21 - no CI runs any test

The repository has five GitHub workflow files, all inherited from upstream. Every one is a release,
build, or AppImage-signing pipeline. **None of them runs `cargo test`, `npm test`, or
`cargo clippy`.** The only checks are "the build succeeded" and "the AppImage installs".

Worse, `origin` is `https://github.com/hsr88/mouzi.git`, the public upstream, so a fork has no CI of
its own at all: these workflow files live in the upstream repository, and the signing jobs depend on
upstream secrets the user does not have.

Net: **89 tests, of which zero are executed automatically by anything.** Every regression discussed
in this document would land silently. This is the single highest-leverage fix in the entire audit,
because it is cheap and it converts all the other findings from "known" to "caught".

### 12.5 HIGH H22 - the watched-root boundary is enforced on one of four cleanup queries, and by no test

The work plan's D4 states that cleanup must stay within watched roots and never operate on whole
drives. Section 2's C5 records that the *scanner* does not enforce this. This pass found the same
boundary is also missing on the *query* side, which is the side that matters for deletion.

`src-tauri/src/cleanup.rs` has four finders. Only one respects the boundary:

- `find_duplicates` calls `db::get_inventory_size_groups()` - **no root parameter**
- `find_large_files` calls `db::get_large_files_from_inventory(min_bytes)` - **no root parameter**
- `find_stale_files` calls `db::get_stale_files_from_inventory(cutoff)` - **no root parameter**
- `find_empty_dirs` calls `db::get_watched_folders()` and filters on `enabled` plus pause mode - the
  only one that does

The signatures are at `src-tauri/src/db.rs:1090`, `:1114` and `:1132`. `file_inventory` rows are never
pruned when a root is un-watched (section 2's C7), so after a user removes a folder from settings, its
entire inventory remains and all of its files stay eligible for trashing by three of the four
finders. `execute_one` re-checks only that the path exists and that it is not the keeper, it never
re-validates containment.

**There is no test of the boundary behaviour of any of the four finders.** Note the honest
mitigating detail: scanning itself is root-scoped, so rows only enter the table through a deliberate
scan, and the exposure requires un-watching a folder after scanning it. That is a common sequence,
not an exotic one.

### 12.6 HIGH H23 - `commands.rs` has no test module at all, and the M1 rewrite made it testable and then did not test it

`src-tauri/src/commands.rs` contains **zero** `#[cfg(test)]` blocks. The fourteen test modules in the
crate are in cleanup, ignore, classify, tray, archive, db, scan, rules and safe_fs.

The sharpest instance: `perform_undo` at `commands.rs:142-205` is the entire M1 rewrite, the per-item
status contract, the collision suffix, the missing-parent-directory recreation, the `undone = 1`
write, and the watcher `ignored` map insertion that suppresses the self-trigger. It was
**deliberately refactored to accept `&rusqlite::Connection` and `&mut HashMap<String, Instant>`
instead of reaching for globals**, which is exactly the seam you would create in order to test it.
The seam was built and then left unused. This is the highest-risk function in the codebase, it moves
real user files and mutates the audit database, and it has no test.

`execute_one` at `cleanup.rs:307-379`, the destructive core that decides between trash,
skip-the-keeper, skip-if-missing, and remove-directory-with-recheck, is likewise untested, as is
`execute_cleanup` at `:387`. So the preview-then-confirm-then-trash-then-audit chain that M3 exists
to deliver has no test on its execution half. Only `walk_empty_dirs`, the read-only discovery step,
is covered.

### 12.7 HIGH H24 - two divergent status vocabularies exist, and neither is the one the plan specified

The plan's D5 mandates per-item status `ok | collision | missing | cross-device | failed` for undo.
The code delivers a different set twice over:

- `commands.rs:130` documents `pub status: String, // "ok" | "collision" | "missing" | "failed"`.
  `cross-device` is **never constructed anywhere in the crate**, so the fifth state does not exist.
- `src/utils/cleanup.ts:26` declares a completely different contract for `CleanupOutcome`: `"ok" |
  "failed" | "skipped"`, matching what `cleanup.rs` actually produces.

No test pins either vocabulary, and nothing checks that the two agree, so they can drift further
without consequence.

Compounding it, the `undoable` flag is structurally dead. `cleanup.rs:391-394` is a `match` whose two
arms are both `false`, so `undoable` is always false. Consequently `db.rs:651`'s
`UPDATE cleanup_actions SET undoable = 0, status = 'undone'` is unreachable, and **no `undo_cleanup`
function exists at all**. The cleanup audit trail therefore has no restore path in the app, consistent
with D5's decision that Recycle Bin restore is native, but the schema advertises a capability the
code does not have. `HistoryPanel.tsx` renders no undo affordance, correctly.

### 12.8 Medium findings

- **The plan's test baseline number is wrong.** D9 states the Rust baseline is 6 tests. The actual
  baseline at the merge-base commit is **11** `#[test]` functions (archive 5, ignore 5, rules 1). The
  plan's M0 exit criterion "verify `cargo test` (6 tests)" therefore could never have been satisfied
  as written. Current totals: **57 Rust + 32 vitest = 89**, so +46 Rust tests were added rather than
  the 6 the plan implies.
- **A vacuous test.** `src/__tests__/cleanup.test.ts:40-44` is named "does not mutate the input" but
  sorts a **one-element** array. An in-place sort of one element is undetectable, so the test passes
  whether or not `sortGroups` mutates its argument. It cannot fail for the property it names.
- **A self-referential assertion.** `src/__tests__/dashboard.test.ts:96` reads
  `expect(categoryColor("Unknown")).toBe(categoryColor("Other"))`. It compares the function against
  itself, and would pass even if `categoryColor` returned a constant for every input. The adjacent
  assertion at `:95` pinning `"#6b4f3a` is a genuine test by comparison.
- **A Windows-only test.** `src-tauri/src/cleanup.rs:696` asserts `leaves == vec!["sub1\\sub2"]` with a
  hard-coded backslash separator. `cargo test` is not portable and would fail on Linux or macOS,
  despite the crate containing Linux-specific `EXDEV` handling elsewhere.
- **Leftover temp directories on panic.** `safe_fs.rs` and `cleanup.rs` tests delete their temp trees
  at the **end of the test body** rather than in a `Drop` guard, so any failing assertion leaves files
  behind in `%TEMP%`. The `test_dir` helper does clear the directory on entry, which limits
  accumulation to one run's worth.
- **Conditional coverage presented as green.** `src-tauri/src/scan.rs:244` creates a directory
  symlink to exercise the cycle guard, but on Windows that needs Developer Mode; when the privilege is
  absent it prints a note and **the symlink assertions never run**, yet the test still passes. The
  cycle-guard code is untested on a default Windows install and CI reports success.
- **Global-count coupling between tests.** Several tests assert on whole-table counts:
  `scan.rs:305` asserts `total_files == 5`, and `classify.rs:1003` requires `file_inventory` to be
  completely empty. They share one file-backed database at `%TEMP%/mouzi-db-<pid>`
  (`db.rs:1223-1229`) serialised only by a `TEST_DB_LOCK` mutex. A panic in one test leaves rows
  behind and breaks whichever test runs next, so the suite is order-dependent and will produce
  confusing failures rather than honest ones.
- **A test that cannot detect a broken feature.** `safe_fs.rs:232-253` `test_delete_to_trash_temp_file`
  swallows the error branch with `eprintln!` and still passes, so a completely non-functional Recycle
  Bin path is invisible to the suite. This is C4, the `trash` crate may permanently delete when the
  bin cannot accept an item, meeting a test that structurally cannot notice.
- **Reclaimable bytes is implemented twice.** `cleanup.rs:214-218` computes it in Rust as
  `files[0].size * (len - 1)`, while `src/utils/cleanup.ts` sums per-file sizes in TypeScript. The
  vitest suite covers only the TypeScript half, so nothing detects the two diverging. Both are live:
  `DuplicatesTab.tsx` imports the TypeScript version while the authoritative groups come from Rust.
- **No coverage tooling exists.** `@vitest/coverage` is not a dependency and `vite.config.ts` has no
  `test` block at all, so vitest runs on pure defaults. Nothing measures coverage and no threshold is
  enforced. The one positive: `tsconfig.json` has `"include": ["src"]`, so the test files **are**
  type-checked by `npm run build`.

### 12.9 Required-versus-present, the plan's six mandated test categories

| # | Mandated category | Verdict | Evidence |
|---|--------------------|---------|----------|
| 1 | classifier pure functions | **present, genuinely good** | 19 tests in `classify.rs` covering extension, token, case-insensitivity, `extract_tokens` |
| 2 | dedup size-prefilter and cache invalidation | **present, genuine** | `cleanup.rs:439`, `:494`, `:538`, `:590` |
| 3 | undo move-back fixtures | **partial** | collision and missing-parent at `safe_fs::move_file`; cross-device absent (H19); command layer absent (H23) |
| 4 | migration idempotency, `init_db` twice | **absent and unwritable as specified** | no such test; `sync::OnceCell` makes a second in-process call return `Err` |
| 5 | trash send on temp files only | **present but unable to fail** | `safe_fs.rs:232-253` swallows the error branch with `eprintln!` |
| 6 | Ollama-absent graceful degradation | **absent** | no mock provider, no `detect_provider()` test, detection compiled out under `cfg(test)` (H20) |

A caveat on the first row, since several of those tests are weaker than they look:
`test_suggest_by_filename_token` asserts `conf >= 0.80` for `Screenshot_2024-08-24.png`, which the
`png` extension alone satisfies, so token scoring is never actually exercised.

The second row is the strongest test work in the repository. `cleanup.rs:590` in particular would
fail if the cache keyed on path and size without mtime. Its limit is that it never probes the
same-second mtime case, a real modification between scans, a short read, or a mid-hash write.

On row 4, this is a plan defect and not only an implementation gap. `init_db` uses a
`sync::OnceCell`, so a second in-process call returns `Err`, meaning `assert!(init_db(dir).is_ok())`
would fail by design. Cross-process it is idempotent, but nothing tests either property.

### 12.10 How many tests would survive a deliberate break

**The method.** For each test: if the production function it targets were deleted outright, would the
test go red?

- **TypeScript, 32 cases.** `format.test.ts` 11 of 11 genuine. `dashboard.test.ts` 12 of 13, the
  self-referential `categoryColor` comparison being the exception. `cleanup.test.ts` 7 of 8, the
  one-element mutation test being the exception, though it would still fail if the function threw or
  returned undefined. So **30 of 32**.
- **Rust, 57 tests.** The great majority call real functions on real temp files and would fail
  correctly. The exceptions are: the five Ollama parse tests, which test a copy (H20); the two
  cross-device tests, which never reach the branch they name (H19); the empty-dirs test, which is
  Windows-only; the trash test, which cannot fail; and the scan test, whose symlink assertions are
  conditional. So roughly **46 of 57**.
- **Combined: about 76 of 89 would catch a break in what they name.** That sounds reassuring and is
  not. The failures that matter most are precisely the ones no test is looking for: `commands.rs` has
  no tests at all, so the undo path, the destructive `execute_one`, and every status-contract claim
  are uncovered; and the tests that do cover the destructive frontend helper assert the **buggy**
  behaviour.

Coverage is broad and shallow. Nothing in the suite would fail if `perform_undo` were deleted, if
`execute_one` were deleted, or if the Ollama parser were deleted.

### 12.11 Recorded as sound

- `init_test_db` at `db.rs:1223-1229` uses a file-backed database at `%TEMP%/mouzi-db-<pid>`. **No
  test touches the real application database.** The temp path is keyed by process id, so there is no
  cross-run contamination.
- `archive.rs` carries the five upstream baseline tests, and `zip_slip_entries_are_rejected` is a real
  security assertion: it checks that a traversal entry did not create a file outside the extraction
  root.
- `ignore.rs` gates its case-sensitivity tests properly with `#[cfg(windows)]` and
  `#[cfg(not(windows))]`, and both call the real function. These are model tests.
- `tray.rs:227-243` tests `location_hash_script` against script-injection inputs, stripping quotes and
  newlines. Genuine, and the only security-focused frontend test in the repository.
- The `safe_fs::move_file` tests for normal move, collision suffix, and missing-parent-directory are
  real and would fail correctly.
- Deleting `src/__tests__/smoke.test.ts` is a **contract deviation, not a coverage loss.** The
  deleted file asserted `expect(1+1).toBe(2)` and proved only that the runner starts. Three remaining
  test files exercise the same runner, the same config path, and the same include glob, so a broken
  runner fails in all of them. Record this as a low-severity deviation from the plan's literal wording
  and explicitly do not inflate it.
- No `.only`, `.skip`, or `.todo` appears in any test file, and no `#[ignore]` appears in the Rust
  suite. Nothing is silently disabled.

### 12.12 What this section implies for the Criticals already recorded

- Fixing C1 or C8 will **break** `cleanup.test.ts:59-64`, which asserts the destructive default. That
  test must be rewritten in the same change, or the fix will be reverted by someone who trusts a green
  suite. This coupling is the practical reason the test audit matters more than its finding count
  suggests.
- H21 means every other fix in this document is a one-time manual correction. Nothing prevents the
  same defects returning.
- H23 and H22 together mean the two most dangerous functions in the crate, `perform_undo` and
  `execute_one`, are both untested **and** both reachable with paths the app never validated. Those
  two facts compound.

---

**Scope of this pass.** 10 of the 21 audit reports. No executive summary is added or amended: the
summary in section 1 predates these findings, and a later pass should reconcile it.

---

## 13. Addendum - eighth pass: fork hygiene, type contracts and doc accuracy

**Scope.** 3 of the 21 audit reports, bringing the absorbed total to 13 of 21. The three scopes are
fork hygiene and plan compliance, TypeScript strictness and type contracts, and docs, naming and
cross-language consistency. Findings run C9-C10, H25-H28, M27-M43, plus a Low subsection and a
dedicated refutation subsection.

### 13.1 CRITICAL C9 - four features shipped, no version bump, no changelog, so none of them can be documented

`src-tauri/tauri.conf.json:4` is still `"version": "0.1.6"`, byte-identical to upstream, and
`package.json:4` agrees. There is no `CHANGELOG.md` anywhere in the repository. The only changelog
artefacts are upstream's own Astro marketing-site release notes, `website/src/content/changelog/0-1-5.mdx`
and `0-1-6.mdx`. Both describe upstream work; `0-1-6.mdx` lists quick rule toggles, inline `#`
comments in `.mouziignore`, locked-file handling and translation updates, and none of the four
elevate features.

`README.md:125-127` and `README.md:135-137` still advertise `Mouzi_0.1.5_*` signed installers with
SHA-256 checksums at `README.md:144-147`, while `README.md:257` opens a section titled "Coming in
0.1.6". The README therefore advertises 0.1.5 binaries and previews 0.1.6 in the same document, and
the version those binaries correspond to is not the version the app declares.

Consequence: there is no version number and no changelog entry under which the dashboard, smart
cleanup, AI suggestions or the Recycle Bin work can be recorded. A user downloading `0.1.5` and a
user running this build are indistinguishable from the outside. This is the administrative root of
every documentation defect in 13.5: there is no target to document against.

### 13.2 CRITICAL C10 - the tray menu is the only entry point to three shipped features, and the README documents three other items

Plan D3 required the dashboard be reachable via "new tray menu item + settings link". The tray menu
item exists; the settings link was never built. `src/components/Settings.tsx` contains **zero**
occurrences of `dashboard`, `cleanup` or `suggestions` (verified by grep across the whole file:
0 matches). The menu itself is built at `src-tauri/src/tray.rs:11-19` and carries seven items -
`quit`, `clean_now`, `suggestions`, `dashboard`, `cleanup`, `settings`, and a separator - wired at
`tray.rs:24-44`.

`README.md:171` reads: "Right-click the tray icon for the menu: `Clean Now`, `Settings`, `Quit`." That
is three of the seven, and it omits `Suggestions`, `Dashboard` and `Cleanup` - the three entry points
to the fork's headline features. `README.md:170` documents only the left-click popup.

Consequence: a user following the README literally can never reach the dashboard, the cleanup surface
or the suggestions surface. This is the mechanism by which C9 becomes user-visible. It also **refines**
two earlier findings. The D3 deviation recorded at line 489 as "Partial" is more accurately a **full
non-implementation of the settings link**; the window is built, the second half of the plan is simply
absent. And the stale-tray-menu finding previously treated as cosmetic is a discoverability defect,
not a wording defect.

### 13.3 HIGH H25 - the undo per-file status is returned by the backend and discarded by the frontend

The plan's M1 headline deliverable was a per-file status of `ok | collision | missing | cross-device |
failed`. The backend emits four of the five. In `src-tauri/src/commands.rs`, `missing` is returned at
`:154-159` and `:165-170`, `ok` at `:181-186`, `collision` at `:193-198`, and `failed` at `:199-203`
and again at `:235-240` in the batch path.

The frontend throws the value away. `src/components/Popup.tsx:219` reads
`onClick={() => undoAction(log.id!)}`: no `.then`, no destructuring, no comparison against any status.
`log.id!` is additionally a non-null assertion on `ActionLog.id?: number`.

Consequence: an undo that returned `failed` is visually identical to one that returned `ok`. An undo
that hit a collision and therefore landed the file under a timestamp-suffixed name is also
indistinguishable from a clean restore, even though `restored_to` carries the suffixed path and
`perform_undo` took a different branch for it. The one operation whose entire purpose is restoring
user trust gives the user no feedback at all.

`cross-device` is **unrepresentable**. `src-tauri/src/safe_fs.rs:64 move_file` dispatches to
`copy_delete_fallback` (`:37`) on EXDEV and on Windows `ERROR_NOT_SAME_DEVICE` (`:36`, `:83`), and
that function returns `Ok(MoveOutcome::Moved)`. A cross-device restore is therefore byte-identical to
a same-device one. The plan's fifth status has no code path anywhere in the crate.

### 13.4 HIGH H26 - the undo status has no type-level contract in either language, and no i18n keys

`UndoResult.status` is a bare `String` in Rust (`commands.rs:130`, with the union written only in a
trailing comment) and a bare `string` in TypeScript. Nothing prevents a status being added on one side
and not the other. That is the exact defect class already recorded elsewhere in this audit for the
`hashedCount` alias-casing bug. There is no `switch` and no comparison against an undo status anywhere
in `src`, and `src/i18n/locales/en.json` has no keys for `collision`, for undo-specific `missing`
(the only `missing` string at `:254` belongs to the cleanup surface) or for undo failure.

The contrast within this same codebase is the finding. The cleanup status contract is closed end to
end: `src-tauri/src/cleanup.rs` emits `ok`, `failed` and `skipped` at nine sites;
`src/utils/cleanup.ts:26` types the field as the exact union `"ok" | "failed" | "skipped"`;
`src/components/cleanup/ResultsPanel.tsx:44,47,50` and `:60,62,66,68` handle all three; and
`en.json:223-225` supplies `cleanup.ok`, `cleanup.failed` and `cleanup.skipped`. One contract in this
repository is exemplary and one is unmanaged, and the difference is entirely whether somebody wrote
the union down.

### 13.5 HIGH H27 - there is no way to create an `ignore` rule from the UI, so the Rust `ignore` branch is nearly unreachable

`src/components/Settings.tsx:491` hardcodes `action: "move"` in the object the new-rule button
constructs, and `src-tauri/src/db.rs:343` hardcodes `'move'` in the rule INSERT. There is no action
picker anywhere in the rules form. The Rust `ignore` branch at `src-tauri/src/rules.rs:201`
(`"ignore" => Ok(file_info.path.to_string_lossy().to_string())`) is therefore reachable only by
hand-editing SQLite or by importing a JSON rules file through `import_rules_cmd`.

This is a plan-compliance correction. Line 494 records D8 ("never route cleanup through
`execute_rule`") as **Compliant** on the grounds that `delete` has no UI selector. That reasoning is
right but the framing is generous: there is no action selector at all, so `delete` was never
selectable and hiding it is **vacuously** true. Record D8 as compliant-but-vacuous, not as a
delivered mitigation, so nobody later reads it as evidence that a dangerous option was deliberately
withheld from users.

### 13.6 HIGH H28 - the two extension-to-category tables in the same crate disagree

`src-tauri/src/scan.rs:44 categorize()` and `src-tauri/src/classify.rs:26 extension_to_category()` are
two independent implementations of the same extension-to-category mapping. The comment at
`classify.rs:20` says it "mirrors scan.rs::categorize", so the divergence is unintentional. Verified
divergences:

- `rtf`, `odt`, `ods`, `odp` are Documents in classify (`classify.rs:30`) and fall through to Other in
  scan, whose Documents arm is `scan.rs:50`.
- `heif` and `ico` are Images in classify (`classify.rs:31`) and Other in scan (`scan.rs:53`).
- `flv` is Videos in classify (`:34`) and Other in scan. `wma` is Audio in classify (`:35`) and Other
  in scan. `xz`, `iso` and `dmg` are Archives in classify (`:36`) and Other in scan.
- `exe`, `msi`, `msix`, `appx`, `deb`, `rpm`, `apk` and `appimage` are Archives in classify
  (`:41-43`) and Other in scan, whose Archives arm is `scan.rs:56`. **`exe` is the one that matters in
  practice**: it is the most common file a Downloads-folder organiser encounters.
- Code extensions: classify carries 32 (`classify.rs:37-40`), scan carries 10 (`scan.rs:57`).

Consequence: `file_inventory.category`, written at `scan.rs:102`, and the Suggestions panel's proposed
category, written by classify, disagree about the same file. The dashboard's "By kind" chart is built
from the scan side and the Suggestions surface reasons from the classify side, so a user can be shown
a file as "Other" on one screen and "Archives" on another. Neither table has an `Installers`
category, although `README.md:50` documents an "Installers" rule, so the documented rule and the
shipped taxonomy do not correspond either.

### 13.7 MEDIUM M27 - Ollama cannot be turned on or off

`classify.rs:447` probes `http://localhost:11434/api/tags` and `classify.rs:495` posts to
`http://localhost:11434/api/generate`. Both are hardcoded. There is no setting, no environment
variable, no config key and no UI control. `detect_provider` (`classify.rs:539`) probes
unconditionally on every suggestions request.

So Ollama is not "off by default" and not user-selectable. If something is listening on port 11434,
the app silently starts sending filenames to it. Plan D2 described an "optional enhancement behind a
trait", and that is what shipped, but "optional" here means optional for the user to have installed
Ollama, not optional for the user to select. Anyone documenting "how do I turn the AI off" has to
answer "you cannot, short of not running Ollama".

### 13.8 MEDIUM M28 - there are two parallel i18n stores for one product, and the tray's own labels are English in every locale

The ten files in `src/i18n/locales/*.json` (`de`, `en`, `es`, `fr`, `it`, `ja`, `pl`, `ru`, `uk`,
`vi`) are one store. `src-tauri/src/i18n.rs` is a second, entirely separate hardcoded table for the
tray, notifications and window titles. It is a `HashMap<&'static str, &'static str>` (`:3-5`) built by
nine named language arms (`pl` `:11`, `it` `:28`, `de` `:45`, `fr` `:62`, `ru` `:79`, `ja` `:96`,
`vi` `:113`, `es` `:130`, `uk` `:147`) plus an `_` English default (`:164`). Every arm contains
exactly 15 `strings.insert` calls, 150 in total, covering 15 keys. Two sources of truth for strings
that appear in the same product, with no shared key namespace and no check that they agree.

Within the Rust table the coverage is uneven in a way that is easy to miss: `cleanup` and
`cleanup_title` are translated per language (`Pulizia` `:39`, `Aufräumen` `:56`, `Nettoyage` `:73`,
`D?n d?p` `:124`, `Limpiar` `:141` and others), but `dashboard` and `dashboard_title` are the literal
strings `"Dashboard"` and `"Mouzi Dashboard"` in **all ten** blocks, at `:20-21`, `:37-38`, `:54-55`,
`:71-72`, `:88-89`, `:105-106`, `:122-123`, `:139-140`, `:156-157` and `:173-174`. The two
fork-specific menu labels are the two that were never translated.

### 13.9 MEDIUM M29 - four backend payload types are declared inline a second time, and the compiler cannot catch a backend rename

`src/store/useDashboardStore.ts` declares `CategoryStat` (`:8-12`), `RootStat` (`:22-26`) and
`WeeklyStat` (`:56-59`). Each is then re-declared as an inline structural type in a consuming
component: `src/components/dashboard/CategoryBars.tsx:7` re-declares the category payload as
`Array<{ category: string; files: number; bytes: number }>`, `StorageTreemap.tsx:10` re-declares
`RootStat` as `Array<{ path: string; files: number; bytes: number }>`, and `ActivityTimeline.tsx:6`
re-declares `WeeklyStat` as `Array<{ file_type: string; count: number }>`. A third definition of the
category payload lives in `src/utils/dashboard.ts:17-21` as `CategoryShare`, and
`StorageRibbon.tsx:3,6` is the one component that imports it properly.

So one backend payload has three definitions and another has two. All of them are structurally
compatible, which is the problem: they compile. If the Rust side renames `file_type`, every
duplicated inline type keeps compiling, and the breakage surfaces as `undefined` at runtime rather
than as a build error. Contrast `AgeHistogram.tsx:3`, `FileBrowser.tsx:8` and `InsightCards.tsx:4`,
which import their payload types from the store and are therefore immune. The fix is mechanical:
import the type instead of restating it.

### 13.10 MEDIUM M30 - a correctly-built union type is discarded at its only use site

`src/utils/dashboard.ts:11` defines `export const AGE_BUCKETS = ["7d", "30d", "90d", "365d", "older"] as const`,
which makes the literal union `'7d' | '30d' | '90d' | '365d' | 'older'` available to importers.
`src/components/dashboard/AgeHistogram.tsx:4` imports exactly that.

`AgeBucket.bucket` in `useDashboardStore.ts:39` is typed `string`, throwing the union away at the
point of declaration. Consequently `labels` at `AgeHistogram.tsx:12` must be declared
`Record<string, string>`, and `labels[bucket.bucket]` at `:46` and `:50` compiles as `string` rather
than `string | undefined`. If the backend ever emits a bucket id outside the five, say `"180d"`, the
label renders as `undefined` and is invisible, and `ordered` at `AgeHistogram.tsx:20-22` silently
drops the row because it maps over `AGE_BUCKETS`, not over the data.

This is the only place in the codebase where an already-correct type was available and thrown away,
which makes it a better teaching example than a typical missing-type bug. Type `AgeBucket.bucket` as
`(typeof AGE_BUCKETS)[number]`, and every one of the above becomes a compile error instead of an
invisible chart.

### 13.11 MEDIUM M31 - the one `any` in the codebase sits on the event handler that drives the popup

`src/components/Popup.tsx:72` is `listen("file-organized", (event: any) => {...})`. This is the only
`any` in any type position across all of `src` (verified by grep for `: any` and `as any` across
every `.ts` and `.tsx`: one hit). The `listen` API is generic, so the parameter could have been typed
with no extra work.

Because it is `any`, every field read on the payload is unchecked: `payload.destination_folder`,
`payload.destination`, `payload.file`, `payload.rule`, `payload.success` at `Popup.tsx:74-80`. That
matters because the emit site is a hand-built `serde_json::json!` object in
`src-tauri/src/watcher.rs:110` and `:120`, and those key strings are the **only** contract between the
two halves. There is no `rename_all` to lean on, because `json!` bypasses serde derive entirely. The
`payload.destination_folder || payload.destination` fallback at `Popup.tsx:75` happens to work against
today's emitter, and nothing whatsoever would catch a rename on the Rust side.

### 13.12 MEDIUM M32 - the sole `as unknown as` cast in the codebase is a symptom of one function's design

`src/__tests__/dashboard.test.ts:53` needs
`globalThis as unknown as { window?: { location: { hash: string } } }` in order to fake a window for
`navigateHash`. The cause is in `src/utils/paths.ts`: `navigateHash` at `:28-33` reaches directly for
`window.location` at `:32` with no `typeof window` guard, while its sibling `parseHash` at `:13` in
the same file does guard, defaulting the parameter with
`typeof window !== "undefined" ? window.location.hash : ""`.

Inconsistent guarding between two functions in one small module, and the consequence is concrete: one
of them is untestable in the default vitest node environment, and the test that wants to test it has
to lie to the type system to do so. Add the same `typeof window` guard to `navigateHash` and the cast
disappears along with the need for the fake.

### 13.13 MEDIUM M33 - `parseHash` returns an unconstrained `string` and a test encodes the bug as expected behaviour

`parseHash` in `src/utils/paths.ts:13-26` declares `route: string`, not a union of the five valid
routes. `App.tsx:112-122` is a chain of `hash ===` comparisons with a `:120-121` fallback to
`<Popup />`, so a typo'd or unrecognised route renders the popup rather than failing visibly. The
regex at `paths.ts:17`, `/^#\/?/`, strips only one leading `#` and an optional `/`, so the input
`#/#/dashboard` yields the literal route string `"#/dashboard"`, which matches no branch.

`src/__tests__/dashboard.test.ts:38-41` asserts this input's route is **not** `"dashboard"`:

```
it("does not treat a doubled hash from location.hash = '/#/dashboard' as dashboard", () => {
  expect(parseHash("#/#/dashboard").route).not.toBe("dashboard");
  expect(parseHash("#/dashboard").route).toBe("dashboard");
});
```

The first expectation passes against the buggy value, so the test documents the defect instead of
catching it, and its title describes the defect as intended behaviour. The assertion needs to be
`toBe("")` or the route to be normalised, and the return type needs to be a five-member union. This
is the same pattern as C8: a green test pinning incorrect behaviour. It should be rewritten in the
same change as the fix, or the fix will be reverted by someone who trusts the suite.

### 13.14 MEDIUM M34 - the persisted settings surface is entirely undocumented

The `settings` table carries `autostart`, `grace_period_seconds`, `lock_check_enabled`,
`schedule_enabled`, `schedule_times_per_day`, `schedule_time_1` through `schedule_time_4`
(`src-tauri/src/db.rs:96-105`), plus `language`, `theme`, `telemetry_enabled` and `first_run`
(`db.rs:94-95` and the surrounding struct). None of these column names appears in `README.md`, and
there is no troubleshooting section explaining what a grace period of 300 seconds does, what the
lock check protects against, or how the four schedule slots are used.

Consequence: every one of these is a behaviour a user can observe and change and cannot look up. A
user who wants a scheduled clean has no way to learn the syntax of `schedule_time_1` short of reading
Rust. Note the default grace period is 300 (`rules.rs:209`), and `README.md` does not mention it.

### 13.15 MEDIUM M35 - data location, dev port and uninstall are undocumented, and the backup-relevant commands are undocumented too

`src-tauri/src/lib.rs:102` opens the database with `ProjectDirs::from("cc", "mouzi", "mouzi")`, which
on Windows resolves under the roaming application-data directory. There is no README statement of
where the database lives, no backup guidance, no reset procedure and no uninstall section. The
Recycle Bin work means the app now holds user data across several locations, and none of them are
documented.

Separately, `tauri.conf.json:8` sets `devUrl` to `http://localhost:1420` with `strictPort: true`, so a
port clash is an immediate hard failure with no documented remedy. The README's contributing section
does not mention the port.

And `export_rules_cmd` (`commands.rs:480`) and `import_rules_cmd` (`commands.rs:491`) exist, are
registered Tauri commands, and are documented nowhere. These are the only backup and restore paths
that exist for rules, and `import_rules_cmd` takes a `replace: bool` that determines whether it
merges or overwrites. That is a data-destroying flag with no documentation.

### 13.16 MEDIUM M36 - `package.json` has no lint or format script, which is the root cause of the split quoting convention

`package.json:6-13` declares `dev`, `build`, `preview`, `tauri`, `start` and `test`. There is no
`lint`, no `format`, and no lint or formatter dependency in `devDependencies`. Nothing enforces a
convention, so none exists: upstream's `src/store/useAppStore.ts` uses single quotes (`useAppStore.ts:46`
reads `language: 'en'`) while every elevate-authored file uses double quotes.

Name the missing tool rather than picking one. Until something runs, every "why are these different"
question has the same answer, and the codebase will keep splitting at whatever line the fork was
branched from. The `// @ts-expect-error` inherited into `vite.config.ts` (see 13.17) is a second
construct that no tool would catch, because that file is not typechecked at all.

### 13.17 MEDIUM M37 - the `file_inventory` DDL is duplicated, and every inventory test runs against the copy

`src-tauri/src/db.rs:189-216` creates `file_inventory` and its four indexes in the real schema
(`idx_file_inventory_root`, `_size`, `_category`, `_mtime`). `db.rs:731-750` declares a second,
textually near-identical copy of the table and all four indexes under `#[cfg(test)]`, in
`create_file_inventory_table`.

Consequence: every inventory query test exercises a test-only schema. The two can drift with CI
green - add a column to the real DDL and forget the test copy, or the reverse, and the suite passes
while the app's queries break. This is a structural twin of C1 and C8: the test asserts against
something other than what ships.

### 13.18 MEDIUM M38 - two field-naming conventions coexist, split by which commit introduced them

Upstream's interfaces in `src/store/useAppStore.ts` are snake_case (`autostart`, `grace_period_seconds`,
`lock_check_enabled` at `useAppStore.ts:40-42`) because the corresponding Rust structs do not carry
`#[serde(rename_all = "camelCase")]`. Every elevate-authored type is camelCase:
`DashboardStats` (`useDashboardStore.ts:44-54`), `InventoryFile` (`:14-20`), `CleanupOutcome`
(`src/utils/cleanup.ts:24-28`), `ScanProgress` (`useDashboardStore.ts:61`).

`WeeklyStat.file_type` (`useDashboardStore.ts:57`) is snake_case sitting among camelCase neighbours,
and it is the one that generates the `ActivityTimeline.tsx:6` duplication from M29. One codebase, two
conventions, and the seam between them is invisible: nothing marks which files are pre-fork and which
are post-fork. The fix is a single serde attribute per affected struct plus a rename, and until it
happens a reviewer cannot tell from a diff whether a snake_case field is deliberate or an oversight.

### 13.19 MEDIUM M39 - three different action vocabularies are written into two audit tables

`rules.action` receives `move` and `ignore` (`rules.rs:201-202`, and the `'move'` default at
`db.rs:142`). `action_logs.action` receives the literal `move` (`db.rs:1318` in tests, and the
INSERT in the same shape). `cleanup_actions.action` receives the cleanup `kind` values instead -
`trash_duplicate`, `remove_empty_dir` and siblings (`cleanup.rs:391-393`,
`HistoryPanel.tsx:53` compares `log.action === "remove_empty_dir"`).

Nothing normalises the three vocabularies, and `HistoryPanel.tsx:53` renders the raw value. So the
user-facing history list displays three different naming conventions for "what Mouzi did to this
file", and a query joining across `action_logs` and `cleanup_actions` has to know which table it is
in before it can interpret the column.

### 13.20 LOW

- **The plan's 5-second figure is stale, and the code is correct.** `watcher.rs:12` declares
  `IGNORE_DURATION_SECS: u64 = 30`, not the 5 recorded in plan section 2, and the constant is consumed
  at `watcher.rs:218` and `:270`. M1's "extend beyond the 5s window" was in fact done. Do not re-raise
  the 5-second number.
- **`defaultSettings` (`useAppStore.ts:45`) is dead.** One reference in the whole of `src`: its own
  definition. Every settings read goes through the store, so the export is unreachable.
- **`applyAccept` is public but internal.** It is on the `useSuggestionsStore` interface at
  `useSuggestionsStore.ts:29` and defined at `:60`, but its only two callers are `acceptSuggestion`
  (`:75`) and `acceptAll` (`:103`), both inside the store. It should be private to the store.
- **`CATEGORY_COLORS` is exported but file-local.** Declared at `src/utils/dashboard.ts:1` and
  referenced only at `:14` in the same file. `categoryColor` (`:13`) is the intended public surface.
- **`EMPTY_INSIGHTS` has two export statements.** Declared `const` at `useDashboardStore.ts:78` and
  re-exported by a trailing `export { EMPTY_INSIGHTS }` at `:236`. It is genuinely consumed externally
  (`src/pages/Dashboard.tsx:7,221`), so this is a style oddity, not dead code.
- **`cleanup.rs:391-394` is a constant expressed as a match.** Both arms of
  `match action.kind.as_str() { "remove_empty_dir" => false, _ => false }` evaluate to `false`, so
  `undoable` is always `false` for every cleanup action. The match reads as though empty-dir removal
  is a special case, which implies the other kind might be undoable. It is not. `en.json:229` says so
  correctly ("Empty folder removals are not undoable") but implies trash deletions might be.
- **`cleanup_actions.prev_path` and `.dest` are always NULL.** The struct declares them at
  `db.rs:82-83`, the reader reads them at `db.rs:636`, and the only writer sets both to `None`
  (`cleanup.rs:399`). The `UndoAllResult`-style machinery for cleanup does not exist. Both columns
  are carried forever and read as `None`.
- **`close_settings` no longer describes what it does.** `commands.rs:386 close_settings(app: AppHandle)`
  closes the shared window, which since the C3 window consolidation is the only window. The name is a
  leftover from the multi-window model and will mislead the next reader.
- **The `app` window label, the D3 deviation, the undo status vocabulary and the `prev_path` naming were
  all independently re-raised by two of these three reports.** See 13.21 for the balance.

### 13.21 Refuted

Recorded so they are not raised a third time. Each was checked and cleared.

- **`key={log.id}` where `id: number | null` is not a type error.** React 19's `HTMLAttributes`
  declares `key?: Key | null | undefined` at `node_modules/@types/react/index.d.ts:259`. `null` is
  explicitly permitted. Checked in the installed type definitions. The sites are
  `src/components/Popup.tsx:197` and `src/components/Settings.tsx:713`.
- **`sourceLabel(s.source, t)` is not a type error.** With no `declare module 'i18next'` augmentation
  anywhere in `src`, the installed i18next's `ResourceKeys` falls back to `string`, so `TFunction` is
  assignable to `(key: string) => string`. Checked in the installed `i18next` `.d.ts`. This is a real
  weakness in the setup (no key safety, see M28) but it is not a compile error.
- **Indexed access is not a type error under the current tsconfig.** `noUncheckedIndexedAccess` is off,
  so `files[0]` is non-nullable and TypeScript narrows away any `?.`. The two sites are
  `src/__tests__/cleanup.test.ts:50` and `:55`, both written as plain `a.files[0].path` with no `?.`
  at all. The **runtime** truth is that `files[0]` can be `undefined` for an empty group, so the type
  is lying; that is the same missing-flag class as M30 and is recorded there rather than as an error.
- **The `prev_path` serialisation mismatch is real, but the SQL is not the cause, and the query is not
  aliased.** `db.rs:623-624` reads `SELECT id, timestamp, path, prev_path, dest, action, status,
  undoable FROM cleanup_actions` with **no** `prev_path AS prevPath` alias; the column really is
  selected as `prev_path`, and `db.rs:636` binds it to `row.get(3)`. The defect is solely the missing
  `#[serde(rename_all = "camelCase")]` on the `CleanupAction` struct at `db.rs:77-87`, which carries
  only `#[derive(Debug, Clone, Serialize, Deserialize)]`. Attribute the mismatch to the serialisation
  attribute, not to the query.
- **The 2^53 integer-overflow concern is not live.** Exceeding `Number.MAX_SAFE_INTEGER` (about
  9.007e15) through the `SUM(size_bytes)` path requires a single file of roughly 8 EiB (9.007e15 bytes
  of disk). Through the `(cnt - 1) * size` reclaimable aggregate in `src/utils/cleanup.ts:31-33` it
  requires roughly 8 PiB of duplicated content. Neither is reachable. Record as a non-issue with the
  arithmetic shown, because it is a plausible-sounding concern and closing it saves a future pass the
  work.
- **`strict: true` really is on.** `tsconfig.json` carries `"strict": true`, so plan section 7's "tsc
  strict" claim holds. But `noUncheckedIndexedAccess`, `exactOptionalPropertyTypes`,
  `noImplicitOverride` and `verbatimModuleSyntax` are all **off**, and `tsconfig.node.json` has no
  `strict` at all. `noUnusedLocals`, `noUnusedParameters`, `noFallthroughCasesInSwitch` and
  `isolatedModules` are on.
- **`vite.config.ts` is never typechecked.** `package.json:8` sets `"build": "tsc && vite build"`, and
  plain `tsc` without `-b` checks only the root project, so the `references` entry to
  `tsconfig.node.json` at the end of `tsconfig.json` is not built. The file contains an inherited
  upstream `// @ts-expect-error`, which is a plan-banned construct, and it is consequently never
  checked by anything.
- **The existing `dist/` build is not evidence that the current tree compiles.** `dist/index.html` and
  `dist/assets` are dated 2026-08-24 21:37, while the newest source file under `src` and
  `src-tauri/src` is dated 2026-08-25 09:34. The build predates the current tree by roughly twelve
  hours, so it says nothing about it.
- **Every frontend import resolves.** An initial scan appeared to show ten unresolved imports; that was
  a bug in the checking script, which appended `.json` to paths that already ended in `.json`.
  Corrected, all imports resolve. Recorded because the original finding is wrong and would otherwise
  be re-raised.
- **Zero TODO, FIXME and HACK markers, and zero commented-out code blocks**, anywhere in `src` or
  `src-tauri/src` (verified by grep across every `.ts`, `.tsx` and `.rs` file: 0 hits).
  `console.log` and `debugger` are absent; only `console.error` is used, which is appropriate.
- **Upstream's `.mouziignore` documentation is accurate.** The inline-comment rule is at
  `ignore.rs:21` (an unescaped `#` truncates the line), the `\#` escape is at `ignore.rs:32` on read
  and `ignore.rs:44` on write, empty and comment lines are filtered at `ignore.rs:31`, and the
  `{year}`, `{month}`, `{day}`, `{extension}`, `{filename}` destination placeholders are at
  `rules.rs:141-145`. `README.md:59-62` and `README.md:261` describe these accurately. Recorded as a
  checked-and-correct claim so that the count of real README defects in this document stays honest.
  **`UNVERIFIED`:** the README's wildcard examples at `README.md:62` (`*.tmp`, exact names, trailing
  `/` for folders) were not traced to a specific glob-matching line during this pass; the pattern
  syntax itself was not re-derived here.

### 13.22 Where these three reports agree with, and refine, earlier findings

The cross-language consistency report reached four of the same conclusions as earlier passes without
being given the earlier findings, which is worth recording as independent agreement rather than as
repetition.

- **C3 (line 145) is now better understood as a symptom.** The `app` window label breaks the
  capability ACL, but the cause is the abandoned window model itself: the plan's windows were never
  consolidated, so a label that upstream could hardcode no longer suffices. C3 is not an ACL
  configuration mistake to be fixed in `capabilities/*.json`; it is downstream of a window-architecture
  decision that was never completed. Anyone patching C3 in isolation will produce a passing ACL and
  an unchanged window model.
- **The D3 deviation, recorded at line 489 as "Partial", is a full non-implementation.** The tray menu
  item exists; the settings link does not exist at all, as `Settings.tsx` grep count 0 shows. See C10.
  Line 489's "Partial" should be read as "one half of a two-part plan was delivered".
- **The undo status vocabulary defect is confirmed from a third direction.** H24 (line 1060) recorded
  two divergent status vocabularies. H25 confirms the undo side specifically, and adds what H24 could
  not: that the frontend does not read the undo status at all, and that the plan's fifth status
  (`cross-device`) has no code path because `safe_fs.rs` resolves EXDEV internally.
- **The `prev_path` naming mismatch is confirmed, with the cause relocated.** Attribute it to the
  missing serde attribute on `CleanupAction` (`db.rs:77-87`), not to the query. See 13.21.
- **Four things these reports cleared, not confirmed as defects.** The `key={log.id}` nullability, the
  `sourceLabel` i18next typing, the `files[0]` indexed access and the 2^53 overflow concern were all
  previously flagged as suspicious somewhere in this audit. All four are non-issues. The balance is
  worth stating plainly: these three reports produced two new Criticals, four new Highs and thirteen
  new Mediums, and they also retired four false suspicions. A report that only finds defects is not
  being given credit for the ones it disproves.

### 13.23 Caveats and confidence

- **All three source reports were read-only.** No source file in either mouzi directory was modified.
  `git status --porcelain` is clean apart from `MOUZI-AUDIT.md` itself. No build, test, `cargo`, `npm`,
  `npx` or Tauri command was run, and no database was opened, so every claim here is from static
  reading only.
- **Where a source report's own confidence was medium, it stays medium.** Specifically: the H28
  divergence list is verified extension by extension against both tables, but the claim about *user
  impact* ("the user is shown Other on one screen and Archives on another") is inferred from the code
  paths and was not observed at runtime. C10's "a user following the README literally can never reach
  the dashboard" is likewise inferred from the README text plus the grep count of 0, not observed.
- **Two claims from the source briefs did not survive verification and have been corrected above.**
  First, the brief asserted `db.rs:623-624` contains `prev_path AS prevPath`; it does not, and 13.21
  records the corrected location and cause. Second, the brief located the dashboard components at
  `src/components/*.tsx`; they are at `src/components/dashboard/*.tsx`, and all line numbers in
  M29-M30 are for the `dashboard/` paths.
- **Nothing in this section reopens a finding from sections 1 to 12.** Where a new finding refines an
  old one, 13.22 says so explicitly. No C-number, H-number or M-number from earlier sections is
  renumbered, withdrawn or reused.

---

**Scope of this pass.** 3 of the 21 audit reports, 13 of 21 absorbed in total. No executive summary is
added or amended: the summary in section 1 predates all seven addenda, and a reconciliation pass
should reconcile it.

---

## 14. Addendum - ninth pass: fork hygiene, and the main-thread surface

The fork-hygiene report. Its most consequential claim was checked directly rather than accepted, and
checking it made the finding substantially worse than reported. Two findings in this section were
verified by the orchestrator with a script over the source, not read from a report.

### 14.1 CRITICAL C11 - the fork has no remote of its own, so `git push` targets the public upstream

**Verified directly.** `git remote -v` reports a single remote:

```
origin  https://github.com/hsr88/mouzi.git (fetch)
origin  https://github.com/hsr88/mouzi.git (push)
```

`git remote get-url --push origin` returns `https://github.com/hsr88/mouzi.git`. There is no
`origin`/`upstream` split. The correct configuration for a fork is `origin` pointing at the forker's
own repository and `upstream` pointing at `hsr88/mouzi`; here the only remote is the 828-star upstream
project, which this fork's author does not own, and it is configured as the **push** target.

At the time of writing there are **5 commits on `wip/workspace-window` that the upstream does not
have** (the 14 elevate commits, one work-in-progress commit, and four audit-document commits), plus
a 16.5 MB offline bundle in the temp directory. All of it is local-only right now, which is the
correct state. The hazard is that `git push` with no arguments is the single most ordinary git
command, it is what any reasonable person runs first when they want to back work up, and in this
repository it resolves to writing to a stranger's repository. GitHub would reject it, so the
realistic outcome is a confusing failure rather than damage - but the failure mode depends on whose
credentials happen to be configured, and the correct fix is one command.

Plan D1 required fork hygiene as part of the personal-use quality bar. Attribution, licensing, the
app identifier and the fork notice are all in place (14.8). The remote configuration is the one
piece of D1 that is load-bearing and absent.

**Fix:** create the fork's own repository, then `git remote rename origin upstream` and add
`origin` pointing at it. Do this before any further work, not after.

### 14.2 CRITICAL C12 - all 50 commands are synchronous, so the entire IPC surface runs on the main thread

The source report flagged four slow commands. **I enumerated all 50 `#[tauri::command]` declarations
in `commands.rs` and every one is `pub fn`, not `pub async fn`. There is no
`#[tauri::command(async)]` attribute anywhere in the crate.** Per Tauri's own v2 documentation, a
command declared without `async` is executed on the main thread unless explicitly marked
`#[tauri::command(async)]`.

The consequence is therefore not "four slow commands". It is that **every command the frontend can
invoke executes on the thread that owns the window, the tray, and the watcher.** The full list, as
enumerated:

```
get_system_language  get_rules_cmd  add_rule_cmd  update_rule_cmd  delete_rule_cmd
get_folders_cmd  add_folder_cmd  remove_folder_cmd  update_folder_mode_cmd  get_logs_cmd
get_stats_cmd  undo_action_cmd  undo_all_cmd  get_cleanup_logs_cmd  get_version_cmd
get_settings_cmd  update_settings_cmd  clear_logs_cmd  scan_folder_cmd  import_archive_cmd
open_folder_cmd  get_downloads_folder  initialize_defaults_cmd  close_popup  close_settings
show_notification  enable_autostart_cmd  disable_autostart_cmd  is_autostart_enabled_cmd
load_mouziignore_cmd  save_mouziignore_cmd  get_pending_open_folder_cmd  show_popup_cmd
get_pending_files_cmd  refresh_watcher_cmd  get_schedule_cmd  update_schedule_cmd
export_rules_cmd  import_rules_cmd  start_scan_cmd  is_scanning_cmd
get_dashboard_stats_cmd  get_inventory_files_cmd  find_duplicates_cmd  find_large_files_cmd
find_stale_files_cmd  find_empty_dirs_cmd  execute_cleanup_cmd  get_suggestions_cmd
dismiss_suggestion_cmd  accept_suggestion_cmd
```

The ones that matter most are not the four the report named:

- `execute_cleanup_cmd` - performs real file moves, and it is the command the entire safety argument
  in sections 2 and 11 depends on.
- `get_dashboard_stats_cmd` - 11 SQL statements over a 1M-row inventory.
- `import_archive_cmd` - extracts an archive, writing files to disk.
- `accept_suggestion_cmd` - moves a file into a new directory.
- `find_duplicates_cmd` - hashes file contents.
- `undo_all_cmd` - moves N files back, holding the global database mutex across the whole batch.

There are exactly **four `thread::spawn` sites in the entire crate**, and only **one** of them is
inside a command: `start_scan_cmd` at `commands.rs:543`. The other three are the watcher
(`watcher.rs:54`, `watcher.rs:146`) and the scheduler (`scheduler.rs:36`), all long-lived by design.
So the scanner is the only operation in the application that was given a thread, and every other
operation shares the main one.

This refines and widens the earlier D6 finding. D6 was recorded as "the Ollama path blocks the main
thread for up to 75 seconds", which is true and was the correct finding at the time. The wider
statement is that the application has no off-main-thread command path at all, so *every* slow
operation degrades the UI rather than just the AI one. Confidence: high on the enumeration, which
is mechanical; high on the consequence, which follows from Tauri's documented behaviour.

### 14.3 HIGH H29 - two inherited release workflows would publish the fork's binaries under the upstream's product name and app id

`tauri.conf.json` sets `productName: "Mouzi"` and `identifier: "cc.mouzi.app"`, and `package.json`
carries `version: "0.1.6"`, all inherited from upstream. Five GitHub workflows are inherited
byte-identical, and **two of them are tag-triggered signing and release workflows** that reference
`secrets.SIGNPATH_API_TOKEN` and build a `releaseName` of `Mouzi ${{ github.ref_name }} (macOS test)`.

While `origin` is the upstream (C11) and no tag is ever pushed here, these workflows are dead code.
The moment a tag is pushed, a fork user's Windows build is published under the upstream's product
name and application identifier, signed with a certificate they do not hold. Plan D1 explicitly
excluded installer signing and store distribution; these workflows are the exact thing D1 ruled out,
inherited unchanged. The honest reading is that D1's exclusion was honoured in the code and not in
the automation.

### 14.4 HIGH H30 - SECURITY.md reports vulnerabilities to the upstream and promises a release policy the fork does not have

`SECURITY.md` is byte-identical to upstream, including its instruction to report privately through
`hsr88/mouzi`'s security advisories. The fork adds a large new file-touching attack surface that
the upstream policy does not contemplate: archive extraction and its zip-slip defences, file moves
and cross-device copies, Recycle Bin integration with the silent-permanent-delete behaviour recorded
in C4, and direct SQLite writes driven by frontend-supplied paths (C2). Reporting any of those to
the upstream maintainer is both wrong and, in practice, a disclosure to a third party.

`SECURITY.md:7` also states that updates are provided for the latest released version. That is false
for the fork: plan D1 forbids an updater, no fork release exists, and the version was never bumped
(C9).

### 14.5 HIGH H31 - plan D3 is a full non-implementation, and C3 is a symptom rather than a defect

Recorded as a refinement of an earlier finding. Plan D3 mandated a new 1024x768 window labelled
`dashboard`. The shipped application has **one** runtime window, labelled `app`, sized 1100x820,
built at `tray.rs:178-186`; the dashboard, cleanup, suggestions and settings surfaces are hash
routes inside it. Meanwhile `capabilities/default.json:5` still lists `dashboard`, `cleanup` and
`suggestions` as window labels, and **none of those labels ever exists at runtime**.

This means C3 - the missing `"app"` entry that breaks the Tauri capability ACL - is better
described as a *symptom* of an abandoned window model than as a one-line omission. The capability
file describes a design that was dropped; the ACL breaks because the file and the code disagree
about what a window is. Fixing the one line restores the dialog permissions, but the deeper issue
is that a plan decision was abandoned without the surrounding configuration being updated.

### 14.6 MEDIUM M40 - the plan's six mandated test categories, and its baseline count is wrong

| # | Category | Status |
|---|---|---|
| 1 | classifier pure functions | **PRESENT** - 19 tests, `classify.rs:688-804` |
| 2 | dedup size-prefilter and cache invalidation | **PRESENT** - `cleanup.rs:439,537,589` |
| 3 | undo move-back fixtures | **PARTIAL** - collision `safe_fs.rs:141` and missing-parent `safe_fs.rs:180` are covered; the cross-device branch at `safe_fs.rs:80-82` is not, because `safe_fs.rs:199` calls `copy_delete_fallback` directly and never reaches the detection (H19) |
| 4 | migration idempotency (`init_db` twice) | **ABSENT, and structurally impossible to write as specified** - `init_test_db` guards on `if DB.get().is_none()`, so the second call is a no-op and the assertion the plan describes cannot be made |
| 5 | trash to the Recycle Bin on temp files | **PRESENT BUT VACUOUS** - `safe_fs.rs:232` swallows its `Err` branch, so the test passes whether or not the trash path works |
| 6 | Ollama-absent behaviour via a mock provider | **ABSENT** - no mock provider exists anywhere in the crate, so `detect_provider` is never exercised (H20) |

Two of six are absent and a third is vacuous. The plan also records the upstream baseline as 6 tests;
the actual count at the merge-base commit `c4eac33` is **11**. The plan's own baseline number is
wrong in the conservative direction, which is worth recording so nobody "corrects" a real regression
back to a plan figure.

### 14.7 LOW

- **`FUNDING.yml` still routes to the upstream author's ko-fi.** Defensible, since taking someone's
  donations for your own fork would be worse, but it means the fork's README carries a donation link
  to a stranger. Low, and no change is recommended.
- **No `git` branch protection and no remote means no backup beyond the local bundle.** Covered by
  C11 rather than repeated here.

### 14.8 Recorded as sound, so the counts above are not inflated

- `LICENSE.md`, `CODE_OF_CONDUCT.md`, `SECURITY.md` and `CONTRIBUTING.md` are **byte-identical to
  upstream**. For an MIT fork that is correct: the licence must not be edited, and the code of
  conduct and contributing guide are upstream's to maintain. The MIT notice is intact and the fork
  notice is prominent at `README.md:3`, crediting upstream, stating MIT, and identifying the work as
  a personal fork.
- The application identifier `cc.mouzi.app` is preserved (`tauri.conf.json:5`), satisfying D1's
  explicit requirement to keep it.
- All four new Rust modules - `scan.rs`, `cleanup.rs`, `safe_fs.rs`, `classify.rs` - are free of
  Tauri dependencies, satisfying plan section 6. They are testable in isolation, which is why 57 of
  the tests exist at all.
- `useDashboardStore` imports only the `WatchedFolder` *type* from `useAppStore`, a type-only import
  that is erased at compile time. M2's "dashboard state stays local" mandate is honoured in substance
  even though a global store still exists.
- All ten locales carry the dashboard, cleanup and suggestions keys, satisfying M5.
- The `scan-progress` and `scan-complete` event listeners exist, satisfying M2's notification
  requirement, and the scanner's `PROGRESS_EVERY` emission rate is not a storm (H11 in section 11
  already measured this).

### 14.9 What this section changes about the earlier conclusions

- **C3 is reclassified** from "a missing string in a JSON file" to "an abandoned window model that
  left its configuration behind" (14.5). The one-line fix is still correct; the framing is not.
- **The D6 finding is widened** from one blocking command to the whole IPC surface (14.2). Any
  estimate of UI responsiveness that assumed a command off the main thread was wrong.
- **Two new Criticals are added that no earlier section covered:** the push target (14.1) and the
  main-thread surface (14.2). C11 is the more urgent of the two in practice, because it is a live
  misconfiguration rather than a latent defect, and it is fixed by one command.

---

**Scope of this pass.** 1 of the 21 audit reports, 14 of 21 absorbed in total, plus two findings
verified directly by the orchestrator. Section 1's executive summary now predates eight addenda and
should be reconciled against them.

---

## 15. Addendum - tenth pass: window lifecycle, the capability ACL, and the event bus

The Rust core lifecycle report (`lib.rs`, `tray.rs`, `capabilities/default.json`, `tauri.conf.json`).
It was checked against the pinned Tauri source rather than against documentation prose, and checking
it **narrowed** one earlier Critical and produced a more precise one in its place.

The lockfile pins **tauri 2.11.1**. Three facts about Tauri 2 govern everything below, and each was
read out of the crate source rather than assumed:

1. A webview whose label is matched by **no** capability receives **no** ACL permissions. An empty
   `windows` list matches nothing.
2. **App-defined `#[tauri::command]` handlers bypass the ACL entirely** when the app ships no
   permission manifest. There is no `src-tauri/permissions/` directory and `build.rs` registers none,
   so every one of the 50 commands is callable from any window, local origin, without an ACL check.
3. `plugin:event|listen` and every other `plugin:*` command are **always** ACL-checked, with no
   equivalent bypass.

Facts 2 and 3 together mean the capability defect in section 2 (C3) breaks far less than a first read
suggests, and breaks something different. That correction is recorded in 15.2.

### 15.1 CRITICAL C13 - the window the user actually sees has no capability, so the dashboard's live scan progress can never arrive

**This refines C3 and should be read alongside it.**

The capability file `capabilities/default.json:5` lists its windows as
`["main", "popup", "settings", "dashboard", "cleanup", "suggestions"]`. The windows the application
actually creates at runtime are `main` (from `tauri.conf.json`) and `app` (built at
`tray.rs:178-186`). **`app` is not in the list.** `settings`, `dashboard`, `cleanup` and
`suggestions` are never created as windows at all - they are hash routes inside `app`.

Because `app` matches no capability, and because fact 2 holds, the **data** half of the workspace
works: `invoke("get_dashboard_stats_cmd")`, `invoke("get_inventory_files_cmd")`,
`invoke("find_duplicates_cmd")` and every other app command succeed from `app` with no ACL check. So
the dashboard renders, the treemap draws, the numbers are real.

What breaks is everything routed through a plugin, and the most important casualty is
`plugin:event|listen`:

| Call site | Plugin command | Status in the `app` window |
|---|---|---|
| `App.tsx:63` `listen("file-organized", ...)` | `plugin:event\|listen` | **denied** |
| `useDashboardStore.ts:200` `listen("scan-progress", ...)` | `plugin:event\|listen` | **denied** |
| `useDashboardStore.ts:207` `listen("scan-complete", ...)` | `plugin:event\|listen` | **denied** |
| `App.tsx:70` `onAction(...)` from `plugin-notification` | `plugin:notification\|*` | **denied** |
| `Settings.tsx:5` `save`, `open` from `plugin-dialog` | `plugin:dialog\|*` | **denied** |
| all 50 `invoke(...)` app commands | none - not ACL-gated | **allowed** |

The consequence is that **the dashboard's live scan progress, which is milestone M2's headline
feature, cannot work in the only window the user can open.** `listen` rejects; in `App.tsx:63` the
rejection is unhandled; in `useDashboardStore` the `await` throws so the `.then()` continuation in
`Dashboard.tsx:52` never runs. No `scan-progress` event is ever received, so the progress bar never
moves. The scan still runs, and the numbers are still correct on the next manual refresh - the
window where the defect is invisible.

**Why the plan's own gate would not have caught it.** M2's exit criterion is that files appearing in a
watched folder show up in the dashboard treemap and stats. That passes, because the initial
`refresh()` uses `invoke`, which is not ACL-gated. The failure is confined to the part of M2 that
only exists at runtime.

**Correction to section 2 (C3).** C3 was recorded as "the capability file is missing the `app` label,
which breaks the Tauri ACL, and the app will look like it works because app commands bypass the
ACL". The mechanism and the observation are both confirmed. What was wrong was the implied severity
and the implied symptom: I implied a broad breakage and named the dialogs as the visible failure.
The dialogs are indeed broken. But the *most* serious consequence is not the dialogs - it is
`listen`, and therefore the live progress bar. One line in the JSON still fixes all of it; the label
is `app` and it must be added.

### 15.2 HIGH H32 - "Clean Now" in the tray runs a full recursive walk and file moves on the main thread

`tray.rs:40-42` handles the menu event and calls `perform_clean` directly. `perform_clean`
(`tray.rs:108-123`) calls `db::get_watched_folders()` - which takes the single global database
mutex - and then `manual_scan_folder(&folder.path)` **synchronously, for every enabled watched
folder**, which walks the tree and moves files.

Tauri delivers `on_menu_event` on the main thread. So selecting one menu item freezes the entire
application for the duration of the walk: no window paints, no IPC is served, the tray menu does not
respond, and there is no progress indication and no cancellation. On a large watched folder that is
minutes of a visibly hung application, ending in a tray "Clean Now" that gave no feedback while it
happened (H33).

The scheduler performs **the same work** and gets it right: `scheduler.rs:36` wraps it in
`thread::spawn`. The correct fix is to give `perform_clean` the same treatment, which is a two-line
change.

### 15.3 HIGH H33 - the tray's "Clean Now" result message is emitted to nobody

`tray.rs:118-121` builds a human-readable result string and emits it as the `show-notification` event
whenever `total > 0`. There is **no listener for `show-notification` anywhere in the frontend.** A
full event-to-listener inventory:

| Event | Emitted at | Listened at |
|---|---|---|
| `file-organized` | `watcher.rs:110`, `watcher.rs:120` (per file) | `App.tsx:63`, `Popup.tsx:72` |
| `file-detected` | `watcher.rs:234`, `watcher.rs:288` (per file) | **none** |
| `scan-progress` | `commands.rs:546` | `useDashboardStore.ts:200` |
| `scan-complete` | `commands.rs:549` | `useDashboardStore.ts:207` |
| `show-notification` | `tray.rs:120` | **none** |
| `scheduled-clean-done` | `scheduler.rs:120` | **none** |

So a user who picks "Clean Now" from the tray is told nothing at all - not how many files were
organised, not that the operation ran, not that it did nothing. The result is computed and then
discarded. `git log -S"show-notification"` shows the event was introduced in the base commit
`c4eac33` and has never had a listener, so this is not a regression introduced by the elevate work;
it is an upstream defect the fork inherited and did not fix. The application does have a working
notification plugin, so the plumbing exists and is simply unused on this path.

`scheduled-clean-done` has the same shape: after a scheduled clean nothing in the UI updates until
the user refocuses the window. Lower impact, same cause.

### 15.4 HIGH H34 - milestone M5's event batching is not addressed, and the cost is larger than one emit per file

`file-organized` is emitted **once per file**, from inside the per-file loop at `watcher.rs:110`.
Both listeners respond with no debounce and no coalescing: `App.tsx:63-66` calls `loadLogs()` and
`loadStats()`, and `Popup.tsx:72` additionally calls `getPendingFiles()`.

Those are all app commands, so by fact 2 they are **not** ACL-gated - they run, on the main thread,
each acquiring the single global database mutex. There is a direct contrast in the same codebase:
the dashboard's `scan-complete` path **is** debounced (200 ms, in `useDashboardStore`), so the author
knew the pattern and applied it to one path only.

Measured consequence, and this is the compounding part rather than a separate defect: the leftover
`main` window described in 15.5 also renders the Popup route and therefore also holds a
`file-organized` listener. So **each organised file costs two windows times three database queries,
all serialised on one mutex, all on the main thread** - six main-thread round trips per file, so a
batch of N organised files costs 6N. This is the same class as the confirmed concurrency problem in
the sibling `file-dashboard` project, where a nightly job and a request contended and one scan ran
16.9 minutes against a 7.6-9.0 minute baseline. Different codebase, same shape.

### 15.5 MEDIUM M41 - a leftover invisible window runs the whole app and polls the database every three seconds, forever

`tauri.conf.json` declares a `main` window at 800x600 with `visible: false`. **No Rust code
anywhere references the `"main"` label** - it is never shown, never focused, never closed. Yet it is
created at startup and lives for the life of the process.

It is, however, in the capability list, so it receives full ACL permissions and runs the entire React
application. With an empty location hash, `App.tsx` renders its default route, which is `Popup`. And
`Popup.tsx:91-93` sets a `setInterval` that calls `get_pending_files_cmd` **every three seconds**.
So the application permanently runs a hidden window that executes a database query every three
seconds, forever, for a window no user can see and no code path ever displays.

Two consequences worth separating. The wasted work is real but small. The more interesting
consequence is that this phantom window is the only thing preventing the process from exiting when
every visible window is closed - an accidental dependency on dead configuration for a
correctness-adjacent behaviour. Deleting the `main` window without adding an explicit exit policy
would change the application's lifecycle, so this is not a one-line deletion.

### 15.6 MEDIUM M42 - the single global database mutex is contended from both the main thread and the notify thread

`db::get_db()` returns one `Arc<Mutex<Connection>>` guarding one `Connection`. `tray_lang()`
(`tray.rs:62-66`) acquires it on the **main thread** every time a window is shown or the tooltip is
refreshed, and `update_tray_tooltip` is invoked **per detected file** from `watcher.rs:238` and
`watcher.rs:292` on the **notify thread**.

So the main thread and the notify thread contend for one lock. If the notify thread holds it, the
main thread blocks inside `show_app_window`; if the main thread holds it during a slow query, the
notify event loop stalls and filesystem events are delivered late. No deadlock was found and none is
claimed - the lock ordering is acyclic, consistent with section 10's finding. This is contention and
latency, not deadlock, and it is recorded as such.

Compounding it, `tray_lang()` rebuilds a sixteen-entry `HashMap` on every call, so the per-file
tooltip refresh is a database round trip plus a sixteen-entry map construction, once per detected
file.

### 15.7 MEDIUM M43 - `close_settings` closes the workspace from every route, including the dashboard

`commands.rs:385` `close_settings` calls `crate::tray::hide_app_window`. It is invoked from
`Dashboard.tsx:88` and `Settings.tsx:327` - that is, the dashboard's back control and the settings
page's close control call the same command. Since the consolidation, "close settings" means "hide
the entire application", so the dashboard's back chevron minimises the whole window instead of
navigating back. The name no longer describes what it does, and the behaviour on the dashboard is
not what the control looks like it does.

### 15.8 MEDIUM M44 - the workspace window is destroyed on close, with no handler, losing all state

There is no `on_window_event` handler in the Rust code and no `onCloseRequested` anywhere in the
frontend. Closing the `app` window with the X therefore **destroys** the webview rather than hiding
it. The next tray click rebuilds it, which is a full SPA reload: a fresh WebView2 instance, a fresh
i18n boot, and a fresh round of `invoke` calls for settings, stats and inventory. Any in-progress
filter or selection is gone, and there is no way to detect that the window was closed rather than
hidden - so `useDashboardStore`'s scan listeners, if they were ever registered, would need
re-establishment.

### 15.9 LOW

- **The plan's single-instance claim is stale, not the code.** Plan section 2 recorded
  `lib.rs:71-77` as knowing only `popup` and `settings`. The current code at `lib.rs:76-82` knows
  `app` and `popup`, which is correct for the consolidated window model. Recorded so the plan anchor
  is not "corrected" back.
- **Single-instance focus does not un-minimise.** `lib.rs:76-82` calls `show()` and `set_focus()` but
  never `unminimize()`. On Windows, focusing a minimised window is not reliably sufficient to restore
  it. **UNVERIFIED** - not run.
- **Three `.unwrap()` calls on mutex locks** in the single-instance path and watcher:
  `lib.rs:53`, `lib.rs:156`, `watcher.rs:134`. A panic between lock and write poisons the mutex, and
  every later acquisition panics in turn.
- **`lib.rs:105` `.expect("Failed to initialize database")`** aborts the process if the database
  cannot be opened, and the `create_dir_all` on the line above it swallows its error with `.ok()` -
  so the panic message cannot say which path failed.
- **`TrayI18n::get` has no English fallback.** `i18n.rs:186` returns the key itself on a miss, so a
  key missing from a non-English locale renders the raw key in the tray menu. All ten locales
  currently carry all sixteen keys, so this is latent.
- **`lib.rs:53` does not un-minimise** is listed above; the tooltip on a running scan never reflects
  scan state, only folder counts.

### 15.10 Recorded as sound, so the counts above are not inflated

- **The tray dashboard entry exists and is wired.** `tray.rs:14` registers id `dashboard` with i18n
  key `dashboard`; `tray.rs:31-33` routes it to `show_app_window(app, "/#/dashboard",
  "dashboard_title")`. Plan D3's "entry via tray menu item" is satisfied.
- **The tray's Rust-side i18n is complete and consistent.** `i18n.rs` carries the five new keys in
  all ten locales (verified at the ten insertion blocks from `i18n.rs:20` through `i18n.rs:179`),
  matching the frontend's ten locales exactly. It is a *second* i18n system alongside the frontend's
  JSON files, which is a maintainability concern already recorded as M28, not a defect in the
  coverage.
- **Left-click shows the popup, right-click shows the menu** using Tauri's default behaviour; no
  explicit right-click handler is needed and none is missing.
- **The scheduler does threading correctly** (`scheduler.rs:36`), which is the proof that H32 has a
  known-good precedent inside the same crate rather than requiring a new pattern.
- **Zero `async fn` in the entire Rust backend**, confirmed by enumeration. This independently
  corroborates C12 in section 14, arrived at from the opposite direction - this report was looking
  for async commands and found none.
- **`main.rs` and `build.rs` are trivial**, with no hidden initialisation.

### 15.11 What this section changes about the earlier conclusions

- **C3 is reclassified again, and C13 replaces it as the precise statement.** C3 said the missing
  `app` label "breaks the Tauri capability ACL". True. What it did not say is that app commands are
  exempt, so the breakage is confined to plugin calls - and that the single most important plugin
  call in the application is `listen`, on the scan-progress path that milestone M2 exists to
  deliver. C3 also implied the dialogs were the visible symptom; they are broken, but they are the
  *lesser* failure.
- **C12 is corroborated** by an independent route.
- **D3's non-implementation (H31) is confirmed from the lifecycle side**, and the causal chain is now
  explicit: the capability file was updated to match the *plan* rather than the *code*, which is
  what produced a window list describing windows that are never created.
- **One new High is added that no earlier section covered** - H33, the tray's primary action
  producing no feedback whatsoever, dead since the upstream base commit.

---

**Scope of this pass.** 1 of the 21 audit reports, 15 of 21 absorbed in total. Section 1's executive
summary now predates nine addenda and should be reconciled against them.

---

## 16. Addendum - eleventh pass: the watcher, and the recursion trap

The watcher report. Its central finding is structural rather than a single defect, and it changes how
the two most-cited upstream anchors must be read. Five of the seven Tauri-threading claims in this
section were checked against the `tauri-docs` source for the pinned 2.11.1 rather than assumed.

**Correction to the absorbed count.** Sections 14 and 15 each said "15 of 21"; the true figure after
this section is **15 of 21 reports absorbed, 6 outstanding**. The earlier count double-counted the
orchestrator's own verifications as reports.

### 16.1 CRITICAL C14 - the self-trigger guard covers undo and nothing else, and it leaks an entry per undo

`state.ignored_files` is the map that stops the watcher's own file moves from being re-processed. It
has exactly **two** write sites, both in the undo path: `commands.rs:219` and `commands.rs:228`.
Verified by enumerating writers across the whole crate. **Nothing else writes to it.** In particular:

| Operation | Inserts into `ignored_files`? |
|---|---|
| `perform_undo` / `undo_all_cmd` | **yes** (`commands.rs:219`, `:228`) |
| `process_file` - the watcher's own auto-organise move | no |
| `accept_suggestion_cmd` - the AI suggestion accept flow | no |
| `execute_cleanup` - Recycle Bin deletes and empty-dir removal | no |
| `manual_scan_folder` - scheduled clean, tray "Clean Now", archive import | no |

So plan M1's "watcher suppression for restore ops" was implemented for the one path that was easiest,
and the four paths that move files on a schedule or in bulk were left unsuppressed. This is survivable
today for a reason given in 16.2, and it stops being survivable the moment recursion lands.

**The leak is separate and unconditional.** `watcher.rs:217-222` removes an entry only when an event
for that path arrives *after* it has expired:

```rust
if let Some(&instant) = ignore_guard.get(&path_str) {
    if Instant::now().duration_since(instant) < Duration::from_secs(IGNORE_DURATION_SECS) {
        continue;
    }
    ignore_guard.remove(&path_str);
}
```

For an undo, the restored file generates exactly one event, and it arrives within the 30-second
window - so the handler takes the `continue` branch, the entry is **not** removed, and no further
event ever arrives for that path. The destination half is worse: it is a subfolder, so a non-recursive
watch never sees it at all. **Every undo therefore leaves two permanent entries in a map that nothing
ever prunes.** In an application designed to sit in the tray all day this is an unbounded map, and it
is the same map that gates correctness.

### 16.2 CRITICAL C15 - the non-recursive watch is load-bearing, so fixing it as specified would create an infinite organise loop

`watcher.rs:317` still reads `RecursiveMode::NonRecursive`. **Verified by git history: the only change
ever made to `watcher.rs` in this fork is `IGNORE_DURATION_SECS` from 5 to 30.** The recursion line and
the whole ignore-check block are verbatim upstream.

Plan D4 requires the watcher to be recursive within watched roots, and the plan's own M3 note
acknowledges the gap - "empty-folder sweep, recursive awareness needed, watcher is non-recursive". The
scanner was made recursive. The watcher was not.

**The consequence is the opposite of what the plan assumes.** With a non-recursive watch, an organise
move from `Downloads/report.pdf` to `Downloads/Documents/report.pdf` is invisible to the watcher: the
removal of `report.pdf` is skipped because `path.is_file()` is now false, and the creation of the
`Documents` subfolder is skipped for the same reason. **The non-recursion is the only thing preventing
self-retrigger.** Verified through the full event path rather than assumed.

If D4 is implemented as written - recursive watch, no new suppression - then every organise move
becomes visible, is not in `ignored_files` (16.1), and re-queues itself. The grace period is 300
seconds, so the loop is one file every five minutes rather than instantly, but it never terminates and
it is silent. **D4 cannot be implemented safely without first extending the suppression to
`process_file`, `accept_suggestion_cmd`, `execute_cleanup` and `manual_scan_folder`, and making the
suppression batch-aware rather than a fixed per-path time window.** These two findings are one finding.

### 16.3 CRITICAL C16 - `undo_all` on an accumulated log table is an unbounded mass restore that freezes the app and silently loses file events

`action_logs` grows without bound; the only prune is a user-initiated `clear_logs_cmd`. After a few
months of a tray app auto-organising, it holds thousands of rows. `undo_all_cmd` (`commands.rs:226-242`)
moves **every** `undone = 0` row back, and for the whole batch it holds **the global database mutex
and the `ignored_files` mutex**, doing filesystem I/O per row.

Four distinct failures compound:

1. **The UI freezes.** `undo_all_cmd` is a synchronous command, and Tauri runs synchronous commands on
   the main thread - confirmed against `tauri-docs` `calling-rust.mdx`: *"Commands without the
   `async` keyword are executed on the main thread unless defined with `#[tauri::command(async)]`"*,
   and the crate contains zero `async fn`. So the window is unresponsive for the entire mass restore.
2. **File events are silently lost.** The notify event handler needs the `ignored_files` mutex
   (`watcher.rs:268`), which `undo_all_cmd` holds for the whole batch. notify's event thread is pinned
   for the duration, and on Windows the OS directory-change notification buffer can overflow while it
   is blocked - at which point `ReadDirectoryChangesW` fails and the dropped notifications are **not
   redelivered**. The practical effect is that the watcher silently stops organising a folder until
   the application restarts. Mechanism is documented Windows behaviour; whether it has already
   occurred on this machine is **UNVERIFIED**.
3. **The database mutex is held across filesystem I/O**, so the watcher's per-file
   `get_watched_folders()`, the notify handler's per-event `get_settings()`, the scheduler and the
   scanner all stall for the whole batch.
4. **Cross-device batches exceed the 30-second suppression window, and the app then undoes its own
   undo.** For same-volume local moves the 30 seconds is sufficient - roughly 0.1-0.5 ms per row, so
   even 5,000 rows complete inside it, and this is worth stating plainly rather than assuming. But a
   cross-device restore performs a full `std::fs::copy` per row through
   `copy_delete_fallback`; a few hundred megabytes over SMB or USB runs to tens of seconds. Once
   past 30 seconds, a restored file's event is no longer suppressed, so it is re-queued, and 300
   seconds later `process_file` moves **the just-restored file straight back** into its category
   folder - with a fresh `action_logs` row recording the move, and the original row already marked
   `undone = 1`. The user's undo is reverted by the application itself.

### 16.4 HIGH H35 - the initial scan is quadratic, does three filesystem syscalls and a database query per file, and runs on the main thread during startup

`watcher.rs:208-252` is the initial scan. It is called from `lib.rs:157` inside `.setup()`, which Tauri
runs on the main thread, and again from `add_folder_cmd`, `remove_folder_cmd`,
`update_folder_mode_cmd` and `refresh_watcher_cmd` - all synchronous commands, therefore also the main
thread. Per file it performs:

- `pending.guard.retain(|x| x.path != path)` - a **linear scan** (`watcher.rs:243-248`, `:298-304`), so
  the whole pass is **O(F squared)** in file count, with a `PathBuf` string comparison per element.
- `db::get_settings()` - a mutex acquisition plus a 14-column query, **per file**
  (`watcher.rs:240`, `:295`).
- `is_file_ignored_by_mouziignore` - which calls `load_mouziignore` per file
  (`ignore.rs:6-10`), doing a `Path::exists()` **and** a `fs::read_to_string` of `.mouziignore`
  **from disk, every time**.
- `should_ignore_file` - an `fs::metadata` per file (`rules.rs:66`).

For a watched folder of 5,000 files that is roughly 15,000 synchronous filesystem syscalls and 5,000
SQLite queries before the window appears. The application looks hung on startup, proportionally to the
size of the watched folders. `refresh()` also calls `self.watchers.clear()` and then re-walks **every**
folder, so adding one watched folder re-queues every file in every other folder and resets each file's
300-second grace timer.

**A lost-file race.** The initial scan completes **before** `watcher.watch()` is called
(`watcher.rs:208-252` then `:259-320`). A file created in that window is seen by neither, and there is
no rescan. The window is the duration of the read pass over all preceding folders - hundreds of
milliseconds to seconds on a multi-root setup. A file downloaded into that gap is **silently never
organised**.

### 16.5 HIGH H36 - `find_duplicates_cmd` hashes the whole inventory on the main thread, taking the global mutex per file

`find_duplicates_cmd` (`commands.rs:625`) is synchronous, so it runs on the main thread. It calls
`cleanup::find_duplicates`, which reads every same-size candidate and hashes **full file contents**
with blake3 (`cleanup.rs:75-86`, `:92-108`). Each file's cache lookup and store takes the single global
database mutex (`db::get_db()` is one `Arc<Mutex<Connection>>`). So the main thread spends an unbounded
period reading file contents while repeatedly locking the one connection the rest of the application
needs. On a large inventory with many same-size groups this is minutes of frozen UI plus total database
starvation. This is the single worst main-thread offender found in the audit.

`execute_cleanup_cmd` (`commands.rs:661`) has the same shape for a shorter period: `trash::delete` is a
blocking COM call of roughly 10-50 ms per file, and each `remove_empty_dir` re-runs the full recursive
directory walk (`cleanup.rs:352`) - all on the main thread. Trashing 500 duplicates freezes the window
for seconds to minutes.

`get_dashboard_stats_cmd` (`commands.rs:581`) is also synchronous and issues **seven separate queries**,
each independently re-locking the global mutex, two of which are full-table `GROUP BY` aggregates over
`file_inventory`. Combined with H34 - where the dashboard reloads on every `file-organized` event - one
organised file can cost seven mutex acquisitions and two full-table aggregates on the main thread.

### 16.6 MEDIUM M45 - the suppression entry is inserted after the move, not before it

`perform_undo` calls `move_file` first (`commands.rs:174`) and only then inserts both paths into
`ignored_files` (`commands.rs:177-178`, `:189-190`). The filesystem change therefore happens before the
guard entry exists, so the notify handler can read the map before the writer populates it.

For `undo_all_cmd` the window is closed in practice, because the command holds the `ignored_files`
mutex for the whole batch and the handler blocks on it at `watcher.rs:268` - the mutex accidentally
provides the ordering. For a **single** `undo_action_cmd` there is no such protection, and the handler
performs its own `fs::metadata` and `.mouziignore` reads before reaching the lookup, so the writer
almost always wins. The likelihood becomes non-trivial when that handler's syscalls are slow - a network
share, a cold spin-up, or CPU contention. The fix is to insert before the move, which is strictly
safer and costs nothing. Confidence: the ordering is objectively wrong; practical exploitability is
low and conditional.

### 16.7 MEDIUM M46 - the capability file lists a window label the tray never creates

`capabilities/default.json:5` lists `["main", "popup", "settings", "dashboard", "cleanup",
"suggestions"]`. `tray.rs:172` and `tray.rs:179` build the workspace window with the label **`app`**.
So `dashboard` is in the list but is never a window label, and the label that *is* used is not in the
list. This is the same root cause as C3 and C13, observed from the watcher's side: the capability file
was updated to match the *plan's* window model rather than the *code's*. **Runtime impact UNVERIFIED**
- the application was not run, and it is possible that a second capability file or a remote-URL match
covers it. What is certain is the mismatch.

### 16.8 MEDIUM M47 - absolute rule destinations are live, and the migration that would remove them is dead code

`execute_rule` (`rules.rs:174-180`) explicitly honours absolute rule destinations for backward
compatibility. `db::migrate_rules_to_relative` (`db.rs:296`) exists to convert them, and is **defined
but never called** - the only occurrence of the name in the crate is its own definition.
`add_rule_cmd` and `update_rule_cmd` (`commands.rs:37-46`) validate nothing, so a user can type an
absolute destination into the rules form.

Combined with the unvalidated `add_folder_cmd` (C5), this produces a reproducible wrong behaviour: with
`Downloads` and `Documents` both watched and a rule whose destination is the absolute path
`C:\Users\<name>\Documents`, an organised PDF re-appears as a top-level entry in the `Documents` watch,
is not in `ignored_files`, is re-queued, and 300 seconds later `process_file` resolves the destination
to the file's own location. `fs::rename(x, x)` returns success on Windows, so a **bogus
`action_logs` row with `source == destination` is written and a `file-organized` success event is
emitted** for a file that was not organised. It does not loop infinitely - a same-path rename raises no
filesystem event - but it repeats on every `refresh()` and every application start, and the file stays
misfiled forever. A second latent hazard in the same function: `get_downloads_folder` falls back to
`"C:/Users"` (`commands.rs:363`) when the platform API returns nothing, i.e. a whole user profile.

### 16.9 MEDIUM M48 - D4 remains unguarded at the input layer, which is survivable only because of C15

`add_folder_cmd(app, path: String, mode)` accepts an arbitrary string with no drive-root rejection, no
containment check and no depth budget, and calls `create_dir_all` on it before storing it. Today the
blast radius is bounded by the non-recursive watch, which limits damage to the top level of whatever
root was entered. **The moment D4 recursion lands, an unvalidated `C:\` becomes a whole-drive
recursive watch whose default behaviour is to move the user's files into category subfolders on a
300-second timer.** The input validation and the recursion change must ship together. Recorded here so
that implementing D4 is not treated as a one-line change.

### 16.10 LOW

- **`ignored_files` is keyed on absolute path strings.** A casing difference between what
  `action_logs.source_path` stored and what notify reports would silently defeat the guard on a
  case-insensitive filesystem. Not observed; **UNVERIFIED**.
- **No `Drop` for `FolderWatcher`, no join, and `handle` is write-only.** The watcher processing thread
  (`watcher.rs:54`) and the scheduler thread (`scheduler.rs:36`) are detached, loop forever with no
  shutdown signal, and are never joined. `app.exit(0)` terminates the process without waiting, so the
  OS reclaims everything and the practical impact is nil - but there is no clean shutdown path, and the
  `RecommendedWatcher` handles are only released if `AppState` is dropped, which `process::exit` prevents.
- **`import_archive_cmd` organises files inside the cache staging directory**
  (`%LOCALAPPDATA%\mouzi\cache\archive-imports\import-*`) and returns that path; the organised files are
  never moved to a real destination. Not a watcher defect, noted because it is a move path with no
  suppression - harmless only because the staging directory is not a watched root.

### 16.11 Recorded as sound, so the counts above are not inflated

- **The grace period is a correct per-path debounce.** Every event for a path does
  `retain(|x| x.path != path)` then `push`, rescheduling that path to `now + grace`. Five thousand
  events across five thousand distinct files produce five thousand pending entries and **one** process
  pass, not five thousand. The storm question is therefore answered at the watcher level; what remains
  open is the one-emit-per-file *downstream* cost recorded as H34.
- **Intra-root relative moves do not re-trigger**, verified through the event path rather than assumed,
  and the reason is exactly the non-recursion in C15.
- **`start_scan_cmd` correctly spawns a thread** (`commands.rs:543`), so the scanner is off the main
  thread - unlike the watcher, the dedup finder, the cleanup executor and both undo commands.
- **The `.mouziignore` read error path returns `(false, [])`** (`ignore.rs`), which errs toward *not*
  treating a file as ignored, so an unreadable ignore file cannot cause a skip. Recorded so the finding
  count is not inflated.
- **Zero `async fn` in the entire Rust backend**, confirmed again by enumeration. This is the third
  independent route by which C12 has been corroborated.

### 16.12 What this section changes about the earlier conclusions

- **C15 inverts the reading of two plan anchors.** D4's "make the watcher recursive" and M3's "the
  watcher is non-recursive" were recorded as a missing feature. It is a **trap**: implementing D4 as
  written produces a silent infinite re-organise loop, because the suppression that would prevent it
  exists only for undo (C14).
- **H21's "no CI runs any test" gains a second reason to matter.** A CI job that ran `cargo test` would
  not have caught any of this: every finding here is in code with no test, and the two that are tested
  (`safe_fs.rs:199`, `rules.rs:288`) test the wrong thing (H19).
- **C12 is corroborated a third time**, and H36 names the worst single instance of it.
- **The upstream-defect tally in section 2 is revised.** The watcher was previously listed as
  "non-recursive, untouched". It is untouched, and that is worse than it sounds.

---

**Scope of this pass.** 15 of the 21 audit reports absorbed, **6 outstanding**: dependency and build
configuration, React routing, Zustand stores, dashboard components, i18n, and whole-stack performance.
Section 1's executive summary now predates ten addenda and should be reconciled against them.

## 17. Addendum - ninth pass: dependencies, build config, routing and performance

Source: three read-only sub-agent reports (dependency/build config; routing and window mapping; whole-stack performance). No build, no run, no database access, no network. Every figure below is a static estimate unless stated otherwise.

### 17.1 CRITICAL - C17: `open_folder_cmd` is a PowerShell injection sink reachable from 10 call sites

**Where:** `src-tauri/src/commands.rs:328-337`; call sites `src/components/dashboard/StorageTreemap.tsx:42`, `src/components/dashboard/FileBrowser.tsx:81`, `src/components/cleanup/FileListTab.tsx:84`, `src/components/cleanup/HistoryPanel.tsx:18`, `src/pages/Settings.tsx:429`, `src/pages/Popup.tsx:123`, `src/pages/Popup.tsx:130`, `src/pages/Popup.tsx:265`, `src/App.tsx:73`, `src/App.tsx:84`

```rust
let win_path = path.replace('/', "\\");
std::process::Command::new("powershell")
    .args(["-NoProfile", "-NonInteractive", "-Command",
           &format!("explorer '{}'", win_path)])
    .spawn()
```

Rust's `.args()` does not invoke a shell, but PowerShell parses the `-Command` string itself. A path containing a single quote closes the quoted literal and the remainder executes as PowerShell. A file named `report'; IEX(...); '` inside a watched folder is sufficient. The `path` argument is overwhelmingly not user-typed: it originates from scanned paths and DB rows, so this is attacker-controlled filesystem content reaching a command interpreter.

The app already grants `opener:allow-reveal-item-in-dir` and `opener:allow-open-path` (`src-tauri/capabilities/default.json:9-10`) and registers `tauri_plugin_opener::init()` (`src-tauri/src/lib.rs:42`), which is Tauri's sanctioned, ACL-mediated, scope-checked path for this operation. The frontend imports `@tauri-apps/plugin-opener` nowhere, so the project carries an unused plugin npm entry (`package.json:18`), two granted permissions, and a hand-rolled injection sink at the same time.

Confidence: **High on the code defect** (read directly). **UNVERIFIED on exploitability end-to-end**: the agent did not execute it, and confirming the full chain requires the watcher to DB to UI path exercised live. Fix is `Command::new("explorer").arg(&path)` with no shell and no interpolation, or use the granted plugin. The `path.replace('/', "\\")` normalisation at `commands.rs:328` is also unnecessary; Explorer accepts forward slashes.

### 17.2 CRITICAL - C18: the Duplicates tab deletes the file it renders as "Kept"

**Where:** `src/components/cleanup/DuplicatesTab.tsx:20`, `:36`, `:102-109`, `:119`; `src/utils/cleanup.ts:33`, `:48-62`; guard at `src-tauri/src/cleanup.rs:314-322`

`selected` initialises to `{}` (`:20`). The card renders `keepPath={selected[g.hash] ?? g.files[0]?.path}` (`:119`), so the UI paints `files[0]` with a green highlight and a "Kept" badge. The action builder `buildDuplicateActions` (`utils/cleanup.ts:48-62`) reads `const keepPath = keptByGroup[g.hash]`, which is `undefined` for every group, so `f.path !== keepPath` is true for every file and **every file in every group is pushed to `trash_duplicate`**, including the one displayed as kept. `keepPath: undefined` is dropped by `JSON.stringify`, so the Rust `Option<String>` is `None` and the keeper guard at `cleanup.rs:314-322` never engages.

The confirm button count is computed the same way, so it honestly reads "Confirm (6)" for three 2-file groups, and the header `reclaimable` figure (`utils/cleanup.ts:33`) also assumes `files[0]` survives. The display, the reclaimable number and the action disagree. The user must click a radio in every group before confirming, or each group is wiped entirely.

This is committed, tracked-and-clean code (`src/components/cleanup/**`), not part of the in-flight dashboard refactor, so it is a settled defect rather than work in progress. Confidence: **high**; the behaviour is fully determined by the source and needs no runtime.

### 17.3 CRITICAL - C19: all 51 commands execute on the Tauri main thread, worst case ~77 s freeze

**Where:** `src-tauri/src/commands.rs` (whole file), worst case `src-tauri/src/classify.rs:591`, `classify.rs:444-445`, `classify.rs:489`; the only correct case `commands.rs:543`

Tauri v2 documentation, quoted verbatim in the report: *"Async commands are executed on a separate async task using `async_runtime::spawn`. Commands without the `_async_` keyword are executed on the main thread unless defined with `#[tauri::command(async)]`."* Mechanical enumeration of `commands.rs` yields `total #[tauri::command] = 51`, `total async fn commands = 0`, `total command(async) = 0`. Every one is a bare `pub fn` on the main thread.

Refines the already-recorded "all commands are sync" finding: the sync-ness is not a style issue, it is a freeze. Worst cases, from source:

| Command | Main-thread work | Worst case |
|---|---|---|
| `get_suggestions_cmd` `commands.rs:673` | `detect_provider()` probe plus up to 5 sequential Ollama round-trips | **~2 s + 5 x 15 s = ~77 s** |
| `find_duplicates_cmd` `commands.rs:626` | hashes every same-size file, serially | minutes on a large tree |
| `import_archive_cmd` `commands.rs:300` | full zip/tar extract plus full rules scan | tens of seconds |
| `find_empty_dirs_cmd` `commands.rs:652` | recursive walk of every watched root | seconds |
| `execute_cleanup_cmd` `commands.rs:661` | Recycle Bin shell op plus audit INSERT per file | scales with selection |
| `undo_all_cmd` `commands.rs:224` | per-log file move plus autocommit UPDATE | scales with log count |
| `get_dashboard_stats_cmd` `commands.rs:582` | 6 SQL queries, ~5-6 full passes over `file_inventory` | see 17.27 |

`start_scan_cmd` (`commands.rs:543`) is the one command that gets it right: it `std::thread::spawn`s and returns immediately. D6's mandate ("`ureq` blocking in a spawned std thread") is therefore violated by `get_suggestions_cmd`, which spawns nothing.

The compounding cross-module failure, which no single-module reviewer owns: `src/pages/Suggestions.tsx:80` registers `window.addEventListener("focus", handleFocus)` to `loadSuggestions()` to `get_suggestions_cmd`. Alt-tabbing to the Suggestions window while Ollama is slow hangs the whole app for up to ~77 s. Fix is mechanical: `#[tauri::command(async)]` on roughly 10 heavy commands, or `async fn` plus `spawn_blocking` on the body.

### 17.4 HIGH - H37: the `app` window holds zero Tauri permissions; all event wiring in the workspace window is dead

**Where:** `src-tauri/capabilities/default.json:5`; `src-tauri/src/tray.rs:171`, `:178`, `:181`, `:184-185`; `src-tauri/src/lib.rs:76`, `:131-133`, `:136-141`; dead listeners at `src/pages/Dashboard.tsx:52-58`, `src/store/useDashboardStore.ts:200`, `:207`, `src/App.tsx:63`

`capabilities/default.json:5` declares `["main", "popup", "settings", "dashboard", "cleanup", "suggestions"]`. Four of those six labels (`settings`, `dashboard`, `cleanup`, `suggestions`) are **never created by any code path in the repo**; every `WebviewWindowBuilder` and `get_webview_window` call was grepped. The label actually created and reused by the tray is `app`. `"app"` appears in exactly two places, `tray.rs:171` and `tray.rs:178`.

Refines the already-recorded ACL finding with the precise mechanism: `plugin:event|listen` is denied while all app commands still work, because app-defined commands bypass the ACL when no `src-tauri/permissions/` directory exists and `build.rs` is the bare three-line `tauri_build::build()` with no `AppManifest::commands`. Verified: `src-tauri/permissions` is absent and there is no app manifest. So the page renders, data loads, and only the events are dead, which is why the failure survives any check that stops at the command name.

Exact failure strings, now documented rather than inferred: **release** `Command plugin:event|listen not allowed by ACL`; **dev** `event.listen not allowed. Permissions associated with this command: allow-listen, default`. Consequences: the dashboard progress bar (`:143-155`) can never appear, `scheduleRefreshAfterScan` (`useDashboardStore.ts:190-197`) never fires so the dashboard never auto-refreshes after a scan, and `file-organized` (`App.tsx:63`) never fires in the workspace window. Both `await listen(...)` chains lack `.catch()`, so the rejections are unhandled and invisible with no devtools in a release webview.

No escape hatch via `webviews`: there is exactly one capability file and it has no `webviews` key, and `WebviewWindowBuilder::new` sets the webview label equal to the window label, so the webview label is also `app`. Zero permissions, unambiguously.

Two facts for whoever fixes it. `"windows": ["*"]` is valid glob syntax and would have made this failure class structurally impossible. The fix is not purely additive: adding a `src-tauri/permissions/` dir or an `AppManifest` would stop app commands bypassing the ACL, at which point the `app` window loses all commands, not just the listeners. The capability fix and the app-ACL decision must be made together.

### 17.5 HIGH - H38: no SQLite `PRAGMA` of any kind; ~2,000-3,000 fsyncs per scan

**Where:** `src-tauri/src/db.rs:119-290` (`init_db`); the only `PRAGMA` in the whole crate is `PRAGMA table_info(settings)` at `db.rs:242`

| Pragma | Current value | Consequence |
|---|---|---|
| `journal_mode` | `DELETE` (default) | every commit creates, writes and deletes a rollback journal file |
| `synchronous` | `FULL` (default) | **~2-3 fsync per commit** |
| `busy_timeout` | `0` | lock contention returns `SQLITE_BUSY` immediately; swallowed by `let _` at `cleanup.rs:119` and `commands.rs:762` |
| `cache_size` | `-2000` (~2 MB default) | undersized for a multi-hundred-MB inventory |
| `mmap_size` | `0` | every page read is a `pread` syscall |
| `temp_store` | `DEFAULT` (file-backed) | `GROUP BY` / `ORDER BY` spills hit disk |

The maths: 500k files in 500-row batches is 1,000 commits, and rollback-journal plus `synchronous=FULL` gives **~2,000-3,000 fsync per scan**. At a static-estimated 1 ms per fsync that is ~2-3 s of pure forced flush; at 5-10 ms (slower controller, or the app directory on a synchronised volume) it is 10-30 s. The journal `delete` also churns the directory MFT on the same volume being scanned. Three lines in `init_db` fix the bulk: `journal_mode=WAL`, `synchronous=NORMAL`, `busy_timeout=5000`. `busy_timeout` is a correctness fix as much as a performance one. Optional on the same lines: `cache_size = -20000` and `mmap_size = 268435456`, both pure reads, and 17.27 shows the dashboard is read-heavy.

### 17.6 HIGH - H39: the two hottest dashboard queries degrade to full scan plus full sort of N rows to return 50

**Where:** `src-tauri/src/db.rs:206` (`idx_file_inventory_size`), `db.rs:214` (`idx_file_inventory_mtime`), `db.rs:940` (`ORDER BY size DESC, path ASC`)

`get_inventory_files_on` orders by `(size DESC, path ASC)` against an index on `size` **alone**. Because the index carries no `path` column, SQLite cannot produce rows in that order by scanning the index and must use a sorter, so the two "largest/recent files, limit 50" queries, which are the most frequent queries in the app, become a full table scan plus a full sort of N rows to return 50. Fix is one line each: make the indexes composite, `CREATE INDEX ... ON file_inventory(size, path)` and `(mtime, path)`. This converts a scan-and-sort of 500k rows into a bounded index range scan reading 50 entries, and costs nothing at write time.

### 17.7 HIGH - H40: the 500 ms watcher loop is 120 wakeups per minute forever, with ~300,000 `PathBuf` clones per post-download window

**Where:** `src-tauri/src/watcher.rs:54-66`, `:78-82`, `:240`; `src-tauri/src/rules.rs:207`; default at `src-tauri/src/db.rs:249`

```rust
loop {
    std::thread::sleep(Duration::from_millis(500));   // watcher.rs:56
    let ready: Vec<_> = guard.iter().filter(|p| now >= p.scheduled).cloned().collect();
    guard.retain(|p| now < p.scheduled);              // watcher.rs:63-66
```

The loop never exits and never idles longer: 120 wakeups per minute for the entire life of the app. The cost is not the wakeup but two O(P) passes over the pending queue per tick, where P is the number of files dropped inside the grace period. `grace_period_seconds` defaults to **300 s** (`db.rs:249`), so after a burst of 500 downloaded files P is ~500 for five minutes, which is 600 ticks, and `ready = ...cloned()` performs **~500 `PathBuf` heap clones per tick, ~300,000 allocations per 5-minute window**, plus ~300,000 more `PathBuf` comparisons in `retain`. That is sustained user-visible CPU for five minutes after any ordinary download session.

Separately, `watcher.rs:78-82` calls `get_watched_folders()` (`db.rs:413`: `get_db()` + mutex + `conn.prepare` + full `SELECT` + materialise) **once per file processed**, and `process_file` (`rules.rs:207`) calls `get_settings()` again. Each organised file costs at least two redundant DB round-trips readable once per batch tick. And `watcher.rs:240` calls `get_settings()` per file inside the startup scan, which itself runs on the main thread inside `.setup()` (`lib.rs:154-158` to `watch_folders`), so it blocks app launch for a folder that routinely holds hundreds of files.

Net idle tally across the app: watcher 120/min always, popup 20/min after first open forever, scheduler 1/min always, plus three undebounced `focus` handlers of increasing weight. **~141 unconditional wakeups per minute**, with zero debouncing anywhere in the codebase except the 200 ms search debounce at `FileBrowser.tsx:57-60`.

### 17.8 HIGH - H41: unbounded cleanup queries plus no virtualisation, from a user-typable threshold

**Where:** `src-tauri/src/db.rs:1118` (`get_large_files_from_inventory`), `db.rs:1136` (`get_stale_files_from_inventory`), `src-tauri/src/cleanup.rs:288` (`walk_empty_dirs`), `cleanup.rs:153` (`find_duplicates`); rendered at `src/components/cleanup/FileListTab.tsx:172`, `DuplicatesTab.tsx:115`, `DuplicatesTab.tsx:156`, `EmptyDirsTab.tsx:117`; threshold input at `FileListTab.tsx:123`

The dashboard FileBrowser is correctly capped (see 17.40). The Cleanup tabs are not capped anywhere: `WHERE size >= ?1 ORDER BY size DESC` and `WHERE mtime < ?1 ORDER BY mtime ASC` have no `LIMIT`, `walk_empty_dirs` is an unbounded recursive walk, and `find_duplicates` returns every group and every file in each. The frontend then renders all of it with `data.map(...)` and no virtualisation anywhere in the project (grep for `virtual`, `react-window`, `react-virtual`: zero hits).

Failure mode: set the "large files" threshold to 1 MB on a media library and you get ~30,000 rows. One `invoke` returns roughly 3.6 MB of JSON at ~120 bytes per path, the webview `JSON.parse`s it, then React mounts 30,000 rows. The scroll container is bounded (`max-h-80 overflow-auto`, `FileListTab.tsx:171`); the DOM is not. Two reviewers would file this twice and neither would see it as one bug.

### 17.9 HIGH - H42: ~9 stat-class syscalls and 3 full `.mouziignore` reads per file in the watcher path, against 1 in the scanner

**Where:** `src-tauri/src/watcher.rs:263`, `:75`, `:186`, `:184`, `:194`; `src-tauri/src/rules.rs:12`, `:66`, `:86-94`, `:97`, `:100`, `:219`, `:262`; `src-tauri/src/ignore.rs:5-36`, `ignore.rs:7`, `ignore.rs:10`; correct pattern at `src-tauri/src/scan.rs:166`

Both paths exist in the codebase, and they differ by roughly 9x. The scanner is at multiplier 1.0x (see 17.15). The watcher and rules path costs, per file: `path.is_file()` (1 stat), `should_ignore_file` `fs::metadata` (1 stat), `is_file_ignored_by_mouziignore` `path.exists()` (1 stat) then `fs::read_to_string` (open + read + close = 3), `exists()` + `is_file()` again (2 stats), `is_file_locked` exclusive `OpenOptions::open` (1 open + close), `is_file_ignored_by_mouziignore` **again** (4), `scan_file` (2 stats), `new_path.exists()` (1), `create_dir_all` on an existing dir (1), `fs::rename` (1).

The headline is the `.mouziignore` re-read. `is_file_ignored_by_mouziignore` (`rules.rs:86-94`) calls `load_mouziignore` on every invocation, and `load_mouziignore` (`ignore.rs:5-36`) does an `exists()` then a full `read_to_string`. It is invoked at `watcher.rs:213`, `watcher.rs:265`, `rules.rs:219` and `rules.rs:262`, so **the same file's `.mouziignore` is opened and fully read 2-3 times per organise cycle, per file**. `scan.rs:166` shows the correct pattern: load once per root and pass `&patterns` down. Fix is to cache `load_mouziignore` per parent directory behind the existing `Arc<Mutex<...>>` state, invalidated on the existing file-change event.

### 17.10 HIGH - H43: the popup polls every 3 s forever after the first open

**Where:** `src/pages/Popup.tsx:91-93`; `src-tauri/src/tray.rs:70-72`; `src-tauri/src/commands.rs:433`; `src-tauri/src/watcher.rs:348`; `src-tauri/src/i18n.rs:8`

```tsx
const interval = setInterval(() => { getPendingFiles(); }, 3000);
```

`tray.rs:70-72` **reuses** an existing popup window via `show()` and never destroys it. Tauri `hide()` does not suspend a webview, and the `useEffect` cleanup only runs on unmount, so **this poll runs forever after the user opens the popup once**, including while the window is hidden and while the user is in a different window. 20 IPC round-trips per minute, each costing one `stat` per pending manual file (`manual.retain(|p| Path::new(p).exists())`), a full DB round-trip for `get_settings()` via `tray_lang(app)`, and a fresh 15-entry `HashMap` built from scratch by `TrayI18n::new(lang)` on every call. Fix: gate on `document.visibilityState` and cache `TrayI18n` in a `OnceCell` per language.

### 17.11 HIGH - H44: a rejected `get_settings_cmd` leaves every window on a permanent loading screen

**Where:** `src/store/useAppStore.ts:119-122` (`loadSettings`, no try/catch), `src/App.tsx:44-49` (`boot()` with no `.catch()`), `src/App.tsx:51-55` (effect returns early so `ready` stays false), `src/App.tsx:102-108` (pulsing "Mouzi..." rendered)

On rejection: an unhandled promise rejection, invisible with no devtools; `settings` stays `null`; `ready` never becomes true; every window renders the pulsing "Mouzi..." splash **forever**. No error text, no retry, no way out except killing a tray app. `initI18n` failing at `:54` has the same terminal effect by a different path. The trigger is conditional (IPC or DB failure) but the code has no guard at all. Confidence: high on the path.

### 17.12 HIGH - H45: "Accept all" is one unconfirmed click, pre-armed to create up to 100 global auto-organise rules

**Where:** `src/pages/Suggestions.tsx:210` (pre-checked checkbox), `:219` (the button), `src/store/useSuggestionsStore.ts:43` (`createRuleDefault: true`), `:50` (`limit: 100`); `src-tauri/src/commands.rs:795-805` (`create_suggestion_rule`); `src-tauri/src/rules.rs:149-152` (`find_matching_rule`)

Three compounding facts. First, the checkbox is pre-checked, so the bulk action is armed to install rules the user never opted into. Second, those rules are **global and enabled**: `create_suggestion_rule` inserts `enabled: true, priority: 10, action: "move", destination: category`, and `folder_id: 0` is **never read by the engine** (`find_matching_rule` matches on `enabled` plus extension/pattern only; `folder_id` appears nowhere in `rules.rs`). One click can therefore install rules that immediately act on new files in **every** watched folder, not just the one on screen. The checkbox label (`en.json:250`) reads "Create a rule from this acceptance", which sounds like a one-file side effect. Third, there is no confirmation dialog and no preview of the N moves, and the query limit is 100, so one click can mean up to 100 file moves plus up to 100 rule insertions. This is the substance of the plan's "always preview plus confirm for destructive or suggested actions" requirement.

### 17.13 HIGH - H46: no ErrorBoundary anywhere in `src/`

**Where:** `src/main.tsx:6-9` (mounts `<App/>` bare)

Grep for `ErrorBoundary`, `componentDidCatch`, `window.onerror` returns nothing. Any render-time throw in any page unmounts the whole tree and leaves a blank white window with no message. In a desktop app with no devtools, every silent-`catch` path above terminates in either a blank window or an infinite spinner with nothing on screen. Confidence: high.

### 17.14 HIGH - H47: decision D3 was not implemented as written, and the substitution is what breaks the ACL

**Where:** `src-tauri/src/tray.rs:184-185` (1100x820 window labelled `app`), `tray.rs:81` (popup 300x420), `tray.rs:171-181`; D3 required a 1024x768 window labelled `dashboard`

This corrects any earlier reading that the dashboard is a separate window with its own capability entry. D3 mandates a 1024x768 window labelled `dashboard`. Shipped instead is one window labelled `app` at 1100x820 multiplexing dashboard, cleanup, suggestions and settings by hash, with the popup remaining a 300x420 flyout. The design is arguably better, but it is a deviation from a locked decision, and the two designs are mutually exclusive: one window cannot both *be* the dashboard and host four routes. The capability list was updated to add `dashboard` while the code kept creating `app`, which is the mechanical cause of 17.4.

The second half of D3, the settings link, does not exist: grepping `Settings.tsx` for `navigateHash|show_dashboard|dashboard` returns zero matches, and its only exit is `invoke("close_settings")` at `:327`. The tray menu item exists (`tray.rs:14`, `:31-33`) and is translated in all locales. `show_dashboard_cmd`, `show_cleanup_cmd` and `show_suggestions_cmd` do not exist; only `show_popup_cmd` (`commands.rs:428`) is registered (`lib.rs:197`). The frontend therefore cannot ask Rust to open the workspace window at all, only the tray can, and adding the link is impossible without a new command.

### 17.15 HIGH - H48: `csp: null` with the app rendering attacker-controlled filenames, directly upstream of C17

**Where:** `src-tauri/tauri.conf.json:25`; `tauri-utils-2.9.1/src/config.rs:2896-2901`, `:2911-2924`, `:2921-2922`; `tauri.conf.json:13`

```json
"security": { "csp": null }
```

`null` deserialises to `None`, so **no CSP is injected, in dev or in the built app**. `dangerous_disable_asset_csp_modification` defaults to off but is moot with no policy to modify. Tauri's own doc comment warns: *"Your application might be vulnerable to XSS attacks without this Tauri protection."*

The agent rated this Medium. This addendum raises it to High on the evidence of 17.1: the app displays attacker-controlled filenames in the dashboard treemap, file browser, cleanup lists and rule hits, and C17 is a code-execution sink reached by clicking one of those filenames. React's default escaping means the CSP gap is not independently exploitable through the UI as read, so this is defence in depth rather than a live XSS, but there is **no defence in depth behind React**, and the practical consequence of an XSS in this app is command execution, not a defaced page. A workable policy is close to free given Tailwind: `default-src 'self'; img-src 'self' data:; style-src 'self' 'unsafe-inline'`, plus `devCsp` so HMR does not break.

Compounding: `withGlobalTauri: true` (`tauri.conf.json:13`) with **zero consumers**. Grep of all of `src/` for `__TAURI__` returns no matches; the frontend exclusively uses `@tauri-apps/api` module imports. The global-injection mode attaches the entire Tauri API to `window` in all 6 windows, widening post-XSS surface for no benefit and inhibiting tree-shaking. Set to `false`.

### 17.16 MEDIUM - H49: `ureq` compiles a full rustls stack that is 100% dead

**Where:** `src-tauri/Cargo.toml:38`; `src-tauri/src/classify.rs:447`, `classify.rs:495`; vendored `ureq-2.12.1/Cargo.toml` (`default = ["tls", "gzip"]`, `tls = ["dep:webpki-roots", "dep:rustls", "dep:rustls-pki-types"]`)

Both and only both `ureq` call sites are plaintext loopback: `http://localhost:11434/api/tags` and `/api/generate`. Reverse-resolving every dependency edge in `Cargo.lock` found `ureq 2.12.1` is the **only crate in the 486-crate graph that requires `rustls`**; nothing else reaches it, and `reqwest 0.13.3` (pulled in by Tauri) has no TLS backend enabled at all. Nine package entries exist solely because of the default `tls` feature: `rustls 0.23.43`, `webpki-roots 0.26.11` and `1.0.9` (a dupe pair, both compile), `rustls-webpki 0.103.15`, `rustls-pki-types 1.15.1`, `ring 0.17.14` (C/asm with a `cc` build script), `untrusted 0.9.0`, `subtle 2.6.1`, `zeroize 1.9.0`.

One-line fix: `ureq = { version = "2", default-features = false }`. The `gzip` default is free, since `flate2` is already a direct dependency (`Cargo.toml:33`, used in `archive.rs`). Binary-size impact is an **estimate, not measured** (no build was permitted): roughly 2-3.5 MB of release binary on x86_64 MSVC, plus a C toolchain requirement in the build graph. Marked **UNVERIFIED** for the precise figure. Severity Medium rather than higher: no TLS handshake is ever attempted, so there is no exploitable path today, but it is unnecessary attack surface and it silently makes the crate *require* TLS the moment anyone points the Ollama adapter at a non-loopback host.

### 17.17 MEDIUM - H50: the Rust toolchain floats; Node disagrees with the plan; pnpm and npm are both in play

**Where:** absent `rust-toolchain.toml`, `rust-toolchain`, and `src-tauri/rust-toolchain.toml` (all three checked); absent `.nvmrc`, `.node-version`, `engines`, `packageManager`; `package-lock.json` committed (111,871 bytes), no `pnpm-lock.yaml`

The plan states Rust 1.98.0 as a prerequisite and nothing in the repo enforces it. `Cargo.lock` v4 pins the lockfile format, not the compiler, so a build in six months gets whatever stable is current. Add a two-line `rust-toolchain.toml` with `channel = "1.98.0"`.

The plan declares pnpm 11.23.0. On the audit machine `pnpm -v` returns command not found, `npm -v` is 11.17.0, and the committed lockfile is `package-lock.json`. A pnpm user would generate a second competing lockfile. The plan says Node 22.23.2; installed is v24.19.0, two majors apart, unrecorded and unenforced. Currently benign because every dependency resolves under 24, but nothing would have caught it. Pick one package manager and state it in `packageManager` or the plan.

### 17.18 MEDIUM - H51: 4 npm advisories at CVSS 7.5, all dev-only, all already fixed inside the declared ranges

**Where:** `vite 7.3.3` installed, declared `^7.0.4`; `postcss 8.5.14` installed, declared `^8.5.14`; transitive `browserslist`, `nanoid`, `esbuild`, `@babel/core`, `baseline-browser-mapping`

`npm audit --json`: **7 vulnerabilities, 0 critical, 4 high, 1 moderate, 2 low, zero in a production dependency.** All 19 prod deps are clean; `npm audit --omit=dev` would report 0. The four at CVSS 7.5 with `AV:N/AC:L/PR:N/UI:N/S:U/C:H/I:N/A:N` are `GHSA-fx2h-pf6j-xcff` (vite `server.fs.deny` bypass via Windows alternate paths), `GHSA-r28c-9q8g-f849` (postcss), and two `browserslist` entries at `<=4.28.6`. The two vite ones matter more than their dev-only status suggests because both are **Windows-targeted** (alternate-path deny bypass, and NTLMv2 hash disclosure via UNC path) and the dev loop is vite on `localhost:1420` serving arbitrary watched-folder content. Every fix is already inside the declared range, so `npm update` alone resolves all seven; no manifest edit is required. Severity Medium: dev-machine file disclosure, not app exposure.

Majors sitting behind, worth noting for a fork that plans to persist: `vite 7.3.3 -> 8.3.1`, `vitest 4.1.11 -> 5.0.2`, `@vitejs/plugin-react 4.7.0 -> 6.1.1`, `typescript 5.8.3 -> 7.0.2`, `lucide-react 1.14.0 -> 1.48.0` (34 minors). `typescript` is the only tilde in the manifest (`package.json:35`); everything else is caret.

### 17.19 MEDIUM - H52: `tsc` never type-checks `vite.config.ts`, so the plan's own verification gate does not cover what it claims

**Where:** `tsconfig.json:23-24` (`include: ["src"]`, `references: [{ "path": "./tsconfig.node.json" }]`); `package.json:8` (`"build": "tsc && vite build"`); `vite.config.ts:4`

`tsconfig.node.json` has `composite: true` and `include: ["vite.config.ts"]`, but plain `tsc` does **not** build referenced projects; that needs `tsc -b`. So `vite.config.ts` is never type-checked by `npm run build`, which is the plan's §7 gate. This is why the `@ts-expect-error` at `vite.config.ts:4` has survived: nothing ever looks at that file. It is also a latent time bomb. `@types/node` is absent from both `package.json` and `node_modules`, which is the only reason `process` is untyped and the suppression is needed; adding `@types/node`, a routine thing to do, turns the `@ts-expect-error` into an error itself ("unused directive") and breaks the build. Correct fix: add `@types/node` and delete the suppression. Grep of all of `src/` for `@ts-expect-error|@ts-ignore|as any` returns **zero matches**, so the application code is clean and this is the only violation of the plan's §6 prohibition list.

### 17.20 MEDIUM - H53: irreversibility is disclosed in the wrong place, and the Recycle Bin guarantee is unobservable

**Where:** `src-tauri/src/cleanup.rs:391-394` (writes `undoable: false` for every row, with two identical match arms), `src/store/useCleanupStore.ts:18`, `src/components/cleanup/HistoryPanel.tsx:43`, `en.json:229`, `en.json:219`; `src-tauri/src/safe_fs.rs:99-104`; `cleanup.rs:360`; `cleanup.rs:331-335`

Three related presentational defects. First, `undoable` exists as a DB column and a TS field and is **never read** by `HistoryPanel`, so the history list cannot tell the user that a given file is unrecoverable through either the app or the bin. Second, the one string that discloses irreversibility, `cleanup.historyEmptyDirs` = "Empty folder removals are not undoable." (`en.json:229`), is rendered **only in the History tab's empty state** (`HistoryPanel.tsx:43`), so the user is told precisely when there is nothing to be irreversible about. `EmptyDirsTab` has no consequence statement at all, and `remove_empty_dir` is `fs::remove_dir` (`cleanup.rs:360`), which bypasses the Recycle Bin entirely. The word "Trash" appears in the UI only as a `Trash2` lucide glyph and `cleanup.confirm` is literally "Confirm (N)".

Third, and undercutting all of it: `safe_fs.rs:101-104` is a bare `trash::delete`. On Windows this goes through the shell file-operation API, which **can permanently delete when the bin is unavailable, disabled or over quota while returning success**. The code cannot distinguish that from a genuine bin deposit, and `CleanupOutcome{status:"ok"}` (`cleanup.rs:331-335`) is byte-identical in both cases, so `ResultsPanel`'s green "done: N" is not evidence of recoverability. The doc comment at `safe_fs.rs:99-100`, *"Returns an error if trashing is not supported on the current platform or if the operation fails"*, asserts an error-or-bin dichotomy the crate cannot honour on Windows: a comment that contradicts what the code can deliver. In fairness, the UI **makes no false Recycle Bin claim** (no key in `cleanup.*` or `dashboard.*` mentions one). Confidence: high on the code; the specific Windows fallback conditions are **UNVERIFIED** (not executed).

### 17.21 MEDIUM - H54: scan state can wedge at "Scanning...", and a failed root emits no event at all

**Where:** `src/store/useDashboardStore.ts:191`, `:215`; `src/pages/Dashboard.tsx:133-138`; `src-tauri/src/scan.rs:193`, `scan.rs:167-170`; `commands.rs:552`

`useDashboardStore.ts:191` is the only place `isScanning: false` is set after a scan starts, and it is reached only if the `is_scanning_cmd` round-trip at `:215` resolves **after** the Rust thread's final `is_scanning.store(false)`. The final `scan-complete` is emitted from inside `scan_roots` (`scan.rs:193`), strictly before that store, so the outcome depends on IPC thread-pool scheduling. Miss it and the button stays disabled showing "Scanning..." with no timeout, no poll and no error. Separately, a root whose `clear_inventory_for_root` fails is `continue`d (`scan.rs:167-170`) **without emitting any event**, so `completedRoots` never reaches `rootCount` and the bar reads e.g. "1 / 3 folders" forever. Confidence: high on the code path, medium on triggering.

Related: `scan.rs:167` clears a root's inventory **before** walking it, so any `refresh()` during a scan, such as the focus handler, shows a half-empty dashboard that looks like data loss. There is no scanning-state suppression of the stats view.

### 17.22 MEDIUM - H55: silent error states on three pages that the user cannot distinguish from empty results

**Where:** `src/store/useCleanupStore.ts:111-121` (`loadHistory`, no loading flag, empty `catch {}`), `src/components/cleanup/HistoryPanel.tsx:24`, `:40-45`; `src-tauri/src/commands.rs:677`; `src-tauri/src/classify.rs:568-570`; `src/pages/App.tsx:63` and `Dashboard.tsx:52-58` (missing `.catch()`)

Three distinct presentations of the same class of defect. A failed history query is indistinguishable from an empty history: `loadHistory` has no `loading` flag and swallows its error in an empty `catch {}`, `history` stays `null`, `HistoryPanel.tsx:24` does `history ?? []` and `:40-45` renders `historyEmpty`, so there is no spinner, no error and no retry affordance beyond the Refresh button. A backend error on the suggestions path is invisible: `get_suggestions_cmd` returns `Ok(...)` unconditionally (`commands.rs:677`) and `classify.rs:568-570` does `Err(_) => return Vec::new()`, so a DB failure is presented as "No suggestions right now." And in the `app` window the two event-listener rejections (17.4) are unhandled with no devtools to surface them. Confidence: high on all three.

### 17.23 MEDIUM - H56: suggestion rules are silently not created in the common case, with no field to report it

**Where:** `src-tauri/src/commands.rs:781-793` (dedup early return), `commands.rs:680-687` (`AcceptOutcome` shape), `src-tauri/src/scan.rs:44-59`, `src-tauri/src/db.rs:333`; checkbox at `src/pages/Suggestions.tsx:210`

`create_suggestion_rule`'s dedup check skips insertion when an enabled `move` rule already has the same destination and covers the extension. Concrete: `scan.rs:44-59` classifies `.exe` as `"Other"` so it is a suggestion candidate, the heuristic suggests `"Installers"`, and the default rule at `db.rs:333` is `("Installers", 4, ["exe","msi",...], "Installers")`, so destination matches and extension is covered, producing an early return with no rule and no message. The user watches a pre-checked box do nothing, silently. `AcceptOutcome` (`commands.rs:680-687`) has only `{path, status, message, dest}`, with no field for whether a rule was created, so no UI change can fix this without a backend change. Confidence: high.

### 17.24 MEDIUM - H57: suggestion-driven moves are not suppressed for the watcher

**Where:** `src-tauri/src/commands.rs:698-702` (`accept_suggestion_cmd`), `commands.rs:176-178` (`perform_undo` inserts into `AppState.ignored_files`)

`perform_undo` deliberately inserts both paths into `AppState.ignored_files` so the watcher does not re-trigger on a restore. `accept_suggestion_cmd` takes neither `AppHandle` nor `State`, so it **structurally cannot** register the suppression, and it moves real files inside a watched root with the guard wide open. The asymmetry reads as an oversight rather than a decision. Confidence: medium (the watcher path was traced but not executed).

### 17.25 MEDIUM - H58: an unrecognised hash silently renders the 300x420 popup flyout at 1100x820

**Where:** `src/App.tsx:120-121` (fallthrough to `<Popup/>`), `src/utils/paths.ts:13-26`, `paths.ts:17`; test gap at `src/__tests__/dashboard.test.ts:22-49`

`App.tsx:120-121` falls through to `<Popup/>` for any unrecognised hash. In the 1100x820 workspace window a typo renders the compact popup flyout stretched to full window size: no error, no console, no devtools, invisible by construction. Triggers are case sensitivity (`#/Dashboard`), a trailing slash (`#/dashboard/`), a stale fragment, or any `navigateHash` typo. `parseHash` has no allowlist and `paths.ts:17` defaults only when the hash is empty. `dashboard.test.ts:22-49` tests empty, `?`-query, no-query, `#/#/`, and extra keys but has **no case for an unknown route**, so nothing pins the fallthrough.

Related: `InsightCards.tsx:8` types `onOpen` as `"large" | "stale" | "duplicates" | "suggestions"`, hand-duplicated against `tabFromHash`'s union at `Cleanup.tsx:15` with no shared type, and `tabFromHash` silently defaults to `"duplicates"` for an unrecognised tab. Latent, not live: the day the two lists drift, an insight card opens the Duplicates tab with no error.

### 17.26 MEDIUM - H59: `set_cached_hash` is one autocommitted fsync-bearing transaction per file

**Where:** `src-tauri/src/db.rs:1067-1074`; `db.rs:1054` (`conn.prepare` re-compiled per call); `db.rs:236` (PK), `db.rs:1055`

The strategy is right and the mechanics are not. `hash_cache` has `PRIMARY KEY (path, size, mtime)` and `get_cached_hash` filters on all three columns, a perfect O(log n) index probe; D7's key design works and does hit on rescan, proven by the test at `cleanup.rs:590-641` (mtime-change invalidation). But `get_cached_hash` calls `conn.prepare(...)` on **every single call**, a full `sqlite3_prepare_v2` parse and codegen per file, so 100k same-size files means 100k re-compiles of identical SQL. The writes are worse: `set_cached_hash` is `conn.execute(...)` in **autocommit**, one implicit transaction per row, each paying the `synchronous=FULL` plus rollback-journal cost from 17.5. **100k cache writes is 100k fsync-bearing transactions**, a worse write-amplification defect than the inventory batch, which at least batches 500 at a time. Concurrency is nil: no `rayon`, no `thread::scope`, no `available_parallelism` anywhere in the codebase (zero hits), only 4 fixed `thread::spawn` sites (`watcher.rs:54`, `watcher.rs:146`, `scheduler.rs:36`, `commands.rs:543`). Hashing is serial, single-threaded and on the main thread.

Fixes in value order: hoist the `prepare` into a cached `Statement`; batch `set_cached_hash` into the same 500-row transaction as the inventory, or at minimum wrap the dedup pass's cache writes in one transaction; size a hashing pool from `available_parallelism()`, since blake3 is pure CPU and embarrassingly parallel.

### 17.27 MEDIUM - H60: ~6-7 full passes over `file_inventory` per dashboard refresh, undebounced, on every focus

**Where:** `src-tauri/src/commands.rs:582-601`; `db.rs:790`, `db.rs:850-884`, `db.rs:820`, `db.rs:836`, `db.rs:892`, `db.rs:999-1033`; `src/pages/Dashboard.tsx:71`

`get_dashboard_stats_cmd` issues 6 SQL statements, each taking the global mutex separately, so 6 lock/unlock cycles. Five of them are full passes over the whole table: `COUNT(*)`/`SUM`/`MAX` (full scan), four inside `get_insights_on` (two range scans, a `GROUP BY size` full scan plus temp B-tree, and a `category='Other'` count), and `GROUP BY` in each of `get_category_distribution_on`, `get_root_summaries_on` and `get_age_buckets_on`. For 500k rows at ~60 bytes that is ~30 MB of page reads per pass, **~180-210 MB of page reads per refresh** (static estimate). It fires on every window focus, every refresh button, every `R` keypress and every `scan-complete`, with no debounce.

Credit where due: the **payload** is well bounded. 50 `InventoryFile` per list, capped at 50, plus 7 categories, ~5 roots and 5 age buckets, at ~120 bytes per absolute Windows path in JSON, so the two file lists are ~12 KB. The "50,000 path strings to render a summary" failure mode does not exist in the dashboard.

### 17.28 MEDIUM - H61: no store selectors on five of six page hooks and no `React.memo` anywhere, so ~20,000 renders per 500k-file scan

**Where:** `src/pages/Dashboard.tsx:29`, `src/pages/Popup.tsx:42`, `src/components/cleanup/DuplicatesTab.tsx:19`, `FileListTab.tsx:30`, `EmptyDirsTab.tsx:9`, `src/pages/Suggestions.tsx:52`; only `src/pages/Cleanup.tsx:30` uses a selector; `src/store/useDashboardStore.ts:200-205`; `src/components/dashboard/FileBrowser.tsx:64`, `Dashboard.tsx:268-269`

In Zustand v5 calling `useXStore()` with no selector subscribes to the entire state object, so every `set()` re-renders the whole page subtree. The codebase is internally inconsistent: only `Cleanup.tsx:30` does it right. The compounding chain: `scan-progress` fires every 200 files (`scan.rs:14`, `:107`), `setState` creates a new state object, so `Dashboard` re-renders and all 8 children re-render, since no component uses `React.memo` (grep: zero hits). A 500k-file scan emits 2,500 progress events, hence **~20,000 component renders during one scan**, against a value that feeds one text node. Also `FileBrowser.tsx:64` lists `initialLargest`/`initialRecent` in its dependency array, and `stats` is a new object with new array identities on every `refresh()`, so the memo boundary is inert. `completedRoots` at `useDashboardStore.ts:210-212` is O(n²) but negligible at 1-5 roots.

### 17.29 MEDIUM - H62: the `category` index has 7 distinct values and a TEXT key, and buys nothing

**Where:** `src-tauri/src/db.rs:210`; consumers `db.rs:820`, `db.rs:871`, `db.rs:1186`; table `db.rs:190-200`, indexes `db.rs:201-216`

`idx_file_inventory_category` is the only index on the table with a **TEXT** key, so it pays the widest key comparisons and the most page splits, and it costs one b-tree write per inserted row. It serves `GROUP BY category` over exactly 7 groups and `WHERE category='Other'`, both of which a full scan plus a 7-row temp B-tree handles more cheaply. It is the clearest waste in the index set; dropping it removes roughly 17% of the per-row write cost for essentially zero query regression. `idx_file_inventory_root` is borderline for the same reason (1-5 distinct values, used only by `DELETE ... WHERE root_path=?` at `db.rs:995`); measure before removing. **UNVERIFIED** whether SQLite's planner actually chooses the category index for the `GROUP BY` queries or silently prefers a full scan; an `EXPLAIN QUERY PLAN` would settle it and no database was opened.

Net for reference: 5 index b-trees plus the table b-tree equals **6 b-tree writes per scanned file**.

### 17.30 MEDIUM - H63: archive staging directories are never deleted on success, so disk growth is unbounded

**Where:** `src-tauri/src/archive.rs:35-73` (staging dir creation), `archive.rs:29` (removal, error path only), `src-tauri/src/commands.rs:300-311`, `src-tauri/src/rules.rs:179`

`archive.rs:35-73` creates `cache_dir/archive-imports/import-{pid}-{millis}-{attempt}` and the tree is removed **only on the error path** (`archive.rs:29`). On success, `import_archive_cmd` runs `manual_scan_folder` on the staging dir, which *moves* files into `<staging>/<Category>/` subfolders and never deletes the staging dir. Confirmed by grep: no `remove_dir_all` on the staging path outside `archive.rs:29`. Every archive import permanently leaves a fully populated, sorted copy of the archive on disk, so growth in the user's cache directory is unbounded over a year of imports.

### 17.31 MEDIUM - H64: `collision_safe_path` is O(n squared) with a stat syscall per probe

**Where:** `src-tauri/src/archive.rs:214-239`

`collision_safe_path` linearly probes `dir.join("name (i).ext")` calling `candidate.exists()` per candidate and restarting the counter from 1 for every colliding entry. An archive with *n* same-named entries costs `1+2+...+n ≈ n²/2` `exists()` syscalls; a 5,000-file photo export with duplicate names gives ~12.5 M stat calls on the main thread. Niche trigger, genuine complexity defect.

### 17.32 MEDIUM - H65: `start-mouzi.bat`, the double-clickable launcher, is untracked

**Where:** `start-mouzi.bat` (untracked, 45 bytes); `package.json:7-12`; `tauri.conf.json:7`; `src-tauri/tauri.conf.json:31`

`start-mouzi.bat` contains `@echo off`, `cd /d "%~dp0"`, `npm run tauri -- dev`, which is the correct command and `cd /d "%~dp0"` makes it location-independent. But `git ls-files` does not list it, so the one artifact that makes the app double-clickable, the primary deliverable of a personal-use desktop app, will not survive a fresh clone. Forks are supposed to be hygienic about exactly this. Commit it or note in the README that it is intentionally local. It is also a third spelling of the same command alongside `npm start` and `npm run tauri dev`.

Related script-naming trap, same severity class: `package.json:7` `"dev": "vite"` starts Vite alone, so the page loads, React mounts and **every `invoke()` fails** because Tauri injects no IPC bridge into a plain browser tab; all 65 `invoke()` sites across 13 frontend files break silently. `package.json:11` `"start": "tauri dev"` is the only script that produces a working app. The inversion compounds, because `tauri.conf.json:7` sets `beforeDevCommand: "npm run dev"`, so `npm start` to `tauri dev` to Vite. The naming inverts the universal convention and neither the README nor the plan documents it. **UNVERIFIED** whether the README documents `npm run tauri dev` prominently, since the README was out of that agent's scope. Fix is to rename `start` and put a guard on `dev`.

### 17.33 MEDIUM - H66: `bundle.targets: "all"` requires WiX for MSI, an undocumented clean-machine blocker

**Where:** `src-tauri/tauri.conf.json:31`; `tauri.conf.json:44-46`, `:36`

`targets: "all"` on a Windows-only, unsigned, no-store personal build will also attempt MSI, which needs the WiX toolset. WiX is not part of VS Build Tools and is not documented in the plan, so a clean machine following the plan exactly hits a `tauri build` failure here. **UNVERIFIED** (no `tauri build` was run). Fix: `"targets": ["nsis"]`. The macOS block (`infoPlist: "Info.plist"`, `icon.icns` in `bundle.icon`) is configuration for a platform the app can never target; the files exist so it will not error, but it can never be exercised.

### 17.34 MEDIUM - H67: `website/` is 14.9 MB of Astro marketing site, fully tracked, including signing docs that contradict D1

**Where:** `website/` (92 tracked files, 14.9 MB), `website/AnalyticsConsent.astro` (hardcoded Google Analytics ID `G-GMDTSE5DFS`), `website/building-a-trusted-release-pipeline.mdx`, `website/docs/0-1-6.mdx`, `website/package-lock.json`

A complete independent Astro project with its own dependencies, blog MDX, docs collections, hero videos and image assets. For a personal-use, no-store, unsigned Windows desktop fork none of it ships, none of it builds, and it carries a second `package-lock.json` that will drift independently. It also contains the **upstream release and signing documentation**, which actively contradicts the fork's no-signing, no-store posture and could mislead a future reader. Deleting it is the largest single hygiene win available and is fully reversible.

Related repo weight: `src-tauri/icons` is 57 tracked files with zero orphans, but roughly **39 of 57 files / ~1.05 MB are unreachable** for a Windows-only unsigned build (19 `android/`, 20 `ios/` including a 162 KB `AppIcon-512@2x.png`, 14 `Square*Logo.png` plus `StoreLogo.png` MSIX assets, and `icon.icns` at 393 KB, the single largest file in the set). Deletion is safe **only** if `icon.icns` is also dropped from `bundle.icon` and the `macOS` block removed, or `tauri build` errors on a missing icon.

And five inherited CI workflows, four of which are dead or contradict D1: `build-macos-test.yml` (no macOS target), `test-appimage.yml` (no AppImage target), `signpath-test.yml` (tests code signing, which D1 excludes), plus `build.yml` and `rebuild-release-assets.yml` only partially relevant. The workflow bodies were not read, so whether they gate anything is **UNVERIFIED**, but `signpath-test.yml` existing at all in a no-signing fork will confuse the next person, and with no `rust-toolchain.toml` and no `.nvmrc` whatever they do is unpinned.

### 17.35 LOW - H68: no `[profile.release]`, so no LTO and `codegen-units = 16`

**Where:** `src-tauri/Cargo.toml:1-41` (entire file, no profile section); `Cargo.toml:10` (`crate-type = ["staticlib", "cdylib", "rlib"]`)

Cargo defaults apply: `opt-level = 3`, `lto = false`, `codegen-units = 16`, `panic = "unwind"`, no strip. For a Tauri app bundling SQLite's C amalgamation, `lto = true` with `codegen-units = 1` is the standard recommendation and typically buys both a smaller binary and measurably faster SQLite hot paths. A 6-line profile block, zero risk.

Build-time only, also flagged: `crate-type` builds the library as a Windows static library and a C dynamic library on every desktop build. Those two exist for mobile; the desktop `mouzi.exe` only needs `rlib`, and building them materially slows `cargo build` and `tauri build`. First build is a long cold compile of 486 crates, which the plan's half-day M0 estimate should account for.

### 17.36 LOW - H69: 106 KB of locales bundled and no route code-splitting

**Where:** `src/i18n/index.ts:3-12`; `src/pages/Settings.tsx` (46 KB source); measured from the committed `dist/`: `assets/index-*.js` **419.5 KB raw / 123.5 KB gzip**, `assets/index-*.css` 48.4 KB / 9.4 KB, `mouzilogo.png` 68.4 KB, `index.html` 0.5 KB / 0.3 KB, total `dist/` **536.6 KB**

All 10 locales are statically imported, 106 KB of JSON measured, bundled for a user who needs exactly one; `i18next` supports lazy loading. Single JS chunk, no code splitting: four route surfaces (`Popup`, `Settings`, `Dashboard`, `Cleanup`, `Suggestions`) all land in one `index-*.js` that every window loads in full, so splitting on route would cut the initial parse for the popup, the surface that opens on every tray click. These are the only figures in this addendum read from actual bytes on disk; everything else is a static estimate.

`lucide-react` is the largest single dependency and is imported per-icon (`Dashboard.tsx:18-25` pulls 8, `Suggestions.tsx:7-18` pulls 10). Modern `lucide-react` tree-shakes per icon, so the real cost is likely modest; the report flags it as a check, not a finding. No charting library: `StorageTreemap` and `CategoryBars` are hand-rolled divs, a genuinely good outcome for bundle size.

### 17.37 LOW - H70: two unused Rust crates, a duplicated `windows` generation, and a pre-`OnceLock` dependency

**Where:** `src-tauri/Cargo.toml:27` (`thiserror`), `:29` (`log`), `:26` (`once_cell`), `:36` (`trash`), `src-tauri/src/db.rs:7`; false-positive locals at `db.rs:453`, `classify.rs:326`, `rules.rs:232`, `safe_fs.rs:239`

`thiserror` is unused: zero `use thiserror` and zero `#[derive(Error)]` across all 15 source files and `build.rs`; the codebase uses `Result<T, String>` throughout. `log` is unused, and this one initially looked used with 18 apparent hits, but every hit is a local variable named `log` of type `ActionLog`. Verified: 0 `log::error!`/`warn!`/`info!`/`debug!`/`trace!` invocations, 0 `use log`, 0 logger initialisation. The crate is linked and never called, and even if it were called nothing would emit, because the missing piece is a logger **initializer**, not the facade. Both are pre-existing upstream leftovers.

`trash 3.3.1` is the **sole** reverse-dependent of `windows 0.44.0` in the entire graph, while Tauri already brings `windows 0.61.3`, so two incompatible `windows` generations compile side by side. It also unconditionally pulls `libc` and `scopeguard`. Not fixable without an upstream bump; **UNVERIFIED** whether trash 3.4 moved to `windows 0.61`. The three-way `dirs-sys` duplication (`0.3.7`, `0.4.1`, `0.5.0`) is **not** from `trash`: `dirs 4.0.0` comes from `auto-launch 0.5.0` (the autostart plugin's Linux backend) and the rest from `directories 5`. Pre-existing, not fork-introduced.

`once_cell` has a single import at `db.rs:7` and 4 references; `std::sync::OnceLock` has been stable since Rust 1.70 and the project is on 1.98.

### 17.38 LOW - H71: lockfile duplication census, caret surface, and the version that was never bumped

**Where:** `Cargo.lock` (5,937 lines, format v4, **486 distinct crates, 43 crate names resolved at more than one version simultaneously**), `src-tauri/Cargo.toml:3`, `src-tauri/tauri.conf.json:4`

`windows-sys` fans out to **6 coexisting versions** (`0.45.0`, `0.48.0`, `0.52.0`, `0.59.0`, `0.60.2`, `0.61.2`), `windows` at 2, `winnow` at 3, `hashbrown` at 4, `toml` at 3, `getrandom` at 3. Not all are real weight, since many are target-gated and never compiled on Windows, but the `windows-sys` fan-out is the one that costs compile time on the target platform.

Structural caret risk, not UNVERIFIED: `rusqlite = "0.32"` and `notify = "7"` are carets on lines that have moved several minors, so a build with a stale `Cargo.lock` deleted, or any `cargo update`, silently moves across minors. The `tar = "=0.4.44"` and `zip = "=0.6.6"` exact pins (`Cargo.toml:34-35`, both with `default-features = false` and only `["deflate"]` on zip) show the author already learned this lesson for the two crates with actual advisories; it was not applied to the other 20 entries, nor to the three crates the elevate work added (`trash`, `blake3`, `ureq`, all carets at `:36-38`). `ureq 2.12.1` is 12 minors in and `trash 3.3.1` is 3 in; both warrant a tilde (`~2.12`, `~3.3`). **UNVERIFIED** whether newer releases exist on crates.io: offline, no registry query, no local `rustsec` advisory data, so **all Rust CVE exposure must be treated as unknown, not as clean**. Nothing in the tree is EOL by its own manifest.

`tauri.conf.json:4` and `Cargo.toml:3` are both still `0.1.6`, the upstream tag, after 14 fork commits, 3 new windows, ~3,100 lines of new Rust and 3 new crates, with no fork marker in the manifest. A built `Mouzi.exe` reports `0.1.6` and is indistinguishable from upstream. Cosmetic for personal use; consider `0.2.0-elevate` plus a description bump. The README fork notice covers attribution, which is the part D1 required.

### 17.39 LOW - H72: smaller mechanical defects

**Where and detail:**

- **Payload shape.** `src/store/useCleanupStore.ts:14` declares `prevPath: string | null`, but `CleanupAction` (`src-tauri/src/db.rs:77-87`) has no `#[serde(rename_all)]`, so it serialises `prev_path` and the field is always `undefined`. Not read by any UI today, so nothing breaks visibly, but the type is a lie and the column is unrecoverable.
- **Wiring mismatch.** `src/pages/Dashboard.tsx:88` invokes `close_settings` from the dashboard window, where `Settings.tsx:327` has the same call correctly. Copy-paste; the dashboard's own close path is likely broken.
- **Duplicate capability path.** `show_notification` (`Popup.tsx:114`) is an app-local command while `notification:allow-show` is also granted. Two paths to the same capability.
- **Unused command.** `is_autostart_enabled_cmd` is registered (`commands.rs:406`, `lib.rs:184`) and never invoked. `useAppStore.setAutostart:132-140` only writes the DB setting and never reads real OS autostart state, so the Settings toggle can silently disagree with Windows.
- **Dead window.** `tauri.conf.json:15-22` declares `main` at 800x600, `visible: false`, loading `/` with no hash, so `parseHash` yields `"popup"` and it renders `<Popup/>`. `lib.rs:76-82` never targets it. It boots, loads settings, and registers a listener for nothing. Cosmetic, but a stray `main` window would show popup UI at 800x600.
- **Title never updates.** Only `show_app_window` sets it (`tray.rs:173`), so clicking "Settings" from the dashboard's empty state (`Dashboard.tsx:169`, `:197`) leaves the title reading "Mouzi Dashboard" above the Settings UI.
- **Cleanup tab state is not in the URL.** The header History button (`Cleanup.tsx:66`) and the four tab buttons (`:80`) call `setTab` without writing the hash, while the hash is the only thing `Cleanup.tsx:14` reads on mount, so a `#/cleanup?tab=large` URL can sit behind a UI showing a different tab.
- **`hasAnyData` includes `history`** (`Cleanup.tsx:30-36`), which `HistoryPanel` sets on mount, so visiting the History tab once suppresses the "Run a scan first" hint on every other tab even if no scan has ever run.
- **Untranslated error strings.** `cleanup.noScanData` is dead in all locales (`en.json:202` plus 9 siblings); the role is played by the backend's hardcoded English `"No scan data - run a scan first"` (`commands.rs:628`, `:637`, `:646`, `:655`). So every cleanup error is untranslated in the non-English locales, and `find_*_cmd` and `execute_cleanup_cmd` return English-only text. Same class: `find_mouziignore` aside, `Suggestion` outcomes carry English-only messages.
- **Locale count.** The plan says 10 locales; `i18n/index.ts:27` `SupportedLang` has 10 members and 10 resource entries, but there are **11** locale files on disk. This predates the fork work, since `i18n/index.ts` is unchanged.
- **Dead constant.** `Dashboard.tsx:221` does `stats!.insights ?? EMPTY_INSIGHTS` while `insights` is a non-optional field of `DashboardStats` (`useDashboardStore.ts:53`), so the fallback is unreachable.
- **Unreachable empty state.** `dashboard.neverScanned` (`en.json:146`) sits behind `hasData` (`Dashboard.tsx:183`, `:213-218`), which requires `totalFiles > 0`, so a never-scanned state never reaches it.
- **`autoprefixer`** (`package.json:32`) is almost certainly dead: Tailwind 4 has a built-in Lightning CSS prefixer and does not use autoprefixer, and `postcss.config.js` only loads `@tailwindcss/postcss`. **UNVERIFIED** (the build was not run to confirm it is not picked up transitively).
- **Accept results erased by focus.** `src/store/useSuggestionsStore.ts:47` sets `results: null` at the top of **every** `loadSuggestions`, combined with the focus handler at `Suggestions.tsx:75-82`, so any window refocus between accepting and reading the summary silently wipes it. During `acceptAll` (up to 100 sequential invokes) a single refocus clears results mid-loop; `applyAccept`'s `results: (state.results ?? []).concat(...)` at `:68` rebuilds from empty, so earlier per-item outcomes vanish from view while the moves still happened.
- **Rename reported as success.** `move_file` returns `MovedWithNewName` for a name clash (`commands.rs:729-737`) and `accept_suggestion_cmd` reports it as plain `status:"ok"` with a suffixed `dest`. The user is told the file was moved, not that it was renamed. Accepts are logged to `action_logs` (`commands.rs:747-756`) so they are revertible via the popup's Undo, but the Suggestions page offers no undo, no link to it and no mention.
- **Dead code comments.** `cleanup.rs:32-34` documents "Total reclaimable bytes if one copy of this group is kept" above `pub files: Vec<DuplicateFile>`, a list of files. `cleanup.rs:391-394` is a `match` with two identical arms that reads as if `remove_empty_dir` were a distinguished case; it is a no-op and is a candidate for `clippy::match_same_arms` under the plan's `-D warnings` gate. `useDashboardStore.ts:184` says "Event listeners (call once on mount)" but the module-level single-slot unlisten fields at `:186-187` make "call once" unenforceable under React StrictMode's double-mount (`main.tsx:7`). Accurate and worth keeping: `tray.rs:140-142` on the `#/#/` hash trap with its test at `:244-250`, `useSuggestionsStore.ts:58-59`, `useDashboardStore.ts:58-59`.

### 17.40 REFUTED - checked and cleared, recorded so it is not re-raised

Each of these was actively investigated and found clean. The convention of this audit is that a refuted claim is as valuable as a defect.

- **Scanner syscall counts are optimal; the suspected `canonicalize` bug does not exist.** The audit brief flagged `read_dir` + `metadata` + `canonicalize` as the suspected per-entry syscall problem. Grep for `canonicalize` across all of `src-tauri/src` returns **zero hits**. `entry.file_type()` (`scan.rs:135`) costs **0 syscalls on Windows**, being derived from the `dwFileAttributes` that `FindFirstFileExW` already returned. Traversal is **exactly 1 metadata query per file plus ~1 per directory, multiplier 1.0x**. `MAX_DEPTH=128` (`scan.rs:17`) bounds recursion. This is correct code; no effort belongs here.
- **Inventory write batching is correct.** 1 INSERT per scanned file, **0 UPDATEs** (`scan.rs:98`, `db.rs:769`); 1 transaction per **500** files (`INVENTORY_BATCH_SIZE = 500` at `db.rs:10`, opened `db.rs:762`, committed `db.rs:772`), so 500k files is 1,000 commits, not 500,000. The global `Mutex<Connection>` is **not** held during the walk (`scan.rs:116-124`, documented `scan.rs:160`). `INSERT OR REPLACE` (`db.rs:765`) is effectively a plain INSERT because `clear_inventory_for_root` runs first (`scan.rs:167`), so the REPLACE fallback costs nothing. Batching is right; only the commit *durability* is wrong (17.5).
- **`FileBrowser` row bounding is correct, and the 50,000-row problem is not there.** The dashboard file browser is capped at 50/80 (`FileBrowser.tsx:48`) and `get_inventory_files_cmd` clamps to `.clamp(1, 200)` (`commands.rs:616`), and `get_dashboard_stats_cmd` caps each list at 50 (`commands.rs:585-586`). The dashboard's IPC payload is ~12 KB, not 50,000 path strings. The unbounded problem exists **only** in the Cleanup tabs (17.8). Do not conflate the two.- **No file is ever read entirely into a `Vec<u8>`.** The largest single buffer in the whole codebase is 64 KB: a stack `[u8; 64*1024]` at `cleanup.rs:76`, a heap `vec![0u8; 65536]` at `cleanup.rs:96`, `archive.rs` using `io::copy` (8 KB internal). The 4 GB-into-RAM scenario does not occur; its cost is sequential read plus CPU, and the sample-first design means it is only paid on a size-and-sample collision (`cleanup.rs:190-202`).
- **No whole tree is held in RAM before insert.** `scan.rs` batches at 500 `InventoryRow` (`scan.rs:104`, `db.rs:10`) and flushes per batch (`scan.rs:116-124`).
- **No resize listeners, no scroll handlers, no `requestAnimationFrame` loop anywhere** (grep: zero hits), and no polling interval in the dashboard. That axis is clean.
- **No dead dependencies in `Cargo.toml`.** All 21 direct Rust dependencies are justified by a feature that exists in the code. The `thiserror` and `log` entries in 17.37 are the exception and are flagged there.
- **No unused npm packages, with one exception.** All 10 `dependencies` and 11 `devDependencies` have live import sites, verified specifically: `@tauri-apps/plugin-dialog` at `Settings.tsx:5`, `@tauri-apps/plugin-notification` at `App.tsx:13`. The exception is `@tauri-apps/plugin-opener`, which has **zero import sites in `src/`** while its Rust half is registered (`lib.rs:42`) and its permissions are granted (`capabilities/default.json:9-10`), which is part of why 17.1 is avoidable.
- **Zero `invoke` name, arity or payload mismatches.** Every one of the 65 `invoke()` sites across 13 frontend files was cross-checked against `commands.rs`: every command string resolves, every arity is correct, and every payload shape is right. `CleanupRequest` (`cleanup.rs:37-44`) carries `#[serde(rename_all="camelCase")]` plus explicit `keepPath`, matching `utils/cleanup.ts:18-22` exactly. The `trash_large`/`trash_stale`/`trash_duplicate`/`remove_empty_dir` strings are raw `String` values (`cleanup.rs:311`) so are not subject to any rename. Scan event payloads (`scan.rs:19-33`) match `ScanProgress`/`ScanComplete` (`useDashboardStore.ts:61-71`). `Suggestion` (`classify.rs:551-560`) matches the TS interface field for field. The two real defects in this area are structural, not naming: the ACL denial (17.4) and `prevPath` (17.39).
- **No auto-apply path exists in Suggestions.** `applyAccept` is reachable from exactly two call sites, `acceptSuggestion` (`useSuggestionsStore.ts:75`, the ✓ button at `Suggestions.tsx:282`) and `acceptAll` (`:103`, the button at `:219`). No `useEffect`, no timer, no mount path, no focus path, no event handler applies a suggestion. The plan's "no auto-applied AI rules with no confirmation" constraint is satisfied in the literal sense; the real defect is that the *manual* bulk action is pre-armed (17.12).
- **No popup/dashboard route crossover.** `main.tsx:6-9` mounts the same `<App/>` unconditionally in all three windows and the page is selected purely from `window.location.hash`, which Rust seeds: `WebviewUrl::App("/#/dashboard")` at creation (`tray.rs:181`) or `eval("window.location.hash = '#/dashboard'")` on an existing window (`tray.rs:172` via `workspace_fragment`/`location_hash_script`, `tray.rs:143-163`). The popup is stuck at `#/popup` and the workspace window at `#/dashboard`. No popup-shows-dashboard or dashboard-shows-popup mismatch on the paths the tray actually uses. There is no `getCurrentWindow()`, no `import.meta.env` branch and no env var anywhere in `src/`; the frontend does not need to distinguish windows.
- **No route matches but renders nothing.** All six effective routes render a real component; there are no dangling `navigateHash` links. All 40 i18n keys used by the three pages exist in `en.json` with matching dot-path nesting.
- **The UI makes no false Recycle Bin claim.** No key in `cleanup.*` or `dashboard.*` mentions it. That is the right call. The defect underneath is that the guarantee cannot be kept and is not observable (17.20).
- **D4 holds.** `scan.rs:126-154` walks recursively from each root, skips symlinks and NTFS junctions (`scan.rs:62-63`, `:139-141`), honours `.mouziignore` (`:143-145`, `:166`), and is depth-capped at 128 (`:17`). Roots come only from `get_watched_folders()` (`commands.rs:524-529`); no whole-drive path exists.
- **The hash cache design is right.** `hash_cache` has `PRIMARY KEY (path, size, mtime)` (`db.rs:236`) and `get_cached_hash` (`db.rs:1055`) filters on all three columns, a perfect index probe. It **does** hit on rescan, proven by the test at `cleanup.rs:590-641` (mtime-change invalidation). Only the mechanics are wrong (17.26).
- **Two-phase dedup is genuinely good, and `blake3` was declared correctly.** `cleanup.rs:181-209` samples 64 KB first and computes a full hash only on a sample collision, so a large file is cheap unless it collides. `blake3 1.8.7` has `default = ["std"]` and nothing else: no `rayon`, no `mmap`, no `neon`, closure of only `arrayvec`, `cc`, `cfg-if`, `constant_time_eq`, `cpufeatures`. That matches the plan's D7 use exactly.
- **`tar`/`zip`/`flate2` are used and correctly pinned.** `archive.rs:6-7`, `:252`, `:279`, `:293`, `:296` use all three, with `=0.4.44` and `=0.6.6` exact pins, `default-features = false`, and only `["deflate"]` on zip. This is the pattern the other 20 entries should follow. The test at `archive.rs:374` is named `zip-slip`, so zip-slip protection is being tested.
- **`capabilities/default.json:5` was extended correctly, and the trap it represents was real.** The fork did extend the windows array, closing the plan's flagged CAPTURING anchor, and all 6 labels in it match the labels the plan intended. The defect is that the code then created a different label, not that the array was left stale (17.4, 17.14).
- **D1 items are correct.** `identifier` is `cc.mouzi.app` (`tauri.conf.json:5`), matching D1 exactly; `productName` is `Mouzi` (`:3`); `devUrl` `http://localhost:1420` (`:8`) matches `vite.config.ts:11-12` with `strictPort: true`; `frontendDist` `../dist` (`:10`) resolves to the inspected directory; `installer.nsh` and `Info.plist` both exist; `app.trayIcon: null` (`:27`) with `tray-icon`/`image-png` features (`Cargo.toml:16`) is **correct, not a bug**, because `tray.rs:53` uses `app.default_window_icon()`.
- **`Cargo.lock` is committed and in sync.** `git ls-files` confirms it; the `mouzi 0.1.6` lock entry lists exactly the 24 declared dependencies plus `tauri-build`, no drift. Format v4 needs Cargo 1.78 and the project is on 1.98.
- **MSVC Build Tools 2022 are correctly installed.** `vswhere` reports `C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools` with `Microsoft.VisualStudio.Component.VC.Tools.x86.x64` present; `rustc 1.98.0 (88d9e12ae 2026-08-18)` and `cargo 1.98.0` match the plan. `link.exe` not being on the ambient PATH is expected outside a developer prompt and not a problem, since rustc finds the linker via the registry. This closes the plan's §6 open item definitively.
- **`src-tauri/gen` is correctly gitignored.** 4 files, 5,664 lines on disk, but `git ls-files src-tauri/gen` returns nothing and `src-tauri/.gitignore:5` contains `/gen/schemas`. All 4 are `gen/schemas/*.json` generated by `tauri-build` for IDE autocompletion. `dist/`, `target/` and `node_modules/` are all correctly ignored too, verified with `git check-ignore -v`, and the `.gitignore` is well targeted for this app specifically (`*.db`, `*.db-journal`, `*.exe`, `*.msi`, `*.msix`, `scratch_*`, `/AGENTS.md`, `.kimi/`). `public/mouzilogo.png` (69,998 bytes) is a frontend asset served into `dist/`, correctly placed, not dead.
- **Zero production npm advisories.** All 19 prod deps are clean; `npm audit --omit=dev` would report 0. All 7 findings are dev or transitive-dev (17.18).
- **The `trash` XDG hypothesis is wrong.** The vendored `trash 3.3.1/Cargo.toml` shows `default = ["coinit_apartmentthreaded", "chrono"]`. No XDG and no Linux desktop-integration feature is default-enabled. The `objc`/`malloc_buf` entries under `trash` in `Cargo.lock` are macOS-target-gated **inside the crate**; `Cargo.lock` is platform-agnostic by design and records the union across all targets, so they are not compiled into a Windows binary. The same reasoning applies to the GTK/`webkit2gtk`/`x11`/`zbus` entries.
- **The application source is free of every plan-prohibited construct.** Grep of all of `src/` for `@ts-expect-error|@ts-ignore|as any` returns zero matches, and there is no `todo!()`, `unimplemented!()` or empty catch in application code. The single `@ts-expect-error` violation is in build config (17.19). The empty `catch {}` at `useCleanupStore.ts:111-121` is a swallowed error, not a prohibited empty catch, and is filed as 17.22.

### 17.41 Corrections to earlier recorded claims

Three items in this pass correct or refine statements made elsewhere in the audit or in the agents' own working notes.

1. **Corrects the claim that the capability mismatch may make the entire dashboard path unreachable.** The performance report recorded this as an out-of-scope blocker with the caveat "**UNVERIFIED** on whether `core:default` leaks through to unlisted windows (I am not certain of Tauri's exact default-window permission fallback semantics here)", and speculated that "the dashboard's `invoke` calls are rejected and the entire dashboard path may be unreachable." That speculation is **wrong**, and the routing report closes it: app-defined `#[tauri::command]`s are not ACL-checked unless the app opts into an app ACL manifest, and neither exists here. `src-tauri/permissions` is **absent** and `src-tauri/build.rs` is the bare three-line `tauri_build::build()` with no `Attributes::app_manifest(...).commands([...])`, so `has_app_manifest` is false and all handlers bypass the ACL in every window. The dashboard's `invoke` calls work. Only the event listeners die. This refines 17.4 rather than replacing it, and the practical severity of every performance finding below is unchanged: the code paths are reachable.
2. **Command count: 51 versus 46, and the discrepancy is a counting difference, not a contradiction.** The performance report enumerated `total #[tauri::command] = 51` in `commands.rs`; the routing report refers to "all 46 handlers in `lib.rs:165-217`". These count different sets: the 51 are the command *declarations* in `commands.rs`, the 46 are the handlers *registered* in the `generate_handler!` list. The main-thread conclusion in 17.3 applies to all of them, since none is `async` and none uses `#[tauri::command(async)]`. The gap between the two numbers is **not** accounted for in either report and is left open (17.42).
3. **Refines the Duplicates keeper defect with a scope correction.** 17.2 is stated as a settled defect in committed code. That is correct, and the routing report explicitly separates it from work in flight: `src/components/cleanup/**` and `src/store/useSuggestionsStore.ts` are tracked and clean, so 17.2, 17.12, 17.23 and 17.24 are settled, while findings in `src/pages/*.tsx`, `src/App.tsx`, `useDashboardStore.ts` and the new `src/utils/paths.ts` are uncommitted work in progress and should be read as gaps rather than regressions.

### 17.42 What could not be determined, and why

- **All Rust CVE exposure is unknown, not clean.** The agent had no offline index of newer crate releases, made no registry query, and found no local `rustsec` advisory data. It could not audit the Rust dependency tree for CVEs at all. Whether `trash >= 3.4` moved to `windows 0.61` is likewise **UNVERIFIED**. Nothing was run against crates.io.
- **Every performance number in this addendum is a static estimate, not a measurement.** No build, no run, no directory traversal, no database access. The only figures read from actual bytes on disk are the `dist/` sizes (419.5 KB raw / 123.5 KB gzip JS, 48.4 KB / 9.4 KB CSS, 536.6 KB total) and the 106 KB of bundled locale JSON. Throughput, latency, fsync and syscall-rate figures are derived from first principles and code enumeration.
- **Binary-size impact of dropping the dead TLS subtree is an estimate.** Roughly 2-3.5 MB on x86_64 MSVC, explicitly marked **UNVERIFIED** because no build was permitted.
- **Two SQLite planner questions need `EXPLAIN QUERY PLAN` and were not run**, because no database was opened: whether the planner actually chooses `idx_file_inventory_category` for the `GROUP BY` queries or silently prefers a full scan (17.29), and the same question for the composite-index fix in 17.6.
- **Whether `notify` 7's 2 s `poll_interval` is inert on Windows is UNVERIFIED.** On `ReadDirectoryChangesW` (kernel push) it should be, but if any path selects the `PollWatcher` fallback then a 2 s interval combined with `compare_contents(true)` means re-walking and checksumming the tree every 2 seconds, which would be catastrophic. Worth 10 minutes against the notify 7.0.0 source for the Windows backend's fallback conditions. `notify` 7.0.0 is also the oldest resolved dependency in the lockfile, worth a bump for the `Config` semantics alone.
- **The exact Windows Recycle Bin fallback conditions are UNVERIFIED.** That `trash::delete` can permanently delete while returning `Ok` on Windows is read from the shell file-operation API's documented behaviour, not executed. High confidence on the code, unverified on the platform.
- **Whether the CI workflow bodies gate anything is UNVERIFIED.** The five workflow files were enumerated and named but their bodies were out of scope.
- **`autoprefixer` being dead is UNVERIFIED** without a build.
- **Whether the README documents `npm run tauri dev` prominently is UNVERIFIED**, since the README was out of that agent's scope, so the `npm start` trap may be partially mitigated for anyone who reads it.
- **`escapeValue` was not examined in this pass.** A grep for `escapeValue`, `escapeHtml`, `innerHTML` and `dangerouslySet` across all three transcripts returns zero matches, and none of the three agents covered the HTML-escaping helper. If the audit records a finding there, it comes from a different report; nothing was re-raised or re-verified here, and no conclusion about it should be drawn from this addendum.
- **The 51-versus-46 command-count gap is unexplained**, as noted in 17.41.2. Neither report says which five declarations are not registered in `generate_handler!`, or whether the difference is plugins rather than app commands.
- **Transcript 1's tail is intact** (874 lines, report complete through the summary table and the top-three-fixes list) and **transcript 3's tail is intact** (869 lines, complete through the confidence-and-method caveats). Transcript 2 is 1,171 lines and complete through its close-out. Nothing was lost to truncation, so nothing was dropped for that reason.

### 17.43 Status

The audit is now **21 of 21 reports absorbed and complete**. This addendum closes the ninth pass and the final three reports: dependency hygiene and build config, routing and window-to-route mapping, and cross-cutting performance. All three were read-only: no file in the project was created, edited, moved or deleted, no build, typecheck, run or database access occurred, and nothing was committed. Sections 1 through 16 stand as written; nothing in them is renumbered by this addendum, which introduces C17 through C19 and H37 through H72 only, and supersedes the capability-consequence speculation recorded in 17.41.1.

## 18. Addendum - ninth pass: state layer, components and localisation

Three read-only reports were absorbed here: the Zustand store layer (`src/store/**`),
the component layer (`src/components/**` plus the 8 dashboard components), and
i18n across the 10 locales. Each was read in full up to its truncation point.
Byte sizes: 118,903 / 95,190 / 90,912.

Numbering continues from the existing body: new Criticals are C17+, new Highs H37+.
Where a finding refines or corrects an earlier section, the earlier section is named.

---

### 18.1 CROSS-REFERENCE - duplicates C18 in section 17: `DuplicatesTab` trashes the file it badges "Kept" (refines C1 / C8)

**Where:** `src/components/cleanup/DuplicatesTab.tsx:20`, `:90`, `:95`, `:101-110`, `:119` · `src/utils/cleanup.ts:54`, `:56`, `:57` · `src-tauri/src/cleanup.rs:314`

Independently re-derived by the component agent from source, not inherited.
`selected` starts as `useState({})` (`DuplicatesTab.tsx:20`). The preview renders
`keepPath={selected[g.hash] ?? g.files[0]?.path}` (`:119`) - a **fallback** - so
`files[0]` gets the green keep highlight and a `t("cleanup.kept")` badge. The
request builder at `utils/cleanup.ts:54` reads `keptByGroup[g.hash]` with **no
fallback**, yielding `undefined`; `:56` evaluates `f.path !== undefined`, which is
always true, so every file in every group is pushed; `:57` serialises
`keepPath: undefined`, JSON drops the key, and Rust's guard
`if let Some(ref keep) = action.keep_path` (`cleanup.rs:314`) becomes inert.

Consequence: unless the user clicks a keep-radio in **every** group, 100% of
duplicates reach the Recycle Bin, including the one labelled "Kept". The badge and
the preview highlight both lie; only the confirm-label count (`:101-110`, which also
uses bare `selected[g.hash]`) is truthful. Plan `:103` preview-to-confirm is
violated because the preview renders a state that is not the one that executes.
Confidence: high - the three load-bearing lines were quoted and are mutually
consistent.

Refines C1/C8 rather than restating them: the component transcript adds that the
confirm button's count is truthful while the preview above it is not, so the two
halves of the same dialog disagree with each other, and that Rust's optional-field
guard is what silently absorbs the `undefined` rather than rejecting it.

### 18.2 CRITICAL C20 - the `settings` IPC boundary is unsound, though it works today by accident

**Where:** `src-tauri/src/db.rs:89-105` · `src-tauri/src/commands.rs:266-268` · `src/store/useAppStore.ts:34-43`, `:124-127`, `:45-53`

`db::AppSettings` derives `Deserialize` with **zero** `#[serde(default)]` and zero
`#[serde(skip)]` anywhere in `db.rs` (count = 0) and declares **14 required fields**,
including non-Option `schedule_enabled: bool` and `schedule_times_per_day: i64`. The
TypeScript `AppSettings` interface declares **8** - missing `schedule_enabled`,
`schedule_times_per_day`, `schedule_time_1..4`. `update_settings_cmd` deserializes
the JS object straight into that struct.

The defect is real but does **not** currently break: `get_settings_cmd` returns all
14, and every call site spreads the whole runtime object
(`Settings.tsx:189, 199, 214, 795, 882`; `useAppStore.ts:137`), so the undeclared
fields ride along. The break condition is any partial or `defaultSettings`-shaped
payload, or a Rust migration adding a column - and `saveSettings` (`:124-127`) has no
`catch`, so the failure would surface as an unhandled rejection, not an error message.
`defaultSettings` (`:45-53`) is an 8-field object and is **dead code**, zero
references. Confidence: high on the analysis. See 18.14 for the corrected form of
this finding.

### 18.3 HIGH H73 - `subscribeDashboardEvents` cleanup dereferences module state, not captured locals

**Where:** `src/store/useDashboardStore.ts:186-188`, `:199-234`, `:231-232` · `src/main.tsx:7` · `src/pages/Dashboard.tsx:53-54`

`scanProgressUnlisten` / `scanCompleteUnlisten` / `refreshTimer` are declared at
**module scope** (`:186-188`). The subscribe function assigns them (`:199-234`) and
returns a closure that dereferences *the module variables at call time* (`:231-232`)
rather than the unlisten functions it just created. `main.tsx:7` wraps `<App/>` in
`React.StrictMode`, so every Dashboard mount runs the effect, cleans up and re-runs,
producing two overlapping `subscribeDashboardEvents()` calls. Whichever `listen()`
resolves last owns the module variables; the other pair is never unregistered. In
one resolution order the `cancelled` branch (`Dashboard.tsx:53-54`) tears down the
**live** subscription, leaving the dashboard with no `scan-progress` /
`scan-complete` handling plus a permanently leaked pair that still calls `setState`
and `invoke("is_scanning_cmd")` on every event.

Confidence: high on the defect. **Which resolution order wins is UNVERIFIED**
without running it. A production build drops StrictMode's double-invoke, but the
defect survives any remount-while-subscribe-in-flight, since the subscribe is
`await`ed at `:200` / `:207`.

### 18.4 CROSS-REFERENCE - duplicates H44 in section 17: `loadSettings` has no error path; one transient failure bricks the app

**Where:** `src/store/useAppStore.ts:119-122` · `src/App.tsx:44-49`, `:52`, `:102-108`

`loadSettings` awaits `invoke` with no `try/catch`. `App.tsx:44-49` `boot()` awaits it
with no catch and no retry, `:52` gates i18n init on the result, and `:102-108`
renders only the loading pulse until `ready`. A single SQLite or IPC error leaves
`settings === null` permanently: the user stares at an animated "Loading..." with an
unhandled rejection in the console and no way out short of a process restart.
Confidence: high, the control flow is unambiguous.

### 18.5 CONFIRMATION - extends H34 in section 15: the `file-organized` storm is unbatched; up to 4 listeners and ~7 IPC round-trips per organized file

**Where:** `src/App.tsx:63-66` · `src/components/Popup.tsx:72-88` · `src-tauri/src/tray.rs:78`, `:181` · `src-tauri/src/watcher.rs:110-124`

`App.tsx:63-66` calls `loadLogs()` + `loadStats()` per event, un-awaited, with no
debounce and no in-flight guard. `Popup.tsx:72-88` registers a **second** listener
for the same event and adds `getPendingFiles()`. Both files are loaded into both
webviews, so one organized file produces up to 4 handler runs and ~7 invokes
(`get_logs_cmd` x3, `get_stats_cmd` x3, `get_pending_files_cmd` x1). A 200-file drop
yields ~1,400 IPC round-trips, ~800 `set()` calls and ~800 root re-renders, because
`App.tsx:34` selects the whole store with no selector.

The M5 exit criterion is satisfied for the dashboard only because
`useDashboardStore` never listens to `file-organized` at all - so the dashboard never
reflects organized files, and the inventory is only rebuilt by `scan_roots`
(`scan.rs:163`); `watcher.rs:110-124` emits the event without touching
`file_inventory`. Confidence: high.

### 18.6 HIGH H74 - `role="img"` removes the storage ribbon's 7 interactive buttons from the accessibility tree

**Where:** `src/components/dashboard/StorageRibbon.tsx:42-46`, `:51-62`, `:54`, `:84`

`role="img"` sits on a container whose children are 7 interactive `<button>`s. Per
ARIA, `role="img"` makes all descendants presentational, so every segment button
leaves the accessibility tree, the `title` at `:54` never becomes an accessible
name, and the buttons are unreachable. It is also invalid ARIA (interactive content
inside `img`). The `aria-label` is `t("dashboard.storageDistribution")` - the chart's
own title, conveying no data - so a screen-reader user learns nothing about the
distribution. Confidence: high on the ARIA rule; the exact Chromium pruning is
**UNVERIFIED** at runtime.

### 18.7 HIGH H75 - `fr`, `it` and `uk` translate Undo as Cancel

**Where:** `src/locales/fr.json`, `it.json`, `uk.json` keys `popup.undo` and `settings.history.undo` · rendered at `src/components/Popup.tsx:221` and `src/components/Settings.tsx:752`

`popup.undo` = `Annuler` (fr), `Annulla` (it), `Скасувати` (uk). These are the
standard words for *Cancel*, and each collapses onto that locale's `common.cancel`
value exactly. de, es, pl, ru, vi and ja keep the two keys distinct. Detection is
mechanical (exact-string collision between keys whose English differs) and
unambiguous. Consequence: the Undo button in the tray popup and the visible Undo
label in Settings to History read **"Cancel"** in three of ten locales. A user who
reads "Cancel" will not undo a file move. This is a functional-UI defect, not
cosmetic. Confidence: high on the string identity and high that the three words mean
Cancel - they are not ambiguous.

### 18.8 HIGH H76 - no plural forms exist in any locale; pl, ru and uk render wrong counts today

**Where:** all 10 locale files (`_zero`/`_one`/`_two`/`_few`/`_many`/`_other` count = **0** in every one) · keys `popup.pendingFiles`, `notifications.cleaned`, `settings.rules.importSuccess`, `cleanup.confirm`, `settings.archive.importSuccess`, `dashboard.scanRoots`, `settings.scheduler.time` · call sites `Popup.tsx:116`, `:171`, `:300`, `DuplicatesTab.tsx:101`, `FileListTab.tsx:166`, `EmptyDirsTab.tsx:111`

i18next 26.0.10 uses `pluralSeparator: '_'` and `Intl.PluralRules`. Measured category
counts: pl, ru, uk need `one`/`few`/`many` (3-4 forms) and have 1; en, de, es, it, fr
need 2 and have 1; **ja and vi need `other` only and are already correct - do not
"fix" them.** Live failures: `ru` renders `3 файлов ожидает` where Russian requires
`3 файла`; `1 файлов ожидает` where it requires `1 файл`. `pl`
`Uporządkowano 2 plik(i)` is wrong for `few`. The `(s)` / `(i)` / `(ów)` hack in en,
de, es and fr is the same defect wearing different clothes.

This is a **data-only fix**: all 7 count-bearing call sites already pass `count`, so
i18next already enters plural resolution and falls back to the base key. Adding the
suffixed variants is a pure JSON addition with no code change. Confidence: high.

### 18.9 HIGH H77 - the 7 category names are unlocalisable by construction, and no `categories` namespace exists

**Where:** `src/locales/en.json` (no `categories` key in any of the 10 files; 0 matches for `Documents`/`Images`/`Other`/`Audio`/`Code`/`Videos`) · `src-tauri/src/scan.rs:51-58` · rendered raw at `StorageRibbon.tsx:54`, `:83`, `CategoryBars.tsx:59`, `FileBrowser.tsx:166`, `ActivityTimeline.tsx:37`, `Suggestions.tsx:251`, `:255`, `Popup.tsx:207`, `:244`, `Settings.tsx:720`

The names arrive from Rust as English string literals and are printed untranslated at
10 sites. Worst case is `StorageRibbon.tsx:36-40`, where the English category is
interpolated *into* a translated sentence: a German user reads
`Dokumente · 42 % von 1,2 GB` - one locale, two languages, one headline. In Russian
the `<h2>` reads `Документы · 42% от 12.4 GB`. Severity Medium-High to High for a
10-locale product; confidence: high.

Two English couplings make any JSON-only fix insufficient. `Popup.tsx:22-31`
`getIconForType()` selects icons by English substring (`includes("image")`,
`includes("music")`, `includes("install")`), and `utils/dashboard.ts:13-15`
`categoryColor()` keys on the same literals; localise the category and the icons and
colours vanish. The correct shape is an enum token from Rust mapped to
`t("categories.<token>")` in the frontend, with colour and icon lookup keyed on the
token.

Compounding this, `ActivityTimeline.file_type` is a **union of two incompatible
vocabularies** so a categories fix alone will not fix it: the weekly-stats query
(`db.rs:514`) groups `action_logs.file_type`, which is written both by `rules.rs:239`
(`rule.name`, whose defaults at `db.rs:330-336` include `Installers`, `Music`,
`Others`) and by `commands.rs:754` (`suggested_category`, the 7-value classifier
set). `Music` coexists with `Audio` and `Others` with `Other`, so
`ActivityTimeline.tsx:47` `categoryColor()` silently falls back to the `Other` colour
for three real values. Any fix requires a **data migration**, not a JSON addition.

### 18.10 HIGH H78 - `es.json` is missing the entire `settings.archive` subtree

**Where:** `src/locales/es.json` - 212 leaf keys against en's 220, 229 total paths against 238, **8 leaf keys + 1 object node missing**; `settings.archive.{title, description, import, importing, importingShort, importSuccess, importError, openImportFolder}`

`git status` shows `es.json` modified in this work, so the archive feature's Spanish
strings were simply never written. `Settings.tsx:399-412` renders four of them, so a
Spanish user sees the whole Import-archive card in English inside an otherwise-Spanish
Settings page. `fallbackLng: 'en'` makes it silent. Confidence: high, mechanical.
This is the only locale with a missing key; de, fr, it, ja, pl, ru, uk are all 220/220
with 0 orphans and 0 type mismatches. `vi` is 224 leaf keys with **4 orphans**, all
under `__translator_info__` (translator metadata, never rendered, no `t()` can reach
them) - Low.

### 18.11 HIGH H79 - roughly 130 backend error strings reach the UI verbatim, unlocalisable by i18next

**Where:** `src-tauri/src/cleanup.rs:319`, `:327`, `:348`, `:357`, `:369`, `:376` · `src-tauri/src/safe_fs.rs:41`, `:49`, `:71`, `:86`, `:103` · 46 `.map_err(|e| e.to_string())` sites in `src-tauri/src/commands.rs` (lines 34-692) · plus `rules.rs:184,194,197,249,252`, `watcher.rs:180,314,318`, `cleanup.rs:154,228,238,289`, `scheduler.rs:51,103`, `ignore.rs:47`, `tray.rs:110` · rendered at `ResultsPanel.tsx:84`, `Suggestions.tsx:172`

A Rust `Err(String)` is opaque to i18next. The 6 `CleanupOutcome.message` literals and
5 `safe_fs` "Failed to..." strings become cleanup and undo messages shown raw, and
the 46 `map_err` sites in `commands.rs` alone pass raw `rusqlite` / `std::io` text
(`"no such table: rules"`, `"Access is denied. (os error 5)"`) into the store's
`error` field, which 5 components render untranslated.

Best single citation: `Settings.tsx:301`
`t("settings.archive.importError", { error: String(e) })` where `archive.rs:102` is
`Err("Unsupported archive format. Choose a .zip, .tgz, or .tar.gz file.")`. A
Japanese user reads one sentence in two languages.

The one correct pattern, recorded as the template for the fix: `ScanStarted.reason`
(`commands.rs:512-517`) emits opaque tokens - `no_roots` (`:533`) and
`already_running` (`:540`), `None` on success - and `useDashboardStore.ts:157,162` /
`Dashboard.tsx:99-104` map them to `t("dashboard.scanNoRoots")` /
`t("dashboard.scanAlready")`. The frontend mapping is complete. Every other backend
string should be modelled that way.

Two other user-visible English surfaces outside the webview: `watcher.rs:137`, `:139`,
`:148`, `:166` build OS toasts and an event body in hardcoded English, and
`db.rs:330-336` default rule destinations create real on-disk English directory names
(`Images/`, `Documents/`, `Archives/`, `Installers/`, `Music/`, `Videos/`, `Others/`).
`ignore.rs:42` also writes an English header into the user's own `.mouziignore` file.

**The "~130 distinct user-facing English strings" figure is an order-of-magnitude
estimate, not a verified tally.** It came from a delegated sweep whose final summary
was truncated. The per-file, per-line citations above are the load-bearing part and
were read directly.

### 18.12 MEDIUM H80 - cross-store, cross-webview divergence: the same fact with independent writers

**Where:** `src/store/useAppStore.ts:79`, `:157-160`, `:77`, `:147-150`, `:82` · `src/store/useDashboardStore.ts:92`, `:96`, `:129-138`, `:149-151` · `src/pages/Dashboard.tsx:150-152`, `:216`, `:264` · `src/components/Popup.tsx:234-256`

Two webviews mount the same React app - `"popup"` (300x420, `tray.rs:78`) and
`"app"` (1100x820, `tray.rs:181`) - so each duplicated fact exists **twice** as two
independent store instances, reconciled only by the `file-organized` event, which
updates `logs` / `stats` / `pendingFiles` and **not** `folders`, `rules`, `settings`,
`schedule` or any dashboard, cleanup or suggestion field.

Divergences that are reachable without any unusual sequence:

- **Weekly organized counts.** `useAppStore.stats` and `useDashboardStore.weeklyStats`
  both come from `get_stats_cmd` (`db.rs:510`). Click **Clear logs** in Settings and
  `useAppStore.ts:237` empties copy A only: the popup's weekly-stats bars
  (`Popup.tsx:234-256`) vanish while the dashboard's activity timeline
  (`Dashboard.tsx:264`) keeps the pre-clear bars. **One click, two windows, same
  source, disagreeing.**
- **Root count, in the same window.** `Dashboard.tsx:216` renders
  `stats.watchedRoots.length` and `Dashboard.tsx:150-152` renders
  `useDashboardStore.rootCount`, which is written only inside `startScan`
  (`useDashboardStore.ts:149-151`) and never invalidated. Add or pause a folder after
  a scan and the two numbers differ on one screen.
- **Theme across windows.** `loadSettings` runs once per webview at boot. Change the
  theme in Settings and the popup window keeps the old theme until process restart,
  because `App.tsx:59` `applyTheme` never re-fires there.
- **Inventory totals are structurally stale.** `execute_cleanup` (`cleanup.rs:387-408`)
  never touches `file_inventory`, so after trashing N files the dashboard's totals,
  insights, age histogram and duplicate list still count them until the next scan.
- **A popup open before a folder was added** runs `handleClean` (`Popup.tsx:105-106`)
  against a stale `folders` array and never organizes into the new folder.

Mechanism behind all of it: windows are hidden, not destroyed
(`tray.rs:171-177`, `:194-197`), and **no `reset` action exists in any of the 4
stores**, so state survives hide/show for the whole process lifetime.

### 18.13 MEDIUM H81 - 10 of 11 store subscriptions are whole-store selections under Zustand v5

**Where:** `src/App.tsx:34` (no selector, 2 fields) · `Settings.tsx:125` (24 fields) · `Settings.tsx:985` (1) · `Popup.tsx:54` (11) · `Dashboard.tsx:42` (12) · `Suggestions.tsx:66` (12) · `DuplicatesTab.tsx:19` (10) · `FileListTab.tsx:30` (10) · `EmptyDirsTab.tsx:10` (10) · `HistoryPanel.tsx:10` (2) · the one correct narrow selector is `Cleanup.tsx:30-36`

Under Zustand v5 a whole-store selection re-renders on **any** `set()`. The single
narrow selector proves the pattern was known. Worst sites: `Popup.tsx:54` re-renders
unconditionally every 3 s forever because `useAppStore.ts:243` allocates a fresh
`pendingFiles` array each tick (the interval **is** correctly cleared at
`Popup.tsx:97`, so this is cost, not a leak); `Dashboard.tsx:42` re-renders the whole
7-chart subtree plus `FileBrowser` on every `scan-progress`, which Rust emits once
per 200 files (`scan.rs:14`, `:107`); `Suggestions.tsx:66` takes up to 200 full-page
re-renders from `acceptAll` (`useSuggestionsStore.ts:97-110`).

Compounding cascade: `Dashboard.tsx:267-269` passes `stats.largestFiles` /
`recentFiles` into `FileBrowser`, which lists both in a `useEffect` dep array
(`FileBrowser.tsx:64`), so every `loadStats()` produces new array identities,
re-runs the effect, and re-issues `get_inventory_files_cmd` (`:43`) whenever a filter
is active.

### 18.14 CONFIRMATION - extends H25 in section 13: the "Undo status returned then discarded" finding, confirmed and generalised

**Where:** `src/store/useAppStore.ts:174-188` · `src/components/Popup.tsx:219` · `src/components/Settings.tsx:749`, `:687` · `src-tauri/src/commands.rs:130`

`undoAction` / `undoAll` return `UndoResult` / `UndoAllResult` with status
`ok | collision | missing | failed` (`commands.rs:130`), and all three call sites
throw the result away. A `failed` or `collision` undo is invisible: the row simply
stops being undoable. This is the same class as 18.17 - the backend produces a
per-item status contract that the frontend has no rendering path for - and it is
recorded here so the two are fixed together.

### 18.15 MEDIUM H82 - swallowed errors that turn into silent data loss or silent failure

**Where:** `src/store/useAppStore.ts:249-256`, `:258-261` · `Settings.tsx:135`, `:164-168`, `:230-236` · `Settings.tsx:995` · `Settings.tsx:233-235`, `:959`

- `getSchedule` catches and only `console.error`s, leaving `schedule: null`.
  `Settings.tsx:135` seeds the editor from `defaultSchedule()` and `:164-168` syncs
  only when `schedule` is truthy; `handleSaveSchedule` (`:230-236`) then calls
  `updateSchedule(localSchedule)` unconditionally, so `useAppStore.ts:258-261` writes
  the defaults **over the user's real schedule**. A swallowed read error becomes a
  silent destructive write.
- `Settings.tsx:995` is worse than an empty catch: it converts a failed read into the
  user-facing message "No ignore rules yet."
- `Settings.tsx:233-235` shows nothing at all; clicking Save on the scheduler
  (`Settings.tsx:959`) that fails at `:233` produces no feedback. Across
  `src/components/**` there are **10** swallow-only catches
  (`HistoryPanel.tsx:19-21`, `FileListTab.tsx:85-87`, `Popup.tsx:131-133`, `:266-268`,
  `Settings.tsx:233-235`, `:731-733`, `:1023-105`, `About.tsx:18`); 3 more in
  `Settings.tsx:248-251`, `:269-272`, `:298-303` are correctly handled and excluded.

### 18.16 MEDIUM H83 - un-awaited, re-throwing store actions produce unhandled rejections and no feedback

**Where:** `src/store/useAppStore.ts:194-197`, `:204-207`, `:214-217` · `Settings.tsx:172`, `:454`, `:467`, `:809`, `:883` · `Popup.tsx:103-119`, `:83`

`addFolder` / `removeFolder` / `updateFolderMode` catch, log, then **re-throw**; the
handlers never catch. `Settings.tsx:467` `onClick={() => f.id && removeFolder(f.id)}`
means **a folder removal that fails is completely silent**; `:454` is the same for a
mode change, `:172` awaits without `try`, `:809` and `:883` are floating promises.
`Popup.tsx:103-119` `handleClean` awaits `scanFolder` in a loop with no try/catch, so
a mid-loop rejection discards the "Cleaned N files" toast for folders that *were*
processed. Also `Popup.tsx:83`: a 30 s `setTimeout` per `file-organized` event with no
ref and no cleanup, so N events leave N pending timers, and a second event dismisses
the newer toast early.

### 18.17 MEDIUM H84 - `applyAccept` removes a suggestion even when the move failed

**Where:** `src/store/useSuggestionsStore.ts:60-70` · `src-tauri/src/commands.rs:684`

After the invoke resolves, the item is unconditionally filtered out of the list and
the outcome appended to `results`, with no branch on `outcome.status`
(`"ok" | "missing" | "failed"`). A failed accept makes the row disappear; the only
trace is a red dot in the results banner, and clicking "back to list" shows it gone.
The agent explicitly ruled out optimistic-update framing - the write happens **after**
the invoke resolves - and rated it Medium rather than High on that basis, calling it
an unconditional success-shaped mutation on a failure path. Confidence: high.

### 18.18 MEDIUM H85 - storage ribbon floor inflates the bar, and the legend contradicts it

**Where:** `src/components/dashboard/StorageRibbon.tsx:58`, `:84` · `src/index.css:142-149`, `:155` · `StorageTreemap.tsx:79` · `CategoryBars.tsx:69` · `ActivityTimeline.tsx:46`

Four components, three different minimum-segment floors: `Math.max(pct, 1)%` in the
treemap and category bars, `Math.max(pct, 4)%` in the timeline, and
`Math.max(pct, 1.2)%` **plus** `min-width: 3px` in the ribbon. The ribbon is
`display:flex` with per-segment width percentages inside `overflow:hidden`, so with 7
categories each floored at 1.2% plus ~0.32% of 3px inflates the bar by up to ~10
percentage points of visual width, while the legend at `:84` prints the true `pct%`.
**The bar and its own legend disagree, and small categories are drawn larger than
they are.** A zero-byte category or watched root also draws a visible bar. The floors
are a deliberate visibility hack, but nothing in the UI discloses it.

For contrast, the other end of this file is right: `AgeHistogram` and
`ActivityTimeline` normalise against `max`, not `sum`, which is the correct choice for
a histogram, and the ribbon's `filter()` at `:20` is the only correct zero-size
handling in the folder (it drops `bytes > 0` out).

### 18.19 MEDIUM H86 - 7 hardcoded hex category colours bypass the token system; 2 fail contrast in dark mode

**Where:** `src/utils/dashboard.ts:1-9` · `CategoryBars.tsx:57`, `:70` · `StorageRibbon.tsx:59`, `:81` · `ActivityTimeline.tsx:47` · `FileBrowser.tsx:165` · `src/index.css:23-31` (`--color-surface-dark: #171412`)

`CATEGORY_COLORS` is 7 hex values applied via inline `style={{backgroundColor}}` in 6
places, so the entire categorical colour scale is theme-invariant. In dark mode two
of the seven fail WCAG 1.4.11 (3:1 for graphical objects): **Videos `#6b4f3a`
~2.5:1** and **Code `#3d5a4c` ~2.5:1**, hand-computed from the sRGB relative-luminance
formula - **treat the arithmetic as approximate, the failure is not marginal**. In
light mode the same colours are ~6.7:1 and pass.

Same class: `FileBrowser.tsx:107`, `:114` - `text-white` on `bg-primary` fails in
**both** themes, ~2.1:1 on light-mode `#f59e0b` and ~1.7:1 on dark-mode `#fbbf24`,
against a 4.5:1 AA requirement at 10-12px. And 12 lines of light-only colour classes
with no `dark:` variant sit directly beside copies that do have them
(`ResultsPanel.tsx:43`, `:46`, `:49`; `Settings.tsx:419`, `:421`, `:422`, `:531`,
`:532`, `:468`, `:670`, `:697`; `Popup.tsx:299`); `index.css:81`, `:103` draw
`border: 2px solid white` on the slider thumb.

### 18.20 MEDIUM H87 - accessibility: unnamed controls, unassociated labels, no keyboard path to two interactive surfaces

**Where:** `Settings.tsx:325-332`, `:466-471`, `:662-667`, `:668-673` · `DuplicatesTab.tsx:168-177` · `Popup.tsx:262-273` · 11 unassociated labels at `Settings.tsx:381, 452, 545, 553, 567, 581, 589, 771, 792, 830, 840, 924, 946, 1039, 1082` and `FileListTab.tsx:114` · `Settings.tsx:540-626`

- **5 real `<button>`s with no accessible name** (icon-only, no `title`, no
  `aria-label`): back (`:325-332`), **delete watched folder** (`:466-471`), edit rule
  (`:662-667`), **delete rule** (`:668-673`), and the keep-this-copy radio
  (`DuplicatesTab.tsx:168-177`), whose only child `{isKept && <Check/>}` renders
  *nothing* when unselected - so by default every keep-radio announces as an empty
  button. It is also single-select within a group with no `role="radio"` and no
  `aria-checked`. Sharp detail: `en.json:27 settings.folders.remove` and
  `en.json:51 settings.rules.delete` **already exist and are never used** - the fix is
  two attributes.
- `Popup.tsx:262-273` is `role="button"` with **no `tabIndex` and no `onKeyDown`**,
  so it is unreachable by keyboard; it fires on `onPointerDown` (so right-click
  triggers it) and nests a real `<button>` (`:279`) inside a `role="button"`.
- **11 unassociated labels and 0 `htmlFor` in the entire scope** - labels are
  siblings, not wrappers. The grace-period `<input type="range">` (`Settings.tsx:830`)
  has no label, no `aria-label` and no `aria-valuetext`. 4 controls are correct
  (`EmptyDirsTab.tsx:94`, `FileListTab.tsx:149`, `Settings.tsx:520`, `:600`).
- **No dialogs exist**, so `role="dialog"`, `aria-modal`, focus trap and Escape are
  all zero. The rule editor (`Settings.tsx:540-626`) behaves like a modal: appears
  over the list, no focus moved in, no Escape, 5 inputs injected mid-tab-order.
- Charts have no text alternative beyond titles - no `aria-label` describing values on
  `AgeHistogram` (`title=` at `:46` only), `CategoryBars`, `StorageTreemap` or
  `ActivityTimeline`; no table or summary fallback.
- Only `Settings.tsx:654` has a designed `focus-visible` ring. `index.css:133`, `:164`
  add one for checkboxes and ribbon segments, nothing for buttons.

**0 instances of the clickable-`<div>` anti-pattern** - every actionable control in all
16 components is a real `<button>` or `<input>`. The design system is sound there.

### 18.21 MEDIUM H88 - 15 keys are translated into all 10 locales but never referenced

**Where:** `suggestions.currentFolder`, `suggestions.suggested`, `settings.rules.delete`, `settings.folders.remove`, `settings.rules.action`, `settings.general.telemetry`, `cleanup.noScanData`, `dashboard.activityTimeline`, `app.tagline`, `popup.settings`, `popup.cleanManual`, `settings.folders.silent`, `settings.folders.suggest`, `settings.general.gracePeriodCustom`

The localisation work was done and the UI never wired it. The two that matter most
are a11y defects: `settings.folders.remove` and `settings.rules.delete` exist in all
10 locales while `Settings.tsx:466-471` and `:668-673` render bare icon buttons.
`Settings.tsx:652` is the one place that does build an `aria-label` from `t()`,
proving the pattern was known and then not applied. `dashboard.activityTimeline`
(`Organized this week`) is an exact duplicate of the used
`dashboard.organizedThisWeek`; `cleanup.noScanData` near-duplicates the used
`cleanup.scanFirstHint`.

### 18.22 MEDIUM H89 - system-locale detection exists in Rust and is never used by the frontend

**Where:** `src/App.tsx:53` · `src-tauri/src/commands.rs:17-18`, `:19-29` · `src-tauri/src/tray.rs:62-66` · `src-tauri/src/i18n.rs:113` · `src-tauri/src/tray.rs:119-120`

`App.tsx:53` is the only source of the app language:
`const lang = (settings.language || "en") as SupportedLang`, reading the DB column
(`db.rs:165`, default `'en'`). It never consults the OS - no `navigator.language`, no
`navigator.languages`, no Tauri `getLocale()` anywhere in `src/`. The Rust side *does*
detect it (`commands.rs:17-18` `sys_locale::get_locale()`, crate `sys-locale = "0.3"`,
`Cargo.toml:30`) but feeds it **only to the tray** (`tray.rs:62-66`).

Net effect on Windows: a German-, Russian- or Ukrainian-Windows user gets **English**
until they find Settings to General to Language manually. A second, smaller gap:
`i18n.rs:113` defines a full `vi` block but `commands.rs:19-29`
`get_system_language()` has no `"vi"` arm and falls through to `_ => "en"`, so
Vietnamese can only be reached by manual selection. This is the single
highest-leverage change in the i18n report.

### 18.23 MEDIUM H90 - path handling: `parentDir` returns a drive-relative path, and 3 copies disagree

**Where:** `src/utils/paths.ts:8-11` · `src/components/Settings.tsx:80-86` · `src/components/Popup.tsx:33-38` · `src-tauri/src/commands.rs:328`

One function, three private copies, three different guards (`i > 0`,
`lastSlash <= 0`, `=== -1`). All three return **`"C:"` for `C:\file.txt`**, not
`"C:\"` - drive-*relative*, so Windows resolves it against the process's per-drive
current directory rather than the drive root. The guard inconsistency means a
slash-free `"file.txt"` returns the file itself, and `open_folder_cmd` then opens a
file where a folder was intended. Confidence: high.

Benign and worth recording so nobody re-raises it: `commands.rs:328` does
`path.replace('/', "\\")`, so the frontend's forward-slash reformatting and the
`//server/share` UNC form are corrected downstream - accidentally, not by design.

### 18.24 MEDIUM H91 - 10 path render sites truncate with no tooltip; 3 sites with no truncation can blow out row height

**Where:** `EmptyDirsTab.tsx:129` (worst) · `FileListTab.tsx:187`, `:189` · `HistoryPanel.tsx:71`, `:73` · `ResultsPanel.tsx:78`, `:80` · `Settings.tsx:447` · and un-truncated: `Settings.tsx:719-721`, `ResultsPanel.tsx:83-86`, `Settings.tsx:644`

A 300-450 character path is silently cut with no way to recover the tail.
`EmptyDirsTab.tsx:129` is the sharpest case: in a file organiser whose whole job is
naming empty directories it shows about 40 characters with no `title`, no copy, no
expand. Two sites are correct - `DuplicatesTab.tsx:178-179` has `title={f.path}` and
`Popup.tsx:203-205` has `title={log.file_name}`.

A long path does **not** wrap and blow out the layout at the truncating sites:
`truncate` implies `white-space:nowrap` and the containers scroll (`max-h-80` plus
`overflow-auto` at `HistoryPanel.tsx:49`, `ResultsPanel.tsx:55`,
`EmptyDirsTab.tsx:116`, `FileListTab.tsx:171`). The failure is the opposite - the path
is *invisible*, not wrapped. The genuinely dangerous sites are the three with no
truncation, no `break-all` and no `min-w-0` ancestor:
`Settings.tsx:719-721` (`{log.destination_path}` in a plain `<div>`, which stretches
the flex row at `:712-756` and pushes Undo off-screen),
`ResultsPanel.tsx:83-86` (`r.message`, which embeds the full `C:\...` path from
`cleanup.rs:339`), and `Settings.tsx:644`. The 4 error banners render `String(e)` from
Rust with no `break-all` either.

Two translated sentences handle paths badly: `Popup.tsx:290`
`t("popup.openFolder", { folder: ... })` renders inside `truncate` with no `title`
(`:289`), so the ellipsis eats the path - the only useful part. `Popup.tsx:277`
`t("popup.organized", { file: toast.file })` has the opposite problem: no truncation,
so a long filename wraps and squeezes the dismiss button in the `justify-between` row
at `:274`.

### 18.25 MEDIUM H92 - silent 50-row cap on the largest and recent listings, with a silent jump to 80

**Where:** `src-tauri/src/commands.rs:585-586` · `src/components/dashboard/FileBrowser.tsx:48`, `:101`, `:151`

`largest_files` and `recent_files` are hardcoded to 50 and are not parametrised.
Unfiltered, `FileBrowser` shows 50 rows; the moment the user types a query or picks a
filter it switches to `get_inventory_files_cmd` with `limit: 80`
(`FileBrowser.tsx:48`, clamped 1..200 at `commands.rs:616`). The UI never says
"showing 50 of N", so a user with 4,000 files cannot distinguish "only 50 large files
exist" from "here are 50 of 4,000" - and the count changes from 50 to 80 on the first
keystroke. A storage dashboard that under-reports without saying so is worse than one
that admits a cap.

### 18.26 MEDIUM H93 - `Settings.tsx` carries 39 hardcoded literals beside 87 `t()` calls

**Where:** `src/components/Settings.tsx:1104-1107`, `:737`, `:241`, `:258`, `:279`, `:42-50`, `:720`, `:466`, `:672` · `src/components/Popup.tsx:214`, `:167` · `src/utils/format.ts:8`, `:9`, `:10`, `:18`

Across the 8 non-dashboard components: **39 rendered hardcoded literals - 22
unambiguously English, 10 language endonyms, 7 punctuation or glyphs.** The 4 cleanup
tabs plus `ResultsPanel` are effectively 100% translated (29 `t()`, 0 English
strings). Worst groups:

- `Settings.tsx:1104-1107`, the 4 `.mouziignore` tip bodies - **the single largest
  cluster of untranslated prose in the app**, and the `settings.ignore.tips` header
  directly above them at `:1101` *is* translated, so a Japanese user reads a
  translated "Tips:" followed by four English sentences.
- `title="Open folder"` x2 (`:737`, `Popup.tsx:214`) while `en.json:231`
  `cleanup.openFolder` already exists and is translated in all 10 locales.
- `formatDuration`'s `"0s"` / `${h}h` / `${m}m` / `${s}s` (`:42-50`), all rendered at
  `:827`, locale-incorrect for de/fr/ru/uk, sitting three lines below the translated
  `t("settings.general.gracePeriod")`.
- Native-dialog filter names `"JSON"` x2 and `"Archives"` (`:241`, `:258`, `:279`)
  while every result branch in the same handlers is translated. This one is an
  **architectural gap**, not an oversight: `filters[].name` is handed to the Windows
  open/save dialog and is outside the webview, so i18next cannot reach it.
- `utils/format.ts:18` `toFixed(2)` always emits a **period** decimal separator. A
  French, German or Russian user sees `1.50 MB` where `Intl.NumberFormat` would give
  `1,50 MB`. `formatBytes` never touches i18n.
- Hardcoded `%`, `·` and `→` outside translation at 20 sites. German and French
  require a non-breaking space before `%`; CJK locales generally use no space at all.

At least 15 of the 22 English strings have a translated sibling within about 10 lines,
so the pattern is known and inconsistently applied.

### 18.27 MEDIUM H94 - preview and confirm is unimplemented for 8 of 12 destructive actions

**Where:** `Popup.tsx:161` (Clean Now) · `Settings.tsx:695` (Clear History), `:686` (Revert All), `:466`, `:668` · **0 occurrences of `confirm()` or `alert()` in `src/**`**

Worst first. `Popup.tsx:161` "Clean Now" is the flagship action: it moves files,
shows only an aggregate count, and **no per-file preview exists anywhere**. Then
`Settings.tsx:695` "Clear History", which destroys the undo trail and is therefore
irreversible for the user while styled like a harmless action. Then one-click
unconfirmed: delete watched folder (`:466`), delete rule (`:668`), Revert All (`:686`).
The 3 cleanup tabs do have preview + checkbox + confirm, and `FileListTab` /
`EmptyDirsTab` correctly guard with `disabled={store.busy || checked.size === 0}`;
`DuplicatesTab:95` guards on `busy` only.

### 18.28 MEDIUM H95 - two count sites bypass i18next entirely and cannot be fixed in JSON

**Where:** `src/components/cleanup/DuplicatesTab.tsx:90` · `FileListTab.tsx:156` · `EmptyDirsTab.tsx:101`

`DuplicatesTab.tsx:90` renders `{groups.length} {t("cleanup.groupCount")}` - a bare
count plus a plural-unsafe label ("3 groups"). `FileListTab.tsx:156` and
`EmptyDirsTab.tsx:101` render `{t("cleanup.selectAll")} ({data.length})` - a bare
parenthesised numeral with no noun, which reads acceptably in most languages but is
not localisable and is meaningless to a screen reader. Unlike 18.8 these cannot be
fixed by adding JSON variants; the call sites need `t(key, { count })`.

### 18.29 MEDIUM H96 - 2 index-based keys on lists that provably mutate

**Where:** `src/components/Settings.tsx:942` (list whose length changes at `:924-935`) · `Settings.tsx:1066` (on `patterns`, which splices at `:1004` and `:1010`)

Both are currently invisible because the rows hold no internal state and every field
is controlled. 11 of 13 keys in scope are stable, and every one of the 8 dashboard
components uses a domain key (`item.path`, `item.category`, `file.path`,
`bucket.bucket`, `card.key`, `item.file_type`). All three viz components re-sort on
data change via `useMemo` (`StorageTreemap.tsx:24`, `CategoryBars.tsx:21`,
`ActivityTimeline.tsx:12`), so **index keys would have corrupted the rendering and did
not** - a check that passed, recorded so it is not re-run.

Two effect defects in the same file: `Settings.tsx:153-161` is a self-fighting
write-back effect that rewrites the state the handler just wrote (type `3600` into the
seconds field and it silently becomes `1` with the unit flipping to Hours
mid-keystroke), and `Settings.tsx:991-999` is an uncancelled async race on folder
switch.

### 18.30 LOW H97 - an N-root scan triggers N+1 full dashboard re-fetches against a table being written

**Where:** `src/store/useDashboardStore.ts:207-224`, `:219` · `src-tauri/src/scan.rs:193`, `:120` · `src-tauri/src/commands.rs:582`

Each `scan-complete` calls `refresh()` immediately while `is_scanning_cmd` is still
true, and Rust emits one per root. A 4-root scan means 4 immediate plus 1 trailing
`get_dashboard_stats_cmd` (itself 6 queries including two 50-row listings and the
insights computation) plus 5 `get_stats_cmd`. Because `append_inventory_batch` writes
`file_inventory` **during** the walk, the intermediate reads see a partially
populated table and the dashboard visibly counts up. Each refresh also produces fresh
`largestFiles` / `recentFiles` arrays, re-triggering the `FileBrowser` effect (18.13).

### 18.31 LOW H98 - stale `scanReason` leaves a permanent "a scan is already running" banner

**Where:** `src/store/useDashboardStore.ts:190-197`, `:174-181`, `:157` · `src/pages/Dashboard.tsx:163`

`scheduleRefreshAfterScan` sets `{isScanning:false, scanProgress:null}` but omits
`scanReason`; `syncScanState` omits it too. `Dashboard.tsx:163` renders the banner
whenever `reasonMessage && !isScanning`, so a `"already_running"` reason set at `:157`
leaves "a scan is already running" on screen permanently after that scan has finished.

### 18.32 LOW H99 - orphaned backend function and a mis-naming pair, after the `LargestFiles` deletion

**Where:** `src-tauri/src/db.rs:1005`, `:1008`, `:1267` · `src-tauri/src/scan.rs:297` · `src/components/dashboard/StorageTreemap.tsx` · `ActivityTimeline.tsx` · `en.json:153`

`LargestFiles.tsx` is deleted and its import and JSX removed in the same uncommitted
pass, with **zero references remaining anywhere** in `src/` or `dist/`. M2's "largest
files" requirement is still met, by `FileBrowser.tsx:101`, which renders
`t("dashboard.largestFiles")` as its heading in the default `mode === "size"`, seeded
from `initialLargest` -> `stats.largestFiles`. Two orphans remain, both Low and both
defects of naming rather than behaviour:

- `db.rs:1005 get_largest_files` now has **no production caller** - only its own
  wrapper (`:1008`), a test (`scan.rs:297`) and `db.rs:1267`. The dashboard gets
  largest files from `get_inventory_files("size", ..., 50)` at `commands.rs:585`.
- `ActivityTimeline` is not a timeline: `db.rs:510-521` returns `GROUP BY file_type`
  over 7 days of `action_logs`, a per-type count with **no time axis**, though the
  data source is right. `StorageTreemap` is not a treemap: it groups by the stored
  `root_path` column (`db.rs:836`) and the backend performs **no path parsing at all**
  (zero hits for `dir_of|dirname|parent_of|rsplit` across all 15 files in
  `src-tauri/src/`), so it is a per-watched-root bar list. Both are met-but-understated
  against M2's wording, not defects.

### 18.33 LOW H100 - `en.json:187 "filterBy": "{{name}}"` is untranslatable by construction and overloaded

**Where:** `src/locales/en.json:187` · `src/components/dashboard/FileBrowser.tsx:136`, `:141`

The value is literally `{{name}}` with no surrounding text, so
`t("dashboard.filterBy", …)` renders a bare `Documents` or `Downloads` pill whose
meaning rests entirely on visual chrome, and **a translator has nothing to
translate**. It is also reused for two different nouns - category at `:136`, root
folder at `:141` - with no way for a locale to word them differently. This is an
i18n *modelling* defect, not a missed string. Confidence: high.

### 18.34 LOW H101 - `FileBrowser` renders the app tagline as its loading state

**Where:** `src/components/dashboard/FileBrowser.tsx:148` · `src/locales/en.json:5`

`t("app.loading")` resolves to `"Mouzi..."` - the splash and tagline string, not a
loading indicator. The user sees "Mouzi..." under the file list while searching. The
key exists; it is the wrong key.

### 18.35 LOW H102 - remaining mechanical store and component defects

**Where and detail:**

- `Dashboard.tsx:188-194` - the empty-state "Scan now" button has no `disabled` prop
  (the header button at `:131-139` does). Rust's `AtomicBool` guard
  (`commands.rs:537`) absorbs the duplicates, but `startScan` has no in-flight guard
  in the store.
- `useSuggestionsStore.ts:115` - `dismissAll` omits `error: null`, asymmetric with
  `acceptAll` at `:100`; `:100` in turn omits `results: null` (which
  `loadSuggestions:47` does set), so a stale banner persists and new outcomes append
  to the old array.
- `useSuggestionsStore.ts:9` - `source: "heuristic" | "ollama" | "learned"` against
  `classify.rs:558` `pub source: String`. The `invoke<Suggestion[]>` assertion is
  unchecked and the renderers widen it back to `string` (`Suggestions.tsx:24`, `:37`),
  so the union buys nothing.
- `useAppStore.ts:65` - `status: string; // 'ok' | 'collision' | 'missing' | 'failed'`:
  a literal union was available, and the comment omits `cross-device`, which the plan
  mandates.
- `useAppStore.ts:5`, `:17`, `:24`, `:35` - `id?: number` on records the backend always
  returns with an id, forcing the non-null assertions at `Popup.tsx:209`, `:219` and
  `Settings.tsx:749`.
- `useAppStore.ts:81` + `db.rs:538-573` - `settings` and `schedule` are two store
  fields describing **one** DB row, fetched by two commands at two moments with no
  transactional pairing, so they can describe different DB states.
- `useDashboardStore.ts:191` vs `Dashboard.tsx:263`, `:271-272` - defensive `?? []` /
  `?? EMPTY_INSIGHTS` on fields the Rust struct (`commands.rs:568-578`) always
  populates. The store already has the correct type; the redundancy signals distrust
  in a type the compiler already checks.
- `FileBrowser.tsx:66-77` - the `/` shortcut is a global, window-scoped focus steal
  with no notion of the dashboard being the active view. It does correctly bail on
  `INPUT`/`TEXTAREA`/`SELECT` and does clean up.
- `FileBrowser.tsx:91` - a 1,400 ms `setCopied(null)` `setTimeout` is never stored or
  cleared; rapid clicking stacks timers and unmount leaves a pending `setState`.
- `FileBrowser.tsx:123-129` - the search input has **no accessible name**: placeholder
  only, no `<label>`, no `aria-label`, and a placeholder disappears on first keystroke.
- `FileBrowser.tsx:181`, `:189` - row actions are hover-only
  (`opacity-0 group-hover:opacity-100`). `focus:opacity-100` covers keyboard, but
  there is no hover on touch or pen, and the hit target is `p-1` plus a 12px icon
  ~= 20x20 CSS px, below the WCAG 2.2 24x24 minimum.
- `FileBrowser.tsx:64` - the debounce effect depends on `initialLargest` /
  `initialRecent`. The arrays have stable identity and the effect writes local
  `files` state, not the store, so **no re-render loop today**. It would become one if
  the page ever passed a derived array. **Flagged as latent, UNVERIFIED as a live
  bug.**
- `AgeHistogram.tsx:12-18`, `:46`, `:50` - builds a `Record<string,string>` of labels
  on every render and indexes it with `bucket.bucket`. `AGE_BUCKETS` is `as const`
  (`dashboard.ts:11`) so the 5 keys always hit, but the lookup is untyped and an
  unrecognised bucket id renders an empty label.
- `StorageRibbon.tsx:19-20` - unmemoised while all three siblings use `useMemo`. At
  most 7 categories there is no measurable cost; noted only for folder consistency.
- `DuplicatesTab.tsx:20` - the duplicate "keep" choice is keyed by `g.hash` and is
  never pruned when `handleResultsDone` (`:42-46`) re-runs `findDuplicates`, so a
  stale key can silently apply to a new group. **UNVERIFIED** (depends on hash
  stability across rescans). Related to 18.1 but a distinct, lower-severity mechanism.
- `App.tsx:51-55` - `initI18n` is re-run on every settings write, because the effect
  depends on the whole `settings` object. Toggling the theme, autostart or the grace
  period all re-initialise i18next. Redundant, and the async race could in principle
  leave the language stale. Should key on `settings.language` alone.
- `Cleanup.tsx:77` - `tabs.slice(0, 4).map((t) => …)` shadows the `useTranslation()`
  `t` from `Cleanup.tsx:22`. Benign today (only `t.label` is read) but a live trap.
- `src/utils/format.ts:7-19` - `formatBytes` is 1024-based but labels `KB/MB/GB`, SI
  labels on binary math. Matches Windows Explorer, so informational.
- `src/index.css:81`, `:103` plus 10 sites using `text-[10px]`
  (`HistoryPanel.tsx:72`, `:76`; `DuplicatesTab.tsx:145`, `:185`;
  `ResultsPanel.tsx:79`, `:83`; `FileListTab.tsx:188`, `:195`;
  `Settings.tsx:449`, `:461`, `:638`; `FileBrowser.tsx:39`, `:46`, `:49`, `:170`,
  `:175`, `:182`; `StorageTreemap.tsx:82`) - layout computed at 10 CSS px, so the
  metadata line competes with the 12px filename above it while carrying less
  information. All 48 fixed sizes are CSS px inside WebView2 and scale with the OS;
  **no fixed-pixel geometry was found to break at 125%/150% or at 1024x768.**
- `src-tauri/src/tray.rs:119-120` builds the fully-translated `organized` message then
  emits `app.emit("show-notification", …)` - **no listener for `show-notification`
  exists in `src/`.** The one completely localised end-to-end message reaches nothing.
  This negative claim comes from a delegated grep that was not independently re-run;
  the same applies to the `file-organized` -> `Popup.tsx:72` path. Neither is
  load-bearing.
- 5 backend commands are registered but never invoked from `src/`:
  `get_system_language`, `initialize_defaults_cmd`, `refresh_watcher_cmd`,
  `show_popup_cmd`, `is_autostart_enabled_cmd`.
- `src-tauri/src/classify.rs:493-528` - the 7 Ollama error strings are silently dropped
  at `classify.rs:645` (`if let Ok(Some(ai_cat)) = provider.classify(&filename)`), so
  they do not reach the UI today, but they are `Err` values on the public
  `AiProvider` trait and will leak the moment a caller propagates them.
- `vite.config.ts:4` `// @ts-expect-error process is a nodejs global` - outside
  `src/components` but inside the build config; plan section 6 bans it absolutely. The
  fix is `@types/node`.

### 18.36 REFUTED - the four new dashboard components are wired, not orphaned

Recorded so nobody re-raises it. All four are imported and rendered, with no dynamic
`import()`, no `lazy()`, no barrel `index.ts`, no Tailwind safelist, and no
per-file `tsconfig` entry - all imports are static:

| Component | Import | Render |
|---|---|---|
| `StorageRibbon` | `Dashboard.tsx:10` | `:206` |
| `InsightCards` | `Dashboard.tsx:15` | `:220` |
| `AgeHistogram` | `Dashboard.tsx:14` | `:263` |
| `FileBrowser` | `Dashboard.tsx:16` | `:267` |

`tsconfig.json:19` `noUnusedLocals: true` plus `package.json:8` `"build": "tsc && ..."`
means an unused import cannot survive a build, so "imported but never rendered" is
ruled out structurally as well as empirically. The wire-graph was verified twice,
independently: 18 matches in 5 files, `LargestFiles` zero occurrences anywhere in
`src/`. In the reverse direction, all 8 imports resolve to files on disk, every prop
traces to a real fetch (7 from `get_dashboard_stats_cmd` at
`useDashboardStore.ts:122`, 1 from `get_stats_cmd` at `:131`), and all 8 frontend
`invoke` names resolve to a real `#[tauri::command]` - **0 unmatched**. The untracked
status of the four files is a commit-hygiene issue, not dead code.

### 18.37 REFUTED - no placeholder or fake data shipped anywhere

Repo-wide grep over `src/` for `TODO|FIXME|mock|dummy|sample|fake|hardcoded|demo|stub|XXX|HACK`
returns **5 hits, all false positives** - the Spanish word *todo* ("all") in
`es.json:63, 167, 199, 230, 231`. Zero real markers. No fabricated rows, no
estimated values, no sample datasets. The one hardcoded data path,
`EMPTY_INSIGHTS` (`useDashboardStore.ts:78-86`), is a zero-filled **fallback**, not
fake data. Every number rendered by the 8 dashboard components traces to a real
backend value: `commands.rs:582-599` and `db.rs:818-848` for the storage views,
`db.rs:886-928` for the real `mtime` age bucketing, `db.rs:510-521` for the
`action_logs` count over 7 days, real `file_inventory` rows for `FileBrowser`.
`utils/dashboard.ts:1-9` `CATEGORY_COLORS` and `:11` `AGE_BUCKETS` are display
constants, not data. The "placeholder data in a dashboard" High finding does not
occur. Confidence: high.

### 18.38 REFUTED - translation quality is real, not a mechanical copy

The M5 "mechanical copy" concern does not apply. Byte-identical leaf values against
`en`: de 11/220 (5.0%), es 7/212 (3.3%), fr 16/220 (7.3%), it 8/220 (3.6%),
ja 7/220 (3.2%), pl 6/220 (2.7%), ru 7/220 (3.2%), uk 6/220 (2.7%), vi 6/220 (2.7%).
**No locale is anywhere near "90% untranslated English" - the worst is fr at 7.3%.**
Charset profile corroborates: 100% of de/es/fr/it/pl/vi values contain the expected
script; ja 213/220 (96.8%), ru 213/220, uk 214/220, and every non-conforming value in
those three is a brand name, a path, or `{{name}}`. Length ratio translated/en:
de 1.31, es 1.29, fr 1.31, it 1.28, pl 1.22, ru 1.25, uk 1.23, vi 1.13, ja 0.61 -
all consistent with genuine translation, CJK naturally shorter. Sampled prose is
unambiguously translated: `settings.about.description` is a real three-sentence
translation in all 10.

**The honest limit of the method, stated so it is not over-read:** byte-identity is a
weak signal in Germanic and Romance languages. German `Name`, `Version`, `System`,
`Dashboard` and French `Action`, `Destination`, `Mode`, `Extensions`, `Minutes` are
all correct words and are false positives of the heuristic. The agent flagged only
the cases where the evidence is unambiguous (a key collapsing onto a sibling key with
different English) and marked the rest as needing a native speaker - see 18.44.

The remaining translation defects are 18.7 (a genuine mistranslation the heuristic
*did* catch), 18.10 (a missing subtree) and 18.8 (no plural forms), plus low-severity
over-collapse: fr/it/uk reuse one string for `settings.ignore.title` and
`suggestions.dismiss`; ja reuses `提案` for `settings.folders.suggest`,
`dashboard.openSuggestions`, `suggestions.title` and `suggestions.suggested`; vi
reuses `Mẫu` for `settings.rules.pattern` and `settings.ignore.patterns`, losing the
plural; ru reuses `Раз в день` for the label `settings.scheduler.timesPerDay` and the
value `settings.scheduler.once`; it and pl append a `(regex)` suffix to
`settings.rules.pattern` that `en` does not have. Nuance loss, not breakage.

### 18.39 REFUTED - `escapeValue: false` is not an XSS risk, and there is no raw-HTML sink

`src/i18n/index.ts:29-38` sets `interpolation: { escapeValue: false }` (at `:35`).
**This is correct, not a vulnerability.** i18next's own documentation recommends
`false` for React, because React escapes text children; the setting only matters when
the result is injected as raw HTML. Verified: **0 occurrences of
`dangerouslySetInnerHTML` and 0 of `innerHTML` in `src/**`** (independently
confirmed from the component side - 0 in all 16 components). Every `t()` result lands
in a JSX text child, so the brief's XSS premise does not hold for this codebase. All
245 `t()` call sites pass plain values; no React-node interpolation is used anywhere.
Recorded as a correction to any earlier section that flags `escapeValue: false` as a
finding.

### 18.40 REFUTED - every `t()` key referenced in code exists in the base locale

**0 of 245 static `t()` call sites across 19 files reference a key absent from
`en.json`.** No raw dotted key can be rendered by a typo. All ~30 keys used by the 8
dashboard components were checked individually against `en.json:140-190` and all
exist. The only dynamic call site is `FileListTab.tsx:142 t(emptyKey)`, fed only
`cleanup.noLargeFiles` and `cleanup.noStaleFiles` - both exist. All 21
components and pages correctly import and call `useTranslation()`; 0 components are
missing the hook. Two `t()` keys are *misused* rather than missing
(`FileBrowser.tsx:148` -> `app.loading`, 18.34; `en.json:187 filterBy` is
untranslatable, 18.33), and 15 translated keys are unreferenced (18.21), but no
referenced key is absent.

This is currently latent rather than live: a key missing from all 10 locales would
render as the **raw dotted key string** with no console output and no written
artefact, because `debug` is unset (`false`) and `saveMissing` is unset (`false`).
Verified against the i18next 26.0.10 source in `node_modules`: `resolve()` returns the
key itself when nothing matches. There is therefore no automated drift detector -
the exact safety net the "en.json first" workflow needed. The `es` archive gap
(18.10) is the live demonstration of silent fallback.

### 18.41 REFUTED - placeholder integrity is clean, exhaustively

13 keys carry interpolation; **0 defects across 9 non-English locales x 13 keys = 117
comparisons.** No missing, extra, renamed, duplicated or reordered placeholder; no
unbalanced `{{`/`}}`; no malformed braces in any of the 10 files. Placeholder **order**
matches English everywhere. Order-identity is fine for the Slavic data actually
present - `uk popup.pendingFiles` is `Очікує файлів: {{count}}`, correctly
front-loading the noun and moving the placeholder to the end - but it will need
per-locale reordering once plural forms are added (18.8).

The 13 keys are `popup.organized` (`file`), `popup.openFolder` (`folder`),
`popup.pendingFiles` (`count`), `notifications.cleaned` (`count`),
`settings.rules.importSuccess` (`count`), `settings.archive.importSuccess`
(`extracted`, `count`; 9 locales, es missing), `settings.archive.importError`
(`error`; 9 locales), `settings.scheduler.time` (`number`),
`dashboard.filterBy` (`name`), `dashboard.lead` (`category`, `percent`, `size`),
`dashboard.scanRoots` (`done`, `total`), `cleanup.confirm` (`count`),
`cleanup.reclaimable` (`size`). All 10 files are valid JSON, UTF-8, no BOM, LF line
endings, trailing newline, **zero type mismatches, zero brace-balance errors, zero
empty values.**

### 18.42 REFUTED - the camelCase type-contract claim does not hold as a live defect

**Correction to the "Rust struct missing `#[serde(rename_all = "camelCase")]`" line of
inquiry.** Every camelCase-to-snake_case conversion on the invoke boundary was checked
against the Rust signatures and all of them check out: `accept_suggestion_cmd`
`{suggestedCategory}` -> `suggested_category`; `find_large_files_cmd` `{minBytes}` ->
`min_bytes`; `load_mouziignore_cmd` `{folderPath}` -> `folder_path`;
`CleanupRequest.keep_path` under `rename_all = "camelCase"` (`cleanup.rs:38-44`) <-
`keepPath`; and `DashboardStats`, `ScanProgress`, `ScanComplete`, `ScanStarted`,
`UndoResult` and `AcceptOutcome` all sit under `rename_all = "camelCase"` and match
their TypeScript interfaces field for field. `useDashboardStore.ts:150`
`f.mode !== "paused"` correctly matches `FOLDER_MODE_PAUSED = "paused"` (`db.rs:18`,
used via `is_folder_paused_mode` at `commands.rs:527`).
`get_inventory_files_cmd`'s `""`-for-`None` substitution
(`FileBrowser.tsx:45-46`) is correctly filtered at `commands.rs:613-615`.

**The one real contract defect is the 14-vs-8 field mismatch in 18.2, and it is
currently harmless** - the app works only because `get_settings_cmd` returns all 14
fields and every call site spreads the whole runtime object. An earlier reading of
this finding, which concluded that `saveSettings` always throws and that the theme
cannot be changed, is **wrong** and should not be carried into the audit. The
underlying fragility stands and is recorded at 18.2 with its exact break condition.

### 18.43 REFUTED - no destructive path in the state layer is an un-rolled-back optimistic update

Recorded so the High finding is not re-raised. `executeCleanup`
(`useCleanupStore.ts:97-109`) and `acceptSuggestion` / `applyAccept` are strictly
pessimistic - every `set()` follows a resolved `invoke`. `cleanup.rs:387-408` is
per-item with `ok | failed | skipped` and never aborts a batch. **There is no
finding of the "files marked deleted that are not" shape in the store layer.** The
nearest relative is `applyAccept` (18.17), which is an unconditional
success-shaped mutation *after* the await, rated Medium for that reason.

Also cleared:

- `await` in a non-async function: none. Every async store action is `async`.
- Loading flags stuck on throw: none. All four `useCleanupStore` finders and both
  `useDashboardStore` loaders use `catch { set({ loading/isLoading: false, error }) }`;
  `useAppStore.scanFolder` uses `try/finally` (`:164-171`); `useSuggestionsStore` uses
  `try/finally` (`:78-80`, `:93-94`) plus unconditional trailing
  `set({busy:false})` (`:109`, `:127`).
- Polling interval leaks: none. Exactly one interval exists, `Popup.tsx:91`, and it
  **is** cleared at `:97`. `App.tsx` has none. `FileBrowser.tsx:32`, `:57-63` uses a
  debounce that is also cleared.
- Stale closures in event handlers: none. `App.tsx:64-65`, `Popup.tsx:84-86` and
  `useDashboardStore.ts:209` all use `getState()` or functional `set`;
  `Dashboard.tsx:81` and the focus handler are memoised on stable store actions.
- Conditional hooks: none. Every `use*` precedes the early returns at
  `StorageTreemap.tsx:29`, `CategoryBars.tsx:26`, `StorageRibbon.tsx:22`,
  `ActivityTimeline.tsx:17`, `FileBrowser.tsx:151`. No `invoke` in any render body,
  no `setState` during render, no `ResizeObserver` or interval leaks. All 4
  listeners and timers that exist are cleaned (`FileBrowser.tsx:62`, `:76`).
- Banned constructs in `src/store/**`: **zero `as any`, zero `@ts-ignore`, zero
  `@ts-expect-error`.** Across all of `src` there is exactly one `any`:
  `Popup.tsx:72` `(event: any)` on the `file-organized` payload, which makes
  `payload?.success`, `destination_folder` and `file` at `:74-80` entirely unchecked.
  The only other cast is in a test (`__tests__/dashboard.test.ts:53`). In
  `src/components/**`: 0 `as any`, 0 `@ts-ignore` / `@ts-expect-error`, 0
  `todo!()` / `unimplemented!()` / `let _ =`.
- Persistence: **no `localStorage`, no `sessionStorage`, no `zustand/persist`, no
  Tauri store** - grep for all four returns zero hits in `src/`. Persistence is
  SQLite only, and `settings` is a column-per-field row (`db.rs:163-170`), not a
  JSON blob, so the "persisted blob read without a schema check crashes after a
  downgrade" risk **does not apply**. `CREATE TABLE IF NOT EXISTS` plus defaults mean
  additive migrations get DB defaults.
- Bar charts do **not** divide by the visible sum. `percentOf(part, total)`
  (`utils/dashboard.ts:36-39`) is called with the caller-supplied `totalBytes`, and
  `Dashboard.tsx:208`, `:250` and `:256` all pass `stats!.totalBytes` - a
  whole-inventory scalar from `db.rs:788-800`. Verified first-hand after the agent
  flagged that this had been inferred rather than read. Division by zero, empty,
  single-category and zero-size states are all handled: `percentOf` returns 0 for
  `total <= 0 || part <= 0` (`dashboard.ts:37`), and the five components each bail or
  clamp (`StorageTreemap.tsx:29`, `CategoryBars.tsx:26`, `StorageRibbon.tsx:22`,
  `ActivityTimeline.tsx:17`, `AgeHistogram.tsx:23-24` via `Math.max(…, 1)`).
- No `confirm()`, `alert()` or `window.prompt` anywhere in `src/**`. No
  `dangerouslySetInnerHTML` or `innerHTML` anywhere in `src/**`. Zero
  `!important` in `src/components`. Exactly one inline `style={{}}` -
  `Popup.tsx:248` `width: ${pct}%` - which does not fight Tailwind, since that element
  carries `h-full bg-primary rounded-full` and no width class. The dashboard
  components' inline styles are all dynamic `width` / `height` / `backgroundColor` /
  `opacity` that no utility can express.
- The Tailwind 4 CSS-first theming architecture is sound: no `tailwind.config.*`,
  `postcss.config.js:3 @tailwindcss/postcss`, tokens in `index.css:3-11` (`@theme`)
  indirection to `:root` (`:13-21`) and `.dark` (`:23-31`), `.dark` live via
  `App.tsx:17-20` and `:22-27`. All 8 dashboard components use semantic tokens
  (`bg-surface-dark`, `text-text`, `border-border`) and so follow both themes with
  **zero** `dark:` variants. The colour leak is the narrow 7-hex case in 18.19.
- `FileBrowser` is read-only: toggle sort, debounced search, per-row copy-path and
  open-containing-folder. **No select-all, no checkboxes, no bulk action, no
  drag-and-drop, no delete, no move**, so nothing is pre-armed and the plan's
  preview-to-confirm constraint is not engaged by this component.
- The i18n language switcher works: `Settings.tsx:186-190` awaits
  `i18n.changeLanguage(lang)` then persists, and `changeLanguage` emits
  `languageChanged`, which react-i18next's `useTranslation` subscribes to, so all 19
  components re-render. The value is also written.
- `returnObjects` and `returnNull` are both correctly unset (`false` in v26). No key
  is used as an object and 0 leaves in any of the 10 files are `null` or empty, so
  there is no misconfiguration.

### 18.44 What the three reports could not determine

- **Translation correctness is not mechanically decidable.** All structural figures
  (key parity, placeholders, byte-identity, charset, length ratios, brace balance,
  unused keys) are exact, from parsed JSON and scripted comparison. Judgements of
  quality are not. Marked **UNVERIFIED, needs a native speaker**: `ru
  settings.rules.pattern` = `"Pattern"` (most likely genuinely untranslated; Russian
  would be `Шаблон`/`Маска`; flagged medium-high confidence but not proven), `vi
  suggestions.source.heuristic` = `"Heuristic"` (a possible accepted loanword), the
  German du/Sie register in `settings.folders.modeDesc` and
  `settings.ignore.description`, and whether `es` targets es-ES or es-419
  (`es.json` uses `Agregar`, Latin-American, while the switcher offers plain
  `Español`). Also undecidable: whether any of the ~30 byte-identical values in
  German, French, Italian, Polish, Russian and Ukrainian are genuine translation
  errors rather than correct cognates. **No translation-quality defect is asserted
  in this section beyond 18.7, where the evidence is an exact string collision
  between two keys whose English differs.**
- **No runtime.** No build, no app launch, no IPC, no database. Consequences:
  which `subscribeDashboardEvents` resolution order wins (18.3); whether Chromium
  actually prunes the ribbon's buttons from the accessibility tree (18.6); whether
  the `lg:` breakpoint is ever reached (below); whether the file-listing commands
  are deduplicated server-side; and all i18n runtime claims, which were derived from
  the i18next 26.0.10 source in `node_modules` and from reading the React tree rather
  than observed.
- **The `lg:` breakpoint is UNVERIFIED and unresolved.** `StatCards.tsx:44` and
  `InsightCards.tsx:54` use `grid-cols-2 lg:grid-cols-4`, and `Dashboard.tsx:247`
  and `:262` use `grid-cols-1 lg:grid-cols-2`, so `lg:` (min-width 1024px) gates
  **four** layout decisions. `tauri.conf.json:16-19` declares exactly one window,
  `label: "main", width: 800, height: 600` - **there is no `dashboard` window in the
  config.** If the 1024x768 window is created at runtime via `WebviewWindowBuilder`
  the layouts are correct; if it reuses this config, `lg:` can never apply and all
  four layouts are dead. Either way a breakpoint sitting exactly on a fixed window
  width makes the layout flip on scrollbar presence. **Which window config is
  actually used was not determined.** The component agent also reported no runtime
  `ResizeObserver` or interval leaks in the folder, which is consistent with either
  answer.
- **`HistoryPanel.tsx:63 key={log.id}` may be a null key for every row.**
  `useCleanupStore.ts:11` types the cleanup-log id as `number | null`; if
  `get_cleanup_logs_cmd` does not always populate `id`, the key is null throughout.
  Not determined - the Rust query was not re-read.
- **`FileBrowser.tsx:64` debounce dependency** (18.35) is latent today and marked
  UNVERIFIED as a live bug.
- **`DuplicatesTab.tsx:20` stale `g.hash` keys** (18.35) depend on hash stability
  across rescans, which was not established. **UNVERIFIED.**
- **Truncation.** All three transcripts end mid-work after their reports were
  delivered; each agent's final report is complete, but the reasoning tails contain
  material past the point where the reports were written. Anything asserted only in
  those tails was dropped rather than guessed at. In the i18n report the delegated
  Rust sweep's summary section was itself truncated, which is why the "~130 strings"
  figure in 18.11 is an estimate.
- **Two delegated negative claims were not independently re-run** and are flagged
  where cited: the absence of a `show-notification` listener for `tray.rs:119-120`,
  and the `file-organized` -> `Popup.tsx:72` path (18.35). Neither is load-bearing
  for any severity assignment in this section. The Rust-side i18n details in 18.11
  and 18.22 come from a delegated read-only sweep and are cited as reported; the
  per-line citations are solid but were not personally re-read by that agent.

### 18.45 Completeness

**All 21 of 21 reports are now absorbed. The audit is complete.** This section closes
the last three: the state layer (`src/store/**`, 4 stores), the component layer
(`src/components/**`, 16 components plus the 8 dashboard components), and i18n across
the 10 locales. The three reports are read-only and produced no project changes: no
file was created, edited, moved or deleted, no `npm`, `cargo`, `tauri` or test command
was run, and no database was opened. Nothing outside
`%TEMP%\opencode\sec18-frontend-state.md` was created or modified, and nothing was
committed to git.
