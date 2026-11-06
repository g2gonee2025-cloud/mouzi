use std::fs::{File, Metadata, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};

/// Physical identity, obtained from an open OS handle. Path spelling is never
/// used as a substitute for identity (including case-sensitive NTFS folders).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FileIdentity {
    pub volume: u64,
    pub file_id: [u8; 16],
}

impl FileIdentity {
    pub fn key(&self) -> String {
        let mut key = format!("{:016x}:", self.volume);
        for byte in self.file_id { key.push_str(&format!("{:02x}", byte)); }
        key
    }
}

#[cfg(windows)]
mod windows_fs {
    use super::*;
    use std::ffi::c_void;
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use std::os::windows::ffi::OsStrExt;

    #[repr(C)]
    struct FileIdInfo { volume: u64, id: [u8; 16] }
    #[repr(C)]
    struct ByHandleInfo {
        attributes: u32, created: [u32; 2], accessed: [u32; 2], modified: [u32; 2],
        volume: u32, size_high: u32, size_low: u32, links: u32, index_high: u32, index_low: u32,
    }
    #[repr(C)]
    struct RenameInfo { replace: u8, root: *mut c_void, length: u32, name: [u16; 1] }
    #[repr(C)]
    struct StreamInfo {
        next_entry_offset: u32,
        name_length: u32,
        stream_size: i64,
        allocation_size: i64,
        name: [u16; 500],
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetFileInformationByHandleEx(handle: *mut c_void, class: i32, info: *mut c_void, size: u32) -> i32;
        fn GetFileInformationByHandle(handle: *mut c_void, info: *mut ByHandleInfo) -> i32;
        fn SetFileInformationByHandle(handle: *mut c_void, class: i32, info: *const c_void, size: u32) -> i32;
    }

    pub(super) fn open(path: &Path, directory: bool, moving: bool) -> io::Result<File> {
        let mut options = OpenOptions::new();
        options.read(true).custom_flags(0x02000000 | 0x00200000); // BACKUP_SEMANTICS | OPEN_REPARSE_POINT
        if moving {
            options.access_mode(0x80000000 | 0x00010000).share_mode(1); // READ | DELETE; deny external writes/deletion
        } else {
            options.share_mode(if directory { 1 | 2 } else { 1 }); // ancestors deny deletion; files deny writes/deletion
        }
        options.open(path)
    }

    pub(super) fn identity(file: &File) -> io::Result<FileIdentity> {
        let mut info = FileIdInfo { volume: 0, id: [0; 16] };
        let ok = unsafe { GetFileInformationByHandleEx(file.as_raw_handle(), 18, &mut info as *mut _ as *mut c_void, std::mem::size_of::<FileIdInfo>() as u32) };
        if ok == 0 { return Err(io::Error::last_os_error()); }
        Ok(FileIdentity { volume: info.volume, file_id: info.id })
    }

    pub(super) fn links(file: &File) -> io::Result<u64> {
        let mut info: ByHandleInfo = unsafe { std::mem::zeroed() };
        if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } == 0 { return Err(io::Error::last_os_error()); }
        Ok(info.links as u64)
    }

    pub(super) fn require_unnamed_stream(file: &File) -> Result<(), String> {
        const FILE_STREAM_INFO: i32 = 7;
        const UNNAMED_DATA: &str = "::$DATA";
        let mut info = StreamInfo {
            next_entry_offset: 0,
            name_length: u32::MAX,
            stream_size: 0,
            allocation_size: 0,
            name: [0; 500],
        };
        // Only a complete listing with one unnamed stream is safe to copy.
        // An unsupported query or a listing that does not fit also fails closed.
        let ok = unsafe {
            GetFileInformationByHandleEx(
                file.as_raw_handle(),
                FILE_STREAM_INFO,
                (&mut info as *mut StreamInfo).cast(),
                std::mem::size_of::<StreamInfo>() as u32,
            )
        };
        if ok == 0 {
            return Err(format!(
                "Cannot verify source data streams; source preserved: {}",
                io::Error::last_os_error(),
            ));
        }
        let unnamed = info.name_length == 0
            || (info.name_length as usize == UNNAMED_DATA.len() * 2
                && info.name[..UNNAMED_DATA.len()].iter().copied().eq(UNNAMED_DATA.encode_utf16()));
        if info.next_entry_offset != 0 || !unnamed {
            return Err("Cross-volume copy cannot preserve named or unrecognized data streams; source preserved".into());
        }
        Ok(())
    }

    pub(super) fn rename(file: &File, destination: &Path) -> io::Result<()> {
        // The Win32 wrapper resolves a DOS path. Use the canonical absolute
        // destination while its parent ancestry is pinned by the caller.
        // Include a terminator in the allocation, but not FileNameLength.
        let name: Vec<u16> = destination.as_os_str().encode_wide().collect();
        let offset = std::mem::offset_of!(RenameInfo, name);
        let bytes = offset + (name.len() + 1) * 2;
        let mut buffer = vec![0u64; bytes.div_ceil(8)];
        let info = buffer.as_mut_ptr().cast::<RenameInfo>();
        unsafe {
            (*info).replace = 0; // FileRenameInfo: never replace a destination
            (*info).root = std::ptr::null_mut();
            (*info).length = (name.len() * 2) as u32;
            std::ptr::copy_nonoverlapping(name.as_ptr(), (info as *mut u8).add(offset).cast::<u16>(), name.len());
            if SetFileInformationByHandle(file.as_raw_handle(), 3, info.cast(), bytes as u32) == 0 { return Err(io::Error::last_os_error()); }
        }
        Ok(())
    }

    pub(super) fn remove(file: &File) -> io::Result<()> {
        let delete: i32 = 1;
        if unsafe { SetFileInformationByHandle(file.as_raw_handle(), 4, &delete as *const _ as *const c_void, std::mem::size_of::<i32>() as u32) } == 0 { return Err(io::Error::last_os_error()); }
        Ok(())
    }
}

fn open_guard(path: &Path, directory: bool, moving: bool) -> io::Result<File> {
    #[cfg(windows)] { windows_fs::open(path, directory, moving) }
    #[cfg(not(windows))] { let _ = (directory, moving); File::open(path) }
}

pub fn identity_from_handle(file: &File) -> Result<FileIdentity, String> {
    #[cfg(windows)] { windows_fs::identity(file).map_err(|e| format!("Cannot read file identity: {}", e)) }
    #[cfg(unix)] {
        use std::os::unix::fs::MetadataExt;
        let metadata = file.metadata().map_err(|e| e.to_string())?;
        let mut file_id = [0u8; 16]; file_id[..8].copy_from_slice(&metadata.ino().to_le_bytes());
        Ok(FileIdentity { volume: metadata.dev(), file_id })
    }
    #[cfg(not(any(windows, unix)))] { let _ = file; Err("Physical file identity is unsupported on this platform".into()) }
}

pub fn file_identity(path: &Path) -> Result<FileIdentity, String> {
    let metadata = std::fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if is_link(&metadata) { return Err("Links are not eligible filesystem targets".into()); }
    #[cfg(windows)]
    let file = {
        use std::os::windows::fs::OpenOptionsExt;
        OpenOptions::new().read(true).share_mode(1 | 2 | 4).custom_flags(0x02000000 | 0x00200000).open(path)
    };
    #[cfg(not(windows))]
    let file = File::open(path);
    let file = file.map_err(|e| e.to_string())?;
    if is_link(&file.metadata().map_err(|e| e.to_string())?) { return Err("Target changed to a link".into()); }
    identity_from_handle(&file)
}

pub(crate) fn link_count(file: &File) -> Result<u64, String> {
    #[cfg(windows)] { windows_fs::links(file).map_err(|e| e.to_string()) }
    #[cfg(unix)] { use std::os::unix::fs::MetadataExt; Ok(file.metadata().map_err(|e| e.to_string())?.nlink()) }
    #[cfg(not(any(windows, unix)))] { let _ = file; Err("Link count unavailable".into()) }
}

fn is_link(metadata: &Metadata) -> bool {
    #[cfg(windows)] { use std::os::windows::fs::MetadataExt; metadata.file_attributes() & 0x400 != 0 }
    #[cfg(not(windows))] { metadata.file_type().is_symlink() }
}

fn has_dot_component(path: &Path) -> bool {
    path.as_os_str().as_encoded_bytes().split(|byte| *byte == b'/' || *byte == b'\\')
        .any(|part| part == b"." || part == b"..")
}

fn pin_ancestry(path: &Path) -> Result<Vec<File>, String> {
    pin_ancestry_with(path, None::<fn(&Path) -> io::Result<()>>)
}

fn pin_ancestry_with(
    path: &Path,
    mut create_missing: Option<impl FnMut(&Path) -> io::Result<()>>,
) -> Result<Vec<File>, String> {
    let ancestors: Vec<_> = path.ancestors().filter(|part| !part.as_os_str().is_empty()).collect();
    let mut guards = Vec::with_capacity(ancestors.len());
    for ancestor in ancestors.into_iter().rev() {
        let metadata = match (std::fs::symlink_metadata(ancestor), &mut create_missing) {
            (Err(error), Some(create))
                if error.kind() == io::ErrorKind::NotFound && ancestor.file_name().is_some() =>
            {
                match create(ancestor) {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                    Err(error) => return Err(format!("Cannot create {}: {}", ancestor.display(), error)),
                }
                std::fs::symlink_metadata(ancestor)
            }
            (metadata, _) => metadata,
        }.map_err(|e| format!("Cannot inspect {}: {}", ancestor.display(), e))?;
        if is_link(&metadata) { return Err(format!("Linked ancestry is not allowed: {}", ancestor.display())); }
        if !metadata.is_dir() { return Err(format!("An ancestor is not a directory: {}", ancestor.display())); }
        let guard = open_guard(ancestor, true, false).map_err(|e| format!("Cannot protect {}: {}", ancestor.display(), e))?;
        let metadata = guard.metadata().map_err(|e| e.to_string())?;
        if is_link(&metadata) || !metadata.is_dir() { return Err("An ancestor changed type or became a link".into()); }
        guards.push(guard);
    }
    // Consumers match the closest owning root first.
    guards.reverse();
    Ok(guards)
}

/// Reject drive/share roots, dot aliases and every reparse/symlink ancestor.
pub fn validate_root(path: &Path) -> Result<PathBuf, String> {
    if !path.is_absolute() || has_dot_component(path) { return Err("Choose an absolute folder without dot segments".into()); }
    let guards = pin_ancestry(path)?;
    let canonical = std::fs::canonicalize(path).map_err(|e| e.to_string())?;
    if !canonical.components().any(|part| matches!(part, Component::Normal(_))) { return Err("A volume root cannot be watched".into()); }
    if identity_from_handle(&guards[0])? != file_identity(&canonical)? { return Err("Folder identity changed during validation".into()); }
    Ok(canonical)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ScopedRoot { path: PathBuf, identity: FileIdentity, active: bool }

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootScope { roots: Vec<ScopedRoot> }

pub fn eligible_scope(folders: &[crate::db::WatchedFolder]) -> Result<RootScope, String> {
    let mut roots: Vec<ScopedRoot> = Vec::new();
    for folder in folders {
        let path = validate_root(Path::new(&folder.path))?;
        let identity = file_identity(&path)?;
        let active = folder.enabled && folder.mode != crate::db::FOLDER_MODE_PAUSED;
        if let Some(existing) = roots.iter_mut().find(|root| root.identity == identity) {
            // Conflicting legacy aliases fail closed rather than enabling an inactive root.
            existing.active &= active;
        } else { roots.push(ScopedRoot { path, identity, active }); }
    }
    Ok(RootScope { roots })
}

#[derive(Debug)]
pub struct AncestorGuard { _files: Vec<File> }

#[derive(Debug)]
pub struct ValidatedFile {
    pub canonical_path: PathBuf, pub identity: FileIdentity, pub file: File,
    pub owning_root: PathBuf,
    pub link_count: u64, pub metadata: Metadata, ancestors: AncestorGuard,
}

#[derive(Debug)]
pub struct ValidatedDirectory {
    pub canonical_path: PathBuf, pub identity: FileIdentity, pub file: File,
    pub owning_root: PathBuf,
    pub metadata: Metadata, ancestors: AncestorGuard,
}

impl ValidatedFile { pub fn into_parent_guard(self) -> AncestorGuard { self.ancestors } }
impl ValidatedDirectory { pub fn into_parent_guard(self) -> AncestorGuard { self.ancestors } }

impl RootScope {
    pub fn active_roots(&self) -> Vec<PathBuf> { self.roots.iter().filter(|r| r.active).map(|r| r.path.clone()).collect() }
    pub fn root_identities(&self) -> Vec<FileIdentity> { self.roots.iter().map(|r| r.identity.clone()).collect() }

    fn validate(&self, path: &Path, directory: bool) -> Result<(PathBuf, FileIdentity, File, Metadata, AncestorGuard, PathBuf), String> {
        if !path.is_absolute() || has_dot_component(path) { return Err("Target must be an absolute path without dot segments".into()); }
        for root in &self.roots {
            if validate_root(&root.path)? != root.path || file_identity(&root.path)? != root.identity { return Err("A configured root changed identity".into()); }
        }
        let parent = path.parent().ok_or("Target has no parent directory")?;
        let ancestors = pin_ancestry(parent)?;
        let metadata = std::fs::symlink_metadata(path).map_err(|e| e.to_string())?;
        if is_link(&metadata) || if directory { !metadata.is_dir() } else { !metadata.is_file() } { return Err("Target has the wrong type or is a link".into()); }
        let file = open_guard(path, directory, false).map_err(|e| e.to_string())?;
        let metadata = file.metadata().map_err(|e| e.to_string())?;
        if is_link(&metadata) || if directory { !metadata.is_dir() } else { !metadata.is_file() } { return Err("Target changed type".into()); }
        let identity = identity_from_handle(&file)?;
        if self.roots.iter().any(|root| root.identity == identity) { return Err("A watched root cannot be mutated".into()); }
        let owner = ancestors.iter().find_map(|guard| {
            let identity = identity_from_handle(guard).ok()?;
            self.roots.iter().find(|root| root.identity == identity)
        }).ok_or("Target is not strictly below a registered root")?;
        if !owner.active { return Err("The owning root is disabled or paused".into()); }
        let canonical = std::fs::canonicalize(path).map_err(|e| e.to_string())?;
        if file_identity(&canonical)? != identity { return Err("Target changed identity during validation".into()); }
        Ok((canonical, identity, file, metadata, AncestorGuard { _files: ancestors }, owner.path.clone()))
    }

    pub fn validate_file(&self, path: &Path) -> Result<ValidatedFile, String> {
        let (canonical_path, identity, file, metadata, ancestors, owning_root) = self.validate(path, false)?;
        let link_count = link_count(&file)?;
        Ok(ValidatedFile { canonical_path, identity, file, link_count, metadata, ancestors, owning_root })
    }

    pub fn validate_dir(&self, path: &Path) -> Result<ValidatedDirectory, String> {
        let (canonical_path, identity, file, metadata, ancestors, owning_root) = self.validate(path, true)?;
        Ok(ValidatedDirectory { canonical_path, identity, file, metadata, ancestors, owning_root })
    }
}

/// Physical directory ancestry for live policy checks. Unlike lexical matching,
/// this honors short-name aliases and genuinely distinct case-sensitive paths.
pub fn directory_contains(directory: &Path, ancestor: &Path) -> Result<bool, String> {
    let identity = file_identity(ancestor)?;
    Ok(pin_ancestry(directory)?.iter().any(|file| identity_from_handle(file).ok().as_ref() == Some(&identity)))
}

pub(crate) fn guard_directory(path: &Path) -> Result<AncestorGuard, String> {
    Ok(AncestorGuard { _files: pin_ancestry(path)? })
}

pub(crate) fn create_guarded_directory(path: &Path) -> Result<AncestorGuard, String> {
    if !path.is_absolute() || has_dot_component(path) {
        return Err("Directory must be absolute without dot segments".into());
    }
    let files = pin_ancestry_with(path, Some(|part: &Path| std::fs::create_dir(part)))?;
    Ok(AncestorGuard { _files: files })
}

pub(crate) fn open_guarded_regular_file(path: &Path) -> Result<(File, AncestorGuard), String> {
    if !path.is_absolute() || has_dot_component(path) {
        return Err("File must be absolute without dot segments".into());
    }
    #[cfg(windows)]
    if path.file_name().is_some_and(|name| name.as_encoded_bytes().contains(&b':')) {
        return Err("Alternate data streams are not eligible files".into());
    }
    let ancestors = guard_directory(path.parent().ok_or("File has no parent")?)?;
    let metadata = std::fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if !metadata.is_file() || is_link(&metadata) {
        return Err("Expected a regular file without links".into());
    }
    let file = open_guard(path, false, false).map_err(|e| e.to_string())?;
    let metadata = file.metadata().map_err(|e| e.to_string())?;
    if !metadata.is_file() || is_link(&metadata) {
        return Err("File changed type or became a link".into());
    }
    Ok((file, ancestors))
}

/// Resolve a directory that may not yet exist without accepting linked or
/// traversing ancestry. This is also used to isolate development data.
pub(crate) fn resolve_future_directory(path: &Path) -> Result<PathBuf, String> {
    if !path.is_absolute() || has_dot_component(path) { return Err("Directory must be absolute without dot segments".into()); }
    let existing = path.ancestors().find(|part| part.exists()).ok_or("Directory has no existing ancestor")?;
    let _guard = guard_directory(existing)?;
    let resolved = std::fs::canonicalize(existing).map_err(|e| e.to_string())?;
    Ok(resolved.join(path.strip_prefix(existing).map_err(|e| e.to_string())?))
}

/// Convenience scope validation for callers that already own authorized roots.
pub fn validate_scoped_path(path: &Path, roots: &[PathBuf], expect_dir: bool) -> Result<PathBuf, String> {
    let folders: Vec<_> = roots.iter().map(|root| crate::db::WatchedFolder { id: None, path: root.to_string_lossy().into_owned(), enabled: true, mode: crate::db::FOLDER_MODE_MANUAL.into() }).collect();
    let scope = eligible_scope(&folders)?;
    if expect_dir { Ok(scope.validate_dir(path)?.canonical_path) } else { Ok(scope.validate_file(path)?.canonical_path) }
}

/// The outcome of a safe file move operation.
#[derive(Debug, Clone, PartialEq)]
pub enum MoveOutcome {
    /// File was moved to the exact destination requested.
    Moved,
    /// File was moved but renamed to avoid collision (new name provided).
    MovedWithNewName(String),
}

/// Collapse `.` and `..` without touching the filesystem. `canonicalize` is
/// deliberately not used: it fails on paths that do not exist and would resolve
/// a link to its target, which can sit outside the root the caller checked.
///
/// `pub(crate)` because `suppress.rs` needs the identical normalisation to key
/// its guard map. A second, slightly different copy of this function would mean
/// a path the app armed under one spelling and the watcher reported under
/// another would miss the guard — which is indistinguishable from having no
/// guard at all.
pub(crate) fn normalize_lexically(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            // pop() does nothing once the prefix and root are consumed, so a
            // run of `..` cannot climb above the drive.
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

/// Whether `path` lies inside one of `roots`.
///
/// Compares components rather than testing a string prefix: a prefix test would
/// let `C:\Users\Me` match `C:\Users\Melissa`, and a `..` segment would let a
/// path leave its root while still beginning with it. Comparison is
/// case-insensitive because Windows paths are.
pub fn is_within_any_root(path: &Path, roots: &[PathBuf]) -> bool {
    let target = normalize_lexically(path);
    roots.iter().any(|root| {
        let root = normalize_lexically(root);
        // An empty root would vacuously contain everything.
        if root.as_os_str().is_empty() {
            return false;
        }
        let mut root_components = root.components();
        let mut target_components = target.components();
        loop {
            match (root_components.next(), target_components.next()) {
                (None, _) => return true,
                (Some(_), None) => return false,
                (Some(root_component), Some(target_component)) => {
                    if !root_component
                        .as_os_str()
                        .eq_ignore_ascii_case(target_component.as_os_str())
                    {
                        return false;
                    }
                }
            }
        }
    })
}

/// Resolve the destination path, appending a timestamp suffix if `dest` exists.
/// Pick a name inside `dir` that is not already taken.
///
/// A second-resolution timestamp is not enough of a suffix: two files whose
/// stems and extensions collide in the same folder within one second produce
/// the same candidate, and the rename then replaces the first file outright
/// rather than failing. Counting upwards until the name is free makes the
/// result unique by construction rather than by luck.
pub fn try_unique_destination(dir: &Path, file_name: &str) -> Result<PathBuf, String> {
    if Path::new(file_name).components().count() != 1 || file_name.is_empty() || file_name == "." || file_name == ".." {
        return Err("Destination filename must contain one ordinary component".into());
    }
    let candidate = dir.join(file_name);
    if !path_occupied(&candidate)? {
        return Ok(candidate);
    }
    let source = Path::new(file_name);
    let stem = source
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    let extension = source
        .extension()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    // An extensionless file must not gain a trailing dot, which Windows rejects.
    for attempt in 0..10_000u32 {
        let name = if extension.is_empty() {
            format!("{}_{}", stem, attempt)
        } else {
            format!("{}_{}.{}", stem, attempt, extension)
        };
        let candidate = dir.join(name);
        if !path_occupied(&candidate)? {
            return Ok(candidate);
        }
    }
    Err(format!("No collision-free destination is available for {}", file_name))
}

fn path_occupied(path: &Path) -> Result<bool, String> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true), Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(format!("Cannot inspect destination {}: {}", path.display(), e)),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MoveResult {
    pub source: PathBuf,
    pub destination: PathBuf,
    pub renamed: bool,
    pub identity: FileIdentity,
}

fn collision_error(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::AlreadyExists || {
        #[cfg(windows)] { matches!(error.raw_os_error(), Some(80 | 183)) }
        #[cfg(not(windows))] { false }
    }
}

fn cross_device_error(error: &io::Error) -> bool {
    #[cfg(windows)] { error.raw_os_error() == Some(17) } // ERROR_NOT_SAME_DEVICE; 18 is NO_MORE_FILES
    #[cfg(not(windows))] { error.kind() == io::ErrorKind::CrossesDevices || error.raw_os_error() == Some(18) }
}

fn rename_no_replace(source: &File, src: &Path, parent: &File, destination: &Path) -> io::Result<()> {
    #[cfg(windows)] { let _ = (src, parent); windows_fs::rename(source, destination) }
    #[cfg(not(windows))] {
        // Creating a hard link reserves the destination atomically without replacing it.
        // Unix handles cannot prevent external pathname replacements: check again before unlinking.
        let _ = parent;
        std::fs::hard_link(src, destination)?;
        if file_identity(src).ok() != identity_from_handle(source).ok() {
            let _ = std::fs::remove_file(destination);
            return Err(io::Error::other("Source identity changed before unlink"));
        }
        std::fs::remove_file(src)
    }
}

fn remove_open_source(source: &File, src: &Path) -> Result<(), String> {
    #[cfg(windows)] { let _ = src; windows_fs::remove(source).map_err(|e| e.to_string()) }
    #[cfg(not(windows))] {
        if file_identity(src)? != identity_from_handle(source)? { return Err("Source identity changed before removal".into()); }
        std::fs::remove_file(src).map_err(|e| e.to_string())
    }
}

fn stable_metadata(before: &Metadata, after: &Metadata) -> bool {
    before.len() == after.len() && before.modified().ok() == after.modified().ok() && before.is_file() == after.is_file()
}

fn hash_handle(file: &mut File) -> Result<blake3::Hash, String> {
    file.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    let mut hash = blake3::Hasher::new();
    let mut buffer = [0u8; 64 * 1024];
    loop { let count = file.read(&mut buffer).map_err(|e| e.to_string())?; if count == 0 { break; } hash.update(&buffer[..count]); }
    Ok(hash.finalize())
}

fn copy_verified(source: &mut File, src: &Path, destination: &Path) -> Result<FileIdentity, String> {
    #[cfg(windows)]
    windows_fs::require_unnamed_stream(source)?;
    let before = source.metadata().map_err(|e| e.to_string())?;
    let identity = identity_from_handle(source)?;
    let mut output_options = OpenOptions::new();
    output_options.read(true).write(true).create_new(true);
    #[cfg(windows)] {
        use std::os::windows::fs::OpenOptionsExt;
        output_options.access_mode(0x80000000 | 0x40000000 | 0x00010000).share_mode(1); // deny external write/delete
    }
    let mut output = output_options.open(destination).map_err(|e| if collision_error(&e) { format!("Destination collision: {}", e) } else { e.to_string() })?;
    let output_identity = identity_from_handle(&output)?;
    let transfer: Result<(), String> = (|| {
        source.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
        io::copy(source, &mut output).map_err(|e| e.to_string())?;
        output.flush().map_err(|e| e.to_string())?;
        output.sync_all().map_err(|e| e.to_string())?;
        let copied_hash = hash_handle(&mut output)?;
        let source_hash = hash_handle(source)?;
        let after = source.metadata().map_err(|e| e.to_string())?;
        if !stable_metadata(&before, &after) || identity_from_handle(source)? != identity || file_identity(src)? != identity || output.metadata().map_err(|e| e.to_string())?.len() != before.len() || copied_hash != source_hash {
            return Err("Source changed or transfer verification failed; source preserved".into());
        }
        if file_identity(destination)? != output_identity { return Err("Destination changed during transfer; source preserved".into()); }
        // Windows sharing is per stream. Check again after the transfer before
        // marking the source for deletion.
        #[cfg(windows)]
        windows_fs::require_unnamed_stream(source)?;
        remove_open_source(source, src).map_err(|e| format!("Verified copy at {}, source remains: {}", destination.display(), e))?;
        Ok(())
    })();
    if let Err(ref error) = transfer {
        // Delete only our incomplete copy. A verified copy whose source removal failed
        // is retained for recovery and the error names both locations.
        if !error.starts_with("Verified copy at ") && file_identity(destination).ok().as_ref() == Some(&output_identity) {
            #[cfg(windows)]
            let cleanup = windows_fs::remove(&output);
            #[cfg(not(windows))]
            let cleanup = std::fs::remove_file(destination);
            if let Err(cleanup_error) = cleanup {
                return Err(format!("{}; partial destination retained at {}: {}", error, destination.display(), cleanup_error));
            }
        }
    }
    drop(output);
    transfer.map(|()| output_identity)
}

fn prepare_destination_parent(dest: &Path) -> Result<(PathBuf, Vec<File>), String> {
    if !dest.is_absolute() || has_dot_component(dest) { return Err("Move destination must be absolute and have no dot segments".into()); }
    let parent = dest.parent().ok_or("Destination has no parent")?;
    // Inspect and hold each component before looking up or creating its child.
    let guards = pin_ancestry_with(parent, Some(|path: &Path| std::fs::create_dir(path)))?;
    Ok((std::fs::canonicalize(parent).map_err(|e| e.to_string())?, guards))
}

/// No-replace movement, returning the complete actual destination. On Windows
/// source rename/deletion is bound to its open handle and ancestors are pinned.
/// Path-based Shell deletion still requires releasing its victim handle; Unix
/// external pathname races cannot be fully excluded by these portable guards.
pub fn move_file_checked(src: &Path, dest: &Path) -> Result<MoveResult, String> {
    move_file_checked_inner(src, dest, None, false)
}

pub fn move_file_checked_expected(src: &Path, dest: &Path, expected: &FileIdentity) -> Result<MoveResult, String> {
    move_file_checked_inner(src, dest, Some(expected), false)
}

/// Validate reviewed contents using the same write-denying source handle that
/// performs the rename, so closing a preview handle cannot admit an edit.
pub fn move_file_checked_expected_content(src: &Path, dest: &Path, expected: &FileIdentity, digest: &str) -> Result<MoveResult, String> {
    move_file_checked_content_inner(src, dest, Some(expected), Some(digest), false, true)
}

pub fn remove_empty_directory_expected(path: &Path, expected: &FileIdentity) -> Result<(), String> {
    let _parents = pin_ancestry(path.parent().ok_or("Directory has no parent")?)?;
    let directory = open_guard(path, true, true).map_err(|e|e.to_string())?;
    if !directory.metadata().map_err(|e|e.to_string())?.is_dir() || identity_from_handle(&directory)? != *expected { return Err("Reviewed directory changed identity or type".into()); }
    if std::fs::read_dir(path).map_err(|e|e.to_string())?.next().is_some() { return Err("Directory is no longer empty".into()); }
    #[cfg(windows)] windows_fs::remove(&directory).map_err(|e|e.to_string())?;
    #[cfg(not(windows))] {
        if file_identity(path)? != *expected { return Err("Reviewed directory changed identity".into()); }
        std::fs::remove_dir(path).map_err(|e|e.to_string())?;
    }
    drop(directory);
    Ok(())
}

pub fn move_reviewed_file(src: &Path, dest: &Path, expected: &FileIdentity, digest: &str) -> Result<MoveResult, String> {
    move_file_checked_content_inner(src, dest, Some(expected), Some(digest), false, false)
}

fn move_file_checked_inner(src: &Path, dest: &Path, expected: Option<&FileIdentity>, force_copy: bool) -> Result<MoveResult, String> {
    move_file_checked_content_inner(src, dest, expected, None, force_copy, true)
}

fn move_file_checked_content_inner(src: &Path, dest: &Path, expected: Option<&FileIdentity>, expected_digest: Option<&str>, force_copy: bool, allow_rename: bool) -> Result<MoveResult, String> {
    if !src.is_absolute() || has_dot_component(src) { return Err("Move source must be absolute and have no dot segments".into()); }
    let _source_ancestors = pin_ancestry(src.parent().ok_or("Source has no parent")?)?;
    let metadata = std::fs::symlink_metadata(src).map_err(|e| e.to_string())?;
    if !metadata.is_file() || is_link(&metadata) { return Err("Move source must be a regular file without links".into()); }
    let mut source = open_guard(src, false, true).map_err(|e| e.to_string())?;
    if !source.metadata().map_err(|e| e.to_string())?.is_file() { return Err("Move source changed type".into()); }
    let identity = identity_from_handle(&source)?;
    if expected.is_some_and(|expected| *expected != identity) { return Err("Move source changed identity".into()); }
    // Capture the inventory spelling while the reviewed entry and its
    // ancestors are still guarded. The caller's spelling stays in history.
    let canonical_source = std::fs::canonicalize(src).map_err(|error| error.to_string())?;
    if file_identity(&canonical_source)? != identity {
        return Err("Move source changed identity during canonicalization".into());
    }
    if let Some(expected_digest) = expected_digest {
        if hash_handle(&mut source)?.to_hex().as_str() != expected_digest {
            return Err("Reviewed source contents changed; preview again".into());
        }
    }
    let (parent, guards) = prepare_destination_parent(dest)?;
    let requested_name = dest.file_name().and_then(|name| name.to_str()).ok_or("Destination requires a valid Unicode filename")?;
    let requested = parent.join(requested_name);
    if path_occupied(&requested)? && file_identity(&requested).ok().as_ref() == Some(&identity) { return Err("Source and destination identify the same physical file".into()); }
    for _ in 0..10_001 {
        let actual = if allow_rename { try_unique_destination(&parent, requested_name)? } else {
            if path_occupied(&requested)? { return Err("Reviewed destination is now occupied; preview again".into()); }
            requested.clone()
        };
        let actual_identity = if force_copy {
            match copy_verified(&mut source, src, &actual) { Err(error) if allow_rename && error.starts_with("Destination collision:") => continue, result => result? }
        } else {
            match rename_no_replace(&source, src, &guards[0], &actual) {
                Ok(()) => identity.clone(),
                Err(error) if allow_rename && collision_error(&error) => continue,
                Err(error) if cross_device_error(&error) => {
                    match copy_verified(&mut source, src, &actual) { Err(error) if allow_rename && error.starts_with("Destination collision:") => continue, result => result? }
                },
                Err(error) => return Err(format!("Cannot move {} to {}: {}", src.display(), actual.display(), error)),
            }
        };
        return Ok(MoveResult { source: canonical_source, renamed: actual != requested, destination: actual, identity: actual_identity });
    }
    Err("Destination collision allocation exhausted".into())
}

fn legacy_outcome(result: MoveResult) -> MoveOutcome {
    if result.renamed { MoveOutcome::MovedWithNewName(result.destination.file_name().unwrap_or_default().to_string_lossy().into_owned()) } else { MoveOutcome::Moved }
}

/// Legacy adapter for callers/tests that do not need the destination path.
pub fn move_file(src: &Path, dest: &Path) -> Result<MoveOutcome, String> {
    move_file_checked(src, dest).map(legacy_outcome)
}

#[cfg(test)]
fn copy_delete_fallback(src: &Path, dest: &Path) -> Result<MoveOutcome, String> {
    move_file_checked_inner(src, dest, None, true).map(legacy_outcome)
}

/// Delete a file by moving it to the platform's trash / Recycle Bin.
///
/// Uses the `trash` crate. On Windows this is the Recycle Bin, but only while
/// the bin can accept the item: when the bin is full, the item is too large, or
/// the folder is on a network drive, Windows deletes it permanently instead.
/// `trash::delete` returns `Ok(())` in that case, so the caller cannot tell
/// which of the two happened and must not promise a restore.
/// Returns an error if trashing is not supported on the current platform
/// or if the operation fails.
pub fn delete_to_trash(path: &Path) -> Result<(), String> {
    let path_str = path.to_string_lossy();
    trash::delete(path).map_err(|e| format!("Failed to send {} to trash: {}", path_str, e))?;
    match source_absence(path) {
        SourceAbsence::Absent => Ok(()),
        SourceAbsence::Present => Err(format!("Trash operation returned success but source remains: {}", path_str)),
        SourceAbsence::Unknown => Err(format!("Trash operation returned success but source absence could not be verified: {}", path_str)),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SourceAbsence { Absent, Present, Unknown }

pub(crate) fn source_absence(path: &Path) -> SourceAbsence {
    match std::fs::symlink_metadata(path) {
        Ok(_) => SourceAbsence::Present,
        Err(error) if error.kind() == io::ErrorKind::NotFound => SourceAbsence::Absent,
        Err(_) => SourceAbsence::Unknown,
    }
}

/// What actually became of a file whose `trash::delete` call returned `Ok`.
///
/// `delete()` returns `Result<(), Error>` — a unit type, so there is no flag in
/// it to inspect. On Windows the underlying `IFileOperation::PerformOperations`
/// also returns `S_OK` when the Shell silently deleted the item outright
/// because the bin could not take it (full, item too large, network volume, bin
/// disabled), and `trash` never calls `GetAnyOperationsAborted`. The return
/// value therefore cannot distinguish "recoverable" from "gone forever".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TrashVerdict {
    /// Seen in the Recycle Bin, so the user can restore it.
    InRecycleBin,
    /// Reserved for future direct per-item evidence of permanent deletion.
    /// Counts or failure to find a bin entry never establish this verdict.
    PermanentlyDeleted,
    /// The evidence is missing, so the fate is genuinely unknown.
    Unverified,
}

/// A reading of the Recycle Bin at one instant.
///
/// This is DETECTION, NOT PREVENTION. A path only becomes visible here after
/// the file has already left its original location, and a file the bin refused
/// has already been destroyed by the time anyone could look. There is no
/// pre-flight check that would make the permanent deletion stop happening; the
/// only thing this buys is an honest report. Do not "optimise" it into a check
/// that runs before `delete_to_trash` and conclude the outcome is then safe.
#[derive(Debug, Clone)]
struct TrashEntry {
    id: std::ffi::OsString,
    exact_original_path: Option<PathBuf>,
    identity: Option<FileIdentity>,
    deleted_at: i64,
}

#[derive(Debug, Clone)]
pub(crate) struct TrashSnapshot {
    items: Vec<TrashEntry>,
    /// Whether the bin could actually be read at all. When false, `items` means
    /// "unknown", never "empty".
    known: bool,
}

impl TrashSnapshot {
    /// A reading that failed.
    ///
    /// Deliberately distinct from a reading that found nothing. Conflating the
    /// two is the worst bug available here: one transient Shell error would
    /// turn a whole successful cleanup into a page of "deleted permanently".
    fn unavailable() -> Self {
        Self {
            items: Vec::new(),
            known: false,
        }
    }
}

/// Read the Recycle Bin once.
///
/// Take exactly one reading before a batch and one after. Listing walks every
/// trashed item on the machine, so a reading per file makes a 500-item cleanup
/// 500 full listings and hangs the app.
pub(crate) fn trash_snapshot() -> TrashSnapshot {
    match trash::os_limited::list() {
        Ok(items) => TrashSnapshot {
            items: items
                .iter()
                .map(|item| TrashEntry {
                    id: item.id.clone(),
                    // Windows supplies SIGDN_PARENTRELATIVE display names, with no
                    // indication that extensions were omitted. Do not guess a path.
                    exact_original_path: if cfg!(windows) { None } else { Some(item.original_path()) },
                    identity: file_identity(Path::new(&item.id)).ok(),
                    deleted_at: item.time_deleted,
                })
                .collect(),
            known: true,
        },
        Err(e) => {
            log::warn!("Could not read the Recycle Bin: {}", e);
            TrashSnapshot::unavailable()
        }
    }
}

/// Decide, per attempted path, whether it reached the Recycle Bin.
///
/// `attempted` are the paths whose `trash::delete` call returned `Ok`; verdicts
/// come back in the same order. Pure so the policy can be tested without a
/// Recycle Bin.
///
/// Only a new stable bin item ID with exact per-item evidence can prove a match.
/// Concurrent removals/insertions make aggregate bin counts unsuitable evidence.
#[cfg(test)]
pub(crate) fn classify_trash_outcomes(
    before: &TrashSnapshot,
    after: &TrashSnapshot,
    attempted: &[PathBuf],
) -> Vec<TrashVerdict> {
    classify_trash_evidence(before, after, attempted, None)
}

pub(crate) fn classify_trash_outcomes_with_identity(
    before: &TrashSnapshot,
    after: &TrashSnapshot,
    attempted: &[(PathBuf, FileIdentity)],
) -> Vec<TrashVerdict> {
    let paths: Vec<_> = attempted.iter().map(|(path, _)| path.clone()).collect();
    let identities: Vec<_> = attempted.iter().map(|(_, identity)| identity.clone()).collect();
    classify_trash_evidence(before, after, &paths, Some(&identities))
}

fn classify_trash_evidence(before: &TrashSnapshot, after: &TrashSnapshot, attempted: &[PathBuf], identities: Option<&[FileIdentity]>) -> Vec<TrashVerdict> {
    if !before.known || !after.known {
        return vec![TrashVerdict::Unverified; attempted.len()];
    }

    let mut verdicts = vec![TrashVerdict::Unverified; attempted.len()];
    let mut spent = vec![false; after.items.len()];
    for (index, path) in attempted.iter().enumerate() {
        let matches: Vec<_> = after.items.iter().enumerate().filter(|(i, item)| {
            !spent[*i] && !before.items.iter().any(|old| old.id == item.id && old.deleted_at == item.deleted_at) && (
                identities.is_some_and(|ids| item.identity.as_ref() == Some(&ids[index])) ||
                item.exact_original_path.as_ref().is_some_and(|original| normalize_lexically(original) == normalize_lexically(path))
            )
        }).map(|(i, _)| i).collect();
        // More than one new entry with the same evidence is ambiguous.
        if let [found] = matches.as_slice() {
            let found = *found;
            spent[found] = true;
            verdicts[index] = TrashVerdict::InRecycleBin;
        }
    }

    verdicts
}

#[cfg(test)]
impl TrashSnapshot {
    /// Build a reading from a known set of binned paths, through the same key
    /// builder the real snapshot uses.
    fn from_paths(paths: &[&Path]) -> Self {
        Self {
            items: paths.iter().enumerate().map(|(index, path)| TrashEntry {
                id: format!("fixture:{}:{}", index, path.display()).into(),
                exact_original_path: Some(path.to_path_buf()), identity: None, deleted_at: 0,
            }).collect(),
            known: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn root() -> Vec<PathBuf> {
        vec![PathBuf::from("C:/Users/Me/Documents")]
    }

    #[test]
    fn accepts_a_file_directly_inside_a_root() {
        assert!(is_within_any_root(
            Path::new("C:/Users/Me/Documents/a.txt"),
            &root()
        ));
    }

    #[test]
    fn accepts_a_nested_file_inside_a_root() {
        assert!(is_within_any_root(
            Path::new("C:/Users/Me/Documents/deep/nested/a.txt"),
            &root()
        ));
    }

    #[test]
    fn accepts_the_root_itself() {
        assert!(is_within_any_root(
            Path::new("C:/Users/Me/Documents"),
            &root()
        ));
    }

    #[test]
    fn rejects_a_sibling_whose_name_shares_the_root_prefix() {
        assert!(!is_within_any_root(
            Path::new("C:/Users/Me/DocumentsArchive/a.txt"),
            &root()
        ));
    }

    #[test]
    fn rejects_parent_traversal_that_leaves_the_root() {
        assert!(!is_within_any_root(
            Path::new("C:/Users/Me/Documents/../Secrets/a.txt"),
            &root()
        ));
    }

    #[test]
    fn accepts_parent_traversal_that_stays_inside_the_root() {
        assert!(is_within_any_root(
            Path::new("C:/Users/Me/Documents/sub/../a.txt"),
            &root()
        ));
    }

    #[test]
    fn rejects_parent_traversal_that_climbs_above_the_drive() {
        assert!(!is_within_any_root(
            Path::new("C:/Users/Me/Documents/../../../../a.txt"),
            &root()
        ));
    }

    #[test]
    fn compares_case_insensitively() {
        assert!(is_within_any_root(
            Path::new("c:/users/me/DOCUMENTS/a.txt"),
            &root()
        ));
    }

    #[test]
    fn accepts_when_any_one_root_contains_the_path() {
        let roots = vec![
            PathBuf::from("C:/Users/Me/Pictures"),
            PathBuf::from("C:/Users/Me/Documents"),
        ];
        assert!(is_within_any_root(
            Path::new("C:/Users/Me/Documents/a.txt"),
            &roots
        ));
    }

    #[test]
    fn rejects_a_path_outside_every_root() {
        assert!(!is_within_any_root(Path::new("C:/Windows/System32/a.dll"), &root()));
    }

    #[test]
    fn rejects_everything_when_there_are_no_roots() {
        assert!(!is_within_any_root(
            Path::new("C:/Users/Me/Documents/a.txt"),
            &[]
        ));
    }

    #[test]
    fn rejects_everything_when_a_root_is_empty() {
        assert!(!is_within_any_root(
            Path::new("C:/Users/Me/Documents/a.txt"),
            &[PathBuf::new()]
        ));
    }

    #[test]
    fn unique_destination_keeps_a_free_name_unchanged() {
        let dir = test_dir("unique_free");
        let picked = try_unique_destination(&dir, "report.txt").unwrap();
        assert_eq!(picked, dir.join("report.txt"));
    }

    #[test]
    fn unique_destination_suffixes_a_taken_name() {
        let dir = test_dir("unique_taken");
        fs::write(dir.join("report.txt"), b"original").unwrap();
        let picked = try_unique_destination(&dir, "report.txt").unwrap();
        assert_ne!(picked, dir.join("report.txt"));
        assert!(picked.starts_with(&dir));
    }

    /// The regression: a second collision inside the same call sequence used to
    /// produce a name identical to the first, and the rename replaced the file
    /// that had already been moved there.
    #[test]
    fn unique_destination_never_repeats_a_name_it_already_issued() {
        let dir = test_dir("unique_repeat");
        fs::write(dir.join("report.txt"), b"original").unwrap();
        let first = try_unique_destination(&dir, "report.txt").unwrap();
        fs::write(&first, b"first move").unwrap();
        let second = try_unique_destination(&dir, "report.txt").unwrap();
        assert_ne!(first, second, "two collisions produced the same name");
        assert_eq!(fs::read(dir.join("report.txt")).unwrap(), b"original");
        assert_eq!(fs::read(&first).unwrap(), b"first move");
    }

    #[test]
    fn unique_destination_does_not_add_a_trailing_dot_to_an_extensionless_file() {
        let dir = test_dir("unique_noext");
        fs::write(dir.join("README"), b"original").unwrap();
        let picked = try_unique_destination(&dir, "README").unwrap();
        let name = picked.file_name().unwrap().to_string_lossy().to_string();
        assert!(!name.ends_with('.'), "got {:?}", name);
    }

    #[test]
    fn unique_destination_finds_a_gap_in_an_existing_run_of_suffixes() {
        let dir = test_dir("unique_gap");
        fs::write(dir.join("a.txt"), b"x").unwrap();
        fs::write(dir.join("a_0.txt"), b"x").unwrap();
        fs::write(dir.join("a_2.txt"), b"x").unwrap();
        let picked = try_unique_destination(&dir, "a.txt").unwrap();
        assert_eq!(picked, dir.join("a_1.txt"));
    }

    /// Helper: create a temporary test directory unique to this process.
    fn test_dir(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "mouzi-safe-fs-{}-{}",
            name,
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        root
    }

    #[cfg(windows)]
    #[test]
    fn moving_handle_denies_external_replacement_until_mutation_finishes() {
        let root=test_dir("moving_pin");let source=root.join("source.txt");let other=root.join("external.txt");
        std::fs::write(&source,b"original").unwrap();
        let file=open_guard(&source,false,true).unwrap();
        assert!(std::fs::rename(&source,&other).is_err());
        assert!(std::fs::remove_file(&source).is_err());
        assert_eq!(std::fs::read(&source).unwrap(),b"original");
        drop(file);
        std::fs::rename(&source,&other).unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn test_move_file_normal() {
        let root = test_dir("normal");
        let src = root.join("hello.txt");
        let dest = root.join("target").join("hello.txt");

        fs::write(&src, "world").unwrap();

        let outcome = move_file(&src, &dest).unwrap();
        assert_eq!(outcome, MoveOutcome::Moved);
        assert!(!src.exists(), "source should be removed");
        assert!(dest.exists(), "destination should exist");
        assert_eq!(fs::read_to_string(&dest).unwrap(), "world");

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn test_move_file_collision_creates_suffixed_name() {
        let root = test_dir("collision");
        let src = root.join("doc.txt");
        let dest = root.join("dest").join("doc.txt");

        // Create an existing file at the destination
        fs::create_dir_all(dest.parent().unwrap()).unwrap();
        fs::write(&dest, "original").unwrap();
        // Create the source
        fs::write(&src, "new-version").unwrap();

        let outcome = move_file(&src, &dest).unwrap();
        match outcome {
            MoveOutcome::MovedWithNewName(ref name) => {
                assert!(
                    name.starts_with("doc_"),
                    "expected doc_ prefix, got {}",
                    name
                );
                assert!(name.ends_with(".txt"), "expected .txt suffix, got {}", name);
                // The original dest file should still have its content
                assert_eq!(fs::read_to_string(&dest).unwrap(), "original");
                // The renamed file should have the new content
                assert!(
                    dest.with_file_name(name).exists(),
                    "renamed file should exist"
                );
                assert_eq!(
                    fs::read_to_string(dest.with_file_name(name)).unwrap(),
                    "new-version"
                );
            }
            _ => panic!("Expected MovedWithNewName on collision"),
        }

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn test_move_file_creates_missing_parent_dirs() {
        let root = test_dir("missing_parent");
        let src = root.join("file.txt");
        let dest = root.join("a").join("b").join("c").join("file.txt");

        fs::write(&src, "deep").unwrap();

        // Parent dirs a/b/c do not exist yet
        assert!(!dest.parent().unwrap().exists());

        let outcome = move_file(&src, &dest).unwrap();
        assert_eq!(outcome, MoveOutcome::Moved);
        assert!(dest.exists(), "destination should exist in recreated dirs");
        assert_eq!(fs::read_to_string(&dest).unwrap(), "deep");

        fs::remove_dir_all(&root).ok();
    }

    #[cfg(windows)]
    #[test]
    fn missing_destination_component_is_pinned_before_creating_its_child() {
        let root = test_dir("pin_before_descending");
        let parent = root.join("parent");
        let child = parent.join("child");
        let replacement = root.join("renamed");
        let mut checked_pin = false;
        let guards = pin_ancestry_with(&child, Some(|path: &Path| {
            if path == child {
                assert!(fs::rename(&parent, &replacement).is_err());
                checked_pin = true;
            }
            fs::create_dir(path)
        })).unwrap();
        assert!(checked_pin && child.is_dir());
        drop(guards);
        fs::rename(&parent, &replacement).unwrap();
        fs::remove_dir_all(&root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn link_inserted_during_parent_creation_never_reaches_outside() {
        let root = test_dir("link_during_creation");
        let missing = root.join("missing");
        let outside = root.join("outside");
        fs::create_dir(&outside).unwrap();
        let error = pin_ancestry_with(&missing.join("child"), Some(|path: &Path| {
            if path == missing {
                std::os::unix::fs::symlink(&outside, path)?;
                Err(io::ErrorKind::AlreadyExists.into())
            } else {
                fs::create_dir(path)
            }
        })).unwrap_err();
        assert!(error.contains("Linked ancestry"));
        assert!(!outside.join("child").exists());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn test_move_file_copy_delete_fallback() {
        // Exercise the copy-and-delete path without requiring two volumes.
        let root = test_dir("copy_delete");
        let src = root.join("important.bin");
        let dest = root.join("backup").join("important.bin");

        fs::write(&src, "content").unwrap();
        fs::create_dir_all(dest.parent().unwrap()).unwrap();

        let outcome = copy_delete_fallback(&src, &dest).unwrap();
        assert_eq!(outcome, MoveOutcome::Moved);
        assert!(!src.exists(), "source should be deleted after copy");
        assert!(dest.exists(), "dest should exist after copy");
        assert_eq!(fs::read_to_string(&dest).unwrap(), "content");

        fs::remove_dir_all(&root).ok();
    }

    #[cfg(windows)]
    #[test]
    fn copy_delete_fallback_preserves_a_source_with_a_named_stream() {
        let root = test_dir("copy_with_named_stream");
        let source = root.join("source.bin");
        let stream = root.join("source.bin:mouzi-test");
        let destination = root.join("destination.bin");
        fs::write(&source, b"main").unwrap();
        fs::write(&stream, b"named").unwrap();

        let error = copy_delete_fallback(&source, &destination).unwrap_err();
        assert!(error.contains("named or unrecognized data streams"), "{error}");
        assert_eq!(fs::read(&source).unwrap(), b"main");
        assert_eq!(fs::read(&stream).unwrap(), b"named");
        assert!(!destination.exists());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn test_move_file_returns_error_for_missing_source() {
        let root = test_dir("missing_source");
        let src = root.join("does-not-exist.txt");
        let dest = root.join("somewhere").join("does-not-exist.txt");

        let err = move_file(&src, &dest).unwrap_err();
        assert!(!err.is_empty());

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn empty_directory_removal_requires_identity_and_current_emptiness() {
        let root = test_dir("empty_directory_identity");
        let directory = root.join("reviewed");
        fs::create_dir(&directory).unwrap();
        let reviewed = file_identity(&directory).unwrap();

        let child = directory.join("new-arrival.txt");
        fs::write(&child, b"preserve me").unwrap();
        assert!(remove_empty_directory_expected(&directory, &reviewed).is_err());
        assert_eq!(fs::read(&child).unwrap(), b"preserve me");
        fs::remove_file(&child).unwrap();

        let preserved = root.join("original");
        fs::rename(&directory, &preserved).unwrap();
        fs::create_dir(&directory).unwrap();
        assert!(remove_empty_directory_expected(&directory, &reviewed).is_err());
        assert!(directory.exists() && preserved.exists());

        let replacement_identity = file_identity(&directory).unwrap();
        remove_empty_directory_expected(&directory, &replacement_identity).unwrap();
        assert!(!directory.exists());
        assert!(preserved.exists());
        fs::remove_dir_all(&root).unwrap();
    }

    /// A long-form scratch directory under the user profile.
    ///
    /// `std::env::temp_dir()` hands back the 8.3 short form on some Windows
    /// installs (`C:\Users\HAGOPG~1\...`) while the Recycle Bin always reports
    /// the long form, so a test using it would be measuring the 8.3 mismatch
    /// rather than the trash path.
    fn long_form_dir(name: &str) -> PathBuf {
        let base = std::env::var("USERPROFILE")
            .map(PathBuf::from)
            .unwrap_or_else(|_| std::env::temp_dir());
        let dir = base.join(format!("mouzi-safe-fs-{}-{}", name, std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// The one test that touches the real Recycle Bin. Set
    /// `MOUZI_SKIP_TRASH_INTEGRATION_TEST=1` to opt out explicitly; there is no
    /// silent path where a broken trash implementation still passes.
    #[test]
    #[ignore = "opt-in only: changes the native Recycle Bin"]
    fn delete_to_trash_places_the_file_in_the_recycle_bin() {
        if std::env::var("MOUZI_SKIP_TRASH_INTEGRATION_TEST").as_deref() == Ok("1") {
            eprintln!("skipped: MOUZI_SKIP_TRASH_INTEGRATION_TEST=1");
            return;
        }

        let root = long_form_dir("trash_test");
        let file = root.join("to-trash.txt");
        fs::write(&file, "delete me").unwrap();
        assert!(file.exists());

        let before = trash_snapshot();
        let original_identity = file_identity(&file).unwrap();
        // Previously the Err arm printed a note and the test passed anyway, so a
        // totally broken trash path was invisible to CI. A failure here is a
        // failure.
        delete_to_trash(&file).unwrap_or_else(|e| panic!("delete_to_trash failed: {}", e));
        let after = trash_snapshot();

        assert!(!file.exists(), "file is still at its original path");
        assert!(
            before.known && after.known,
            "the Recycle Bin could not be read; set MOUZI_SKIP_TRASH_INTEGRATION_TEST=1 \
             to skip this test on a machine where the Shell will not enumerate it"
        );
        assert!(
            after.items.iter().any(|entry| !before.items.iter().any(|old| old.id == entry.id)),
            "no new stable Recycle Bin item ID was observed; recovery remains unverified"
        );
        // The strongest form of the assertion: the snapshot has to positively
        // find the item and attribute it, not merely notice a larger bin.
        assert_eq!(
            classify_trash_outcomes_with_identity(&before, &after, &[(file.clone(), original_identity)]),
            vec![TrashVerdict::InRecycleBin],
            "the item landed but the snapshot could not match it back to its path"
        );

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn delete_to_trash_reports_an_error_for_a_path_that_does_not_exist() {
        let root = test_dir("trash_missing");
        let missing = root.join("never-existed.txt");

        let err = delete_to_trash(&missing)
            .expect_err("trashing a path that does not exist should not report success");
        assert!(!err.is_empty());

        fs::remove_dir_all(&root).ok();
    }

    // ── Recycle Bin classification ─────────────────────────────

    fn classify(before: &TrashSnapshot, after: &TrashSnapshot, paths: &[&str]) -> Vec<TrashVerdict> {
        let attempted: Vec<PathBuf> = paths.iter().map(PathBuf::from).collect();
        let verdicts = classify_trash_outcomes(before, after, &attempted);
        assert_eq!(verdicts.len(), paths.len());
        verdicts
    }

    #[test]
    fn a_newly_appearing_path_is_reported_as_in_the_bin() {
        let before = TrashSnapshot::from_paths(&[Path::new("C:/Users/Me/old.txt")]);
        let after = TrashSnapshot::from_paths(&[
            Path::new("C:/Users/Me/old.txt"),
            Path::new("C:/Users/Me/new.txt"),
        ]);
        assert_eq!(
            classify(&before, &after, &["C:/Users/Me/new.txt"]),
            vec![TrashVerdict::InRecycleBin]
        );
    }

    #[test]
    fn missing_bin_evidence_remains_unverified() {
        let before = TrashSnapshot::from_paths(&[Path::new("C:/Users/Me/other.txt")]);
        let after = TrashSnapshot::from_paths(&[Path::new("C:/Users/Me/other.txt")]);
        assert_eq!(
            classify(&before, &after, &["C:/Users/Me/victim.txt"]),
            vec![TrashVerdict::Unverified]
        );
    }

    /// The regression this whole module exists for: a failed reading must not be
    /// read as an empty bin, which would condemn every file in the batch.
    #[test]
    fn a_failed_reading_is_unknown_not_empty() {
        let before = TrashSnapshot::from_paths(&[Path::new("C:/Users/Me/other.txt")]);
        let after = TrashSnapshot::unavailable();
        assert_eq!(
            classify(&before, &after, &["C:/Users/Me/other.txt", "C:/Users/Me/victim.txt"]),
            vec![TrashVerdict::Unverified, TrashVerdict::Unverified]
        );
    }

    /// A failed *before* reading claims nothing at all, even for a path plainly
    /// absent from a good after-reading: without the before count there is
    /// nothing to corroborate an absence against, and an uncorroborated absence
    /// is how a recoverable file gets reported as destroyed.
    #[test]
    fn a_failed_before_reading_leaves_everything_unproven() {
        let before = TrashSnapshot::unavailable();
        let after = TrashSnapshot::from_paths(&[Path::new("C:/Users/Me/landed.txt")]);
        assert_eq!(
            classify(
                &before,
                &after,
                &["C:/Users/Me/landed.txt", "C:/Users/Me/victim.txt"]
            ),
            vec![TrashVerdict::Unverified, TrashVerdict::Unverified]
        );
    }

    /// A pre-existing entry cannot establish the outcome of a new attempt.
    #[test]
    fn a_pre_existing_entry_does_not_vouch_for_this_attempt() {
        let before = TrashSnapshot::from_paths(&[Path::new("C:/Users/Me/recycled.txt")]);
        let after = TrashSnapshot::from_paths(&[Path::new("C:/Users/Me/recycled.txt")]);
        assert_eq!(
            classify(&before, &after, &["C:/Users/Me/recycled.txt"]),
            vec![TrashVerdict::Unverified]
        );
    }

    /// The other half of the coin: when the bin's count accounts for the item,
    /// a failed match is a matching artefact and must never be reported as a
    /// deletion. This is the case an unforeseen path mismatch would land in.
    #[test]
    fn a_failed_match_is_not_a_deletion_when_the_bin_grew_by_one() {
        let before = TrashSnapshot::from_paths(&[]);
        // The entry landed but under a key our matcher cannot reproduce.
        let after = TrashSnapshot::from_paths(&[Path::new("Z:/elsewhere/something.txt")]);
        assert_eq!(
            classify(&before, &after, &["C:/Users/Me/victim.txt"]),
            vec![TrashVerdict::Unverified]
        );
    }

    #[test]
    fn a_shortfall_smaller_than_the_unmatched_count_is_not_attributed() {
        let before = TrashSnapshot::from_paths(&[]);
        let after = TrashSnapshot::from_paths(&[Path::new("Z:/elsewhere/something.txt")]);
        // One item landed, so at most one of the two was destroyed — not both,
        // and there is no way to say which.
        assert_eq!(
            classify(
                &before,
                &after,
                &["C:/Users/Me/a.txt", "C:/Users/Me/b.txt"]
            ),
            vec![TrashVerdict::Unverified, TrashVerdict::Unverified]
        );
    }

    #[test]
    fn a_bin_emptied_mid_batch_is_not_read_as_a_shortfall() {
        let before = TrashSnapshot::from_paths(&[
            Path::new("C:/Users/Me/a.txt"),
            Path::new("C:/Users/Me/b.txt"),
        ]);
        let after = TrashSnapshot::from_paths(&[]);
        // Growth is -2, so the shortfall would be 4 for a batch of 2. Claiming
        // deletions on that arithmetic would be nonsense.
        assert_eq!(
            classify(&before, &after, &["C:/Users/Me/a.txt"]),
            vec![TrashVerdict::Unverified]
        );
    }

    #[test]
    fn extensionless_display_names_cannot_vouch_for_distinct_files() {
        let before = TrashSnapshot::from_paths(&[]);
        let after = TrashSnapshot::from_paths(&[
            Path::new("C:/Users/Me/report"),
            Path::new("C:/Users/Me/report"),
        ]);
        assert_eq!(
            classify(
                &before,
                &after,
                &["C:/Users/Me/report.txt", "C:/Users/Me/report.docx"]
            ),
            vec![TrashVerdict::Unverified, TrashVerdict::Unverified]
        );
    }

    /// Dropping extensions previously allowed different files to vouch for each other.
    #[test]
    fn matching_never_guesses_an_omitted_extension() {
        let before = TrashSnapshot::from_paths(&[]);
        let after = TrashSnapshot::from_paths(&[Path::new("C:/Users/Me/report")]);
        assert_eq!(
            classify(&before, &after, &["C:/Users/Me/report.txt"]),
            vec![TrashVerdict::Unverified]
        );
    }

    #[test]
    fn uncertain_path_aliases_without_identity_remain_unverified() {
        let before = TrashSnapshot::from_paths(&[]);
        let after = TrashSnapshot::from_paths(&[Path::new("c:/users/me/report")]);
        assert_eq!(
            classify(&before, &after, &["C:\\Users\\Me\\Report.TXT"]),
            vec![TrashVerdict::Unverified]
        );
    }

    #[test]
    fn matching_normalises_dot_segments_like_the_root_check() {
        let before = TrashSnapshot::from_paths(&[]);
        let after = TrashSnapshot::from_paths(&[Path::new("C:/Users/Me/a.txt")]);
        assert_eq!(
            classify(&before, &after, &["C:/Users/Me/sub/../a.txt"]),
            vec![TrashVerdict::InRecycleBin]
        );
    }

    #[test]
    fn verdicts_come_back_in_the_order_the_paths_were_attempted() {
        let before = TrashSnapshot::from_paths(&[]);
        let after = TrashSnapshot::from_paths(&[Path::new("C:/Users/Me/b.txt")]);
        assert_eq!(
            classify(
                &before,
                &after,
                &["C:/Users/Me/a.txt", "C:/Users/Me/b.txt"]
            ),
            vec![TrashVerdict::Unverified, TrashVerdict::InRecycleBin]
        );
    }
}
