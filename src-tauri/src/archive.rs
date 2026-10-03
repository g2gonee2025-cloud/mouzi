use flate2::read::GzDecoder;
use std::fs::{self, File, FileTimes, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, Ordering};
use tar::Archive;
use zip::ZipArchive;

#[derive(Debug, Clone)]
pub struct ArchiveExtraction {
    pub staging_dir: PathBuf,
    pub extracted_files: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ArchiveFormat {
    Zip,
    Tgz,
}

// Limits cover declared sizes and actual streamed bytes, including tar headers.
#[derive(Clone, Copy)]
struct Limits { entries: usize, file_bytes: u64, total_bytes: u64, archive_bytes: u64, duration: Duration }
const LIMITS: Limits = Limits { entries: 20_000, file_bytes: 256 * 1024 * 1024, total_bytes: 2 * 1024 * 1024 * 1024, archive_bytes: 1024 * 1024 * 1024, duration: Duration::from_secs(120) };
static IMPORT: Mutex<Option<Arc<AtomicBool>>> = Mutex::new(None);

pub struct ImportJob { pub cancelled: Arc<AtomicBool> }
impl ImportJob {
    pub fn start() -> Result<Self, String> {
        let mut active = IMPORT.lock().map_err(|_| "Archive job lock failed")?;
        if active.is_some() { return Err("An archive import is already running".into()); }
        let cancelled = Arc::new(AtomicBool::new(false));
        *active = Some(cancelled.clone());
        Ok(Self { cancelled })
    }
}
impl Drop for ImportJob { fn drop(&mut self) { if let Ok(mut active) = IMPORT.lock() { *active = None; } } }
pub fn cancel_import() -> Result<(), String> {
    if let Some(cancelled) = IMPORT.lock().map_err(|_| "Archive job lock failed")?.as_ref() { cancelled.store(true, Ordering::Relaxed); }
    Ok(())
}

struct Budget<'a> { limits: Limits, started: Instant, cancelled: &'a AtomicBool, bytes: u64, files: usize, last_progress: Instant, progress: &'a mut dyn FnMut(u64, usize) }
struct GuardedReader<'a, R> { inner: R, remaining: u64, started: Instant, duration: Duration, cancelled: &'a AtomicBool }
impl<R: Read> Read for GuardedReader<'_, R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() { return Ok(0); }
        if self.cancelled.load(Ordering::Relaxed) { return Err(io::Error::other("Archive import cancelled")); }
        if self.started.elapsed() >= self.duration { return Err(io::Error::other("Archive import time limit exceeded")); }
        if self.remaining == 0 { return Err(io::Error::other("Archive stream size limit exceeded")); }
        let length = buffer.len().min(self.remaining.min(64 * 1024) as usize);
        let count = self.inner.read(&mut buffer[..length])?;
        self.remaining -= count as u64;
        Ok(count)
    }
}
impl Budget<'_> {
    fn check(&self) -> Result<(), String> {
        if self.cancelled.load(Ordering::Relaxed) { return Err("Archive import cancelled".into()); }
        if self.started.elapsed() >= self.limits.duration { return Err("Archive import time limit exceeded".into()); }
        Ok(())
    }
    fn copy(&mut self, input: &mut impl Read, output: &mut File, expected: u64) -> Result<(), String> {
        if expected > self.limits.file_bytes || expected > self.limits.total_bytes.saturating_sub(self.bytes) { return Err("Archive uncompressed size limit exceeded".into()); }
        let mut buffer = [0u8; 64 * 1024];
        let mut copied = 0u64;
        loop {
            self.check()?;
            let count = input.read(&mut buffer).map_err(|e| e.to_string())?;
            if count == 0 { break; }
            copied += count as u64;
            self.bytes += count as u64;
            if copied > self.limits.file_bytes || self.bytes > self.limits.total_bytes { return Err("Archive uncompressed size limit exceeded".into()); }
            output.write_all(&buffer[..count]).map_err(|e| e.to_string())?;
            if self.last_progress.elapsed() >= Duration::from_millis(250) { (self.progress)(self.bytes, self.files); self.last_progress = Instant::now(); }
        }
        if copied != expected { return Err("Archive entry size differs from its declared size".into()); }
        output.sync_all().map_err(|e| e.to_string())
    }
}

pub fn extract_archive(path: &Path, cancelled: &AtomicBool, mut progress: impl FnMut(u64, usize)) -> Result<ArchiveExtraction, String> {
    archive_format(path)?;
    crate::safe_fs::file_identity(path)?; // reject source links
    let metadata = fs::metadata(path).map_err(|e| e.to_string())?;
    if !metadata.is_file() || metadata.len() > LIMITS.archive_bytes { return Err("Archive input limit is 1 GiB of regular file data".into()); }
    let staging_dir = create_staging_dir()?;
    save_manifest(&staging_dir, Some(path), "extracting", &[], None)?;
    match extract_archive_to_with_progress(path, &staging_dir, LIMITS, cancelled, &mut progress) {
        Ok(extracted_files) => {
            save_manifest(&staging_dir, Some(path), "extracted", &[], None)?;
            Ok(ArchiveExtraction {
            staging_dir,
            extracted_files,
        }) },
        Err(err) => {
            // Successful entries and partially extracted data may be useful for
            // recovery. Never sweep an import folder after sorting may begin.
            let manifest = save_manifest(&staging_dir, Some(path), "failed", &[], Some(&err));
            Err(format!("{err}. Import data retained at {}{}", staging_dir.display(), manifest.err().map(|e| format!("; {e}")).unwrap_or_default()))
        }
    }
}

fn create_staging_dir() -> Result<PathBuf, String> {
    let base_dir = crate::application_data_dir().map_err(|e| e.to_string())?.join("archive-imports");

    fs::create_dir_all(&base_dir).map_err(|e| {
        format!(
            "Failed to create archive import folder {}: {}",
            base_dir.display(),
            e
        )
    })?;

    for attempt in 0..100 {
        let millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let dir = base_dir.join(format!(
            "import-{}-{}-{}",
            std::process::id(),
            millis,
            attempt
        ));
        match fs::create_dir(&dir) {
            Ok(()) => return Ok(dir),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => {
                return Err(format!(
                    "Failed to create archive import folder {}: {}",
                    dir.display(),
                    e
                ))
            }
        }
    }

    Err("Failed to create a unique archive import folder".to_string())
}

#[cfg(test)]
fn extract_archive_to(path: &Path, staging_dir: &Path) -> Result<usize, String> {
    extract_archive_to_with(path, staging_dir, LIMITS, &AtomicBool::new(false))
}

#[cfg(test)]
fn extract_archive_to_with(path: &Path, staging_dir: &Path, limits: Limits, cancelled: &AtomicBool) -> Result<usize, String> {
    extract_archive_to_with_progress(path, staging_dir, limits, cancelled, &mut |_, _| {})
}

fn extract_archive_to_with_progress(path: &Path, staging_dir: &Path, limits: Limits, cancelled: &AtomicBool, progress: &mut dyn FnMut(u64, usize)) -> Result<usize, String> {
    let mut budget = Budget { limits, started: Instant::now(), cancelled, bytes: 0, files: 0, last_progress: Instant::now(), progress };
    budget.check()?;
    if fs::metadata(path).map_err(|e| e.to_string())?.len() > limits.archive_bytes { return Err("Archive input size limit exceeded".into()); }
    fs::create_dir_all(staging_dir).map_err(|e| {
        format!(
            "Failed to create archive import folder {}: {}",
            staging_dir.display(),
            e
        )
    })?;

    let result = match archive_format(path)? {
        ArchiveFormat::Zip => extract_zip_to(path, staging_dir, &mut budget),
        ArchiveFormat::Tgz => extract_tgz_to(path, staging_dir, &mut budget),
    };
    (budget.progress)(budget.bytes, budget.files);
    result
}

fn archive_format(path: &Path) -> Result<ArchiveFormat, String> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("")
        .to_lowercase();

    if name.ends_with(".zip") {
        Ok(ArchiveFormat::Zip)
    } else if name.ends_with(".tgz") || name.ends_with(".tar.gz") {
        Ok(ArchiveFormat::Tgz)
    } else {
        Err("Unsupported archive format. Choose a .zip, .tgz, or .tar.gz file.".to_string())
    }
}

fn extract_zip_to(path: &Path, staging_dir: &Path, budget: &mut Budget<'_>) -> Result<usize, String> {
    let file = File::open(path)
        .map_err(|e| format!("Failed to open archive {}: {}", path.display(), e))?;
    let mut archive =
        ZipArchive::new(file).map_err(|e| format!("Failed to read zip archive: {}", e))?;

    if archive.len() > budget.limits.entries { return Err("Archive entry count limit exceeded".into()); }
    let mut extracted = 0;
    for i in 0..archive.len() {
        budget.check()?;
        let mut entry = archive
            .by_index(i)
            .map_err(|e| format!("Failed to read zip entry: {}", e))?;

        if entry.unix_mode().is_some_and(|mode| mode & 0o170000 == 0o120000) { return Err("Archive links are not supported".into()); }
        if entry.is_dir() {
            continue;
        }

        let entry_path = entry
            .enclosed_name()
            .map(Path::to_path_buf)
            .ok_or_else(|| format!("Archive contains an unsafe path: {}", entry.name()))?;
        let size = entry.size();
        check_disk_space(staging_dir, size)?;
        let (target, mut output) = create_output(staging_dir, &entry_path)?;
        let date = entry.last_modified();
        budget.copy(&mut entry, &mut output, size)?;
        let timestamp = chrono::NaiveDate::from_ymd_opt(date.year() as i32, date.month() as u32, date.day() as u32)
            .and_then(|day| day.and_hms_opt(date.hour() as u32, date.minute() as u32, date.second() as u32))
            .map(|date| date.and_utc().timestamp()).filter(|seconds| *seconds >= 0).map(|seconds| UNIX_EPOCH + Duration::from_secs(seconds as u64));
        preserve_modified_time(&output, &target, timestamp)?;
        extracted += 1;
        budget.files = extracted;
    }

    Ok(extracted)
}

fn extract_tgz_to(path: &Path, staging_dir: &Path, budget: &mut Budget<'_>) -> Result<usize, String> {
    let file = File::open(path)
        .map_err(|e| format!("Failed to open archive {}: {}", path.display(), e))?;
    let decoder = GzDecoder::new(file);
    // Header/padding overhead is bounded separately from extracted file bytes.
    let stream_limit = budget.limits.total_bytes.saturating_add((budget.limits.entries as u64).saturating_mul(1024));
    let mut archive = Archive::new(GuardedReader { inner: decoder, remaining: stream_limit, started: budget.started, duration: budget.limits.duration, cancelled: budget.cancelled });
    let entries = archive
        .entries()
        .map_err(|e| format!("Failed to read tar archive: {}", e))?;

    let mut extracted = 0;
    for (index, entry) in entries.enumerate() {
        budget.check()?;
        if index >= budget.limits.entries { return Err("Archive entry count limit exceeded".into()); }
        let mut entry = entry.map_err(|e| format!("Failed to read tar entry: {}", e))?;
        if entry.header().entry_type().is_symlink() || entry.header().entry_type().is_hard_link() { return Err("Archive links are not supported".into()); }
        if !entry.header().entry_type().is_file() {
            continue;
        }

        let entry_path = entry
            .path()
            .map_err(|e| format!("Failed to read tar entry path: {}", e))?
            .into_owned();
        ensure_safe_relative_path(&entry_path)?;
        let size = entry.size();
        check_disk_space(staging_dir, size)?;
        let modified = entry.header().mtime().ok().and_then(|seconds| UNIX_EPOCH.checked_add(Duration::from_secs(seconds)));
        let (target, mut output) = create_output(staging_dir, &entry_path)?;
        budget.copy(&mut entry, &mut output, size)?;
        preserve_modified_time(&output, &target, modified)?;
        extracted += 1;
        budget.files = extracted;
    }

    Ok(extracted)
}

fn create_output(staging_dir: &Path, entry_path: &Path) -> Result<(PathBuf, File), String> {
    ensure_safe_relative_path(entry_path)?;
    let file_name = entry_path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| format!("Archive entry has no file name: {}", entry_path.display()))?;

    if file_name.is_empty() {
        return Err(format!(
            "Archive entry has no file name: {}",
            entry_path.display()
        ));
    }

    // Windows alternate streams and reserved names are never output targets.
    if file_name.chars().any(|c| c.is_control() || ":<>|?*\\/".contains(c)) || file_name.ends_with(['.', ' ']) { return Err("Archive contains an unsupported filename".into()); }
    let stem = entry_path.file_stem().and_then(|s| s.to_str()).unwrap_or(file_name);
    let extension = entry_path.extension().and_then(|s| s.to_str());
    for index in 0..10_000 {
        let name = if index == 0 { file_name.to_string() } else { match extension { Some(ext) => format!("{stem} ({index}).{ext}"), None => format!("{stem} ({index})") } };
        let target = staging_dir.join(name);
        match OpenOptions::new().write(true).create_new(true).open(&target) {
            Ok(file) => return Ok((target, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(format!("Cannot extract {}: {error}", target.display())),
        }
    }
    Err("Archive filename collision limit exceeded".into())
}

fn ensure_safe_relative_path(path: &Path) -> Result<(), String> {
    let mut has_name = false;

    for component in path.components() {
        match component {
            Component::Normal(_) => has_name = true,
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(format!(
                    "Archive contains an unsafe path: {}",
                    path.display()
                ))
            }
        }
    }

    if has_name {
        Ok(())
    } else {
        Err(format!(
            "Archive entry has no file name: {}",
            path.display()
        ))
    }
}

fn preserve_modified_time(file: &File, path: &Path, modified: Option<SystemTime>) -> Result<(), String> {
    if let Some(modified) = modified { file.set_times(FileTimes::new().set_modified(modified)).map_err(|e| format!("Cannot preserve timestamp of {}: {e}", path.display()))?; }
    Ok(())
}

fn check_disk_space(path: &Path, bytes: u64) -> Result<(), String> {
    #[cfg(windows)] {
        use std::os::windows::ffi::OsStrExt;
        #[link(name="kernel32")]
        unsafe extern "system" { fn GetDiskFreeSpaceExW(path: *const u16, available: *mut u64, total: *mut u64, free: *mut u64) -> i32; }
        let path: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        let mut available = 0u64;
        if unsafe { GetDiskFreeSpaceExW(path.as_ptr(), &mut available, std::ptr::null_mut(), std::ptr::null_mut()) } == 0 { return Err(format!("Cannot check extraction disk space: {}", io::Error::last_os_error())); }
        if available < bytes.saturating_add(128 * 1024 * 1024) { return Err("Insufficient extraction space; 128 MiB reserve required".into()); }
    }
    #[cfg(not(windows))] { let _ = (path, bytes); }
    Ok(())
}

/// The manifest is beside the staging directory so the organizer cannot file
/// it as imported content. It survives failed/cancelled and successful imports.
pub fn save_manifest(staging: &Path, source: Option<&Path>, status: &str, results: &[(String, String, String)], message: Option<&str>) -> Result<(), String> {
    let manifest = staging.with_extension("manifest.json");
    let mut value = if manifest.exists() {
        crate::safe_fs::file_identity(&manifest)?;
        serde_json::from_slice::<serde_json::Value>(&fs::read(&manifest).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?
    } else { serde_json::json!({"version":1,"createdAt":chrono::Utc::now().to_rfc3339(),"source":source.map(|p|p.to_string_lossy().into_owned()),"staging":staging.to_string_lossy(),"limits":{"entries":LIMITS.entries,"fileBytes":LIMITS.file_bytes,"totalBytes":LIMITS.total_bytes,"seconds":LIMITS.duration.as_secs()}}) };
    let mut remaining = Vec::new();
    for entry in fs::read_dir(staging).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let metadata = fs::symlink_metadata(entry.path()).map_err(|e| e.to_string())?;
        remaining.push(serde_json::json!({"name":entry.file_name().to_string_lossy(),"bytes":metadata.len(),"isFile":metadata.is_file()}));
    }
    if status == "extracted" { value["extractedEntries"] = serde_json::json!(remaining.clone()); }
    value["remainingEntries"] = serde_json::json!(remaining);
    value["status"] = serde_json::json!(status);
    value["message"] = serde_json::json!(message);
    value["updatedAt"] = serde_json::json!(chrono::Utc::now().to_rfc3339());
    value["sortedResults"] = serde_json::json!(results);
    let mut file = OpenOptions::new().write(true).create(true).truncate(true).open(&manifest).map_err(|e| e.to_string())?;
    file.write_all(&serde_json::to_vec_pretty(&value).map_err(|e| e.to_string())?).and_then(|_|file.sync_all()).map_err(|e|format!("Cannot persist import manifest {}: {e}",manifest.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use zip::write::FileOptions;

    static TEST_DIR_COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn test_dir(prefix: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "mouzi-archive-test-{}-{}-{}",
            prefix,
            std::process::id(),
            TEST_DIR_COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn file_names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().to_string())
            .collect();
        names.sort();
        names
    }

    fn write_zip(path: &Path, files: &[(&str, &[u8])]) {
        let file = File::create(path).unwrap();
        let mut writer = zip::ZipWriter::new(file);
        let options = FileOptions::default();

        for (name, bytes) in files {
            writer.start_file(*name, options).unwrap();
            writer.write_all(bytes).unwrap();
        }

        writer.finish().unwrap();
    }

    fn write_tgz(path: &Path, files: &[(&str, &[u8])]) {
        let file = File::create(path).unwrap();
        let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
        let mut builder = tar::Builder::new(encoder);

        for (name, bytes) in files {
            let mut header = tar::Header::new_gnu();
            header.set_size(bytes.len() as u64);
            header.set_cksum();
            builder.append_data(&mut header, *name, *bytes).unwrap();
        }

        builder.finish().unwrap();
    }

    #[test]
    fn zip_extracts_nested_files_flat() {
        let root = test_dir("zip-flat");
        let archive_path = root.join("takeout.zip");
        let staging = root.join("staging");
        write_zip(
            &archive_path,
            &[
                ("Photos/2023/01/img.jpg", b"image"),
                ("Drive/doc.pdf", b"document"),
            ],
        );

        let count = extract_archive_to(&archive_path, &staging).unwrap();

        assert_eq!(count, 2);
        assert_eq!(file_names(&staging), vec!["doc.pdf", "img.jpg"]);
        assert_eq!(fs::read(staging.join("img.jpg")).unwrap(), b"image");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn duplicate_file_names_are_renamed() {
        let root = test_dir("zip-duplicates");
        let archive_path = root.join("takeout.zip");
        let staging = root.join("staging");
        write_zip(
            &archive_path,
            &[("a/IMG_0001.jpg", b"first"), ("b/IMG_0001.jpg", b"second")],
        );

        let count = extract_archive_to(&archive_path, &staging).unwrap();

        assert_eq!(count, 2);
        assert_eq!(
            file_names(&staging),
            vec!["IMG_0001 (1).jpg", "IMG_0001.jpg"]
        );
        assert_eq!(fs::read(staging.join("IMG_0001.jpg")).unwrap(), b"first");
        assert_eq!(
            fs::read(staging.join("IMG_0001 (1).jpg")).unwrap(),
            b"second"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn tgz_extracts_nested_files_flat() {
        let root = test_dir("tgz-flat");
        let archive_path = root.join("takeout.tgz");
        let staging = root.join("staging");
        write_tgz(
            &archive_path,
            &[
                ("Photos/2023/01/img.jpg", b"image"),
                ("Drive/doc.pdf", b"document"),
            ],
        );

        let count = extract_archive_to(&archive_path, &staging).unwrap();

        assert_eq!(count, 2);
        assert_eq!(file_names(&staging), vec!["doc.pdf", "img.jpg"]);
        assert_eq!(fs::read(staging.join("doc.pdf")).unwrap(), b"document");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn zip_slip_entries_are_rejected() {
        let root = test_dir("zip-slip");
        let archive_path = root.join("takeout.zip");
        let staging = root.join("staging");
        write_zip(&archive_path, &[("../../evil.txt", b"bad")]);

        let err = extract_archive_to(&archive_path, &staging).unwrap_err();

        assert!(err.contains("unsafe path"));
        assert!(!root.join("evil.txt").exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn unsupported_archive_extensions_are_rejected() {
        let root = test_dir("unsupported");
        let archive_path = root.join("takeout.rar");
        fs::write(&archive_path, b"not an archive").unwrap();
        let staging = root.join("staging");

        let err = extract_archive_to(&archive_path, &staging).unwrap_err();

        assert!(err.contains("Unsupported archive format"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn extraction_enforces_entry_file_total_input_and_time_limits() {
        let root = test_dir("limits");
        let archive = root.join("files.zip");
        write_zip(&archive, &[("a.txt", b"1234"), ("b.txt", b"5678")]);
        for (name, limits, expected) in [
            ("count", Limits { entries: 1, ..LIMITS }, "count limit"),
            ("file", Limits { file_bytes: 3, ..LIMITS }, "size limit"),
            ("total", Limits { total_bytes: 6, ..LIMITS }, "size limit"),
            ("input", Limits { archive_bytes: 1, ..LIMITS }, "input size"),
            ("time", Limits { duration: Duration::ZERO, ..LIMITS }, "time limit"),
        ] {
            let error = extract_archive_to_with(&archive, &root.join(name), limits, &AtomicBool::new(false)).unwrap_err();
            assert!(error.contains(expected), "{name}: {error}");
        }
        assert_eq!(fs::read(root.join("total/a.txt")).unwrap(), b"1234");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cancellation_and_progress_use_production_extraction() {
        let root = test_dir("cancel");
        let archive = root.join("files.zip");
        write_zip(&archive, &[("a.txt", b"1234")]);
        let cancelled = AtomicBool::new(true);
        assert!(extract_archive_to_with(&archive, &root.join("cancelled"), LIMITS, &cancelled).unwrap_err().contains("cancelled"));
        assert!(!root.join("cancelled/a.txt").exists());
        cancelled.store(false, Ordering::Relaxed);
        let mut progress = Vec::new();
        assert_eq!(extract_archive_to_with_progress(&archive, &root.join("done"), LIMITS, &cancelled, &mut |bytes,files|progress.push((bytes,files))).unwrap(),1);
        assert_eq!(progress.last(),Some(&(4,1)));
        let timestamp = fs::metadata(root.join("done/a.txt")).unwrap().modified().unwrap();
        assert!(timestamp > UNIX_EPOCH + Duration::from_secs(300_000_000), "ZIP timestamp was replaced with 1970");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn bounded_stream_and_manifest_preserve_recovery_evidence() {
        let root = test_dir("manifest");
        let staging = root.join("staging");
        fs::create_dir(&staging).unwrap();
        fs::write(staging.join("photo.jpg"), b"original").unwrap();
        let source = root.join("input.tgz");
        save_manifest(&staging, Some(&source), "extracted", &[], None).unwrap();
        fs::rename(staging.join("photo.jpg"), root.join("photo.jpg")).unwrap();
        save_manifest(&staging, None, "completed", &[("photo.jpg".into(),"Images".into(),root.join("photo.jpg").to_string_lossy().into_owned())], None).unwrap();
        let manifest: serde_json::Value = serde_json::from_slice(&fs::read(staging.with_extension("manifest.json")).unwrap()).unwrap();
        assert_eq!(manifest["source"], source.to_string_lossy().as_ref());
        assert_eq!(manifest["extractedEntries"].as_array().unwrap().len(),1);
        assert_eq!(manifest["sortedResults"].as_array().unwrap().len(),1);
        let cancelled = AtomicBool::new(false);
        let mut reader = GuardedReader { inner: &b"12345"[..], remaining:3, started:Instant::now(), duration:Duration::from_secs(1), cancelled:&cancelled };
        let mut bytes = Vec::new();
        assert!(reader.read_to_end(&mut bytes).unwrap_err().to_string().contains("stream size limit"));
        assert_eq!(bytes,b"123");
        fs::remove_dir_all(root).unwrap();
    }
}
