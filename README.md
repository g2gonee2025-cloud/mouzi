# Mouzi

> **Fork notice:** This is a personal fork of [hsr88/mouzi](https://github.com/hsr88/mouzi) (MIT), extended with a storage dashboard, smart cleanup (duplicates, large files, stale files, empty directories), AI-assisted classification, a Recycle Bin path for deletions, and an undo and audit trail. All credit for the original organiser goes to the upstream authors.

> **Your downloads, tamed.**

Mouzi is a file organiser that lives in your system tray and keeps folders such as Downloads automatically tidy. It runs quietly in the background, watches folders you select, and moves files according to rules you define.

> **What Mouzi does not do:** it does not rename files. The rules engine implements only two actions, `move` and `ignore`; any other action is rejected with an `Unknown action` error (`src-tauri/src/rules.rs:190`). Renaming is deferred. Mouzi does not delete files by default, and it has no automatic updater.

[![Product Hunt](https://img.shields.io/badge/Product%20Hunt-Launch-orange?logo=producthunt&color=ff6154)](https://www.producthunt.com/products/mouzi?launch=mouzi)
[![Reddit](https://img.shields.io/badge/Reddit-r%2FMouzi-FF4500?logo=reddit)](https://www.reddit.com/r/Mouzi/)
[![X](https://img.shields.io/badge/X-@hsrvibe-black?logo=x&logoColor=white)](https://x.com/hsrvibe)

[![Windows](https://img.shields.io/badge/Windows-10%2F11-blue?logo=windows)](https://mouzi.cc)
[![Linux](https://img.shields.io/badge/Linux-AppImage%2Fdeb%2Frpm-yellow?logo=linux)](https://mouzi.cc)
[![Tauri](https://img.shields.io/badge/Built%20with-Tauri-FFC131?logo=tauri)](https://tauri.app)
[![Rust](https://img.shields.io/badge/Backend-Rust-000000?logo=rust)](https://www.rust-lang.org)
[![React](https://img.shields.io/badge/Frontend-React-61DAFB?logo=react)](https://react.dev)
[![License](https://img.shields.io/badge/License-MIT-green.svg)](LICENSE)

Upstream release artefacts:

[![Download upstream Mouzi](https://a.fsdn.com/con/app/sf-download-button)](https://sourceforge.net/projects/mozui/files/latest/download)

---

## Screenshots

These are upstream screenshots and predate the fork.

<img width="640" height="360" alt="mouzigiflinux_maly" src="https://github.com/user-attachments/assets/32dc0286-fdb0-411e-8237-f589c2f17082" />

<img width="500" height="361" alt="resized-1_1781356999" src="https://github.com/user-attachments/assets/22555e17-b58a-4a70-9da2-47d8f778b9ea" />
<img width="500" height="361" alt="resized-2_1781357019" src="https://github.com/user-attachments/assets/75ddb288-ff70-4b78-927a-b31e31cbcecd" />
<img width="500" height="314" alt="resized-mouzilinux" src="https://github.com/user-attachments/assets/2ed8b18f-4833-40f2-ab19-9d0a63014f88" />
<img width="500" height="315" alt="resized-mozuilinux2" src="https://github.com/user-attachments/assets/fb6bca80-0b5e-4622-8efc-3ceca186a829" />

---

## Prerequisites

| Requirement | Version | Notes |
|-------------|---------|-------|
| Windows | 10 or 11 | [Microsoft Edge WebView2 Runtime](https://developer.microsoft.com/en-us/microsoft-edge/webview2/) required; pre-installed on most systems |
| Linux | any modern distro | `libwebkit2gtk-4.1`, `libayatana-appindicator3` |
| Node.js | 22 or newer | Only needed to build from source |
| Rust | latest stable | Only needed to build from source |
| Visual Studio Build Tools | with Windows SDK and MSVC | Windows only, only to build from source |
| Ollama | optional | Only needed for AI-assisted classification, see [Using Ollama](#using-ollama) |

You do not need a local model, an account, or a network connection to use Mouzi. Every feature works offline, with the single exception of AI-assisted suggestions, which need a locally running Ollama.

---

## Features

### Silent by default
- Runs in the background once started, and starts with Windows if autostart is enabled
- Organises new files as they arrive
- Shows a desktop notification with the count of organised files
- Nothing is sent to a server; see [Privacy and network access](#privacy-and-network-access)

### Rules engine
Default rules, inserted on first run (`src-tauri/src/db.rs:343`):

| Rule | Extensions | Destination |
|------|-----------|-------------|
| Images | `jpg`, `jpeg`, `png`, `gif`, `webp`, `bmp`, `svg`, `ico`, `heic`, `heif` | `Images` |
| Documents | `pdf`, `doc`, `docx`, `xls`, `xlsx`, `ppt`, `pptx`, `txt`, `rtf`, `odt` | `Documents` |
| Archives | `zip`, `rar`, `7z`, `tar`, `gz`, `bz2`, `xz` | `Archives` |
| Installers | `exe`, `msi`, `msix`, `appx` | `Installers` |
| Music | `mp3`, `wav`, `flac`, `aac`, `ogg`, `wma`, `m4a` | `Music` |
| Videos | `mp4`, `avi`, `mkv`, `mov`, `wmv`, `flv`, `webm` | `Videos` |
| Others | `*` (catch-all, priority 99) | `Others` |

Note that the storage dashboard uses a separate, coarser set of categories from the rules engine: `Documents`, `Images`, `Videos`, `Audio`, `Archives`, `Code`, and `Other` (`src-tauri/src/scan.rs:44`). Installer files therefore appear under `Other` on the dashboard, even though the rules engine moves them to `Installers`.

### Customisable rules
- Rules match on extensions or on a regular expression
- Destination paths accept the placeholders `{year}`, `{month}`, `{day}`, `{extension}`, `{filename}`
- Rules are ordered by priority and the first match wins
- Every rule can be enabled or disabled

### Ignore rules
- A `.mouziignore` file per folder, similar in spirit to `.gitignore`
- Supports wildcards (`*.tmp`), exact names (`.DS_Store`) and directories (`node_modules/`)
- Checked both when a file event arrives and again in `process_file` as a safety net (`src-tauri/src/rules.rs:207`)

### Folder modes
Each watched folder runs in one of three modes (`src-tauri/src/db.rs:16`):

| Mode | Behaviour |
|------|-----------|
| `silent` | Files are organised as they arrive. This is the default |
| `manual` | Files are collected and only processed when you run Clean Now |
| `paused` | The folder is watched but nothing is moved |

### Storage dashboard
- Recursive file inventory held in SQLite, with size, modified time and category
- Per-category totals, largest files, and age buckets
- Refresh is manual; the walk reports progress while it runs

### Smart cleanup
Runs against the inventory, and each action is recorded:

| Action | Effect |
|--------|--------|
| `trash_duplicate` | Sends duplicate files to the Recycle Bin |
| `trash_large` | Sends large files to the Recycle Bin |
| `trash_stale` | Sends stale files to the Recycle Bin |
| `remove_empty_dir` | Removes directories left empty once files are gone |

Deletions go through the `trash` crate (`src-tauri/src/safe_fs.rs:186`), so on Windows they land in the Recycle Bin rather than being removed permanently. Empty-directory removals are not undoable, because trash deletions are not logged for restore (`src-tauri/src/cleanup.rs:385`).

### Google Takeout import
- Imports `.zip`, `.tgz`, and `.tar.gz` archives
- Files are extracted to a staging folder and sorted with your existing rules

### AI-assisted classification
- Filename and extension heuristics run locally and need nothing installed
- Learned hints are mined from your own action log, so the classifier adapts to the moves you make by hand (`src-tauri/src/classify.rs:315`)
- If a local Ollama instance is detected, it can additionally classify filenames over `localhost`; see [Using Ollama](#using-ollama)

### History and undo
- Every move is logged locally in SQLite
- A single move can be undone, or the whole log can be undone in one action (`undo_action_cmd` and `undo_all_cmd`, `src-tauri/src/commands.rs:422`)
- History can be cleared at any time

### Scheduled cleaning
- Up to four times per day, controlled from settings (`src-tauri/src/db.rs:108`)

### Grace period and lock checking
- New files are ignored until they have settled, 300 seconds by default (`src-tauri/src/db.rs:249`)
- Files that another process still has open are skipped

### Multi-language
The interface language is auto-detected from the system, and falls back to English when unsupported. English, Polish, Italian, German, French, Russian, Japanese, Vietnamese, Spanish, and Ukrainian are available.

### Dark mode
Follows the system theme, or can be forced to light or dark in settings.

---

## Privacy and network access

Mouzi has no telemetry enabled by default and no cloud component. Concretely:

- **Nothing leaves your machine.** There is no analytics endpoint, no crash reporting service, and no account.
- **File names stay local** unless you are running Ollama. When a local Ollama instance is detected, file names are sent to `http://localhost:11434`, an HTTP endpoint on your own machine, to be classified (`src-tauri/src/classify.rs:495`). That request never leaves the loopback interface.
- **If Ollama is not running, nothing is sent anywhere.** Detection is a probe to `http://localhost:11434/api/tags`; if it fails, the built-in heuristics are used on their own.
- **System files are ignored.** `desktop.ini`, `Thumbs.db`, `.DS_Store` and other OS hidden files are not touched.
- **All data is stored locally** in your user profile; see [Data location](#data-location).

---

## First run

1. Build or download the application, then start it.
2. The popup appears on first launch so you can see the app is running (`src-tauri/src/lib.rs:130`).
3. Open **Settings** and add a folder to watch. Downloads is the usual starting point.
4. Choose a folder mode. `silent` organises immediately; `manual` waits for you to run Clean Now.
5. Review the default rules and adjust destinations if you want a different layout.
6. Optionally enable autostart so Mouzi comes back after a reboot.

---

## The tray menu

Mouzi has no taskbar or dock window. Everything is reached from the tray icon, and the tray menu is the only route to three shipped features: Suggestions, Dashboard, and Cleanup. Left-click the tray icon to toggle the small popup showing recent actions and a Clean Now button.

Right-click the tray icon for the menu (`src-tauri/src/tray.rs:11`):

| Menu item | What it does |
|-----------|--------------|
| Clean Now | Scans watched folders immediately and applies rules, regardless of folder mode |
| Suggestions | Review classification suggestions and learned hints, and accept or reject them |
| Dashboard | Opens the storage dashboard: category totals, largest files, age buckets |
| Cleanup | Runs duplicate, large, stale, and empty-directory cleanup |
| Settings | Opens the shared workspace window on the settings route |
| (separator) | Visual divider |
| Quit | Exits the application |

There is no separate Settings window. Selecting it reuses the workspace window on a different route (`src-tauri/src/tray.rs:168`).

Window inventory:

| Window | Size | Created | Shown |
|--------|------|---------|-------|
| `main` | 800 by 600 | At startup, from `tauri.conf.json:16` | Never. Declared `visible: false` and `skipTaskbar: true`, and no code path shows it |
| `popup` | 300 by 420, frameless, always on top, no taskbar entry | On demand, `src-tauri/src/tray.rs:75` | On tray left-click |
| `app` | 1100 by 820 | On demand, `src-tauri/src/tray.rs:178` | From any tray item that needs the full interface |

The workspace window is reused across routes rather than reopened. If it already exists, Mouzi changes the URL fragment and the title instead of building a second window (`src-tauri/src/tray.rs:171`), so selecting a different tray item while it is open navigates the existing window rather than stacking new ones. It has a minimum size of 800 by 600.

---

## Watched folders

Folders are managed in **Settings**. For each folder you can:

| Option | Values | Meaning |
|--------|--------|---------|
| Path | any readable directory | The folder to watch |
| Mode | `silent`, `manual`, `paused` | See [Folder modes](#folder-modes) |

To stop Mouzi acting on a folder without deleting it, set its mode to `paused`. Removing a folder deletes it and its rules. There is no separate enable and disable switch: the `enabled` column on the `watched_folders` table is created and read, but no command updates it (`src-tauri/src/db.rs:447` inserts `1`, and `update_folder_mode` at `src-tauri/src/db.rs:463` is the only update).

Notes on behaviour:

- **Rules are global, not per folder.** One rule set is shared by every watched folder. Rule matching loads the whole rule list ordered by priority and takes the first match (`get_rules` in `src-tauri/src/db.rs:369`, consumed by `find_matching_rule` in `src-tauri/src/rules.rs`), with no filter on which folder the file came from. The `folder_id` column on the rules table is not used to scope matching. A rule therefore applies to every watched folder.
- Destination paths are resolved relative to the folder containing the file, not the watched-folder root. For a file directly inside the watched folder, `Images` means `<watched folder>/Images`; for a file in a subdirectory, the destination is created inside that subdirectory (`src-tauri/src/rules.rs`). A destination stored as an absolute path is used as-is, for compatibility with rules saved by older versions.
- If a destination file already exists, Mouzi does not overwrite it. It appends a numeric suffix to make the name unique (`src-tauri/src/safe_fs.rs:103`).
- A `.mouziignore` file in the watched folder is honoured automatically.
- Because rules are shared, a catch-all rule placed near the top will match files in every watched folder, not just one.

---

## Writing a rule

Open **Settings** and add a rule. A rule is a name, a priority, a match condition, a destination, and an action. Remember that the rule set is shared across every watched folder.

| Field | Meaning |
|-------|---------|
| Name | Label shown in the list; purely descriptive |
| Priority | Lower numbers are evaluated first. The first match wins |
| Extensions | Comma-separated list without dots, for example `pdf,docx`. The special value `*` matches any extension |
| Pattern | Regular expression tested against the file name. Leave empty to match on extensions alone |
| Destination | Path relative to the folder containing the file. Placeholders are expanded |
| Action | `move` or `ignore`. No other value is accepted |
| Enabled | On or off. A disabled rule is skipped entirely |

Extension and pattern are combined with AND, not OR (`matches_rule` in `src-tauri/src/rules.rs`). A rule matches only when both of the following hold:

- The extension list contains `*`, or contains the file's extension. **A rule with an empty extension list never matches anything**, even if its pattern is correct.
- The pattern is empty, or the pattern is a valid regular expression that matches the file name. An invalid regular expression is treated as no match rather than raising an error.

Destination placeholders:

| Placeholder | Expands to |
|-------------|-----------|
| `{year}` | Four-digit year |
| `{month}` | Two-digit month |
| `{day}` | Two-digit day |
| `{extension}` | Extension without the dot |
| `{filename}` | File name without extension |

Worked example. To sort invoices into a dated folder regardless of extension, create a rule with:

| Field | Value |
|-------|-------|
| Priority | 1 |
| Extensions | `*` |
| Pattern | `.*invoice.*` |
| Destination | `Documents/{year}/{month}` |
| Action | `move` |

The extensions field is set to `*` rather than left empty, because the two conditions are combined. An invoice arriving as `invoice_2026_05.pdf` then lands in `Documents/2026/05/`.

Keep the catch-all rule last. Because the first match wins, any rule above it whose extension list is `*` will shadow every rule below it.

---

## Using Ollama

AI-assisted classification is entirely optional and entirely local.

**Turning it on**

1. Install [Ollama](https://ollama.com).
2. Pull a model, for example:

   ```bash
   ollama pull llama3.2
   ```

3. Leave the Ollama server running. Mouzi probes `http://localhost:11434/api/tags` when it needs a classification; if the probe succeeds, filenames are sent to that local endpoint for a label.

**Turning it off**

Mouzi has no setting for this, because there is nothing to switch off in the application. Stopping the Ollama server is the switch:

```bash
ollama stop llama3.2
```

Stopping or uninstalling Ollama entirely has the same effect. With no local endpoint answering, Mouzi falls back to its built-in heuristics and the learned hints mined from your own history. No file name is sent anywhere.

Be clear about the trust boundary: Ollama is a separate process, and a model you have pulled is third-party code. The prompt sent is your file name, so only run a model you are willing to see those names.

---

## Data location

Everything Mouzi knows lives in a single SQLite database named `mouzi.db`, inside the data directory resolved by the [`directories`](https://docs.rs/directories) crate as `ProjectDirs::from("cc", "mouzi", "mouzi")` (`src-tauri/src/lib.rs:102`).

| Platform | Location |
|----------|----------|
| Windows | Under `%APPDATA%`, in a `mouzi` directory |
| Linux | Under `$XDG_DATA_HOME`, usually `~/.local/share/mouzi` |
| macOS | Under `~/Library/Application Support` |

The database holds your watched folders, rules, settings, the file inventory, the action log, and any pending cleanup actions. It is a single file with no external dependencies.

---

## Backing up

To back up, quit Mouzi from the tray menu and copy `mouzi.db` to a safe location. To restore, quit Mouzi, put the file back, and start it again.

If you do not quit first, the copy may be taken while a write is in progress. SQLite's own integrity checks will catch a bad copy, but you may have lost the most recent action log entries.

Mouzi never writes outside the folders you have explicitly asked it to watch, except when moving files to the destinations your rules define. Those moves are not reversible by a database restore; use History and Undo for that, which operates on the log inside the database.

---

## Resetting

To clear all history and start the action log again, use the clear history control in the interface. To remove everything, including settings, rules, the inventory, and the file itself: quit Mouzi, delete `mouzi.db`, and start Mouzi. It recreates the database and reinstalls the default rules on next launch.

This does not move any files back. Files already moved are not tracked outside the database, so clear history while you still need to undo something is a bad idea.

---

## Architecture

```
+---------------------------------------------------------+
|  Frontend (React 19 + TypeScript + Tailwind)            |
|  +- Popup window, 300x420, frameless                     |
|  +- Workspace window, 1100x820, shared                   |
|     hosts settings, dashboard, suggestions, cleanup      |
+---------------------------------------------------------+
|  Tauri 2.x Bridge                                        |
+---------------------------------------------------------+
|  Backend (Rust)                                          |
|  +- File watcher (notify crate)                          |
|  +- Rules engine (move / ignore only)                   |
|  +- Inventory scanner and classifier                     |
|  +- Cleanup with Recycle Bin (trash crate)               |
|  +- Scheduler (time-based cleaning)                      |
|  +- SQLite database (rusqlite)                           |
|  +- Archive extraction (Takeout import)                  |
|  +- System tray and notifications                        |
+---------------------------------------------------------+
```

---

## Run from source

From this project directory, not from a global `tauri` binary:

```bash
npm install
npm run tauri -- dev
```

On Windows, `start-mouzi.bat` in this folder runs the same command.

### Setup from a fresh clone

```bash
# Clone this fork
git clone https://github.com/g2gonee2025-cloud/mouzi.git
cd mouzi

# Optionally add upstream as a read-only reference
git remote add upstream https://github.com/hsr88/mouzi.git
git remote -v
```

`origin` is this fork and is the remote you push to. `upstream` is the original project, kept for reference and for pulling in upstream fixes; treat it as read-only.

Then install and run:

```bash
# Install frontend dependencies
npm install

# Run in development mode, with hot reload for frontend and Rust
npm run tauri dev
```

### Build from source

```bash
# Production build
npm run tauri build
```

Output lands in `src-tauri/target/release/bundle/`. The bundle targets are declared as `all` (`src-tauri/tauri.conf.json:31`), which on Windows means the NSIS installer and the MSI package. There is no portable build target in this fork.

---

## Troubleshooting

**The app will not start, and the dev server reports the port is in use**

The dev server is pinned to port 1420 with `strictPort: true` (`vite.config.ts`), so Vite will not fall back to another port; it fails instead. Something else is already listening on 1420. Find it and stop it:

```powershell
Get-NetTCPConnection -LocalPort 1420 | Select-Object OwningProcess
Stop-Process -Id <pid>
```

On Linux and macOS, use `lsof -i :1420`. HMR uses 1421, so free both if you are seeing websocket errors.

**Nothing happens when I drop a file into a watched folder**

Work through these in order:

1. Is the folder's mode `manual` or `paused`? `manual` waits for Clean Now, and `paused` does nothing at all. Note that mode is per folder but rules are global, so a catch-all rule can match files in a folder you did not expect.
2. Has the file passed the grace period? The default is 300 seconds, so a file you just created will be deliberately left alone.
3. Is another program holding the file open? Lock checking skips locked files.
4. Does a `.mouziignore` entry match it?
5. Does any rule match? A rule needs its extension list to contain `*` or the file's extension, so a rule with an empty extension list never fires. Because the first match wins, a rule above with `*` in its extension list shadows everything below it.

**A file was moved and I want it back**

Use History and Undo. Undo works from the action log, so do not clear history first.

**A deletion went to the wrong place**

Cleanup uses the Recycle Bin on Windows, so the file should be recoverable from it. Empty-directory removals are the exception: they are removed outright and are not undoable.

**The popup does not appear when I click the tray icon**

Some Linux desktops reserve the first tray slot for other items. The app already calls `set_activation_policy(Accessory)` on macOS to stay out of the dock, but on Linux the icon may be pushed out of the visible tray overflow. Check the overflow area.

**Ollama suggestions are not appearing**

Confirm the server is up and the model is pulled:

```bash
ollama list
```

Mouzi uses the model name `llama3.2` (`src-tauri/src/classify.rs:481`). If that model is not present, classification falls back to the local heuristics. Requests also time out quickly, 2 seconds to connect and 15 seconds in total, so a slow machine will fall back rather than block.

**The window is blank or the app crashes on Linux**

Confirm `libwebkit2gtk-4.1` and `libayatana-appindicator3` are installed. This fork targets Wayland, and the upstream project carried a Wayland crash workaround that is retained.

**I want to see what is actually stored**

The database is a plain SQLite file, so any SQLite client will open it. Copy it somewhere else first, as described under [Data location](#data-location).

---

## Uninstalling

Quit Mouzi from the tray menu first, so the watcher releases your folders and no further moves happen.

On Windows, use the standard uninstaller from Settings, Apps, Installed apps. On Linux, remove the package you installed.

Then delete the data directory listed under [Data location](#data-location) to remove the database, the file inventory, and your rules. Leaving it in place does no harm, and keeping it means a reinstall picks up your configuration.

Mouzi has no updater and no autostart entry that survives uninstall beyond the Windows registry value, which the uninstaller removes. If autostart was enabled and the entry remains, delete it from Task Manager, Startup tab.

---

## Roadmap

### Already implemented

Default rules, multi-language support, dark mode, history and undo, single-move and undo-all, Windows autostart, custom rules shared across watched folders, folder modes (`silent`, `manual`, `paused`), system and hidden files ignored, browser temporary files ignored, `.mouziignore` with inline comments, grace period, file lock check, single-instance guard, first-run popup visibility, clickable notification, Linux port, Google Takeout archive import, Wayland crash workaround, storage dashboard, duplicate, large, stale and empty-directory cleanup, Recycle Bin deletion via the `trash` crate, AI-assisted classification, and learned hints from your own manual moves.

### Upcoming

- [ ] Rename files, as a per-rule action alongside `move` and `ignore`
- [ ] Rename "Clean Downloads" to "Organize Downloads" to avoid implying that files will be deleted ([#51](https://github.com/hsr88/mouzi/issues/51))
- [ ] Detect extensionless files by magic bytes and route them through the existing rules engine ([#49](https://github.com/hsr88/mouzi/issues/49))
- [ ] Confirmation dialog for the Delete History button
- [ ] Batch group selected files
- [ ] Suggest mode, with modal confirmation per file
- [ ] Custom notification after intercepting files, for example "Dangerous file moved by Mouzi"
- [ ] Extension normalisation rule, with a per-rule toggle and custom mappings such as `.jpeg` to `.jpg`
- [ ] Rename the "Edit" button to "Save" in the rule editor
- [ ] Better error messages for invalid rules and input
- [ ] Optional screenshot cleanup, moving screenshots older than a chosen age to the Recycle Bin
- [ ] Windows Explorer context menu, "Add to Mouzi"
- [ ] npm wrapper for cross-platform CLI install
- [ ] macOS port

### Deliberately out of scope

- Auto-update. There is no updater and no update-check endpoint. The About screen links to the upstream download page. This fork is a locally built, unsigned, single-user application
- Portable build target

---

## Community use case

A computer repair technician uses Mouzi to automatically move ScreenConnect installers out of the Downloads folder before less technical users can open them, adding an extra layer of friction against remote access scams.

[Read the case study: how Mouzi is being used to reduce ScreenConnect scam risk](https://straycode.dev/blog/how-a-simple-downloads-organizer-is-being-used-to-stop-screenconnect-scams)

> Mouzi is a file organiser, not antivirus or endpoint security software. Filename-based rules should be used as an additional safeguard, not as a replacement for security software and user education.

---

## Help translate Mouzi

Want to use Mouzi in another language or improve an existing translation? Community translations are welcome, and you do not need to work on the rest of the application.

- Read the step-by-step translation guide in [CONTRIBUTING.md](CONTRIBUTING.md).
- Browse the current locale files in `src/i18n/locales` and `src-tauri/src/i18n.rs`. The tray menu strings are a separate Rust table and need changing too.
- Translation updates can be submitted directly as a pull request.

If you want to add a new language, open an issue first so the language code can be confirmed and duplicate work avoided.

---

## Upstream releases

The following are upstream 0.1.5 artefacts, published by the original project. This fork is at version 0.2.0 and does not publish its own binaries.

### Windows

| Installer | Size | Best for |
|-----------|------|----------|
| [`Mouzi_0.1.5_x64-setup.exe`](https://mouzi.cc/download) | about 3.7 MB | Regular users, auto-installer |
| [`Mouzi_0.1.5_x64_en-US.msi`](https://mouzi.cc/download) | about 5.3 MB | Enterprise and Active Directory |

### Linux

| Package | Size | Best for |
|---------|------|----------|
| [`Mouzi_0.1.5_amd64.AppImage`](https://mouzi.cc/download/linux) | about 84.4 MB | Universal, works on most distros |
| [`Mouzi_0.1.5_amd64.deb`](https://mouzi.cc/download/linux) | about 7.1 MB | Debian, Ubuntu, Mint, Pop!_OS |
| [`Mouzi-0.1.5-1.x86_64.rpm`](https://mouzi.cc/download/linux) | about 7.1 MB | Fedora, openSUSE, RHEL |

SHA-256 checksums, as published upstream:

```text
Mouzi_0.1.5_x64-setup.exe:   ee9b173aaa10c03fac5c201345cb388115bbb0c78e2e294a92d18fad454e99a2
Mouzi_0.1.5_amd64.AppImage:  6801341766201a7a2a6c45fb3217757567b27507bef74b5e7bf6d847fdbfc5bb
Mouzi_0.1.5_amd64.deb:       73301c563a7c00dd4a68d143550d953ca8644c206a7603b4f3f8be066e0efe6a
Mouzi-0.1.5-1.x86_64.rpm:    62a2e565f4dcd3e6a19c19122d4399b77684c07826419d2515a1e1aa532f56d6
```

---

## Support upstream

If Mouzi saves you time and keeps your Downloads folder sane, consider supporting the original project.

[![ko-fi](https://ko-fi.com/img/githubbutton_sm.svg)](https://ko-fi.com/hsr)

You can also support upstream through [GitHub Sponsors](https://github.com/sponsors/hsr88).

Or visit the project homepage: [mouzi.cc](https://mouzi.cc)

---

## See also

[Ordir](https://github.com/landnthrn/ordir)

Order folders any way you want inside Windows File Explorer, and add custom thumbnails.

---

## License

Mouzi is released under the [MIT License](LICENSE).

---

## Acknowledgements

Built with [Tauri](https://tauri.app), [React](https://react.dev), [Tailwind CSS](https://tailwindcss.com), and [Rust](https://www.rust-lang.org).
