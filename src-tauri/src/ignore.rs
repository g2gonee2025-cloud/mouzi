use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{ErrorKind, Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

const IGNORE_FILE: &str = ".mouziignore";

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
    let Some(mut file) = open_ignore_file(&path)? else {
        return Ok(Vec::new());
    };
    let mut content = String::new();
    file.read_to_string(&mut content).map_err(|error| {
        format!("Unable to read ignore protection file {}: {error}", path.display())
    })?;

    Ok(content
        .lines()
        .map(|l| {
            let l = l.trim();
            let mut escaped = false;
            let mut end = l.len();

            for (i, c) in l.char_indices() {
                match c {
                    '\\' => escaped = !escaped,
                    '#' if !escaped => {
                        end = i;
                        break;
                    }
                    _ => escaped = false,
                }
            }

            l[..end].trim().to_string()
        })
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| l.replace(r"\#", "#"))
        .collect())
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
    let mut content = String::from("# Mouzi ignore rules\n# https://mouzi.cc/docs\n\n");
    for p in patterns {
        content.push_str(&p.replace('#', r"\#"));
        content.push('\n');
    }
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
    let result = written.and_then(|()| fs::rename(&stage, &path));
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
    #[cfg(windows)]
    let name = name.to_lowercase();

    for original_pat in patterns {
        #[cfg(windows)]
        let pat = original_pat.to_lowercase();
        #[cfg(not(windows))]
        let pat = original_pat.as_str();

        // Directory pattern: ends with /
        if pat.ends_with('/') {
            let dir_pat = &pat[..pat.len() - 1];
            if name.eq_ignore_ascii_case(dir_pat) {
                return true;
            }
            continue;
        }
        // Wildcard pattern: contains *
        if pat.contains('*') {
            if glob_match(&name, &pat) {
                return true;
            }
            continue;
        }
        // Literal match
        if name.eq_ignore_ascii_case(&pat) {
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
