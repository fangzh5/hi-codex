//! Windows-only persistence for the opt-in account manager.
//! DPAPI binds saved credentials to the current Windows user. Plaintext active
//! auth files are staged with an owner/system-only DACL before atomic replace.
use std::fs::{self, File};
use std::io::{self, Write};
use std::mem::{size_of, zeroed};
use std::os::windows::fs::MetadataExt;
use std::os::windows::io::FromRawHandle;
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicU64, Ordering};
use windows_sys::Win32::Foundation::{LocalFree, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::Cryptography::{
    CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
};
use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, MoveFileExW, ReplaceFileW, CREATE_NEW, FILE_ATTRIBUTE_NORMAL, FILE_GENERIC_WRITE,
    MOVEFILE_WRITE_THROUGH, OPEN_ALWAYS,
};

static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

fn wide_path(path: &Path) -> Vec<u16> {
    path.as_os_str().encode_wide().chain(Some(0)).collect()
}
use std::os::windows::ffi::OsStrExt;

fn private_file(path: &Path, disposition: u32) -> Result<File, String> {
    // OW is the owner-rights SID; do not inherit a permissive parent DACL.
    let sddl: Vec<u16> = "D:P(A;;FA;;;SY)(A;;FA;;;OW)"
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let mut descriptor = null_mut();
    unsafe {
        if ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            null_mut(),
        ) == 0
        {
            return Err("Could not prepare private credential file permissions".into());
        }
        let attributes = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor,
            bInheritHandle: 0,
        };
        let handle = CreateFileW(
            wide_path(path).as_ptr(),
            FILE_GENERIC_WRITE,
            0,
            &attributes,
            disposition,
            FILE_ATTRIBUTE_NORMAL,
            0,
        );
        LocalFree(descriptor);
        if handle == INVALID_HANDLE_VALUE {
            return Err(
                "Could not create private credential file (another operation may be running)"
                    .into(),
            );
        }
        Ok(File::from_raw_handle(handle as *mut _))
    }
}

pub fn lock(path: &Path) -> Result<File, String> {
    private_file(path, OPEN_ALWAYS)
}

pub fn protect(bytes: &[u8]) -> Result<Vec<u8>, String> {
    crypt(bytes, false)
}

pub fn unprotect(bytes: &[u8]) -> Result<Vec<u8>, String> {
    crypt(bytes, true)
}

fn crypt(bytes: &[u8], decrypt: bool) -> Result<Vec<u8>, String> {
    let input = CRYPT_INTEGER_BLOB {
        cbData: u32::try_from(bytes.len()).map_err(|_| "Credential data is too large")?,
        pbData: bytes.as_ptr() as *mut u8,
    };
    let mut output: CRYPT_INTEGER_BLOB = unsafe { zeroed() };
    unsafe {
        let success = if decrypt {
            CryptUnprotectData(
                &input,
                null_mut(),
                null(),
                null(),
                null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        } else {
            CryptProtectData(
                &input,
                null(),
                null(),
                null(),
                null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        };
        if success == 0 {
            return Err(if decrypt {
                "Could not decrypt account storage for this Windows user"
            } else {
                "Could not encrypt account storage"
            }
            .into());
        }
        let result = std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
        // Clear the system allocation before returning it to Windows.
        for index in 0..output.cbData as usize {
            std::ptr::write_volatile(output.pbData.add(index), 0);
        }
        LocalFree(output.pbData as *mut _);
        Ok(result)
    }
}

/// Reserved, same-volume recovery name. A missing primary can be read from here.
pub fn recovery_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(".hicodex-backup");
    PathBuf::from(name)
}

fn regular_file_exists(path: &Path) -> Result<bool, String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() && metadata.file_attributes() & 0x400 == 0 => Ok(true),
        Ok(_) => Err("Credential paths must be regular files, not links or reparse points".into()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err("Could not inspect credential file".into()),
    }
}

fn replace_file(path: &Path, temp: &Path, backup: Option<&Path>) -> io::Result<()> {
    let destination = wide_path(path);
    let source = wide_path(temp);
    let backup = backup.map(wide_path);
    let success = unsafe {
        if let Some(backup) = backup {
            // Preserve the existing ACL and retain the old file even on partial failure.
            ReplaceFileW(
                destination.as_ptr(),
                source.as_ptr(),
                backup.as_ptr(),
                0,
                null(),
                null(),
            )
        } else {
            // No replace flag: a concurrent creation must fail, not be overwritten.
            MoveFileExW(
                source.as_ptr(),
                destination.as_ptr(),
                MOVEFILE_WRITE_THROUGH,
            )
        }
    };
    if success == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    atomic_write_using(path, bytes, true, replace_file)
}

/// Used when recovery observed no active auth: a later creation must win.
pub fn atomic_create(path: &Path, bytes: &[u8]) -> Result<(), String> {
    atomic_write_using(path, bytes, false, replace_file)
}

fn atomic_write_using(
    path: &Path,
    bytes: &[u8],
    allow_replace: bool,
    replace: impl FnOnce(&Path, &Path, Option<&Path>) -> io::Result<()>,
) -> Result<(), String> {
    let parent = path.parent().ok_or("Credential path has no parent")?;
    let sequence = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
    let temp = parent.join(format!(".hicodex-{}-{sequence}.tmp", std::process::id()));
    let backup = recovery_path(path);
    let mut created_temp = false;
    let mut keep_temp = false;
    let result = (|| {
        let mut file = private_file(&temp, CREATE_NEW)?;
        created_temp = true;
        file.write_all(bytes)
            .and_then(|_| file.sync_all())
            .map_err(|_| "Could not persist credential data")?;
        drop(file);

        let exists = regular_file_exists(path)?;
        if exists && !allow_replace {
            return Err(
                "Credentials appeared during recovery; no existing file was overwritten".into(),
            );
        }
        let has_backup = regular_file_exists(&backup)?;
        if exists && has_backup {
            // The caller has loaded the current primary. Retire a previous recovery
            // copy only while that primary still exists; ReplaceFile creates the next.
            fs::remove_file(&backup).map_err(|_| "Could not retire previous recovery file")?;
        }
        if let Err(error) = replace(path, &temp, exists.then_some(backup.as_path())) {
            // ReplaceFile can rename the original to backup and then fail. Restore
            // only into a missing path; never replace a concurrent writer's file.
            if matches!(regular_file_exists(path), Ok(false))
                && matches!(regular_file_exists(&backup), Ok(true))
            {
                let _ = replace_file(path, &backup, None);
            }
            keep_temp = !matches!(regular_file_exists(path), Ok(true));
            return Err(format!(
                "Could not replace credential file ({error}). Check the active account before retrying. Any remaining recovery file ends in .hicodex-backup."
            ));
        }
        if regular_file_exists(&backup)? {
            fs::remove_file(&backup).map_err(|_| {
                "Credentials were saved, but the previous recovery file could not be removed"
            })?;
        }
        Ok(())
    })();
    // Never remove a pre-existing file when CREATE_NEW failed.
    if created_temp && !keep_temp {
        let _ = fs::remove_file(&temp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_folder() -> PathBuf {
        let folder = std::env::temp_dir().join(format!(
            "hicodex-storage-test-{}-{}",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&folder).unwrap();
        folder
    }

    #[test]
    fn dpapi_round_trip_and_corruption() {
        let plain = b"synthetic credential fixture";
        let encrypted = protect(plain).unwrap();
        assert!(!encrypted.windows(plain.len()).any(|window| window == plain));
        assert_eq!(unprotect(&encrypted).unwrap(), plain);
        assert!(unprotect(b"not a DPAPI blob").is_err());
    }

    #[test]
    fn atomic_replace_failure_keeps_original_and_cleans_staging_file() {
        let folder = fixture_folder();
        let path = folder.join("auth.json");
        atomic_write(&path, b"original fixture").unwrap();
        let held = lock(&path).unwrap();
        assert!(atomic_write(&path, b"replacement fixture").is_err());
        drop(held);
        assert_eq!(std::fs::read(&path).unwrap(), b"original fixture");
        assert_eq!(std::fs::read_dir(&folder).unwrap().count(), 1);
        std::fs::remove_file(&path).unwrap();
        std::fs::remove_dir(&folder).unwrap();
    }

    #[test]
    fn successful_replace_retires_previous_recovery_copy() {
        let folder = fixture_folder();
        let path = folder.join("accounts.dpapi");
        atomic_write(&path, b"original fixture").unwrap();
        atomic_write(&recovery_path(&path), b"older fixture").unwrap();
        atomic_write(&path, b"replacement fixture").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"replacement fixture");
        assert_eq!(fs::read_dir(&folder).unwrap().count(), 1);
        fs::remove_dir_all(folder).unwrap();
    }

    #[test]
    fn partial_replace_failure_restores_original_from_backup() {
        let folder = fixture_folder();
        let path = folder.join("accounts.dpapi");
        atomic_write(&path, b"original fixture").unwrap();
        let result = atomic_write_using(&path, b"replacement fixture", true, |path, _, backup| {
            // Model ERROR_UNABLE_TO_MOVE_REPLACEMENT_2 after the original rename.
            replace_file(backup.unwrap(), path, None)?;
            Err(io::Error::from_raw_os_error(1177))
        });
        assert!(result.is_err());
        assert_eq!(fs::read(&path).unwrap(), b"original fixture");
        assert_eq!(fs::read_dir(&folder).unwrap().count(), 1);
        fs::remove_dir_all(folder).unwrap();
    }

    #[test]
    fn failed_rollback_keeps_recovery_and_staging_files() {
        let folder = fixture_folder();
        let path = folder.join("accounts.dpapi");
        atomic_write(&path, b"original fixture").unwrap();
        let mut held = None;
        let result = atomic_write_using(&path, b"replacement fixture", true, |path, _, backup| {
            let backup = backup.unwrap();
            replace_file(backup, path, None)?;
            held = Some(lock(backup).unwrap());
            Err(io::Error::from_raw_os_error(1177))
        });
        assert!(result.is_err());
        assert!(!path.exists());
        drop(held);
        assert_eq!(fs::read(recovery_path(&path)).unwrap(), b"original fixture");
        assert_eq!(fs::read_dir(&folder).unwrap().count(), 2);
        fs::remove_dir_all(folder).unwrap();
    }

    #[test]
    fn missing_destination_does_not_overwrite_concurrent_creation() {
        let folder = fixture_folder();
        let path = folder.join("auth.json");
        let result = atomic_write_using(
            &path,
            b"replacement fixture",
            false,
            |path, temp, backup| {
                assert!(backup.is_none());
                fs::write(path, b"concurrent fixture")?;
                replace_file(path, temp, backup)
            },
        );
        assert!(result.is_err());
        assert_eq!(fs::read(&path).unwrap(), b"concurrent fixture");
        assert!(atomic_create(&path, b"later fixture").is_err());
        assert_eq!(fs::read(&path).unwrap(), b"concurrent fixture");
        assert_eq!(fs::read_dir(&folder).unwrap().count(), 1);
        fs::remove_dir_all(folder).unwrap();
    }
}
