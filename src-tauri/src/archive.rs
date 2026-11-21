use crate::safe_fs::{self, AncestorGuard};
use flate2::read::GzDecoder;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs::{self, File, FileTimes, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, Ordering};
use tar::Archive;
use zip::ZipArchive;

#[derive(Debug)]
pub struct ArchiveExtraction {
    pub staging_dir: PathBuf,
    pub extracted_files: usize,
    _staging_guard: AncestorGuard,
}

#[derive(Debug)]
pub(crate) struct ArchiveExtractionError {
    pub(crate) message: String,
    pub(crate) retained: Option<RetainedArchiveExtraction>,
}

#[derive(Debug)]
pub(crate) struct RetainedArchiveExtraction {
    pub(crate) staging_dir: PathBuf,
    pub(crate) extracted_files: Option<usize>,
}

impl ArchiveExtractionError {
    fn retained(staging_dir: &Path, extracted_files: Option<usize>, message: String) -> Self {
        Self {
            message,
            retained: Some(RetainedArchiveExtraction { staging_dir: staging_dir.to_path_buf(), extracted_files }),
        }
    }
}

impl From<String> for ArchiveExtractionError {
    fn from(message: String) -> Self {
        Self { message, retained: None }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ArchiveFormat {
    Zip,
    Tgz,
}

// Limits cover declared sizes and actual streamed bytes, including tar headers.
#[derive(Clone, Copy)]
struct Limits { entries: usize, file_bytes: u64, total_bytes: u64, archive_bytes: u64, zip_metadata_bytes: u64, duration: Duration }
const LIMITS: Limits = Limits { entries: 20_000, file_bytes: 256 * 1024 * 1024, total_bytes: 2 * 1024 * 1024 * 1024, archive_bytes: 1024 * 1024 * 1024, zip_metadata_bytes: 32 * 1024 * 1024, duration: Duration::from_secs(120) };
pub(crate) static IMPORT: ImportLifecycle = ImportLifecycle::new();

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
enum ImportStatus { Idle, Running, Completed, Failed }

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
enum ImportPhase { Extracting, Sorting }

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ArchiveImportReceipt {
    pub extracted_count: Option<usize>,
    pub sorted_count: usize,
    pub staging_path: String,
    pub issue: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct PreviousArchiveImport {
    source_name: String,
    receipt: ArchiveImportReceipt,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchiveImportSnapshot {
    revision: u64,
    job_id: Option<u64>,
    source_name: Option<String>,
    status: ImportStatus,
    phase: Option<ImportPhase>,
    bytes: u64,
    files: usize,
    cancel_requested: bool,
    result: Option<ArchiveImportReceipt>,
    previous_receipt: Option<PreviousArchiveImport>,
    error: Option<String>,
}

impl ArchiveImportSnapshot {
    const IDLE: Self = Self {
        revision: 0,
        job_id: None,
        source_name: None,
        status: ImportStatus::Idle,
        phase: None,
        bytes: 0,
        files: 0,
        cancel_requested: false,
        result: None,
        previous_receipt: None,
        error: None,
    };
}

struct ImportState {
    snapshot: ArchiveImportSnapshot,
    cancellation: Option<Arc<AtomicBool>>,
}

pub(crate) struct ImportLifecycle(Mutex<ImportState>);

impl ImportLifecycle {
    const fn new() -> Self {
        Self(Mutex::new(ImportState { snapshot: ArchiveImportSnapshot::IDLE, cancellation: None }))
    }

    pub(crate) fn snapshot(&self) -> Result<ArchiveImportSnapshot, String> {
        self.0.lock().map(|state| state.snapshot.clone()).map_err(|_| "Archive job lock failed".into())
    }

    pub(crate) fn start<'a>(&'a self, source: &Path, publish: &'a dyn Fn(&ArchiveImportSnapshot)) -> Result<ImportJob<'a>, String> {
        let cancelled = Arc::new(AtomicBool::new(false));
        let snapshot = {
            let mut state = self.0.lock().map_err(|_| "Archive job lock failed")?;
            if state.cancellation.is_some() { return Err("An archive import is already running".into()); }
            let id = state.snapshot.revision + 1;
            let previous_receipt = match (state.snapshot.source_name.take(), state.snapshot.result.take()) {
                (Some(source_name), Some(receipt)) => Some(PreviousArchiveImport { source_name, receipt }),
                _ => state.snapshot.previous_receipt.take(),
            };
            state.snapshot = ArchiveImportSnapshot {
                revision: id,
                job_id: Some(id),
                source_name: Some(source.file_name().unwrap_or(source.as_os_str()).to_string_lossy().into_owned()),
                status: ImportStatus::Running,
                phase: Some(ImportPhase::Extracting),
                previous_receipt,
                ..ArchiveImportSnapshot::IDLE
            };
            state.cancellation = Some(cancelled.clone());
            state.snapshot.clone()
        };
        let job = ImportJob { lifecycle: self, id: snapshot.revision, cancelled, publish };
        publish(&snapshot);
        Ok(job)
    }

    pub(crate) fn cancel(&self, job_id: Option<u64>) -> Result<Option<ArchiveImportSnapshot>, String> {
        self.change_active(job_id, |state| {
            if state.snapshot.cancel_requested { return false; }
            let Some(cancelled) = &state.cancellation else { return false; };
            cancelled.store(true, Ordering::Relaxed);
            state.snapshot.cancel_requested = true;
            true
        })
    }

    fn change_active(
        &self, job_id: Option<u64>, change: impl FnOnce(&mut ImportState) -> bool,
    ) -> Result<Option<ArchiveImportSnapshot>, String> {
        let mut state = self.0.lock().map_err(|_| "Archive job lock failed")?;
        if state.cancellation.is_none() || job_id.is_some_and(|id| state.snapshot.job_id != Some(id)) || !change(&mut state) {
            return Ok(None);
        }
        state.snapshot.revision += 1;
        Ok(Some(state.snapshot.clone()))
    }
}

pub(crate) struct ImportJob<'a> {
    lifecycle: &'a ImportLifecycle,
    id: u64,
    pub cancelled: Arc<AtomicBool>,
    publish: &'a dyn Fn(&ArchiveImportSnapshot),
}

impl ImportJob<'_> {
    pub(crate) fn progress(&self, bytes: u64, files: usize) {
        self.change(|state| {
            if (state.snapshot.bytes, state.snapshot.files) == (bytes, files) { return false; }
            state.snapshot.bytes = bytes;
            state.snapshot.files = files;
            true
        });
    }

    pub(crate) fn sorting(&self) {
        self.change(|state| {
            state.snapshot.phase = Some(ImportPhase::Sorting);
            true
        });
    }

    pub(crate) fn finish(&self, outcome: Result<ArchiveImportReceipt, &str>) {
        self.change(|state| {
            state.cancellation = None;
            state.snapshot.phase = None;
            match outcome {
                Ok(receipt) => {
                    if let Some(count) = receipt.extracted_count {
                        state.snapshot.files = count;
                    }
                    state.snapshot.status = ImportStatus::Completed;
                    state.snapshot.result = Some(receipt);
                    state.snapshot.previous_receipt = None;
                }
                Err(error) => {
                    state.snapshot.status = ImportStatus::Failed;
                    state.snapshot.error = Some(compact_import_issue(error));
                }
            }
            true
        });
    }

    fn change(&self, change: impl FnOnce(&mut ImportState) -> bool) {
        if let Ok(Some(snapshot)) = self.lifecycle.change_active(Some(self.id), change) {
            (self.publish)(&snapshot);
        }
    }
}

impl Drop for ImportJob<'_> {
    fn drop(&mut self) {
        self.finish(Err("Archive import stopped before reporting its result"));
    }
}

pub(crate) fn compact_import_issue(issue: &str) -> String {
    let mut chars = issue.chars();
    let mut result: String = chars.by_ref().take(512).collect();
    if chars.next().is_some() {
        result.push('…');
    }
    result
}

#[derive(Default)]
struct ExtractionReceipt {
    extracted: Vec<ExtractedEntry>,
    incomplete: Option<IncompleteEntry>,
    published_names: PublishedArchiveNames,
}

struct PublishedArchiveNames {
    names: HashSet<String>,
    ascii_folded_names: HashSet<String>,
    next_owned_suffix: HashMap<(String, bool), usize>,
    foreign_probe_limit: usize,
}

impl Default for PublishedArchiveNames {
    fn default() -> Self {
        Self {
            names: HashSet::new(),
            ascii_folded_names: HashSet::new(),
            next_owned_suffix: HashMap::new(),
            foreign_probe_limit: 10_000,
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ExtractedEntry {
    name: String,
    bytes: u64,
    is_file: bool,
}

struct IncompleteEntry {
    name: String,
    archive_path: String,
    expected_bytes: u64,
}

/// The journal records the handle identity before any payload is written,
/// then the exact destination before its atomic no-replace publication.
#[derive(Deserialize, Serialize)]
#[serde(tag = "state", rename_all = "camelCase", rename_all_fields = "camelCase")]
enum EntryEvent {
    Staged { name: String, archive_path: String, expected_bytes: u64, identity: String },
    Ready { name: String, destination: String },
}

struct EntryJournal(File);

impl EntryJournal {
    fn append(&mut self, event: &EntryEvent) -> Result<(), String> {
        let mut line = serde_json::to_vec(event).map_err(|error| error.to_string())?;
        line.push(b'\n');
        self.0.write_all(&line).and_then(|()| self.0.sync_data())
            .map_err(|error| format!("Cannot persist archive entry journal: {error}"))
    }
}

struct Budget<'a> {
    limits: Limits,
    started: Instant,
    cancelled: &'a AtomicBool,
    bytes: u64,
    receipt: ExtractionReceipt,
    staging_guard: &'a AncestorGuard,
    journal: Option<&'a mut EntryJournal>,
    last_progress: Instant,
    progress_interval: Duration,
    progress: &'a mut dyn FnMut(u64, usize),
}
struct GuardedReader<'a, R> { inner: R, remaining: u64, started: Instant, duration: Duration, cancelled: &'a AtomicBool }
impl<R: Read> Read for GuardedReader<'_, R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() { return Ok(0); }
        if self.cancelled.load(Ordering::Relaxed) { return Err(io::Error::other("Archive import cancelled")); }
        if self.started.elapsed() >= self.duration { return Err(io::Error::other("Archive import time limit exceeded")); }
        if self.remaining == 0 {
            // A read at the exact limit still needs to distinguish EOF from overflow.
            return match self.inner.read(&mut buffer[..1])? {
                0 => Ok(0),
                _ => Err(io::Error::other("Archive stream size limit exceeded")),
            };
        }
        let length = buffer.len().min(self.remaining.min(64 * 1024) as usize);
        let count = self.inner.read(&mut buffer[..length])?;
        self.remaining -= count as u64;
        Ok(count)
    }
}
impl Budget<'_> {
    fn check(&self) -> Result<(), String> {
        check_extraction_active(self.cancelled, self.started, self.limits.duration)
    }
    fn copy(&mut self, input: &mut impl Read, output: &mut File, expected: u64) -> Result<(), String> {
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
            if self.last_progress.elapsed() >= self.progress_interval {
                (self.progress)(self.bytes, self.receipt.extracted.len());
                self.last_progress = Instant::now();
            }
        }
        if copied != expected { return Err("Archive entry size differs from its declared size".into()); }
        Ok(())
    }

    fn extract_entry(
        &mut self,
        input: &mut impl Read,
        staging: &Path,
        entry_path: &Path,
        size: u64,
        modified: Option<SystemTime>,
    ) -> Result<(), String> {
        let file_name = output_name(entry_path)?;
        if size > self.limits.file_bytes || size > self.limits.total_bytes.saturating_sub(self.bytes) {
            return Err("Archive uncompressed size limit exceeded".into());
        }
        check_disk_space(staging, size)?;
        let (stage_name, stage, mut output) = create_entry_stage(staging, self.receipt.extracted.len(), file_name)?;
        self.receipt.incomplete = Some(IncompleteEntry {
            name: stage_name.clone(),
            archive_path: entry_path.to_string_lossy().into_owned(),
            expected_bytes: size,
        });

        if let Some(journal) = &mut self.journal {
            journal.append(&EntryEvent::Staged {
                name: stage_name.clone(),
                archive_path: entry_path.to_string_lossy().into_owned(),
                expected_bytes: size,
                identity: safe_fs::identity_from_handle(&output)?.key(),
            })?;
        }
        self.copy(input, &mut output, size)?;
        preserve_modified_time(&output, &stage, modified)?;
        output.sync_all().map_err(|error| error.to_string())?;
        self.check()?;
        let journal = &mut self.journal;
        let (cancelled, started, duration) = (self.cancelled, self.started, self.limits.duration);
        let name = publish_entry(
            staging, self.staging_guard, &stage, &output, file_name, &mut self.receipt.published_names,
            || check_extraction_active(cancelled, started, duration),
            |destination| {
                if let Some(journal) = journal {
                    journal.append(&EntryEvent::Ready { name: stage_name.clone(), destination: destination.into() })?;
                }
                Ok(())
            },
        )?;
        self.receipt.incomplete = None;
        self.receipt.extracted.push(ExtractedEntry { name, bytes: size, is_file: true });
        Ok(())
    }
}

fn check_extraction_active(cancelled: &AtomicBool, started: Instant, duration: Duration) -> Result<(), String> {
    if cancelled.load(Ordering::Relaxed) { return Err("Archive import cancelled".into()); }
    if started.elapsed() >= duration { return Err("Archive import time limit exceeded".into()); }
    Ok(())
}

pub(crate) fn extract_archive(
    path: &Path,
    cancelled: &AtomicBool,
    observe_staging: impl FnOnce(&Path) -> Result<(), String>,
    progress: impl FnMut(u64, usize),
) -> Result<ArchiveExtraction, ArchiveExtractionError> {
    let base_dir = crate::application_data_dir().map_err(|e| e.to_string())?.join("archive-imports");
    extract_archive_in(path, &base_dir, cancelled, observe_staging, progress)
}

fn extract_archive_in(
    path: &Path,
    base_dir: &Path,
    cancelled: &AtomicBool,
    observe_staging: impl FnOnce(&Path) -> Result<(), String>,
    mut progress: impl FnMut(u64, usize),
) -> Result<ArchiveExtraction, ArchiveExtractionError> {
    let (source, _source_guard, format) = open_archive(path, LIMITS)?;
    let (staging_dir, staging_guard) = create_staging_dir_in(base_dir)?;
    let setup_error = |error| ArchiveExtractionError::retained(
        &staging_dir, None, format!("Import staging retained at {}. {error}", staging_dir.display()),
    );
    observe_staging(&staging_dir).map_err(setup_error)?;
    let journal_path = staging_dir.with_extension("entries.jsonl");
    let mut journal = EntryJournal(safe_fs::create_guarded_staging_file(&journal_path)
        .map_err(|error| setup_error(format!("Cannot create archive entry journal: {error}")))?);
    let journal_name = journal_path.file_name()
        .ok_or_else(|| setup_error("Archive entry journal has no filename".into()))?
        .to_string_lossy();
    save_manifest_with_receipt(&staging_dir, Some(path), "extracting", &[], None, None, Some(&journal_name))
        .map_err(setup_error)?;
    let (result, receipt) = extract_archive_from(
        source, format, &staging_dir, &staging_guard, LIMITS, cancelled, &mut progress, Some(&mut journal),
    );
    let extracted_files = receipt.extracted.len();
    match result {
        Ok(()) => {
            save_manifest_with_receipt(&staging_dir, Some(path), "extracted", &[], None, Some(&receipt), None)
                .map_err(|error| ArchiveExtractionError::retained(&staging_dir, Some(extracted_files), format!(
                    "Import staging retained at {}. Manifest finalization failed: {error}", staging_dir.display(),
                )))?;
            Ok(ArchiveExtraction {
                staging_dir,
                extracted_files,
                _staging_guard: staging_guard,
            })
        }
        Err(err) => {
            // Successful entries and partially extracted data may be useful for
            // recovery. Never sweep an import folder after sorting may begin.
            let manifest = save_manifest_with_receipt(&staging_dir, Some(path), "failed", &[], Some(&err), Some(&receipt), None);
            let mut message = format!("{err}. Import data retained at {}", staging_dir.display());
            if let Err(error) = manifest {
                message.push_str(&format!(". Manifest finalization failed: {error}"));
            }
            let known_count = (extracted_files != 0 || receipt.incomplete.is_some()).then_some(extracted_files);
            Err(ArchiveExtractionError::retained(&staging_dir, known_count, message))
        }
    }
}

fn create_staging_dir_in(base_dir: &Path) -> Result<(PathBuf, AncestorGuard), String> {
    let _base_guard = safe_fs::create_guarded_directory(base_dir)?;

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
            Ok(()) => {
                let guard = safe_fs::guard_directory(&dir)?;
                return Ok((dir, guard));
            }
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

fn open_archive(path: &Path, limits: Limits) -> Result<(File, AncestorGuard, ArchiveFormat), String> {
    let format = archive_format(path)?;
    let (source, guard) = safe_fs::open_guarded_regular_file(path)
        .map_err(|e| format!("Failed to open archive {}: {e}", path.display()))?;
    if source.metadata().map_err(|e| e.to_string())?.len() > limits.archive_bytes {
        return Err("Archive input size limit exceeded".into());
    }
    Ok((source, guard, format))
}

#[cfg(test)]
fn extract_archive_to_with_progress(path: &Path, staging_dir: &Path, limits: Limits, cancelled: &AtomicBool, progress: &mut dyn FnMut(u64, usize)) -> Result<usize, String> {
    let (source, _source_guard, format) = open_archive(path, limits)?;
    let staging_guard = safe_fs::create_guarded_directory(staging_dir)?;
    let (result, receipt) = extract_archive_from(
        source, format, staging_dir, &staging_guard, limits, cancelled, progress, None,
    );
    result.map(|()| receipt.extracted.len())
}

fn extract_archive_from<'a>(
    source: File, format: ArchiveFormat, staging_dir: &Path, staging_guard: &'a AncestorGuard, limits: Limits,
    cancelled: &'a AtomicBool, progress: &'a mut dyn FnMut(u64, usize), journal: Option<&'a mut EntryJournal>,
) -> (Result<(), String>, ExtractionReceipt) {
    let mut budget = Budget {
        limits, started: Instant::now(), cancelled, bytes: 0, receipt: ExtractionReceipt::default(), staging_guard, journal,
        last_progress: Instant::now(), progress_interval: Duration::from_millis(250), progress,
    };
    let result = (|| {
        budget.check()?;
        let staging = fs::canonicalize(staging_dir).map_err(|error| error.to_string())?;
        let result = match format {
            ArchiveFormat::Zip => extract_zip_to(source, &staging, &mut budget),
            ArchiveFormat::Tgz => extract_tgz_to(source, &staging, &mut budget),
        };
        (budget.progress)(budget.bytes, budget.receipt.extracted.len());
        result
    })();
    (result, budget.receipt)
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

// zip 0.6.6 probes for footer signatures one byte at a time. Cache aligned
// windows so those probes do not turn into a seek and disk read per byte.
const ZIP_READ_WINDOW: usize = 16 * 1024;
struct ZipReader<'a> {
    file: File,
    len: u64,
    position: u64,
    window: [u8; ZIP_READ_WINDOW],
    window_start: u64,
    window_len: usize,
    cancelled: &'a AtomicBool,
    started: Instant,
    duration: Duration,
}

impl<'a> ZipReader<'a> {
    fn new(file: File, budget: &Budget<'a>) -> io::Result<Self> {
        let len = file.metadata()?.len();
        if len > budget.limits.archive_bytes {
            return Err(io::Error::other("Archive input size limit exceeded"));
        }
        Ok(Self {
            file,
            len,
            position: 0,
            window: [0; ZIP_READ_WINDOW],
            window_start: 0,
            window_len: 0,
            cancelled: budget.cancelled,
            started: budget.started,
            duration: budget.limits.duration,
        })
    }

    fn read_at(&mut self, position: u64, buffer: &mut [u8]) -> io::Result<()> {
        self.seek(SeekFrom::Start(position))?;
        self.read_exact(buffer)
    }
}

impl Read for ZipReader<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if self.cancelled.load(Ordering::Relaxed) {
            return Err(io::Error::other("Archive import cancelled"));
        }
        if self.started.elapsed() >= self.duration {
            return Err(io::Error::other("Archive import time limit exceeded"));
        }
        if buffer.is_empty() || self.position >= self.len {
            return Ok(0);
        }
        if self.position < self.window_start || self.position >= self.window_start + self.window_len as u64 {
            let start = self.position / ZIP_READ_WINDOW as u64 * ZIP_READ_WINDOW as u64;
            let len = (self.len - start).min(ZIP_READ_WINDOW as u64) as usize;
            self.file.seek(SeekFrom::Start(start))?;
            self.file.read_exact(&mut self.window[..len])?;
            self.window_start = start;
            self.window_len = len;
        }
        let start = (self.position - self.window_start) as usize;
        let len = buffer.len().min(self.window_len - start);
        buffer[..len].copy_from_slice(&self.window[start..start + len]);
        self.position += len as u64;
        Ok(len)
    }
}

impl Seek for ZipReader<'_> {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        self.position = match position {
            SeekFrom::Start(offset) => Some(offset),
            SeekFrom::End(offset) => self.len.checked_add_signed(offset),
            SeekFrom::Current(offset) => self.position.checked_add_signed(offset),
        }
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Invalid ZIP seek"))?;
        Ok(self.position)
    }
}

fn zip_field<const N: usize>(bytes: &[u8], offset: usize) -> [u8; N] {
    bytes[offset..offset + N].try_into().expect("fixed ZIP header field")
}

fn find_zip64_end(reader: &mut ZipReader<'_>, mut position: u64, last: u64) -> io::Result<u64> {
    let mut bytes = [0; ZIP_READ_WINDOW];
    while position <= last {
        // Keep three bytes of overlap so signatures across windows are visible.
        let candidates = (last - position + 1).min((bytes.len() - 3) as u64) as usize;
        reader.read_at(position, &mut bytes[..candidates + 3])?;
        if let Some(index) = bytes[..candidates + 3].windows(4).position(|magic| magic == b"PK\x06\x06") {
            return Ok(position + index as u64);
        }
        position += candidates as u64;
    }
    Err(io::Error::other("Missing ZIP64 central directory end"))
}

fn preflight_zip(reader: &mut ZipReader<'_>, limits: Limits) -> io::Result<usize> {
    const END_LEN: usize = 22;
    const LOCATOR_LEN: usize = 20;
    const ENTRY_LEN: usize = 46;
    // Mirror the footer selection in pinned zip 0.6.6 spec.rs and read.rs.
    let tail_len = reader.len.min((END_LEN + usize::from(u16::MAX)) as u64) as usize;
    if tail_len < END_LEN {
        return Err(io::Error::other("Missing ZIP central directory end"));
    }
    let mut tail = vec![0; tail_len];
    let tail_start = reader.len - tail_len as u64;
    reader.read_at(tail_start, &mut tail)?;
    let end_index = tail[..tail_len - END_LEN + 4].windows(4)
        .rposition(|magic| magic == b"PK\x05\x06")
        .ok_or_else(|| io::Error::other("Missing ZIP central directory end"))?;
    let end_position = tail_start + end_index as u64;
    let end = &tail[end_index..end_index + END_LEN];
    let comment_len = usize::from(u16::from_le_bytes(zip_field(end, 20)));
    if comment_len > tail_len - end_index - END_LEN {
        return Err(io::Error::other("Truncated ZIP comment"));
    }

    // The parser looks relative to EOF, even if the classic fields fit.
    let mut locator = [0; LOCATOR_LEN];
    if let Some(position) = reader.len.checked_sub((END_LEN + LOCATOR_LEN + comment_len) as u64) {
        reader.read_at(position, &mut locator)?;
    }
    let invalid_offset = || io::Error::other("Invalid ZIP central directory offset");
    let (count, start, end) = if &locator[..4] == b"PK\x06\x07" {
        let nominal = u64::from_le_bytes(zip_field(&locator, 8));
        let last = end_position.checked_sub(60).ok_or_else(invalid_offset)?;
        let position = find_zip64_end(reader, nominal, last)?;
        let mut end = [0; 56];
        reader.read_at(position, &mut end)?;
        let count = u64::from_le_bytes(zip_field(&end, 32));
        let offset = u64::from_le_bytes(zip_field(&end, 48));
        let start = offset.checked_add(position - nominal).ok_or_else(invalid_offset)?;
        (count, start, position)
    } else {
        let count = u64::from(u16::from_le_bytes(zip_field(end, 8)));
        let size = u64::from(u32::from_le_bytes(zip_field(end, 12)));
        let offset = u64::from(u32::from_le_bytes(zip_field(end, 16)));
        let start = end_position.checked_sub(size)
            .filter(|start| *start >= offset)
            .ok_or_else(invalid_offset)?;
        (count, start, end_position)
    };
    if count > limits.entries as u64 {
        return Err(io::Error::other("Archive entry count limit exceeded"));
    }
    if start > end {
        return Err(invalid_offset());
    }

    // The parser allocates the variable fields in each record. Walk only their
    // fixed headers first so forged lengths cannot drive those allocations.
    let mut position = start;
    let mut entry = [0; ENTRY_LEN];
    for _ in 0..count {
        if end - position < ENTRY_LEN as u64 {
            return Err(io::Error::other("Truncated ZIP central directory entry"));
        }
        reader.read_at(position, &mut entry)?;
        if &entry[..4] != b"PK\x01\x02" {
            return Err(io::Error::other("Invalid ZIP central directory entry"));
        }
        let variable_len: u64 = [28, 30, 32].into_iter()
            .map(|offset| u64::from(u16::from_le_bytes(zip_field(&entry, offset))))
            .sum();
        position += ENTRY_LEN as u64 + variable_len;
        if position > end {
            return Err(io::Error::other("Invalid ZIP central directory entry length"));
        }
        if position - start > limits.zip_metadata_bytes {
            return Err(io::Error::other("Archive ZIP metadata size limit exceeded"));
        }
    }
    Ok(count as usize)
}

fn extract_zip_to(file: File, staging_dir: &Path, budget: &mut Budget<'_>) -> Result<(), String> {
    let mut reader = ZipReader::new(file, budget).map_err(|e| e.to_string())?;
    let count = preflight_zip(&mut reader, budget.limits)
        .map_err(|e| format!("Failed to read zip archive: {e}"))?;
    budget.check()?;
    let mut archive = ZipArchive::new(reader).map_err(|e| format!("Failed to read zip archive: {e}"))?;
    if archive.len() != count {
        return Err("ZIP central directory changed during validation".into());
    }
    budget.check()?;

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
        let date = entry.last_modified();
        let timestamp = chrono::NaiveDate::from_ymd_opt(date.year() as i32, date.month() as u32, date.day() as u32)
            .and_then(|day| day.and_hms_opt(date.hour() as u32, date.minute() as u32, date.second() as u32))
            .map(|date| date.and_utc().timestamp()).filter(|seconds| *seconds >= 0).map(|seconds| UNIX_EPOCH + Duration::from_secs(seconds as u64));
        budget.extract_entry(&mut entry, staging_dir, &entry_path, size, timestamp)?;
    }

    Ok(())
}

fn extract_tgz_to(file: File, staging_dir: &Path, budget: &mut Budget<'_>) -> Result<(), String> {
    let decoder = GzDecoder::new(file);
    // Header/padding overhead is bounded separately from extracted file bytes.
    let stream_limit = budget.limits.total_bytes.saturating_add((budget.limits.entries as u64).saturating_mul(1024));
    let mut archive = Archive::new(GuardedReader { inner: decoder, remaining: stream_limit, started: budget.started, duration: budget.limits.duration, cancelled: budget.cancelled });
    let entries = archive
        .entries()
        .map_err(|e| format!("Failed to read tar archive: {}", e))?;

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
        let size = entry.size();
        let modified = entry.header().mtime().ok().and_then(|seconds| UNIX_EPOCH.checked_add(Duration::from_secs(seconds)));
        budget.extract_entry(&mut entry, staging_dir, &entry_path, size, modified)?;
    }

    // TAR ends at its first zero header. Finish gzip under the same budget so
    // flate2 checks the footer even when TAR stops before reaching it.
    io::copy(&mut archive.into_inner(), &mut io::sink())
        .map_err(|e| format!("Failed to finish gzip stream: {e}"))?;
    Ok(())
}

fn output_name(entry_path: &Path) -> Result<&str, String> {
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

    // Windows device names must be checked even when the staged publish uses a verbatim path.
    if file_name.chars().any(|c| c.is_control() || ":<>|?*\\/".contains(c))
        || file_name.ends_with(['.', ' '])
        || (cfg!(windows) && (file_name.contains('"') || is_windows_device_name(file_name)))
    {
        return Err("Archive contains an unsupported filename".into());
    }
    Ok(file_name)
}

fn is_windows_device_name(file_name: &str) -> bool {
    let stem = file_name.split('.').next().unwrap_or_default().trim_end_matches(' ').to_ascii_uppercase();
    matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$")
        || stem.strip_prefix("COM").or_else(|| stem.strip_prefix("LPT"))
            .is_some_and(|suffix| matches!(suffix, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"))
}

fn create_entry_stage(staging: &Path, entry_index: usize, target_name: &str) -> Result<(String, PathBuf, File), String> {
    for attempt in 0..10_000 {
        let name = format!(".mouzi-archive-entry-{entry_index}-{attempt}.part");
        if name.eq_ignore_ascii_case(target_name) { continue; }
        let path = staging.join(&name);
        match safe_fs::create_guarded_staging_file(&path) {
            Ok(file) => return Ok((name, path, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(format!("Cannot stage {}: {error}", path.display())),
        }
    }
    Err("No free archive staging filename".into())
}

fn publish_entry(
    staging: &Path, staging_guard: &AncestorGuard, stage: &Path, output: &File, file_name: &str,
    published_names: &mut PublishedArchiveNames,
    mut check_active: impl FnMut() -> Result<(), String>,
    mut record_destination: impl FnMut(&str) -> Result<(), String>,
) -> Result<String, String> {
    let leaf = Path::new(file_name);
    let stem = leaf.file_stem().and_then(|s| s.to_str()).unwrap_or(file_name);
    let extension = leaf.extension().and_then(|s| s.to_str());
    // Query the pinned directory while it contains this entry's staged file.
    // A confirmed case-insensitive directory can reuse ASCII alias ownership.
    let ascii_case_aliases = staging_guard.has_ascii_case_aliases();
    let cursor_key = (
        if ascii_case_aliases { file_name.to_ascii_lowercase() } else { file_name.to_owned() },
        ascii_case_aliases,
    );
    // Remember only the contiguous prefix of names this import already
    // published. A foreign file can disappear, so its slot remains eligible
    // and receives a fresh filesystem check on the next entry.
    let mut next_owned = published_names.next_owned_suffix.get(&cursor_key).copied().unwrap_or(0);
    // The import can publish up to LIMITS.entries files with the same basename.
    // Bound filesystem probes separately when other occupants block publication.
    let mut probes_remaining = published_names.foreign_probe_limit;
    for index in next_owned.. {
        check_active()?;
        let name = if index == 0 { file_name.to_string() } else { match extension { Some(ext) => format!("{stem} ({index}).{ext}"), None => format!("{stem} ({index})") } };
        let already_owned = if ascii_case_aliases {
            published_names.ascii_folded_names.contains(&name.to_ascii_lowercase())
        } else {
            published_names.names.contains(&name)
        };
        if already_owned {
            if next_owned == index { next_owned += 1; }
            continue;
        }
        if probes_remaining == 0 { break; }
        probes_remaining -= 1;
        let target = staging.join(&name);
        if safe_fs::path_occupied(&target)? { continue; }
        record_destination(&name)?;
        check_active()?;
        match safe_fs::publish_staged_file(output, stage, &target) {
            Ok(true) => {
                published_names.names.insert(name.clone());
                published_names.ascii_folded_names.insert(name.to_ascii_lowercase());
                if next_owned == index { next_owned += 1; }
                if index > 0 && next_owned > 0 {
                    published_names.next_owned_suffix.insert(cursor_key, next_owned);
                }
                return Ok(name);
            }
            Ok(false) => continue,
            Err(error) => return Err(format!("Cannot publish {}: {error}", target.display())),
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
    save_manifest_with_receipt(staging, source, status, results, message, None, None)
}

fn save_manifest_with_receipt(
    staging: &Path,
    source: Option<&Path>,
    status: &str,
    results: &[(String, String, String)],
    message: Option<&str>,
    extraction: Option<&ExtractionReceipt>,
    journal_name: Option<&str>,
) -> Result<(), String> {
    let _staging_guard = safe_fs::guard_directory(staging)?;
    let manifest = staging.with_extension("manifest.json");
    let mut value = match fs::symlink_metadata(&manifest) {
        Ok(_) => {
            let (file, _guard) = safe_fs::open_guarded_regular_file(&manifest)?;
            serde_json::from_reader::<_, serde_json::Value>(BufReader::new(file)).map_err(|e| e.to_string())?
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound =>
            serde_json::json!({"version":1,"createdAt":chrono::Utc::now().to_rfc3339(),"source":source.map(|p|p.to_string_lossy().into_owned()),"staging":staging.to_string_lossy(),"limits":{"entries":LIMITS.entries,"fileBytes":LIMITS.file_bytes,"totalBytes":LIMITS.total_bytes,"zipMetadataBytes":LIMITS.zip_metadata_bytes,"seconds":LIMITS.duration.as_secs()}}),
        Err(error) => return Err(error.to_string()),
    };
    if let Some(name) = journal_name {
        value["entryJournal"] = serde_json::json!(name);
    }
    let incomplete = extraction.and_then(|receipt| receipt.incomplete.as_ref());
    let mut incomplete_bytes = None;
    let mut remaining = Vec::new();
    for entry in fs::read_dir(staging).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let metadata = fs::symlink_metadata(entry.path()).map_err(|e| e.to_string())?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let mut row = serde_json::json!({"name":name,"bytes":metadata.len(),"isFile":metadata.is_file()});
        if incomplete.is_some_and(|pending| pending.name == name) {
            incomplete_bytes = metadata.is_file().then_some(metadata.len());
            row["incomplete"] = serde_json::json!(true);
        }
        remaining.push(row);
    }
    if let Some(receipt) = extraction {
        value["extractedEntries"] = serde_json::json!(receipt.extracted);
        value["extractedCount"] = serde_json::json!(receipt.extracted.len());
        let incomplete_entries = incomplete.map(|pending| serde_json::json!({
            "name": pending.name,
            "archivePath": pending.archive_path,
            "bytes": incomplete_bytes,
            "expectedBytes": pending.expected_bytes,
        }));
        value["incompleteEntries"] = serde_json::json!(incomplete_entries.into_iter().collect::<Vec<_>>());
    } else if status == "extracted" {
        value["extractedEntries"] = serde_json::json!(remaining);
    }
    value["remainingEntries"] = serde_json::json!(remaining);
    value["status"] = serde_json::json!(status);
    value["message"] = serde_json::json!(message);
    value["updatedAt"] = serde_json::json!(chrono::Utc::now().to_rfc3339());
    value["sortedResults"] = serde_json::json!(results);

    let bytes = serde_json::to_vec_pretty(&value).map_err(|e| e.to_string())?;
    replace_manifest(&manifest, &bytes).map_err(|e| format!("Cannot persist import manifest {}: {e}", manifest.display()))
}

fn replace_manifest(manifest: &Path, bytes: &[u8]) -> io::Result<()> {
    // Write in the same directory so publishing the new receipt is one rename.
    for attempt in 0..100 {
        let temporary = manifest.with_extension(format!("json.{}.{}.tmp", std::process::id(), attempt));
        let mut file = match OpenOptions::new().write(true).create_new(true).open(&temporary) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        };
        let written = file.write_all(bytes).and_then(|_| file.sync_all());
        drop(file);
        let result = written.and_then(|_| fs::rename(&temporary, manifest));
        if result.is_err() {
            let _ = fs::remove_file(temporary);
        }
        return result;
    }
    Err(io::Error::other("Import manifest temporary filename limit exceeded"))
}

const MAX_RECOVERY_RECORD_BYTES: u64 = 1024 * 1024;
const MAX_RECOVERY_JOURNAL_BYTES: u64 = 512 * 1024 * 1024;

struct RecordedEntry {
    stage: IncompleteEntry,
    identity: String,
    destination: Option<String>,
}

fn is_recovery_leaf(name: &str) -> bool {
    output_name(Path::new(name)).is_ok_and(|leaf| leaf == name)
}

// A crash can tear the last write. Replay only the complete, well-formed
// prefix. Directory entries outside that evidence are left unverified.
fn read_entry_journal(file: File) -> Result<(Vec<RecordedEntry>, bool), String> {
    let length = file.metadata().map_err(|error| error.to_string())?.len();
    let mut reader = BufReader::new(file).take(MAX_RECOVERY_JOURNAL_BYTES);
    let mut entries: Vec<RecordedEntry> = Vec::new();
    let mut names = HashSet::new();
    let mut identities = HashSet::new();
    let mut line = Vec::new();
    let mut expected_total = 0u64;
    loop {
        line.clear();
        let count = (&mut reader).take(MAX_RECOVERY_RECORD_BYTES + 1).read_until(b'\n', &mut line)
            .map_err(|error| error.to_string())?;
        if count == 0 {
            return Ok((entries, length > MAX_RECOVERY_JOURNAL_BYTES));
        }
        if count as u64 > MAX_RECOVERY_RECORD_BYTES || !line.ends_with(b"\n") {
            break;
        }
        match serde_json::from_slice::<EntryEvent>(&line) {
            Ok(EntryEvent::Staged { name, archive_path, expected_bytes, identity })
                if entries.len() < LIMITS.entries
                    && is_recovery_leaf(&name)
                    && name.starts_with(".mouzi-archive-entry-") && name.ends_with(".part")
                    && expected_bytes <= LIMITS.file_bytes
                    && expected_bytes <= LIMITS.total_bytes.saturating_sub(expected_total)
                    && names.insert(name.clone())
                    && identities.insert(identity.clone()) =>
            {
                expected_total += expected_bytes;
                entries.push(RecordedEntry {
                    stage: IncompleteEntry { name, archive_path, expected_bytes },
                    identity,
                    destination: None,
                });
            }
            Ok(EntryEvent::Ready { name, destination }) if is_recovery_leaf(&destination)
                && !name.eq_ignore_ascii_case(&destination) =>
            {
                let Some(entry) = entries.last_mut().filter(|entry| entry.stage.name == name) else { break; };
                entry.destination = Some(destination);
            }
            _ => break,
        }
    }
    Ok((entries, true))
}

fn matching_payload_size(path: &Path, identity: &str) -> Option<u64> {
    let (metadata, observed) = safe_fs::inventory_file_snapshot(path).ok()??;
    (observed.key() == identity).then_some(metadata.len())
}

fn reconcile_extraction(staging: &Path, journal: File, manifest: &mut serde_json::Value) -> Result<(), String> {
    let (entries, truncated) = read_entry_journal(journal)?;
    let mut extracted = Vec::new();
    let mut incomplete = Vec::new();
    let mut unverified = Vec::new();
    let mut recorded = HashSet::new();
    let mut partial_names = HashSet::new();
    for RecordedEntry { stage, identity, destination } in entries {
        let staged_path = staging.join(&stage.name);
        if let Some(bytes) = matching_payload_size(&staged_path, &identity)
            .filter(|bytes| *bytes <= stage.expected_bytes)
        {
            recorded.insert(stage.name.clone());
            partial_names.insert(stage.name.clone());
            incomplete.push(serde_json::json!({
                "name": stage.name, "archivePath": stage.archive_path, "bytes": bytes,
                "expectedBytes": stage.expected_bytes,
            }));
        } else if let Some(name) = destination.as_ref().filter(|name| {
            safe_fs::source_absence(&staged_path) == safe_fs::SourceAbsence::Absent
                && matching_payload_size(&staging.join(name), &identity) == Some(stage.expected_bytes)
        }) {
            recorded.insert(name.clone());
            extracted.push(ExtractedEntry { name: name.clone(), bytes: stage.expected_bytes, is_file: true });
        } else {
            unverified.push(serde_json::json!({
                "name": stage.name, "archivePath": stage.archive_path, "destination": destination,
                "expectedBytes": stage.expected_bytes,
            }));
        }
    }
    let (remaining, directory_truncated) = recovery_remaining_entries(staging, &partial_names)?;
    for row in &remaining {
        if let Some(name) = row["name"].as_str().filter(|name| !recorded.contains(*name)) {
            unverified.push(serde_json::json!({"name":name}));
        }
    }
    manifest["extractedCount"] = serde_json::json!(extracted.len());
    manifest["extractedEntries"] = serde_json::json!(extracted);
    manifest["incompleteEntries"] = serde_json::json!(incomplete);
    manifest["unverifiedEntries"] = serde_json::json!(unverified);
    manifest["remainingEntries"] = serde_json::json!(remaining);
    manifest["recoveryEvidenceTruncated"] = serde_json::json!(truncated || directory_truncated);
    Ok(())
}

fn recovery_remaining_entries(staging: &Path, partial_names: &HashSet<String>)
    -> Result<(Vec<serde_json::Value>, bool), String>
{
    let mut remaining = Vec::new();
    for item in fs::read_dir(staging).map_err(|error| error.to_string())? {
        if remaining.len() == LIMITS.entries {
            return Ok((remaining, true));
        }
        let item = item.map_err(|error| error.to_string())?;
        let metadata = fs::symlink_metadata(item.path()).map_err(|error| error.to_string())?;
        let name = item.file_name().to_string_lossy().into_owned();
        let mut value = serde_json::json!({"name":name,"bytes":metadata.len(),"isFile":metadata.is_file()});
        if partial_names.contains(&name) {
            value["incomplete"] = serde_json::json!(true);
        }
        remaining.push(value);
    }
    Ok((remaining, false))
}

struct RecoveredImport {
    status: String,
    message: Option<String>,
}

fn reconcile_pending_manifest(base: &Path, source: &str, staging: &Path) -> Result<Option<RecoveredImport>, String> {
    // Only a direct child registered by this profile is eligible. The
    // manifest and journal names are computed here, never followed from JSON.
    if staging.parent() != Some(base) || !staging.file_name().is_some_and(|name| name.to_string_lossy().starts_with("import-")) {
        return Ok(None);
    }
    let _staging_guard = safe_fs::guard_directory(staging)?;
    let manifest_path = staging.with_extension("manifest.json");
    if safe_fs::source_absence(&manifest_path) == safe_fs::SourceAbsence::Absent {
        return Ok(None);
    }
    let mut manifest = {
        let (file, _guard) = safe_fs::open_guarded_regular_file(&manifest_path)?;
        serde_json::from_reader::<_, serde_json::Value>(BufReader::new(file).take(64 * 1024 * 1024))
            .map_err(|error| error.to_string())?
    };
    let journal_path = staging.with_extension("entries.jsonl");
    let journal_name = journal_path.file_name().and_then(|name| name.to_str());
    if manifest["version"].as_u64() != Some(1)
        || manifest["source"].as_str() != Some(source)
        || manifest["staging"].as_str() != staging.to_str()
        || journal_name.is_none()
        || manifest["entryJournal"].as_str() != journal_name
    {
        return Ok(None);
    }
    match manifest["status"].as_str() {
        Some("extracting") => {
            let (journal, _guard) = safe_fs::open_guarded_regular_file(&journal_path)?;
            reconcile_extraction(staging, journal, &mut manifest)?;
        }
        Some("extracted") => {
            // Sorting may already have moved some published entries. Keep the
            // recorded extraction receipt and refresh only remaining entries.
            let (remaining, truncated) = recovery_remaining_entries(staging, &HashSet::new())?;
            manifest["remainingEntries"] = serde_json::json!(remaining);
            manifest["recoveryEvidenceTruncated"] = serde_json::json!(truncated);
        }
        Some(status @ ("completed" | "partial" | "failed" | "interrupted")) => {
            // The manifest can reach its final state before SQLite records it.
            return Ok(Some(RecoveredImport {
                status: status.to_string(),
                message: manifest["message"].as_str().map(str::to_owned),
            }));
        }
        _ => return Ok(None),
    }
    manifest["status"] = serde_json::json!("interrupted");
    manifest["message"] = serde_json::json!(ARCHIVE_INTERRUPTED_MESSAGE);
    manifest["updatedAt"] = serde_json::json!(chrono::Utc::now().to_rfc3339());
    let bytes = serde_json::to_vec_pretty(&manifest).map_err(|error| error.to_string())?;
    replace_manifest(&manifest_path, &bytes).map_err(|error| error.to_string())?;
    Ok(Some(RecoveredImport {
        status: "interrupted".into(),
        message: Some(ARCHIVE_INTERRUPTED_MESSAGE.into()),
    }))
}

const ARCHIVE_INTERRUPTED_MESSAGE: &str = "Archive import was interrupted when Mouzi closed. Review the retained files and recovery records.";

pub(crate) fn recover_pending_imports(data_dir: &Path) {
    // Take the prior-process snapshot before any new import can start.
    let pending = match crate::db::pending_archive_imports() {
        Ok(pending) if pending.is_empty() => return,
        Ok(pending) => pending,
        Err(error) => { log::warn!("Cannot read archive recovery records: {error}"); return; }
    };
    let base = data_dir.join("archive-imports");
    if let Err(error) = std::thread::Builder::new().name("mouzi-archive-recovery".into()).spawn(move || {
        for (id, source, staging) in pending {
            match reconcile_pending_manifest(&base, &source, Path::new(&staging)) {
                Ok(Some(recovered)) => {
                    if let Err(error) = crate::db::finalize_operation_intent(
                        id, &recovered.status, Some(&staging), recovered.message.as_deref(),
                    ) {
                        log::warn!("Cannot finish recovered archive import {id}: {error}");
                    }
                }
                Ok(None) => {}
                Err(error) => log::warn!("Cannot reconcile archive import {id}: {error}"),
            }
        }
    }) {
        log::warn!("Cannot start archive recovery: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::io::Write;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use zip::write::FileOptions;

    static TEST_DIR_COUNTER: AtomicUsize = AtomicUsize::new(0);

    #[test]
    fn archive_import_state_preserves_progress_and_partial_receipt_across_reads() {
        let lifecycle = ImportLifecycle::new();
        let events = RefCell::new(Vec::new());
        let publish = |snapshot: &ArchiveImportSnapshot| events.borrow_mut().push(snapshot.clone());
        assert_eq!(lifecycle.snapshot().unwrap(), ArchiveImportSnapshot::IDLE);
        let job = lifecycle.start(Path::new("fixture.zip"), &publish).unwrap();
        let started = lifecycle.snapshot().unwrap();
        assert_eq!((started.status, started.phase), (ImportStatus::Running, Some(ImportPhase::Extracting)));
        assert!(lifecycle.start(Path::new("busy.zip"), &publish).is_err());
        assert_eq!(lifecycle.snapshot().unwrap(), started);

        job.progress(4096, 1);
        let progress = lifecycle.snapshot().unwrap();
        assert_eq!((progress.job_id, progress.bytes, progress.files), (started.job_id, 4096, 1));
        assert!(progress.revision > started.revision);
        job.sorting();
        assert_eq!(lifecycle.snapshot().unwrap().phase, Some(ImportPhase::Sorting));
        let cancel = lifecycle.cancel(started.job_id).unwrap().unwrap();
        publish(&cancel);
        assert!(cancel.cancel_requested && job.cancelled.load(Ordering::Relaxed));
        assert!(lifecycle.cancel(started.job_id).unwrap().is_none());

        let receipt = ArchiveImportReceipt {
            extracted_count: Some(2),
            sorted_count: 1,
            staging_path: "C:/Mouzi/staging/fixture".into(),
            issue: Some("Archive import cancelled".into()),
        };
        job.finish(Ok(receipt));
        let completed = lifecycle.snapshot().unwrap();
        assert_eq!(serde_json::to_value(&completed).unwrap(), serde_json::json!({
            "revision": 5,
            "jobId": started.job_id,
            "sourceName": "fixture.zip",
            "status": "completed",
            "phase": null,
            "bytes": 4096,
            "files": 2,
            "cancelRequested": true,
            "result": {
                "extractedCount": 2,
                "sortedCount": 1,
                "stagingPath": "C:/Mouzi/staging/fixture",
                "issue": "Archive import cancelled",
            },
            "error": null,
            "previousReceipt": null,
        }));
        drop(job);
        assert_eq!(lifecycle.snapshot().unwrap(), completed);
        assert_eq!(events.into_inner().last(), Some(&completed));
    }

    #[test]
    fn archive_import_keeps_the_last_recoverable_source_until_another_has_a_receipt() {
        let lifecycle = ImportLifecycle::new();
        let publish = |_: &ArchiveImportSnapshot| {};
        let first = ArchiveImportReceipt {
            extracted_count: Some(2), sorted_count: 1, staging_path: "staging/first".into(), issue: None,
        };
        lifecycle.start(Path::new("inputs/first.zip"), &publish).unwrap().finish(Ok(first.clone()));

        for name in ["notes.txt.gz", "retry.txt.gz"] {
            let job = lifecycle.start(Path::new(name), &publish).unwrap();
            job.finish(Err("Unsupported archive format"));
            let failed = lifecycle.snapshot().unwrap();
            assert_eq!(failed.status, ImportStatus::Failed);
            assert_eq!(failed.source_name.as_deref(), Some(name));
            assert_eq!(failed.error.as_deref(), Some("Unsupported archive format"));
            assert_eq!((failed.files, failed.bytes, failed.result), (0, 0, None));
            let prior = failed.previous_receipt.unwrap();
            assert_eq!(prior.source_name, "first.zip");
            assert_eq!(prior.receipt, first);
        }

        let last = ArchiveImportReceipt {
            extracted_count: Some(3), sorted_count: 0, staging_path: "staging/third".into(),
            issue: Some("Import cancelled".into()),
        };
        lifecycle.start(Path::new("third.zip"), &publish).unwrap().finish(Ok(last.clone()));
        let completed = lifecycle.snapshot().unwrap();
        assert_eq!(completed.status, ImportStatus::Completed);
        assert_eq!(completed.source_name.as_deref(), Some("third.zip"));
        assert_eq!(completed.result.as_ref(), Some(&last));
        assert_eq!((completed.previous_receipt, completed.error), (None, None));

        lifecycle.start(Path::new("later.gz"), &publish).unwrap().finish(Err("Unsupported archive format"));
        let prior = lifecycle.snapshot().unwrap().previous_receipt.unwrap();
        assert_eq!(prior.source_name, "third.zip");
        assert_eq!(prior.receipt, last);
    }

    #[test]
    fn archive_import_state_ignores_old_jobs_and_retains_failure_after_drop() {
        let lifecycle = ImportLifecycle::new();
        let publish = |_: &ArchiveImportSnapshot| {};
        let old_job = lifecycle.start(Path::new("missing.zip"), &publish).unwrap();
        let old_id = lifecycle.snapshot().unwrap().job_id;
        old_job.finish(Err("Archive fixture not found"));
        let first_failure = lifecycle.snapshot().unwrap();
        assert_eq!(first_failure.status, ImportStatus::Failed);
        assert_eq!(first_failure.error.as_deref(), Some("Archive fixture not found"));
        assert!(first_failure.result.is_none() && first_failure.previous_receipt.is_none());

        let current_job = lifecycle.start(Path::new("current.zip"), &publish).unwrap();
        let started = lifecycle.snapshot().unwrap();
        assert!(started.revision > first_failure.revision);
        assert_ne!(started.job_id, old_id);
        assert!(lifecycle.cancel(old_id).unwrap().is_none());
        old_job.progress(10, 1);
        drop(old_job);
        assert!(lifecycle.start(Path::new("busy.zip"), &publish).is_err());
        assert!(!current_job.cancelled.load(Ordering::Relaxed));
        assert_eq!(lifecycle.snapshot().unwrap(), started);

        assert!(lifecycle.cancel(None).unwrap().unwrap().cancel_requested);
        drop(current_job);
        let abandoned = lifecycle.snapshot().unwrap();
        assert_eq!((abandoned.job_id, abandoned.status, abandoned.phase), (started.job_id, ImportStatus::Failed, None));
        assert_eq!(abandoned.error.as_deref(), Some("Archive import stopped before reporting its result"));
        assert!(lifecycle.start(Path::new("retry.zip"), &publish).is_ok());
    }

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

    fn write_zip64(path: &Path, prefix_len: usize, count: u64, poison_central: bool) {
        write_zip(path, &[("nested/entry.txt", b"data")]);
        let mut zip = fs::read(path).unwrap();
        let end = zip.split_off(zip.len() - 22);
        let central_size = u32::from_le_bytes(zip_field(&end, 12));
        let central_offset = u32::from_le_bytes(zip_field(&end, 16));
        if poison_central {
            zip[central_offset as usize] = 0;
        }
        let zip64_offset = zip.len() as u64;
        let mut output = vec![b'x'; prefix_len];
        output.extend(zip);
        output.extend(b"PK\x06\x06");
        output.extend(44u64.to_le_bytes());
        output.extend(45u16.to_le_bytes());
        output.extend(45u16.to_le_bytes());
        output.extend([0; 8]);
        output.extend(count.to_le_bytes());
        output.extend(count.to_le_bytes());
        output.extend(u64::from(central_size).to_le_bytes());
        output.extend(u64::from(central_offset).to_le_bytes());
        output.extend(b"PK\x06\x07");
        output.extend(0u32.to_le_bytes());
        output.extend(zip64_offset.to_le_bytes());
        output.extend(1u32.to_le_bytes());
        // The classic footer retains the original ordinary one-entry fields.
        output.extend(end);
        fs::write(path, output).unwrap();
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

        builder.into_inner().unwrap().finish().unwrap();
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
    fn zip_preflight_preserves_long_comments_and_prefixed_zip64() {
        let root = test_dir("zip-preflight-compatible");
        let source = root.join("comment.zip");
        let mut writer = zip::ZipWriter::new(File::create(&source).unwrap());
        writer.start_file("entry.txt", FileOptions::default()).unwrap();
        writer.write_all(b"commented").unwrap();
        writer.set_raw_comment(vec![b'c'; usize::from(u16::MAX)]);
        drop(writer.finish().unwrap());
        drop(writer);

        let staging = root.join("comment");
        assert_eq!(extract_archive_to(&source, &staging).unwrap(), 1);
        assert_eq!(fs::read(staging.join("entry.txt")).unwrap(), b"commented");

        let source = root.join("prefixed.zip");
        write_zip64(&source, ZIP_READ_WINDOW + 7, 1, false);
        let staging = root.join("prefixed");
        assert_eq!(extract_archive_to(&source, &staging).unwrap(), 1);
        assert_eq!(fs::read(staging.join("entry.txt")).unwrap(), b"data");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn zip_preflight_rejects_forged_count_before_central_parse() {
        let root = test_dir("zip-preflight-count");
        let source = root.join("forged.zip");
        // zip 0.6.6 preallocates when the claimed count fits below the EOCD offset.
        let count = LIMITS.entries + 1;
        write_zip64(&source, count, count as u64, true);
        let staging = root.join("staging");
        let error = extract_archive_to(&source, &staging).unwrap_err();
        assert!(error.contains("entry count limit"), "{error}");
        assert!(file_names(&staging).is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn staging_observation_failure_stops_before_manifest_and_payload() {
        let root = test_dir("archive-staging-observation");
        let source = root.join("fixture.zip");
        write_zip(&source, &[("entry.txt", b"data")]);
        let mut observed = None;
        let error = extract_archive_in(
            &source,
            &root.join("archive-imports"),
            &AtomicBool::new(false),
            |staging| {
                observed = Some(staging.to_path_buf());
                Err("forced journal error".into())
            },
            |_, _| panic!("extraction started without a staging receipt"),
        ).unwrap_err();
        let staging = observed.expect("staging receipt attempted");
        assert!(error.message.contains("forced journal error"), "{error:?}");
        assert!(error.message.contains(staging.to_string_lossy().as_ref()), "{error:?}");
        let retained = error.retained.unwrap();
        assert_eq!(retained.staging_dir, staging);
        assert_eq!(retained.extracted_files, None);
        assert!(file_names(&staging).is_empty());
        assert!(!staging.with_extension("manifest.json").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn retained_archive_summary_reports_completed_entry_at_count_limit() {
        let _db = crate::db::serialise_test_db();
        crate::db::init_test_db();
        let root = test_dir("archive-limit-summary");
        let source = root.join("fixture.tgz");
        let mut compressed = flate2::write::GzEncoder::new(File::create(&source).unwrap(), flate2::Compression::default());
        let mut entries = tar::Builder::new(&mut compressed);
        let mut header = tar::Header::new_gnu();
        header.set_mode(0o644);
        header.set_size(4);
        entries.append_data(&mut header, "first.txt", &b"kept"[..]).unwrap();
        // Directory headers count against the limit but do not produce files.
        // The final regular entry falls one past the normal 20,000-entry limit.
        for index in 1..LIMITS.entries {
            let mut directory = tar::Header::new_gnu();
            directory.set_entry_type(tar::EntryType::Directory);
            directory.set_mode(0o755);
            directory.set_size(0);
            entries.append_data(&mut directory, format!("folder-{index}/"), io::empty()).unwrap();
        }
        entries.append_data(&mut header, "last.txt", &b"last"[..]).unwrap();
        entries.finish().unwrap();
        drop(entries);
        compressed.finish().unwrap();

        let intent = crate::db::begin_operation_intent("archive_import", source.to_str().unwrap(), None).unwrap();
        let error = extract_archive_in(
            &source, &root.join("archive-imports"), &AtomicBool::new(false),
            |staging| crate::db::observe_pending_archive_staging(intent, staging.to_str().unwrap())
                .map_err(|error| error.to_string()),
            |_, _| {},
        ).unwrap_err();
        let receipt = crate::commands::finish_archive_extraction_failure(intent, error).unwrap();
        let staging = Path::new(&receipt.staging_path);
        let kept = fs::read(staging.join("first.txt")).unwrap();
        let last_exists = staging.join("last.txt").exists();
        let manifest: serde_json::Value = serde_json::from_slice(&fs::read(staging.with_extension("manifest.json")).unwrap()).unwrap();
        crate::db::lock_db().unwrap().execute("DELETE FROM operation_intents WHERE id=?1", [intent]).unwrap();
        fs::remove_dir_all(root).unwrap();

        assert_eq!(kept, b"kept");
        assert!(!last_exists);
        assert_eq!(manifest["status"], "failed");
        assert_eq!(manifest["extractedCount"], 1);
        assert!(receipt.issue.unwrap().contains("entry count limit"));
        assert_eq!(receipt.sorted_count, 0);
        assert!(receipt.results.is_empty());
        assert_eq!(receipt.extracted_count, Some(1));
    }

    #[test]
    fn archive_extraction_counts_distinguish_unstarted_incomplete_and_success() {
        let root = test_dir("archive-extraction-count-controls");
        let base = root.join("archive-imports");
        let cancelled = AtomicBool::new(false);
        let unsupported = extract_archive_in(&root.join("unsupported.txt"), &base, &cancelled,
            |_| panic!("unsupported input allocated staging"), |_, _| {}).unwrap_err();
        assert!(unsupported.retained.is_none());

        let broken_zip = root.join("broken.zip");
        fs::write(&broken_zip, b"ZIP header is missing").unwrap();
        let invalid = extract_archive_in(&broken_zip, &base, &cancelled, |_| Ok(()), |_, _| {})
            .unwrap_err().retained.unwrap();
        assert_eq!(invalid.extracted_files, None);

        let truncated_tar = root.join("truncated.tgz");
        let mut compressed = flate2::write::GzEncoder::new(File::create(&truncated_tar).unwrap(), flate2::Compression::default());
        let mut header = tar::Header::new_gnu();
        header.set_path("first.txt").unwrap();
        header.set_mode(0o644);
        header.set_size(8);
        header.set_cksum();
        compressed.write_all(header.as_bytes()).unwrap();
        compressed.write_all(b"part").unwrap();
        compressed.finish().unwrap();
        let incomplete = extract_archive_in(&truncated_tar, &base, &cancelled, |_| Ok(()), |_, _| {})
            .unwrap_err().retained.unwrap();
        assert_eq!(incomplete.extracted_files, Some(0));
        let retained = file_names(&incomplete.staging_dir);
        assert_eq!(retained.len(), 1);
        assert_eq!(fs::read(incomplete.staging_dir.join(&retained[0])).unwrap(), b"part");

        let valid_zip = root.join("valid.zip");
        write_zip(&valid_zip, &[("first.txt", b"complete")]);
        let success = extract_archive_in(&valid_zip, &base, &cancelled, |_| Ok(()), |_, _| {}).unwrap();
        assert_eq!(success.extracted_files, 1);
        assert_eq!(fs::read(success.staging_dir.join("first.txt")).unwrap(), b"complete");
        drop(success);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn manifest_and_journal_finalization_failures_keep_archive_count_and_path() {
        let _db = crate::db::serialise_test_db();
        crate::db::init_test_db();
        let root = test_dir("archive-extraction-finalization");
        let source = root.join("fixture.zip");
        write_zip(&source, &[("first.txt", b"complete")]);
        let staged = std::cell::OnceCell::new();
        let error = extract_archive_in(
            &source, &root.join("archive-imports"), &AtomicBool::new(false),
            |staging| {
                staged.set(staging.to_path_buf()).unwrap();
                Ok(())
            },
            |_, count| {
                if count == 1 {
                    let manifest = staged.get().unwrap().with_extension("manifest.json");
                    fs::remove_file(&manifest).unwrap();
                    fs::create_dir(manifest).unwrap();
                }
            },
        ).unwrap_err();
        let summary = crate::commands::finish_archive_extraction_failure(-1, error).unwrap();
        assert_eq!(summary.extracted_count, Some(1));
        assert_eq!(Path::new(&summary.staging_path), staged.get().unwrap());
        assert_eq!(fs::read(staged.get().unwrap().join("first.txt")).unwrap(), b"complete");
        let issue = summary.issue.unwrap();
        assert!(issue.contains("Manifest finalization failed"));
        assert!(issue.contains("Import journal finalization failed"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn zip_preflight_bounds_actual_central_entry_lengths() {
        let root = test_dir("zip-preflight-length");
        let source = root.join("fixture.zip");
        write_zip(&source, &[("entry.txt", b"data")]);
        let mut bytes = fs::read(&source).unwrap();
        let end = &bytes[bytes.len() - 22..];
        let size = u32::from_le_bytes(zip_field(end, 12)) as u64;
        let offset = u32::from_le_bytes(zip_field(end, 16)) as usize;

        let limits = Limits { zip_metadata_bytes: size, ..LIMITS };
        let cancelled = AtomicBool::new(false);
        assert_eq!(extract_archive_to_with(&source, &root.join("exact"), limits, &cancelled).unwrap(), 1);
        let smaller = Limits { zip_metadata_bytes: size - 1, ..limits };
        let error = extract_archive_to_with(&source, &root.join("over"), smaller, &cancelled).unwrap_err();
        assert!(error.contains("metadata size limit"), "{error}");

        bytes[offset + 28..offset + 30].copy_from_slice(&u16::MAX.to_le_bytes());
        fs::write(&source, bytes).unwrap();
        let error = extract_archive_to(&source, &root.join("forged")).unwrap_err();
        assert!(error.contains("entry length"), "{error}");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn zip_parser_reads_observe_cancellation_after_preflight() {
        let root = test_dir("zip-parser-cancel");
        let source = root.join("fixture.zip");
        write_zip(&source, &[("entry.txt", b"data")]);
        let cancelled = AtomicBool::new(false);
        let staging_guard = safe_fs::guard_directory(&root).unwrap();
        let budget = Budget {
            limits: LIMITS,
            started: Instant::now(),
            cancelled: &cancelled,
            bytes: 0,
            receipt: ExtractionReceipt::default(),
            staging_guard: &staging_guard,
            journal: None,
            last_progress: Instant::now(),
            progress_interval: Duration::from_millis(250),
            progress: &mut |_, _| {},
        };
        let (file, guard, _) = open_archive(&source, LIMITS).unwrap();
        let mut reader = ZipReader::new(file, &budget).unwrap();
        assert_eq!(preflight_zip(&mut reader, LIMITS).unwrap(), 1);
        cancelled.store(true, Ordering::Relaxed);
        let error = ZipArchive::new(reader).err().expect("cancelled ZIP constructor");
        assert!(error.to_string().contains("cancelled"), "{error}");
        drop(guard);
        drop(staging_guard);
        fs::remove_dir_all(root).unwrap();
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
    fn tgz_checks_footer_at_the_stream_limit() {
        let root = test_dir("tgz-footer");
        let source = root.join("fixture.tar.gz");
        write_tgz(&source, &[("entry.txt", b"data")]);

        // One header, one padded data block, and two zero blocks.
        let limits = Limits { entries: 1, total_bytes: 1024, ..LIMITS };
        let cancelled = AtomicBool::new(false);
        assert_eq!(extract_archive_to_with(&source, &root.join("exact"), limits, &cancelled).unwrap(), 1);

        let too_small = Limits { total_bytes: 1023, ..limits };
        let error = extract_archive_to_with(&source, &root.join("over"), too_small, &cancelled).unwrap_err();
        assert!(error.contains("stream size limit"), "{error}");

        let mut bytes = fs::read(&source).unwrap();
        let crc_start = bytes.len() - 8;
        bytes[crc_start] ^= 1;
        fs::write(&source, bytes).unwrap();
        let error = extract_archive_to_with(&source, &root.join("corrupt"), limits, &cancelled).unwrap_err();
        assert!(error.contains("gzip"), "{error}");
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn archive_rejects_junctions_in_source_and_staging_ancestry() {
        let root = test_dir("archive-junction");
        let outside = root.join("outside");
        let junction = root.join("junction");
        fs::create_dir(&outside).unwrap();
        let created = std::process::Command::new("powershell.exe")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "$null = New-Item -ItemType Junction -Path $env:MOUZI_FIXTURE_JUNCTION -Value $env:MOUZI_FIXTURE_TARGET -ErrorAction Stop",
            ])
            .env("MOUZI_FIXTURE_JUNCTION", &junction)
            .env("MOUZI_FIXTURE_TARGET", &outside)
            .output()
            .unwrap();
        assert!(created.status.success(), "{}", String::from_utf8_lossy(&created.stderr));

        write_zip(&outside.join("fixture.zip"), &[("entry.txt", b"data")]);
        assert!(open_archive(&junction.join("fixture.zip"), LIMITS).unwrap_err().contains("Linked ancestry"));
        assert!(create_staging_dir_in(&junction.join("archive-imports")).unwrap_err().contains("Linked ancestry"));
        assert!(!outside.join("archive-imports").exists());

        fs::remove_dir(junction).unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn opened_archive_disallows_source_replacement() {
        let root = test_dir("archive-source-handle");
        let path = root.join("fixture.zip");
        let renamed = root.join("renamed.zip");
        write_zip(&path, &[("entry.txt", b"data")]);
        let (source, guard, _) = open_archive(&path, LIMITS).unwrap();
        assert!(fs::rename(&path, &renamed).is_err());
        assert!(OpenOptions::new().write(true).open(&path).is_err());
        drop(source);
        fs::rename(&path, renamed).unwrap();
        drop(guard);
        fs::remove_dir_all(root).unwrap();
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
    fn cancellation_preserves_complete_entries_and_labels_partial_bytes() {
        let root = test_dir("cancel-partial-entry");
        let complete = b"first payload";
        let second = vec![b'x'; 192 * 1024];
        let entries = [("deep/first.txt", complete.as_slice()), ("deep/second.bin", second.as_slice())];

        for format in [ArchiveFormat::Zip, ArchiveFormat::Tgz] {
            let extension = match format { ArchiveFormat::Zip => "zip", ArchiveFormat::Tgz => "tgz" };
            let source = root.join(format!("fixture.{extension}"));
            let staging = root.join(extension);
            match format {
                ArchiveFormat::Zip => write_zip(&source, &entries),
                ArchiveFormat::Tgz => write_tgz(&source, &entries),
            }
            let original_archive = fs::read(&source).unwrap();
            let (file, source_guard, _) = open_archive(&source, LIMITS).unwrap();
            let staging_guard = safe_fs::create_guarded_directory(&staging).unwrap();
            save_manifest(&staging, Some(&source), "extracting", &[], None).unwrap();
            let cancelled = AtomicBool::new(false);
            let mut progress = |bytes, files| {
                if files == 1 && bytes > complete.len() as u64 {
                    cancelled.store(true, Ordering::Relaxed);
                }
            };
            let mut budget = Budget {
                limits: LIMITS, started: Instant::now(), cancelled: &cancelled, bytes: 0,
                receipt: ExtractionReceipt::default(), staging_guard: &staging_guard, journal: None, last_progress: Instant::now(),
                progress_interval: Duration::ZERO, progress: &mut progress,
            };
            let canonical_staging = fs::canonicalize(&staging).unwrap();
            let result = match format {
                ArchiveFormat::Zip => extract_zip_to(file, &canonical_staging, &mut budget),
                ArchiveFormat::Tgz => extract_tgz_to(file, &canonical_staging, &mut budget),
            };
            let error = result.unwrap_err();
            assert!(error.contains("cancelled"), "{error}");
            save_manifest_with_receipt(&staging, Some(&source), "failed", &[], Some(&error), Some(&budget.receipt), None).unwrap();

            let receipt: serde_json::Value =
                serde_json::from_slice(&fs::read(staging.with_extension("manifest.json")).unwrap()).unwrap();
            assert_eq!(receipt["status"], "failed");
            assert_eq!(receipt["extractedCount"], 1);
            assert_eq!(receipt["extractedEntries"].as_array().unwrap().len(), 1);
            assert_eq!(receipt["extractedEntries"][0]["name"], "first.txt");
            assert_eq!(receipt["incompleteEntries"].as_array().unwrap().len(), 1);
            let incomplete = &receipt["incompleteEntries"][0];
            let partial_name = incomplete["name"].as_str().unwrap();
            assert!(partial_name.ends_with(".part"));
            assert_eq!(Path::new(incomplete["archivePath"].as_str().unwrap()).file_name().unwrap(), "second.bin");
            assert_eq!(incomplete["expectedBytes"], second.len() as u64);
            let partial_path = staging.join(partial_name);
            let partial_bytes = fs::read(&partial_path).unwrap();
            assert!(!partial_bytes.is_empty() && partial_bytes.len() < second.len());
            assert_eq!(partial_bytes, second[..partial_bytes.len()]);
            assert_eq!(incomplete["bytes"], partial_bytes.len() as u64);
            assert!(receipt["remainingEntries"].as_array().unwrap().iter().any(|row| row["name"] == partial_name && row["incomplete"] == true));
            assert!(crate::rules::should_ignore_file(&partial_path));
            assert_eq!(fs::read(staging.join("first.txt")).unwrap(), complete);
            assert!(!staging.join("second.bin").exists());
            assert_eq!(fs::read(&source).unwrap(), original_archive);
            drop(staging_guard);
            drop(source_guard);
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn published_entry_preserves_already_occupied_names_and_picks_a_suffix() {
        let root = test_dir("archive-occupied-name");
        let source = root.join("fixture.zip");
        write_zip(&source, &[("report.txt", b"new contents")]);
        let staging = root.join("staging");
        fs::create_dir_all(staging.join("report.txt")).unwrap();
        fs::write(staging.join("report (1).txt"), b"previous contents").unwrap();

        assert_eq!(extract_archive_to(&source, &staging).unwrap(), 1);
        assert!(staging.join("report.txt").is_dir());
        assert_eq!(fs::read(staging.join("report (1).txt")).unwrap(), b"previous contents");
        assert_eq!(fs::read(staging.join("report (2).txt")).unwrap(), b"new contents");
        assert!(file_names(&staging).iter().all(|name| !name.ends_with(".part")));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn published_archive_names_can_reach_the_entry_limit() {
        // Resume the normal per-import cursor just before the last allowed entry.
        let last_index = LIMITS.entries - 1;
        let mut published = PublishedArchiveNames::default();
        published.names.insert("index.txt".into());
        for index in 1..last_index {
            published.names.insert(format!("index ({index}).txt"));
        }
        published.ascii_folded_names = published.names.clone();
        published.next_owned_suffix.insert(("index.txt".into(), false), last_index);
        published.next_owned_suffix.insert(("index.txt".into(), true), last_index);

        let root = test_dir("archive-last-owned-suffix");
        let staging = fs::canonicalize(&root).unwrap();
        let staging_guard = safe_fs::guard_directory(&staging).unwrap();
        let (_, stage, mut output) = create_entry_stage(&staging, last_index, "index.txt").unwrap();
        output.write_all(b"last entry").unwrap();
        let mut ready = Vec::new();
        let result = publish_entry(&staging, &staging_guard, &stage, &output, "index.txt", &mut published,
            || Ok(()), |name| { ready.push(name.to_owned()); Ok(()) });
        drop(output);
        let name = format!("index ({last_index}).txt");
        let contents = fs::read(staging.join(&name)).ok();
        drop(staging_guard);
        fs::remove_dir_all(root).unwrap();
        assert_eq!(result.unwrap(), name);
        assert_eq!(ready, [name]);
        assert_eq!(contents.as_deref(), Some(b"last entry".as_slice()));
    }

    #[cfg(windows)]
    fn assert_mixed_case_publication(root: PathBuf, require_case_sensitive: bool) {
        let staging = fs::canonicalize(&root).unwrap();
        let staging_guard = safe_fs::guard_directory(&staging).unwrap();
        let mut published = PublishedArchiveNames {
            foreign_probe_limit: 3,
            ..PublishedArchiveNames::default()
        };
        let mut ready = Vec::new();
        let mut publish = |index, name, bytes: &[u8]| {
            let (_, stage, mut output) = create_entry_stage(&staging, index, name).unwrap();
            output.write_all(bytes).unwrap();
            publish_entry(&staging, &staging_guard, &stage, &output, name, &mut published, || Ok(()), |name| {
                ready.push(name.to_owned());
                Ok(())
            })
        };
        for index in 0..3 {
            publish(index, "index.txt", b"lowercase import").unwrap();
        }
        let aliases = fs::read(staging.join("INDEX.txt")).ok().as_deref() == Some(b"lowercase import");
        let expected = if aliases { "INDEX (3).txt" } else { "INDEX.txt" };
        let mixed_case = publish(3, "INDEX.txt", b"uppercase import");
        let original_files = ["index.txt", "index (1).txt", "index (2).txt"]
            .map(|name| fs::read(staging.join(name)).ok());
        let mixed_case_bytes = fs::read(staging.join(expected)).ok();
        let disk_names = file_names(&staging);

        let foreign = staging.join("report.txt");
        fs::write(&foreign, b"foreign").unwrap();
        let occupied_slot = publish(4, "report.txt", b"import beside foreign");
        let foreign_bytes = fs::read(&foreign).unwrap();
        fs::remove_file(&foreign).unwrap();
        let freed_slot = publish(5, "report.txt", b"import in vacated slot");
        let freed_bytes = fs::read(&foreign).ok();
        drop(staging_guard);
        fs::remove_dir_all(root).unwrap();

        assert!(!require_case_sensitive || !aliases, "fixture must distinguish ASCII case");
        assert_eq!(mixed_case.as_deref(), Ok(expected));
        assert!(disk_names.iter().any(|name| name == expected));
        assert!(original_files.iter().all(|bytes| bytes.as_deref() == Some(b"lowercase import")));
        assert_eq!(mixed_case_bytes.as_deref(), Some(b"uppercase import".as_slice()));
        assert_eq!(occupied_slot.as_deref(), Ok("report (1).txt"));
        assert_eq!(foreign_bytes, b"foreign");
        assert_eq!(freed_slot.as_deref(), Ok("report.txt"));
        assert_eq!(freed_bytes.as_deref(), Some(b"import in vacated slot".as_slice()));
        assert_eq!(ready, ["index.txt", "index (1).txt", "index (2).txt", expected, "report (1).txt", "report.txt"]);
    }

    #[cfg(windows)]
    #[test]
    fn mixed_case_publications_do_not_spend_foreign_probe_budget() {
        assert_mixed_case_publication(test_dir("archive-case-publication"), false);
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "requires a case-sensitive NTFS parent directory"]
    fn mixed_case_publications_keep_distinct_sensitive_names() {
        let parent = std::env::var_os("MOUZI_ARCHIVE_CASE_SENSITIVE_TEST_ROOT")
            .expect("provide a case-sensitive NTFS parent directory");
        let root = PathBuf::from(parent).join(format!(
            "mouzi-archive-case-{}-{}", std::process::id(),
            TEST_DIR_COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        fs::create_dir(&root).unwrap();
        assert_mixed_case_publication(root, true);
    }

    #[test]
    fn cancel_during_filename_search_preserves_existing_and_staged_files() {
        let root = test_dir("archive-cancel-filename-search");
        let staging = fs::canonicalize(&root).unwrap();
        let staging_guard = safe_fs::guard_directory(&staging).unwrap();
        fs::write(staging.join("report.txt"), b"existing").unwrap();
        let (_, stage, mut output) = create_entry_stage(&staging, 0, "report.txt").unwrap();
        output.write_all(b"staged").unwrap();
        let mut checks = 0;
        let mut ready = Vec::new();
        let result = publish_entry(
            &staging, &staging_guard, &stage, &output, "report.txt",
            &mut PublishedArchiveNames::default(),
            || {
                checks += 1;
                if checks == 1 { Ok(()) } else { Err("cancelled".into()) }
            },
            |name| { ready.push(name.to_owned()); Ok(()) },
        );
        drop(output);
        assert_eq!(result, Err("cancelled".to_string()));
        assert!(ready.is_empty());
        assert_eq!(fs::read(staging.join("report.txt")).unwrap(), b"existing");
        assert_eq!(fs::read(stage).unwrap(), b"staged");
        assert!(!staging.join("report (1).txt").exists());
        drop(staging_guard);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn publication_rechecks_foreign_names_and_never_reuses_its_own_names() {
        let root = test_dir("archive-name-ownership");
        let staging = fs::canonicalize(&root).unwrap();
        let staging_guard = safe_fs::guard_directory(&staging).unwrap();
        let external_directory = staging.join("report (1).txt");
        fs::create_dir(&external_directory).unwrap();
        let mut publications = PublishedArchiveNames::default();
        let mut publish = |index, bytes: &[u8], record: &mut dyn FnMut(&str) -> Result<(), String>| {
            let (_, stage, mut output) = create_entry_stage(&staging, index, "report.txt").unwrap();
            output.write_all(bytes).unwrap();
            publish_entry(&staging, &staging_guard, &stage, &output, "report.txt", &mut publications, || Ok(()), record).unwrap()
        };
        assert_eq!(publish(0, b"first", &mut |_| Ok(())), "report.txt");
        let mut proposed = Vec::new();
        assert_eq!(publish(1, b"second", &mut |name| {
            proposed.push(name.to_owned());
            if name == "report (2).txt" {
                fs::write(staging.join(name), b"concurrent file").unwrap();
            }
            Ok(())
        }), "report (3).txt");
        assert_eq!(proposed, ["report (2).txt", "report (3).txt"]);
        fs::remove_dir(&external_directory).unwrap();
        fs::remove_file(staging.join("report.txt")).unwrap();
        assert_eq!(publish(2, b"third", &mut |_| Ok(())), "report (1).txt");
        assert_eq!(fs::read(staging.join("report (1).txt")).unwrap(), b"third");
        assert_eq!(fs::read(staging.join("report (2).txt")).unwrap(), b"concurrent file");
        assert_eq!(fs::read(staging.join("report (3).txt")).unwrap(), b"second");
        drop(staging_guard);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn interrupted_manifest_replays_the_journal_by_identity_without_changing_payloads() {
        let root = test_dir("interrupted-extraction");
        let base = root.join("archive-imports");
        let staging = base.join("import-1-2-0");
        fs::create_dir_all(&staging).unwrap();
        let source = root.join("original.zip");
        fs::write(&source, b"fixture archive").unwrap();
        let manifest_path = staging.with_extension("manifest.json");
        let journal_path = staging.with_extension("entries.jsonl");
        let mut journal = EntryJournal(safe_fs::create_guarded_staging_file(&journal_path).unwrap());
        let journal_name = journal_path.file_name().unwrap().to_str().unwrap();
        save_manifest_with_receipt(&staging, Some(&source), "extracting", &[], None, None, Some(journal_name)).unwrap();
        let canonical_staging = fs::canonicalize(&staging).unwrap();
        let make_entry = |journal: &mut EntryJournal, index, archive_path: &str, bytes: &[u8], size| {
            let (name, path, mut file) = create_entry_stage(&canonical_staging, index, archive_path).unwrap();
            journal.append(&EntryEvent::Staged {
                name: name.clone(), archive_path: archive_path.into(), expected_bytes: size,
                identity: safe_fs::identity_from_handle(&file).unwrap().key(),
            }).unwrap();
            file.write_all(bytes).unwrap();
            file.sync_all().unwrap();
            (name, path, file)
        };
        let (name, path, file) = make_entry(&mut journal, 0, "normal.txt", b"complete", 8);
        let destination = "normal.txt";
        journal.append(&EntryEvent::Ready { name, destination: destination.into() }).unwrap();
        assert!(safe_fs::publish_staged_file(&file, &path, &canonical_staging.join(destination)).unwrap());
        drop(file);

        let (name, path, file) = make_entry(&mut journal, 1, "changed.txt", b"original", 8);
        journal.append(&EntryEvent::Ready { name, destination: "changed.txt".into() }).unwrap();
        drop(file);
        // Create the foreign file before removing the recorded one so a
        // filesystem cannot recycle its identity for the replacement.
        fs::write(staging.join("changed.txt"), b"external").unwrap();
        let (partial_name, _, file) = make_entry(&mut journal, 2, "unfinished.bin", b"partial", 12);
        drop(file);
        fs::remove_file(path).unwrap();
        journal.0.write_all(b"{\"state\":\"rea").unwrap();
        journal.0.sync_data().unwrap();
        drop(journal);
        fs::write(staging.join(".mouzi-archive-entry-3-0.part"), b"unrecorded").unwrap();

        let source_text = source.to_str().unwrap();
        let recovered = reconcile_pending_manifest(&base, source_text, &staging).unwrap().unwrap();
        assert_eq!(recovered.status, "interrupted");
        let observed = fs::read(&manifest_path).unwrap();
        let manifest: serde_json::Value = serde_json::from_slice(&observed).unwrap();
        assert_eq!(manifest["status"], "interrupted");
        assert_eq!(manifest["source"], source_text);
        assert_eq!(manifest["staging"], staging.to_str().unwrap());
        assert_eq!(manifest["extractedCount"], 1);
        assert_eq!(manifest["extractedEntries"][0]["name"], "normal.txt");
        assert_eq!(manifest["incompleteEntries"].as_array().unwrap().len(), 1);
        assert_eq!(manifest["incompleteEntries"][0]["name"], partial_name);
        assert_eq!(manifest["incompleteEntries"][0]["expectedBytes"], 12);
        assert_eq!(manifest["incompleteEntries"][0]["bytes"], 7);
        assert_eq!(manifest["recoveryEvidenceTruncated"], true);
        let unverified = manifest["unverifiedEntries"].as_array().unwrap();
        assert!(unverified.iter().any(|entry| entry["destination"] == "changed.txt"));
        assert!(unverified.iter().any(|entry| entry["name"] == ".mouzi-archive-entry-3-0.part"));
        assert_eq!(fs::read(staging.join("normal.txt")).unwrap(), b"complete");
        assert_eq!(fs::read(staging.join("changed.txt")).unwrap(), b"external");
        assert_eq!(fs::read(staging.join(partial_name)).unwrap(), b"partial");
        assert_eq!(fs::read(&source).unwrap(), b"fixture archive");
        let recovered = reconcile_pending_manifest(&base, source_text, &staging).unwrap().unwrap();
        assert_eq!(recovered.status, "interrupted");
        assert_eq!(fs::read(&manifest_path).unwrap(), observed);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn recovery_leaves_legacy_and_terminal_manifests_intact() {
        let root = test_dir("old-manifest-recovery");
        let staging = root.join("import-1-2-0");
        fs::create_dir(&staging).unwrap();
        let source = root.join("legacy.zip");
        let manifest = staging.with_extension("manifest.json");
        let source_text = source.to_str().unwrap();
        save_manifest(&staging, Some(&source), "extracting", &[], None).unwrap();
        let original = fs::read(&manifest).unwrap();
        assert!(reconcile_pending_manifest(&root, source_text, &staging).unwrap().is_none());
        assert_eq!(fs::read(&manifest).unwrap(), original);
        let journal = staging.with_extension("entries.jsonl");
        for (status, message) in [("completed", None), ("partial", Some("fixture sorting failure")), ("failed", Some("fixture extraction failure"))] {
            save_manifest_with_receipt(&staging, Some(&source), status, &[], message, None, journal.file_name().unwrap().to_str()).unwrap();
            let terminal = fs::read(&manifest).unwrap();
            let recovered = reconcile_pending_manifest(&root, source_text, &staging).unwrap().unwrap();
            assert_eq!(recovered.status, status);
            assert_eq!(recovered.message.as_deref(), message);
            assert_eq!(fs::read(&manifest).unwrap(), terminal);
        }
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

    #[cfg(windows)]
    #[test]
    fn failed_manifest_publication_preserves_previous_receipt() {
        use std::os::windows::fs::OpenOptionsExt;

        let root = test_dir("manifest-replace");
        let staging = root.join("staging");
        fs::create_dir(&staging).unwrap();
        let manifest = staging.with_extension("manifest.json");
        save_manifest(&staging, None, "extracting", &[], None).unwrap();
        let previous = fs::read(&manifest).unwrap();

        // Allow readers and writers, but deny the rename needed for publication.
        let locked = OpenOptions::new().read(true).share_mode(1 | 2).open(&manifest).unwrap();
        let error = save_manifest(&staging, None, "extracted", &[], None).unwrap_err();
        assert!(error.contains("Cannot persist import manifest"), "{error}");
        assert_eq!(fs::read(&manifest).unwrap(), previous);
        assert_eq!(file_names(&root), vec!["staging", "staging.manifest.json"]);
        drop(locked);

        save_manifest(&staging, None, "extracted", &[], None).unwrap();
        assert_ne!(fs::read(&manifest).unwrap(), previous);
        fs::remove_dir_all(root).unwrap();
    }
}
