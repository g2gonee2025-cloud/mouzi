use super::is_regular_file;
use crate::safe_fs::{identity_from_handle, link_count};
use std::fs::File;
use std::io::{self, Read, Seek, Write};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::Path;
use std::ptr::{null, null_mut};
use windows_sys::Win32::Foundation::{
    ERROR_INVALID_FUNCTION, ERROR_NOT_SUPPORTED, ERROR_RM_NOT_ACTIVE,
    ERROR_TRANSACTIONS_UNSUPPORTED_REMOTE, GENERIC_READ, GENERIC_WRITE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::{
    CommitTransaction, CreateFileTransactedW, CreateTransaction, ReOpenFile,
    FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    OPEN_EXISTING,
};

struct Transaction(OwnedHandle);

impl Transaction {
    fn begin() -> io::Result<Self> {
        // Default transaction settings, no inherited handles or external coordinator.
        let raw = unsafe { CreateTransaction(null_mut(), null_mut(), 0, 0, 0, 0, null()) };
        if raw == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        // Closing the last handle rolls back a transaction that did not commit.
        Ok(Self(unsafe { OwnedHandle::from_raw_handle(raw) }))
    }

    fn open_existing(&self, path: &Path) -> io::Result<File> {
        let mut name: Vec<u16> = path.as_os_str().encode_wide().collect();
        if name.contains(&0) {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "Invalid file name"));
        }
        name.push(0);
        // OPEN_EXISTING leaves bytes intact until identity, type, links and contents are checked.
        let raw = unsafe {
            CreateFileTransactedW(
                name.as_ptr(), GENERIC_READ | GENERIC_WRITE, FILE_SHARE_READ, null(),
                OPEN_EXISTING, FILE_FLAG_OPEN_REPARSE_POINT, null_mut(), self.0.as_raw_handle(),
                null(), null(),
            )
        };
        if raw == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        Ok(unsafe { File::from_raw_handle(raw) })
    }

    fn commit(self) -> io::Result<()> {
        if unsafe { CommitTransaction(self.0.as_raw_handle()) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

pub(super) fn replace(
    path: &Path, mut previous: File, original: Option<&str>, content: &str,
) -> Result<bool, String> {
    let save_error = |err: io::Error| {
        let unavailable = matches!(
            err.raw_os_error().map(|code| code as u32),
            Some(ERROR_INVALID_FUNCTION | ERROR_NOT_SUPPORTED | ERROR_RM_NOT_ACTIVE | ERROR_TRANSACTIONS_UNSUPPORTED_REMOTE)
        );
        if unavailable {
            format!("Windows file transactions are unavailable for {}. Edit the file in a text editor. ({err})", path.display())
        } else {
            format!("Cannot safely update {}: {err}", path.display())
        }
    };
    let expected_id = identity_from_handle(&previous)?;
    // Pin the original file ID without keeping its write/delete sharing restrictions.
    let raw = unsafe {
        ReOpenFile(previous.as_raw_handle(), 0, FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE, 0)
    };
    if raw == INVALID_HANDLE_VALUE {
        return Err(save_error(io::Error::last_os_error()));
    }
    let _identity_pin = unsafe { OwnedHandle::from_raw_handle(raw) };
    let mut snapshot = Vec::new();
    let expected = match original {
        Some(text) => text.as_bytes(),
        None => {
            previous.read_to_end(&mut snapshot).map_err(save_error)?;
            &snapshot
        }
    };
    drop(previous);
    #[cfg(test)]
    super::tests::run_competing_save(path);

    // The transacted writer excludes competing writes and renames from validation through commit.
    // On filesystems without transactions, leave the existing policy intact.
    let transaction = Transaction::begin().map_err(save_error)?;
    let mut file = match transaction.open_existing(path) {
        Ok(file) => file,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(err) => return Err(save_error(err)),
    };
    if !is_regular_file(&file.metadata().map_err(save_error)?) || link_count(&file)? != 1 {
        return Err(format!("Cannot save {}: expected a regular file with one link", path.display()));
    }
    if identity_from_handle(&file)? != expected_id {
        return Ok(false);
    }
    let mut current = Vec::new();
    file.read_to_end(&mut current).map_err(save_error)?;
    if current != expected {
        return Ok(false);
    }

    file.rewind().map_err(save_error)?;
    file.write_all(content.as_bytes()).map_err(save_error)?;
    file.set_len(content.len() as u64).map_err(save_error)?;
    transaction.commit().map_err(save_error)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn interrupted_update_preserves_the_original_and_excludes_other_writers() {
        let folder = super::super::tests::temporary_folder();
        let path = folder.join(super::super::IGNORE_FILE);
        let editor = folder.join("editor.tmp");
        fs::write(&path, b"keep.tmp\n").unwrap();
        fs::write(&editor, b"editor.tmp\n").unwrap();
        let transaction = Transaction::begin().unwrap();
        let mut file = transaction.open_existing(&path).unwrap();
        file.write_all(b"partial").unwrap();
        let external_write = fs::write(&path, b"competing write");
        let external_replace = fs::rename(&editor, &path);
        drop(file);
        drop(transaction); // Simulate a failed save before commit.
        let recovered = fs::read(&path).unwrap();
        fs::remove_dir_all(folder).unwrap();
        assert!(external_write.is_err());
        assert!(external_replace.is_err());
        assert_eq!(recovered, b"keep.tmp\n");
    }
}
