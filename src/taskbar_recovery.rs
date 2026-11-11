//! A small write-ahead record for the native spacing owned by HiCodex.
use crate::rebar::Band;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use windows_sys::Win32::Foundation::{CloseHandle, FILETIME};
use windows_sys::Win32::Storage::FileSystem::{
    MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
};
use windows_sys::Win32::System::Threading::{
    GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Record {
    pub band: Band,
    pub original: u32,
    pub applied: u32,
    created: u64,
}

unsafe fn process_created(pid: u32) -> Option<u64> {
    let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
    if handle == 0 {
        return None;
    }
    let mut created: FILETIME = std::mem::zeroed();
    let mut exit = created;
    let mut kernel = created;
    let mut user = created;
    let success = GetProcessTimes(handle, &mut created, &mut exit, &mut kernel, &mut user);
    CloseHandle(handle);
    (success != 0)
        .then_some((u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime))
}

impl Record {
    pub unsafe fn new(band: Band, original: u32, applied: u32) -> Option<Self> {
        Some(Self {
            band,
            original,
            applied,
            created: process_created(band.process)?,
        })
    }

    pub unsafe fn matches(self, current: Band) -> bool {
        self.band.same_owner(current)
            && (current.header == self.applied || current.header == self.band.header)
            && process_created(current.process) == Some(self.created)
    }

    fn encode(self) -> String {
        format!(
            "1 {} {} {} {} {} {} {} {} {}",
            self.band.window,
            self.band.process,
            self.band.child,
            self.band.id,
            self.band.min_child,
            self.original,
            self.applied,
            self.created,
            self.band.header
        )
    }

    fn decode(text: &str) -> Option<Self> {
        let parts: Vec<_> = text.split_whitespace().collect();
        if parts.len() != 10 || parts[0] != "1" {
            return None;
        }
        let original = parts[6].parse().ok()?;
        Some(Self {
            band: Band {
                window: parts[1].parse().ok()?,
                process: parts[2].parse().ok()?,
                child: parts[3].parse().ok()?,
                id: parts[4].parse().ok()?,
                header: parts[9].parse().ok()?,
                min_child: parts[5].parse().ok()?,
            },
            original,
            applied: parts[7].parse().ok()?,
            created: parts[8].parse().ok()?,
        })
    }
}

fn path() -> Option<PathBuf> {
    Some(
        PathBuf::from(std::env::var_os("LOCALAPPDATA")?)
            .join("HiCodex")
            .join("taskbar-recovery.txt"),
    )
}

pub fn load() -> Option<Record> {
    let mut text = String::new();
    File::open(path()?)
        .ok()?
        .take(513)
        .read_to_string(&mut text)
        .ok()?;
    if text.len() > 512 {
        return None;
    }
    Record::decode(&text)
}

fn write_at(path: &Path, record: Record) -> std::io::Result<()> {
    fs::create_dir_all(
        path.parent()
            .ok_or_else(|| std::io::Error::other("missing parent"))?,
    )?;
    let temporary = path.with_extension("tmp");
    let result = (|| {
        let mut file = File::create(&temporary)?;
        file.write_all(record.encode().as_bytes())?;
        file.sync_all()?;
        drop(file);
        use std::os::windows::ffi::OsStrExt;
        let source: Vec<_> = temporary.as_os_str().encode_wide().chain(Some(0)).collect();
        let destination: Vec<_> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        if unsafe {
            MoveFileExW(
                source.as_ptr(),
                destination.as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        } == 0
        {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

pub fn save(record: Record) -> bool {
    path().is_some_and(|path| write_at(&path, record).is_ok())
}

pub fn clear() -> bool {
    let Some(path) = path() else {
        return false;
    };
    match fs::remove_file(path) {
        Ok(()) => true,
        Err(error) => error.kind() == std::io::ErrorKind::NotFound,
    }
}

// A separate process survives a crash of the meter. The event handshake ensures
// it has opened the exact parent process before we ever alter Explorer.
pub fn ensure_guardian() -> bool {
    use std::os::windows::process::CommandExt;
    use std::sync::{Mutex, OnceLock};
    use windows_sys::Win32::Foundation::WAIT_OBJECT_0;
    use windows_sys::Win32::System::Threading::*;
    static CHILD: OnceLock<Mutex<Option<std::process::Child>>> = OnceLock::new();
    let Ok(mut child) = CHILD.get_or_init(|| Mutex::new(None)).lock() else {
        return false;
    };
    if child
        .as_mut()
        .is_some_and(|c| matches!(c.try_wait(), Ok(None)))
    {
        return true;
    }
    let pid = std::process::id();
    let Some(created) = (unsafe { process_created(pid) }) else {
        return false;
    };
    let event_name = format!("Local\\HiCodex.Recovery.{pid}.{created}");
    let wide: Vec<u16> = event_name.encode_utf16().chain(Some(0)).collect();
    let event = unsafe { CreateEventW(std::ptr::null(), 1, 0, wide.as_ptr()) };
    if event == 0 {
        return false;
    }
    let spawned = std::env::current_exe().ok().and_then(|exe| {
        std::process::Command::new(exe)
            .args([
                "--taskbar-guardian",
                &pid.to_string(),
                &created.to_string(),
                &event_name,
            ])
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .ok()
    });
    let ready = spawned.is_some() && unsafe { WaitForSingleObject(event, 5000) } == WAIT_OBJECT_0;
    unsafe {
        CloseHandle(event);
    }
    if ready {
        *child = spawned;
    } else if let Some(mut c) = spawned {
        let _ = c.kill();
        let _ = c.wait();
    }
    ready
}

pub fn run_guardian(pid: u32, created: u64, event_name: &str) {
    use windows_sys::Win32::Foundation::WAIT_OBJECT_0;
    use windows_sys::Win32::System::Threading::*;
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | 0x00100000, 0, pid);
        if process == 0 {
            return;
        }
        if process_created(pid) != Some(created) {
            CloseHandle(process);
            return;
        }
        let wide: Vec<u16> = event_name.encode_utf16().chain(Some(0)).collect();
        let event = OpenEventW(EVENT_MODIFY_STATE, 0, wide.as_ptr());
        if event == 0 {
            CloseHandle(process);
            return;
        }
        let ready = SetEvent(event) != 0;
        CloseHandle(event);
        if !ready || WaitForSingleObject(process, INFINITE) != WAIT_OBJECT_0 {
            CloseHandle(process);
            return;
        }
        CloseHandle(process);
        // A newly launched meter owns recovery if it acquired the instance lock.
        let Ok(_instance) = crate::instance::InstanceGuard::acquire() else {
            return;
        };
        windows_sys::Win32::UI::HiDpi::SetProcessDpiAwarenessContext(-4isize);
        for _ in 0..20 {
            if crate::taskbar::restore_left() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(250));
        }
        // Preserve the record for startup recovery if Explorer is unavailable.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::System::Threading::GetCurrentProcessId;
    #[test]
    fn recovery_roundtrip_and_owner_checks() {
        let band = Band {
            window: 1,
            process: unsafe { GetCurrentProcessId() },
            child: 2,
            id: 3,
            header: 0,
            min_child: 144,
        };
        let record = unsafe { Record::new(band, 0, 939) }.unwrap();
        assert_eq!(Record::decode(&record.encode()), Some(record));
        let current = Band {
            header: 939,
            ..band
        };
        assert!(unsafe { record.matches(current) });
        assert!(!unsafe {
            record.matches(Band {
                header: 940,
                ..current
            })
        });
        assert!(!unsafe {
            record.matches(Band {
                child: 4,
                ..current
            })
        });
        assert!(!unsafe {
            Record {
                created: record.created + 1,
                ..record
            }
            .matches(current)
        });
        assert!(Record::decode("1 invalid").is_none());
    }
    #[test]
    fn write_ahead_record_atomically_replaces_old_layout() {
        let path = std::env::temp_dir()
            .join(format!("hicodex-recovery-{}", std::process::id()))
            .join("record.txt");
        let band = Band {
            window: 1,
            process: unsafe { GetCurrentProcessId() },
            child: 2,
            id: 3,
            header: 0,
            min_child: 144,
        };
        let first = unsafe { Record::new(band, 0, 939) }.unwrap();
        write_at(&path, first).unwrap();
        let second = Record {
            applied: 400,
            ..first
        };
        write_at(&path, second).unwrap();
        assert_eq!(
            Record::decode(&fs::read_to_string(&path).unwrap()),
            Some(second)
        );
        assert!(!path.with_extension("tmp").exists());
        fs::remove_file(&path).unwrap();
        fs::remove_dir(path.parent().unwrap()).unwrap();
    }
}
