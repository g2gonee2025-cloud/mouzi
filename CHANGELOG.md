# Changelog

All notable changes to this project are recorded here.

This project is a personal fork of [hsr88/mouzi](https://github.com/hsr88/mouzi) (MIT).
Upstream releases are not listed here — see upstream's own changelog for those.
Everything below `0.2.0` is fork work unless marked otherwise.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

Nothing yet.

## [0.2.0] - 2026-09-27

First fork release. Adds a storage dashboard, a cleanup assistant, and a
suggestion engine on top of upstream's file-organisation rules engine, and
fixes several data-loss and injection defects found while building them.

### Added

- **Storage dashboard.** Recursive inventory of every watched folder into
  `file_inventory`, with total files and bytes, per-category distribution,
  per-root breakdown, an age histogram, an activity timeline, a storage ribbon
  and a filterable file browser. Served from a single workspace window.
- **Cleanup assistant.** Four finders — duplicates, large files, stale files,
  empty directories — each producing per-item actions with a per-item result
  status (`skipped` / `ok` / `failed`) rather than an all-or-nothing result.
- **Content-based duplicate detection.** Duplicates are identified solely by a
  full-content [blake3](https://github.com/BLAKE3-team/BLAKE3) digest. File
  size is used only to decide which files are worth hashing, never as evidence
  that two files are identical. Files above 100 MB are hashed in two phases: a
  64 KB sample prefilter, then a full digest, so a sample digest is never
  presented as proof. Digests are cached against `(path, size, mtime)`.
- **Suggestion engine.** A pure-Rust heuristic classifier (extension, filename
  tokens, and co-occurrence learned from your own move history) that always
  works offline. [Ollama](https://ollama.com) is detected at runtime and used
  when present; its output is validated against a fixed category allowlist and
  silently falls back to the heuristic on any failure. Every suggestion is
  previewed before you accept it, and accepting moves rather than deletes.
- **Trash safety.** Removals go to the Windows Recycle Bin via the
  [`trash`](https://crates.io/crates/trash) crate. Destructive actions cannot
  touch a path outside a watched folder.
- **Ten-language UI** (`en`, `de`, `es`, `fr`, `it`, `ja`, `pl`, `ru`, `uk`,
  `vi`) including the new dashboard, cleanup and suggestion surfaces.
- **`CHANGELOG.md`** and a version number that actually moves.

### Fixed

- **Undo reported success when it had failed.** Undo now returns a per-file
  status and restores missing parent directories, and destination collisions
  are resolved with a suffix instead of overwriting the existing file.
- **Duplicate groups could delete every copy, including the one marked
  "kept."** The keeper is now resolved in exactly one place and the button's
  count, the per-file badge and the action list are all derived from it, so
  they cannot disagree.
- **Path traversal into the command layer.** The cleanup command accepted any
  path on the machine. It now refuses anything outside a watched folder, using
  component-wise containment that cannot be fooled by a `..` segment, a
  case-different spelling, or a sibling folder sharing a name prefix.
- **Shell injection in "open folder".** A path containing a single quote broke
  out of an interpolated PowerShell command and executed it. Paths and URLs are
  now passed as process arguments.
- **A watched folder could be a whole drive, or did not exist.** Folder
  selection now validates the input instead of creating whatever it was given.
- **Silent overwrites on move collisions.** A collision within the same second
  produced an identical suffixed name, so the second move destroyed the first.
  A free name is now found by counting, and the rules engine shares that code.
- **Long operations froze the window.** The ten longest commands now run off
  the main thread, so hashing gigabytes of file content or waiting on a local
  model no longer locks the UI.
- **Window capability mismatch.** The consolidated workspace window was not
  listed in `capabilities/default.json`, so event listening, the file dialog
  and notifications were all denied to it.

### Known limitations

Recorded honestly rather than left to be discovered:

- The watcher is still non-recursive. Folder organisation applies to files in
  watched folders themselves, not their subtrees. Making it recursive requires
  simultaneous work on self-trigger suppression, or the app will re-queue the
  files it just moved.
- Removal is reversible **while the Recycle Bin can accept the item**. When the
  per-volume bin quota is exceeded, or the item is too large, or the path is on
  a network volume, Windows deletes permanently and the operation still reports
  success. The app cannot detect this.
- A file replaced by an equal-size file within the same second can be served a
  stale cached digest and appear as a duplicate when it is not.
- Eleven duplicate groups above the 500-copy hashing cap are never content
  verified and are reported as unverified rather than as duplicates.
- Search results are capped at 200 rows with no paging; the true total is
  shown alongside.
