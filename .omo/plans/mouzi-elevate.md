# Mouzi Elevate — Work Plan

**Status:** Draft for Momus review
**Date:** 2026-08-24
**Operator intent:** Fork the proven, MIT-licensed `hsr88/mouzi` (Tauri 2 + Rust + React 19 + SQLite system-tray file organizer, 828★) and elevate it into a desktop **dashboard + file management + organization assistant**. Don't reinvent the wheel — reuse mouzi's rules/watcher/scheduler/DB; add features on top.

---

## 1. Decisions (locked, per Metis pre-planning)

| # | Fork | Decision |
|---|---|---|
| D1 | Personal vs publishable | **Personal-use quality bar + fork hygiene** (MIT attribution, keep `cc.mouzi.app` id). No installer signing/updater/store. |
| D2 | AI integration | **Heuristic-first, Ollama-optional.** Pure-Rust classifier (extension + filename tokens + co-occurrence from `action_logs`) always works offline; Ollama detected at runtime (`GET localhost:11434/api/tags`), optional enhancement behind a trait, 2s timeout, strict JSON parse with fallback. |
| D3 | Dashboard form | **New 1024×768 window** `"dashboard"`; entry = new tray menu item + settings link. |
| D4 | Dedup scope | **Recursive within watched roots only.** Never whole drives. |
| D5 | Reversibility | **Two-tier:** (1) fix broken undo → proper move-back with per-file status; (2) deletions → Windows Recycle Bin via `trash` crate. Restore from Recycle Bin stays native (trash crate can't restore). |
| D6 | Ollama runtime | **Rust backend** (`ureq` blocking in spawned std thread — app has no tokio). Frontend fetch hits webview CORS. |
| D7 | Dedup hashing | **Size-prefilter → hash only same-size groups; persistent cache** table keyed `(path, size, mtime)` for incremental rescans. |
| D8 | Rule-action stubs | `execute_rule` (`rules.rs:185-206`) only implements `move`/`ignore` — `delete`/`rename` return `Err("Unknown action")`. **Hide `delete` in rules UI; never route cleanup through `execute_rule`.** |
| D9 | Frontend tests | No vitest exists. Add vitest **only for pure-logic helpers**; skip component tests in v1. `cargo test` = 6 tests baseline. |
| D10 | Migration style | Additive-only: `CREATE TABLE IF NOT EXISTS` + conditional `ALTER` (pattern `db.rs:159-188`). Scanner uses its own read connection. |

## 2. Target repo & baseline facts

- Clone: `C:\Users\Hagop Ghazarian\Desktop\mouzi-elevate` (HEAD `c4eac33`, v0.1.6)
- Toolchain ready: Node 22.23.2, pnpm 11.23.0, Rust 1.98.0, WebView2 151.0.4129.101 ✓
- Repro cuts (verified anchors):
  - Watcher non-recursive: `watcher.rs:317` (`RecursiveMode::NonRecursive`)
  - Broken undo: `commands.rs:126-170` (bare `fs::rename`, `let _` swallows errors, `commands.rs:139`)
  - Watcher self-trigger guard: `watcher.rs:12` (`ignored_files`, 5s `IGNORE_DURATION_SECS`)
  - Global event emit: `watcher.rs:110` (`file-organized` broadcast)
  - Sync initial scan: `watcher.rs:208-252`
  - Rules engine: `rules.rs:185-206`; collision suffix precedent `rules.rs:194-196`
  - Schema/migrations: `db.rs:107-206`; weekly stats `db.rs:425-436`
  - CAPTURING: `capabilities/default.json:5` windows array — **new window MUST be added here**
  - Single-instance focus: `lib.rs:71-77` — only knows popup/settings
  - Tray menu/entry: `tray.rs:11-48` (menu), `56-94` (popup), `128-149` (settings)
  - Router: `App.tsx:32`, `107-111` (hash-based `#/popup` `#/settings`); events `App.tsx:53-57`
  - Store: `useAppStore.ts` (single global Zustand store)
  - i18n: 10 locales, `fallbackLng: 'en'` (`i18n/index.ts:33`)
  - Deps: rusqlite 0.32 bundled, notify 7. No tokio. No trash crate. No `file_inventory` table (only `action_logs`, `rules`, `watched_folders`, `settings`).

## 3. Milestones (critical path = recursive scan/inventory module — all features consume it)

### M0 — Fork hygiene + baseline (½ day)
- README: fork notice + MIT attribution to upstream.
- Verify `cargo test` (6 tests) + `npm run build` (tsc) green from the clone.
- Add vitest devDep; one smoke test to prove infra works.
- **Exit:** baseline passes.

### M1 — Trust core: fix undo + trash safety (1 day)
- Rewrite `undo_action_cmd`/`undo_all_cmd`: move-back that recreates missing dirs, resolves collisions (timestamp suffix), returns per-file status `ok | collision | missing | cross-device | failed`, never `let _`-swallows.
- Add `trash` crate; new `delete_to_trash` path for cleanup features.
- New `cleanup_actions` audit table (id, path, action, prev_path, dest, status, ts, undoable).
- Watcher suppression for restore ops (extend beyond 5s window — broad enough for batches).
- Hide `delete` rule action in rules UI.
- **Exit:** unit tests for move-back fixtures (collision, missing-dir, cross-device); `cargo test` green.

### M2 — Scanner + Dashboard (the flagship, 2-3 days)
- New `scan.rs` (UI-agnostic, zero Tauri deps): recursive walk of watched roots, `file_inventory` table (path, size, mtime, type, root_id), background thread, progress events, own read connection.
- New dashboard window: treemap of watched-folder storage, file-type distribution, activity timeline (from `action_logs`), weekly stats, largest files, rule-hit counts.
- Wire: `capabilities/default.json` +`dashboard`; tray menu item; `App.tsx` route; dashboard-local state (NOT `useAppStore`).
- **Exit:** `npm run tauri dev` smoke — files created in a test watched folder appear in dashboard treemap/stats.

### M3 — Smart cleanup (1-2 days, depends M1+M2)
- Dedup: size-prefilter → hash same-size groups (BLAKE3 not needed — use blake3 or SHA-256; prefer `blake3` crate) → persistent hash cache keyed `(path,size,mtime)`.
- Large-file finder (>N MB, configurable), stale files (>1 yr untouched), empty-folder sweep (recursive awareness needed — watcher is non-recursive).
- All: **preview → confirm → trash + audit → restore via undo trail**.
- **Exit:** unit tests for size-prefilter grouping + cache invalidation (mtime change); manual smoke with a temp fixture tree.

### M4 — AI classification + rule learning (1-2 days, depends M2; parallel with M3)
- Heuristic classifier first: extension + name tokens + co-occurrence from `action_logs` → suggested category/rule.
- Ollama adapter behind trait (`classify.rs` → `AiProvider`): `ureq` GET `/api/tags` runtime detect; POST `/api/generate` with strict `serde_json` parse, 2s timeout, fallback to heuristic.
- Suggestion preview UI (unclassified files → suggested rule + destination, user confirms).
- Rule learning v1: from in-app confirmations only (NOT Explorer manual moves — not observable without destination watching).
- **Exit:** unit tests for classifier pure fn; Ollama-absent path degrades gracefully (simulated via mock provider).

### M5 — Polish (1 day, depends M2-M4)
- i18n: en.json first (fallback covers rest), mechanical copy to 10 locales.
- Empty states, loading states, perf tuning (event batching — avoid `file-organized` storm reloading dashboard), docs.
- **Exit:** full `cargo test` + `npm run build` green; manual smoke of all four features.

## 4. Parallelization & dependencies
- M1, M2-backend, i18n-string collection can all start after M0 (different files; coordinate schema via additive-only rule).
- M3 and M4 both depend only on M2 → run concurrently after M2.
- **Never start M4 before M2** — AI has no data source (needs inventory + unclassified view).

## 5. Deferred (explicitly cut from v1)
- Whole-drive/multi-disk scanning
- ONNX local tagging (upstream roadmap item)
- Auto-applied AI rules with no confirmation
- Rule learning from Explorer manual moves (needs destination-folder watching — later)
- Implementing the `rename` rule action
- Undo for Recycle Bin deletions (native second chance)
- Cross-machine sync, telemetry, long-term stats warehousing
- Per-folder dashboard breakdowns, deep treemap drill-down, notification prefs

## 6. Hard constraints (operator + inherited repo)
- **NEVER:** `as any` / `@ts-ignore` / `@ts-expect-error` / `todo!()` / `unimplemented!()` / empty catch blocks; delete failing tests to pass; commit unless explicitly asked.
- New backend modules keep zero Tauri deps (UI-agnostic, like `watcher.rs`).
- Every destructive command returns per-item status; no `Result<()>` for multi-file ops.
- AI never a hard dependency; always preview + confirm for destructive/suggested actions.
- MSVC toolchain needed for Rust builds on Windows — target exists (Rust 1.98 installed; verify `link.exe` present, else install VS Build Tools — check `cargo build` early in M0).

## 7. Verification commands (run at each exit gate)
- `cargo test` (baseline 6 tests → grows)
- `cargo clippy -- -D warnings`
- `npm run build` (tsc strict)
- `npm run tauri dev` manual smoke per milestone
- Dedicated unit tests: classifier pure fn, dedup size-prefilter + cache invalidation, undo move-back fixtures, migration idempotency (init_db twice), trash send on temp files only

## 8. Open item for operator (default assumed)
- App/feature naming for the dashboard if user wants to rebrand later — **default: keep mouzi branding**, note in README as fork. No rename in v1.