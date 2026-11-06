# Mouzi Elevate

This is a personal fork of [hsr88/mouzi](https://github.com/hsr88/mouzi), licensed under MIT. The upstream authors built the original tray organizer. Elevate adds a storage dashboard, reviewed cleanup actions, filename suggestions and local operation history. Version 0.2.0 is built locally; this repository does not publish or sign releases.

The supported build target for this fork is Windows 10/11 with WebView2. Upstream Linux/macOS files remain as reference; their runtime and packaging behavior have not been validated for these repairs. The `website/` directory is an unchanged upstream reference, with upstream identity, downloads and analytics. It is not an Elevate website to deploy.

## First launch and folders

New profiles start without watched folders, scheduled cleaning, local AI or Windows autostart. Default organizer rules are initialized together with the detected supported system language. Existing profiles keep their saved preferences and rules.

Add a folder in Settings. New folders are added in Manual mode. Choose the behavior deliberately:

| Mode | Behavior |
|---|---|
| Manual | Organize only through Clean Now or an explicit folder action |
| Silent | Watch arrivals and organize settled files automatically |
| Paused | Keep the configured folder, without organization or cleanup authority |

Clean Now reads the current enabled, nonpaused folders in the backend. An empty configuration performs no organization. Volume roots, dot traversal, links and linked ancestors are rejected as watched roots. Physical aliases refer to the same folder. Conflicting legacy aliases and invalid configured roots are blocked rather than silently removed or remoded.

Organizer rules are global; the stored `folder_id` is retained for compatibility and does not implement per-folder rules. Rules support **move** and **ignore**. They do not implement arbitrary filename renaming. Collision suffixes preserve an occupied destination. A relative destination is resolved under the watched root, including when the source is in a descendant. Explicit absolute destinations remain supported. Filename patterns are regular expressions, and extension tokens are normalized when saved.

Automatic and scheduled organization respect the grace period (300 seconds by default) and a minimum two-second settling interval. Explicit manual cleaning/import sorting bypasses the age grace period. Transient locks are retried with bounded backoff; exhausted retries require a later event or explicit action. Already filed outputs and queued work invalidated by Undo are skipped.

A folder's `.mouziignore` supports exact names, directory names ending in `/`, and `*` wildcards. Missing protection files mean no extra patterns; unreadable protection files pause work with an error. Hidden/system and temporary download files are also skipped by the organizer.

## Storage dashboard

Inventory scans are recursive, independently of the deferred recursive organizer option. They skip links and junctions. The deepest registered root owns an overlapping subtree, including when that child root is paused. Completed root generations publish transactionally. Failed roots retain their last completed snapshot with stale/error metadata; empty configured roots remain visible.

Dashboard totals, categories, root metadata and file lists describe scan data. They are not live free-space measurements. External edits can make inventory stale until another scan. App-managed moves and removals reconcile inventory and invalidate content caches. Categories are Documents, Images, Videos, Audio, Archives, Code and Other; user rule names are separate labels. Installers are included in Archives for classification.

## Reviewed cleanup

Duplicate, large-file, stale-file and empty-directory discovery is limited to current eligible roots. Hard links and physical aliases are excluded from reclaimable duplicates. Discovery reads current files rather than treating cached hashes as deletion evidence.

The backend prepares an expiring, single-use review containing the exact selected actions, criteria, file identities, contents and keeper evidence. Execution accepts its plan ID. It revalidates live group members and the keeper before removal. Changed files, disabled roots, replaced directories and newly nonempty directories stop or skip the affected work. A successful duplicate action retains the keeper. Failed candidates remain available for retry.

File cleanup asks Windows to send files to the Recycle Bin. Windows may permanently delete an item when recycling is unavailable. **A successful request does not guarantee recoverability.** The app checks source absence and uses per-item evidence where available; ambiguous recovery is reported as unverified. Bin count changes do not prove permanent deletion. Empty directories are removed directly and have no restore action in Mouzi.

Windows moves and empty-directory removal use open-handle identity checks and no-replace operations. Native Shell trash still needs a short interval after releasing the victim handle; a hostile concurrent replacement in that interval is not fully excluded. Keep that limitation in mind when using deletion on folders modified by other programs. Non-Windows filesystem race behavior is not certified by this fork.

## Suggestions and optional local AI

Filename and extension heuristics work offline. Learning uses explicit accepted suggestions; automatic organizer history, arbitrary rule names and undone acceptance do not train the classifier. Accepted categories persist through rescans while the file fingerprint matches. Scores rank suggestions and are not measured probabilities.

Suggestions prepare an immutable per-file backend plan before confirmation. It captures the category, contents, eligible root and collision-safe destination. A changed file, root or newly occupied reviewed destination requires another review. Results retain the actual destination and report partial finalization separately from a failed move. Already filed acceptance saves the classification without inventing a move log. Dismissals use file fingerprints and can be reset.

Creating a future extension rule is off by default. The review states its global scope, priority, existing coverage/conflicts and deduplicated count. Such rules apply to eligible watched folders added later as well. Conflicting automation must be edited explicitly.

Local AI is disabled until enabled in Settings. Choose an installed Ollama model there. When needed, the backend contacts only `http://127.0.0.1:11434`, with redirects and environment proxies disabled, bounded responses and request budgets. It verifies the selected model against the model list. Filenames are sent to that local service; file contents are not. Missing models or optional provider failures fall back to offline results without changing their provenance. Switching off Local AI disables future model requests; a request already in flight can finish within its timeout.

## Archives and recovery records

Imports accept `.zip`, `.tgz` and `.tar.gz`. Nested files are flattened with collision-safe names, then sorted using existing rules. Unsafe paths, links and unsupported output filenames are rejected. The source archive is retained.

Default extraction limits are 1 GiB input, 20,000 entries, 256 MiB per file, 2 GiB of extracted file bytes and 120 seconds. TAR header/metadata streaming has an additional bounded allowance. On Windows, each file checks available space with a 128 MiB reserve. Actual streamed bytes are checked as well as declared sizes. Progress and cancellation are available; cancellation interrupts copying or stops between sorted files. Existing timestamps are preserved when valid; absent timestamps keep extraction time. ZIP timestamps have no timezone and are interpreted as UTC.

New imports live under the application's data directory in `archive-imports/`, with a neighboring `.manifest.json`. Manifests identify the source, extracted/remaining entries, sorting results and status. Cancelled, failed and successful data is retained for recovery. Older cache imports are also retained; deleting cache folders can destroy the only extracted copy. Partial sorting errors include the staging path and keep per-file operation records, even when the final import summary cannot be completed.

SQLite stores an operation intent before app-managed file mutations and finalizes history/inventory afterward. Persistence failures are surfaced with actual filesystem outcomes and pending recovery evidence. The journal is recovery evidence, not automatic crash rollback or a finished recovery UI.

Move history supports single Undo and Undo All, including accepted suggestions. If the original name is occupied, Undo restores the file with a suffix. Move records with a saved file identity let Undo verify the file at the recorded destination. Older records without that identity remain visible and require manual restoration when a file is present. An interrupted Undo keeps a recovery record and asks you to inspect the recorded paths before restoring manually. The interface counts restored files and missing files separately. Restore cleanup trash from the Windows Recycle Bin.

## Scheduling and settings

Settings writes patch only the changed preferences. Schedule writes update only schedule fields; committed revisions keep retained windows synchronized. Scheduled cleaning coalesces due slots from the current local date and the preceding six dates into one scan of current files. It claims each local date and time once and saves the check time in the same transaction before scanning. Saving the schedule stops a previous batch from starting more moves. Failed or interrupted claims are not retried automatically. If the clock moves backward, the scheduler revisits today's due slots and keeps existing claims.

## Build and verification

Install Node.js **22.12 through 22.x, or 24.0 and newer**, Rust **1.98.0**, Visual Studio C++ Build Tools with the Windows SDK, and WebView2. `rust-toolchain.toml` pins the Rust compiler. The npm and Cargo lockfiles are committed.

```powershell
npm ci
npm test -- --maxWorkers=2
npm run build
Push-Location src-tauri
cargo test --lib --locked --jobs 2 -- --test-threads=2
Pop-Location
npm run tauri build -- --no-bundle --ci -- --locked --jobs 2
```

The final command produces an unsigned executable at `src-tauri/target/release/mouzi.exe`. Tauri builds the frontend as part of this command. Standard builds produce the executable only. Explicit installer requests fail because installer identity and upgrade behavior still need verification. CI runs both test suites on `main`, `wip/**`, pull requests and manual dispatch. Its log records the checked-out commit, runner and toolchain versions, and the executable's size and SHA-256 digest.

The real Recycle Bin integration test is ignored by default. Run it only on disposable fixtures through the explicit manual CI input or an explicitly chosen local test. Passing ordinary tests does not establish Shell recycling, real cross-volume movement, desktop event timing or installer upgrade behavior.

## Development data

```powershell
npm run tauri dev
```

Debug builds use `cc.mouzi.dev`, the product name Mouzi Development, and `%APPDATA%\mouzi\mouzi-dev\data`. They never register Windows autostart. An absolute `MOUZI_DATA_DIR` override can select another isolated fixture profile; production data and its descendants are rejected. Existing development folders still refer to real files, so choose disposable folders for GUI testing.

Release builds retain `cc.mouzi.app` and `%APPDATA%\mouzi\mouzi\data\mouzi.db`, shared with baseline Mouzi. Side-by-side release installation and graceful installer upgrades still need a separate distribution decision. Quit both apps and preserve the full data directory, including import data and database journals, before an upgrade or migration. This repair has not run a live-profile migration.

`MOUZI-AUDIT.md` and `.omo/` retain historical plans/evidence; their old findings and acceptance claims do not describe this repair's current validation. See the 2026-10-03 independent review and implementation register supplied with this task for current evidence and remaining work.

## License

MIT. Preserve the upstream attribution and [license](LICENSE.md).
