use serde::Serialize;
use std::collections::HashMap;
use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{ErrorKind, Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

const IGNORE_FILE: &str = ".mouziignore";
const IGNORE_HEADER: &str = "# Mouzi ignore rules\n# https://mouzi.cc/docs\n\n";

fn is_regular_file(metadata: &Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return false;
        }
    }
    metadata.is_file()
}

fn open_ignore_file(path: &Path) -> Result<Option<File>, String> {
    let error = |cause: &dyn std::fmt::Display| {
        format!("Cannot open ignore protection file {}: {cause}", path.display())
    };
    let before = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(cause) if cause.kind() == ErrorKind::NotFound => return Ok(None),
        Err(cause) => return Err(error(&cause)),
    };
    let unsafe_file = || error(&"expected a regular file with one link");
    if !is_regular_file(&before) {
        return Err(unsafe_file());
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x00200000;
        const FILE_SHARE_READ: u32 = 1;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT).share_mode(FILE_SHARE_READ);
    }
    let file = options.open(path).map_err(|cause| error(&cause))?;
    let opened = file.metadata().map_err(|cause| error(&cause))?;
    if !is_regular_file(&opened) || crate::safe_fs::link_count(&file).map_err(|cause| error(&cause))? != 1 {
        return Err(unsafe_file());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if (before.dev(), before.ino()) != (opened.dev(), opened.ino()) {
            return Err(error(&"file changed while opening"));
        }
    }
    Ok(Some(file))
}

/// Load non-empty, non-comment patterns from a folder's protection file.
/// An absent file has no rules; an existing file that cannot be read is an error.
pub fn load_mouziignore(folder_path: &str) -> Result<Vec<String>, String> {
    let path = Path::new(folder_path).join(IGNORE_FILE);
    let mut file = open_ignore_file(&path)?;
    let content = read_ignore_content(file.as_mut(), &path)?;
    Ok(parse_patterns(content.as_deref().unwrap_or_default()))
}

fn read_ignore_content(file: Option<&mut File>, path: &Path) -> Result<Option<String>, String> {
    let Some(file) = file else { return Ok(None); };
    let mut content = String::new();
    file.read_to_string(&mut content).map_err(|error| {
        format!("Unable to read ignore protection file {}: {error}", path.display())
    })?;
    Ok(Some(content))
}

fn comment_start(line: &str) -> Option<usize> {
    let mut escaped = false;
    for (index, character) in line.char_indices() {
        match character {
            '\\' => escaped = !escaped,
            '#' if !escaped => return Some(index),
            _ => escaped = false,
        }
    }
    None
}

fn line_pattern(line: &str, comment: Option<usize>) -> Option<String> {
    let pattern = line[..comment.unwrap_or(line.len())].trim();
    (!pattern.is_empty()).then(|| pattern.replace(r"\#", "#"))
}

fn parse_patterns(content: &str) -> Vec<String> {
    content.strip_prefix('\u{feff}').unwrap_or(content).lines()
        .filter_map(|line| line_pattern(line, comment_start(line)))
        .collect()
}

/// Rules loaded once for a folder. Loading errors must pause that folder's work.
#[derive(Clone, Debug, Default)]
pub struct IgnorePatterns {
    patterns: Vec<String>,
}

impl IgnorePatterns {
    pub fn from_folder(folder_path: &str) -> Result<Self, String> {
        load_mouziignore(folder_path).map(|patterns| Self { patterns })
    }

    pub fn is_ignored(&self, name: &str) -> bool {
        is_ignored(name, &self.patterns)
    }

    /// Match the file and each directory that inventory traverses below its owning root.
    pub(crate) fn ignores_inventory_path(&self, relative: &Path) -> bool {
        if self.patterns.is_empty() {
            return false;
        }
        relative.file_name().is_some_and(|name| self.is_ignored(&name.to_string_lossy()))
            || relative.parent().is_some_and(|directory| {
                directory.components().any(|part| {
                    is_ignored_entry(&part.as_os_str().to_string_lossy(), true, &self.patterns)
                })
            })
    }

    pub fn as_slice(&self) -> &[String] {
        &self.patterns
    }
}

/// Save patterns to .mouziignore in the given folder.
/// Writes a header comment, then one pattern per line.
pub fn save_mouziignore(folder_path: &str, patterns: &[String]) -> Result<(), String> {
    let folder = Path::new(folder_path);
    let path = folder.join(IGNORE_FILE);
    let existing = open_ignore_file(&path)?;
    let content = new_ignore_document(patterns);
    write_ignore_document(folder, &path, existing, &content)
}

fn new_ignore_document(patterns: &[String]) -> String {
    let mut content = String::from(IGNORE_HEADER);
    for p in patterns {
        content.push_str(&p.replace('#', r"\#"));
        content.push('\n');
    }
    content
}

#[derive(Debug, Serialize)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum IgnoreSaveOutcome {
    Saved { patterns: Vec<String> },
    Conflict { patterns: Vec<String> },
}

pub fn save_mouziignore_checked(
    folder_path: &str, expected: &[String], patterns: &[String],
) -> Result<IgnoreSaveOutcome, String> {
    let folder = Path::new(folder_path);
    let path = folder.join(IGNORE_FILE);
    let mut existing = open_ignore_file(&path)?;
    let document = read_ignore_content(existing.as_mut(), &path)?;
    let current = parse_patterns(document.as_deref().unwrap_or_default());
    if patterns == expected || patterns == current.as_slice() {
        return Ok(IgnoreSaveOutcome::Saved { patterns: current });
    }
    if current.as_slice() != expected {
        return Ok(IgnoreSaveOutcome::Conflict { patterns: current });
    }
    let content = match document {
        Some(document) => edit_ignore_document(&document, patterns),
        None => new_ignore_document(patterns),
    };
    write_ignore_document(folder, &path, existing, &content)?;
    Ok(IgnoreSaveOutcome::Saved { patterns: parse_patterns(&content) })
}

fn take_pattern<'a>(remaining: &mut HashMap<&'a str, usize>, pattern: &str) -> bool {
    let Some(count) = remaining.get_mut(pattern) else { return false; };
    if *count == 0 { return false; }
    *count -= 1;
    true
}

fn edit_ignore_document(document: &str, patterns: &[String]) -> String {
    let (signature, body) = document.strip_prefix('\u{feff}')
        .map_or(("", document), |body| ("\u{feff}", body));
    let newline = if body.split_once('\n').is_some_and(|(line, _)| line.ends_with('\r')) { "\r\n" } else { "\n" };
    let mut remaining = HashMap::new();
    for pattern in patterns { *remaining.entry(pattern.as_str()).or_insert(0usize) += 1; }

    let mut content = signature.to_string();
    for line in body.split_inclusive('\n') {
        let comment = comment_start(line);
        match line_pattern(line, comment) {
            None => content.push_str(line),
            Some(pattern) if take_pattern(&mut remaining, &pattern) => content.push_str(line),
            Some(_) => {
                if let Some(index) = comment {
                    // A removed rule can leave its hand-authored annotation behind.
                    let indent = line.len() - line.trim_start().len();
                    content.push_str(&line[..indent]);
                    content.push_str(&line[index..]);
                }
            }
        }
    }
    let mut appended = false;
    for pattern in patterns {
        if !take_pattern(&mut remaining, pattern) { continue; }
        if content.len() > signature.len() && !content.ends_with('\n') { content.push_str(newline); }
        content.push_str(&pattern.replace('#', r"\#"));
        appended = true;
    }
    if appended && body.ends_with('\n') { content.push_str(newline); }
    content
}

fn write_ignore_document(folder: &Path, path: &Path, existing: Option<File>, content: &str) -> Result<(), String> {
    // Stage the full replacement before publishing it. Never truncate a path
    // supplied by the filesystem, even when the checked leaf is regular.
    static NEXT_STAGE: AtomicU64 = AtomicU64::new(0);
    let serial = NEXT_STAGE.fetch_add(1, Ordering::Relaxed);
    let stage = folder.join(format!("{IGNORE_FILE}.{}.{serial}.tmp", std::process::id()));
    let mut file = OpenOptions::new().write(true).create_new(true).open(&stage)
        .map_err(|error| format!("Cannot stage ignore protection file: {error}"))?;
    let written = file.write_all(content.as_bytes()).and_then(|()| file.sync_all());
    drop(file);
    // Windows must release the checked leaf before renaming over its name.
    // The rename updates the entry itself, never writes through a leaf link.
    drop(existing);
    let result = written.and_then(|()| fs::rename(&stage, path));
    if let Err(error) = result {
        let _ = fs::remove_file(&stage);
        return Err(format!("Cannot save ignore protection file {}: {error}", path.display()));
    }
    Ok(())
}

/// Check if a file name matches any of the ignore patterns.
/// Supports: literal match, `*` wildcard (any number of `*`), and `folder/` directory suffix.
/// On Windows, matching is case-insensitive for both literals and wildcards.
pub fn is_ignored(name: &str, patterns: &[String]) -> bool {
    is_ignored_entry(name, false, patterns)
}

pub(crate) fn is_ignored_entry(name: &str, is_directory: bool, patterns: &[String]) -> bool {
    #[cfg(windows)]
    let name = name.to_lowercase();

    for original_pat in patterns {
        #[cfg(windows)]
        let original_pat = original_pat.to_lowercase();
        let pat = match original_pat.strip_suffix('/') {
            Some(directory) if is_directory => directory,
            Some(_) => continue,
            None => original_pat.as_str(),
        };
        if pat.contains('*') {
            if glob_match(&name, pat) {
                return true;
            }
        } else if name.eq_ignore_ascii_case(pat) {
            return true;
        }
    }
    false
}

/// Match the complete name, allowing each `*` to consume zero or more bytes.
/// On Windows both `name` and `pat` are expected to already be lowercased.
fn glob_match(name: &str, pat: &str) -> bool {
    let name = name.as_bytes();
    let pat = pat.as_bytes();
    let (mut name_index, mut pattern_index) = (0, 0);
    let mut last_star = None;
    let mut star_end = 0;

    while name_index < name.len() {
        if pattern_index < pat.len() && pat[pattern_index] == b'*' {
            last_star = Some(pattern_index);
            pattern_index += 1;
            star_end = name_index;
        } else if pattern_index < pat.len() && pat[pattern_index] == name[name_index] {
            name_index += 1;
            pattern_index += 1;
        } else if let Some(star_index) = last_star {
            // A literal suffix may occur more than once. Retry after extending
            // the preceding wildcard instead of committing to its first match.
            star_end += 1;
            name_index = star_end;
            pattern_index = star_index + 1;
        } else {
            return false;
        }
    }

    while pattern_index < pat.len() && pat[pattern_index] == b'*' {
        pattern_index += 1;
    }
    pattern_index == pat.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temporary_folder() -> std::path::PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("mouzi-ignore-test-{unique}"));
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn loads_utf8_ignore_files_with_a_signature() {
        let folder = temporary_folder();
        let file = folder.join(IGNORE_FILE);
        let read = |document: &str| {
            fs::write(&file, document).unwrap();
            load_mouziignore(folder.to_str().unwrap()).unwrap()
        };
        let ordinary = read("résumé*.pdf\r\nlater\u{feff}name.txt\r\n");
        let marked = read("\u{feff}résumé*.pdf\r\nlater\u{feff}name.txt\r\n");
        let first_comment = read("\u{feff}# local notes\r\n*.draft\r\n");
        fs::remove_dir_all(folder).unwrap();

        assert_eq!(ordinary, ["résumé*.pdf", "later\u{feff}name.txt"]);
        assert_eq!(marked, ordinary);
        assert_eq!(first_comment, ["*.draft"]);
    }

    #[test]
    fn loads_inline_comments_and_escaped_hashes() {
        let folder = temporary_folder();
        fs::write(
            folder.join(".mouziignore"),
            "*.tmp # temporary files\nreport\\#final.pdf # literal hash\n# full comment\n",
        )
        .unwrap();

        let patterns = load_mouziignore(folder.to_str().unwrap()).unwrap();
        assert_eq!(patterns, vec!["*.tmp", "report#final.pdf"]);

        fs::remove_dir_all(folder).unwrap();
    }

    #[test]
    fn save_escapes_literal_hashes() {
        let folder = temporary_folder();
        save_mouziignore(folder.to_str().unwrap(), &["report#final.pdf".to_string()]).unwrap();

        let saved = fs::read_to_string(folder.join(".mouziignore")).unwrap();
        assert!(saved.contains("report\\#final.pdf"));
        assert_eq!(
            load_mouziignore(folder.to_str().unwrap()).unwrap(),
            vec!["report#final.pdf"]
        );
        save_mouziignore(folder.to_str().unwrap(), &["replacement".to_string()]).unwrap();
        assert_eq!(load_mouziignore(folder.to_str().unwrap()).unwrap(), vec!["replacement"]);

        fs::remove_dir_all(folder).unwrap();
    }

    #[test]
    fn checked_ignore_save_keeps_editor_changes_and_preserves_document_format() {
        let folder = temporary_folder();
        let directory = folder.to_str().unwrap();
        let file = folder.join(IGNORE_FILE);
        fs::write(&file, "\u{feff}# personal notes\r\n\r\n  original.tmp  # retain this annotation\r\n").unwrap();
        let displayed = load_mouziignore(directory).unwrap();
        assert_eq!(displayed, ["original.tmp"]);

        let edited = "\u{feff}# personal notes\r\n\r\n  original.tmp  # retain this annotation\r\neditor.tmp\r\n";
        fs::write(&file, edited).unwrap();
        let outcome = save_mouziignore_checked(directory, &displayed, &["original.tmp".into(), "draft#file.tmp".into()]).unwrap();
        let IgnoreSaveOutcome::Conflict { patterns: current } = outcome else { panic!("expected conflict") };
        assert_eq!(current, ["original.tmp", "editor.tmp"]);
        assert_eq!(fs::read_to_string(&file).unwrap(), edited);

        let comments = "\u{feff}# revised personal notes\r\n\r\n  original.tmp  # retain this annotation\r\neditor.tmp\r\n";
        fs::write(&file, comments).unwrap();
        let no_change = save_mouziignore_checked(directory, &current, &current).unwrap();
        assert!(matches!(no_change, IgnoreSaveOutcome::Saved { patterns } if patterns == current));
        assert_eq!(fs::read_to_string(&file).unwrap(), comments);

        let outcome = save_mouziignore_checked(directory, &current, &["editor.tmp".into(), "draft#file.tmp".into()]).unwrap();
        assert!(matches!(outcome, IgnoreSaveOutcome::Saved { patterns } if patterns == ["editor.tmp", "draft#file.tmp"]));
        assert_eq!(fs::read_to_string(&file).unwrap(),
            "\u{feff}# revised personal notes\r\n\r\n  # retain this annotation\r\neditor.tmp\r\ndraft\\#file.tmp\r\n");
        fs::remove_dir_all(folder).unwrap();
    }

    #[test]
    fn checked_ignore_save_creates_missing_file_only_for_a_real_edit() {
        let folder = temporary_folder();
        let directory = folder.to_str().unwrap();
        let file = folder.join(IGNORE_FILE);
        assert!(matches!(save_mouziignore_checked(directory, &[], &[]).unwrap(),
            IgnoreSaveOutcome::Saved { patterns } if patterns.is_empty()));
        assert!(!file.exists());
        let new = vec!["report#final.pdf".to_string()];
        assert!(matches!(save_mouziignore_checked(directory, &[], &new).unwrap(),
            IgnoreSaveOutcome::Saved { patterns } if patterns == new));
        assert_eq!(fs::read_to_string(&file).unwrap(), "# Mouzi ignore rules\n# https://mouzi.cc/docs\n\nreport\\#final.pdf\n");
        fs::remove_dir_all(folder).unwrap();
    }

    #[test]
    #[cfg(windows)]
    fn wildcard_case_insensitive_on_windows() {
        assert!(is_ignored("FOO.TMP", &["*.tmp".to_string()]));
        assert!(is_ignored("Foo.Tmp", &["*.tmp".to_string()]));
        assert!(is_ignored("BAR.EXE", &["*.exe".to_string()]));
        assert!(is_ignored("prefixSUFFIX.txt", &["prefix*.TXT".to_string()]));
        assert!(is_ignored("README", &["readme".to_string()]));
        assert!(!is_ignored("foo.tmp", &["*.txt".to_string()]));
        assert!(!is_ignored("FOO.TMP", &["*.txt".to_string()]));
    }

    #[test]
    fn directory_wildcards_match_only_directory_names() {
        let patterns = parse_patterns("Camera-*/\nTrip\\#*raw*/ # original photos\n");
        for name in ["Camera-", "Camera-2024", "Trip#raw", "Trip#2-raw-export"] {
            assert!(is_ignored_entry(name, true, &patterns), "{name}");
            assert!(!is_ignored_entry(name, false, &patterns), "{name}");
        }
        assert!(!is_ignored_entry("MyCamera-2024", true, &patterns));
        assert_eq!(is_ignored_entry("CAMERA-2024", true, &patterns), cfg!(windows));
    }

    #[test]
    fn multiple_wildcards_and_spaces() {
        assert!(is_ignored(
            "The Chronicle Herald (Metro)_20260612.txt",
            &["*Metro*".to_string()]
        ));
        assert!(is_ignored(
            "The Chronicle Herald (Metro)_20260612.txt",
            &["*Chronicle Herald*".to_string()]
        ));
        assert!(is_ignored(
            "some.Metro.file.txt",
            &["*Metro*.txt".to_string()]
        ));
        assert!(is_ignored("file.name.txt", &["file.*.txt".to_string()]));
        assert!(!is_ignored("foo.txt", &["*metro*".to_string()]));
    }

    #[test]
    #[cfg(not(windows))]
    fn wildcard_case_sensitive_on_non_windows() {
        assert!(is_ignored("foo.tmp", &["*.tmp".to_string()]));
        assert!(!is_ignored("FOO.TMP", &["*.tmp".to_string()]));
    }

    #[test]
    fn suffix_wildcards_retry_repeated_literals() {
        assert!(is_ignored(
            "report.txt.backup.txt",
            &["*.txt".to_string()]
        ));
        assert!(is_ignored("fooXXbarYYbar", &["foo*bar".to_string()]));
        assert!(is_ignored(
            "fooXXbarYYbarZZbaz",
            &["foo*bar*baz".to_string()]
        ));
        assert!(!is_ignored("report.txt.backup", &["*.txt".to_string()]));
        assert!(!is_ignored("XfooXXbar", &["foo*bar".to_string()]));
    }

    #[test]
    fn wildcard_handles_empty_matches_and_unicode() {
        assert!(glob_match("", "***"));
        assert!(glob_match("foo", "f**o*o"));
        assert!(glob_match("résumé.backup.résumé", "*résumé"));
        assert!(glob_match("a日本語日本語b", "a*日本語*b"));
        assert!(!glob_match("", "*a*"));
        assert!(!glob_match("foo", "f**o*o*o"));
    }

    #[test]
    fn missing_ignore_file_is_empty() {
        let folder = temporary_folder();
        let patterns = IgnorePatterns::from_folder(folder.to_str().unwrap()).unwrap();
        assert!(patterns.as_slice().is_empty());
        assert!(!patterns.is_ignored("report.txt"));
        fs::remove_dir_all(folder).unwrap();
    }

    #[test]
    fn unreadable_ignore_file_is_an_error() {
        let folder = temporary_folder();
        // A directory is deterministically unreadable as text on every platform,
        // including Windows and processes that can bypass file permission bits.
        fs::create_dir(folder.join(".mouziignore")).unwrap();
        let error = load_mouziignore(folder.to_str().unwrap()).unwrap_err();
        assert!(error.contains(".mouziignore"));
        assert!(IgnorePatterns::from_folder(folder.to_str().unwrap()).is_err());
        assert!(save_mouziignore(folder.to_str().unwrap(), &[]).is_err());
        assert!(folder.join(IGNORE_FILE).is_dir());
        fs::remove_dir_all(folder).unwrap();
    }

    #[test]
    fn invalid_utf8_ignore_file_is_an_error() {
        let folder = temporary_folder();
        fs::write(folder.join(".mouziignore"), [0xff, 0xfe]).unwrap();
        assert!(load_mouziignore(folder.to_str().unwrap()).is_err());
        fs::remove_dir_all(folder).unwrap();
    }

    #[test]
    fn hard_linked_ignore_file_is_rejected_without_touching_its_target() {
        let parent = temporary_folder();
        let folder = parent.join("watched");
        fs::create_dir(&folder).unwrap();
        let sentinel = parent.join("sentinel.txt");
        fs::write(&sentinel, "private value").unwrap();
        fs::hard_link(&sentinel, folder.join(IGNORE_FILE)).unwrap();

        let folder_path = folder.to_str().unwrap();
        assert!(load_mouziignore(folder_path).is_err());
        assert!(save_mouziignore(folder_path, &["replacement".into()]).is_err());
        assert_eq!(fs::read_to_string(sentinel).unwrap(), "private value");
        fs::remove_dir_all(parent).unwrap();
    }

    #[test]
    #[cfg(unix)]
    fn dangling_ignore_symlink_is_an_error() {
        let folder = temporary_folder();
        std::os::unix::fs::symlink(folder.join("missing"), folder.join(".mouziignore"))
            .unwrap();
        assert!(load_mouziignore(folder.to_str().unwrap()).is_err());
        assert!(save_mouziignore(folder.to_str().unwrap(), &[]).is_err());
        fs::remove_dir_all(folder).unwrap();
    }
}
