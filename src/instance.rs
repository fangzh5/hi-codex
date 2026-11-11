//! Instance locks shared by releases, without depending on the EXE path.
use std::ptr::null;
use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HANDLE};
use windows_sys::Win32::System::Threading::CreateMutexW;

// Every released version since v0.1.0 uses this session-local name. Retain it
// so that either an older or newer version can start first in the same session.
const LEGACY_SESSION_MUTEX: &str = r"Local\HiCodex.SingleInstance";
// New builds also coordinate across Windows sessions (including RDP).
const GLOBAL_MUTEX: &str = r"Global\HiCodex.SingleInstance";

#[derive(Debug, PartialEq, Eq)]
pub enum InstanceError {
    AlreadyRunning,
    Unavailable,
}

struct HeldMutex(HANDLE);

impl HeldMutex {
    fn create(name: &str) -> Result<Self, InstanceError> {
        let name: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
        unsafe {
            let handle = CreateMutexW(null(), 0, name.as_ptr());
            if handle == 0 {
                return Err(InstanceError::Unavailable);
            }
            // GetLastError must be checked before making any other Win32 call.
            if GetLastError() == ERROR_ALREADY_EXISTS {
                CloseHandle(handle);
                return Err(InstanceError::AlreadyRunning);
            }
            Ok(Self(handle))
        }
    }
}

impl Drop for HeldMutex {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}

pub struct InstanceGuard {
    _session: HeldMutex,
    _global: HeldMutex,
}

impl InstanceGuard {
    pub fn acquire() -> Result<Self, InstanceError> {
        Self::acquire_named(LEGACY_SESSION_MUTEX, GLOBAL_MUTEX)
    }

    fn acquire_named(session_name: &str, global_name: &str) -> Result<Self, InstanceError> {
        let session = HeldMutex::create(session_name)?;
        // On failure, `session` is dropped so a later launch is not blocked.
        let global = HeldMutex::create(global_name)?;
        Ok(Self {
            _session: session,
            _global: global,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{HeldMutex, InstanceError, InstanceGuard};
    use std::io::{self, BufRead, BufReader, Write};
    use std::process::{Child, ChildStdout, Command, Stdio};
    use std::sync::atomic::{AtomicU32, Ordering};

    static NEXT_ID: AtomicU32 = AtomicU32::new(0);
    const READY: &str = "HICODEX_INSTANCE_TEST_READY";

    fn names() -> (String, String, String) {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let suffix = format!("{}.{}", std::process::id(), id);
        (
            format!(r"Local\HiCodex.Test.SessionA.{suffix}"),
            format!(r"Local\HiCodex.Test.SessionB.{suffix}"),
            format!(r"Global\HiCodex.Test.{suffix}"),
        )
    }

    // Launched as a separate test process to exercise real named kernel locks.
    #[test]
    fn child_holds_instance_lock() {
        let Ok(mode) = std::env::var("HICODEX_TEST_INSTANCE_MODE") else {
            return;
        };
        let session = std::env::var("HICODEX_TEST_SESSION_MUTEX").unwrap();
        let global = std::env::var("HICODEX_TEST_GLOBAL_MUTEX").unwrap();
        let legacy;
        let current;
        if mode == "legacy" {
            legacy = Some(HeldMutex::create(&session).unwrap());
            current = None;
        } else {
            legacy = None;
            current = Some(InstanceGuard::acquire_named(&session, &global).unwrap());
        }
        println!("{READY}");
        io::stdout().flush().unwrap();
        let mut line = String::new();
        io::stdin().read_line(&mut line).unwrap();
        drop((legacy, current));
    }

    fn start_child(mode: &str, session: &str, global: &str) -> (Child, BufReader<ChildStdout>) {
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "instance::tests::child_holds_instance_lock",
                "--nocapture",
            ])
            .env("HICODEX_TEST_INSTANCE_MODE", mode)
            .env("HICODEX_TEST_SESSION_MUTEX", session)
            .env("HICODEX_TEST_GLOBAL_MUTEX", global)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdout = BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        loop {
            line.clear();
            assert_ne!(
                stdout.read_line(&mut line).unwrap(),
                0,
                "child did not acquire lock"
            );
            if line.contains(READY) {
                break;
            }
        }
        (child, stdout)
    }

    fn stop_child(mut child: Child, _stdout: BufReader<ChildStdout>) {
        child.stdin.take().unwrap().write_all(b"\n").unwrap();
        assert!(child.wait().unwrap().success());
    }

    #[test]
    fn legacy_and_new_instances_block_one_another_in_the_same_session() {
        let (session, _, global) = names();
        let (child, stdout) = start_child("legacy", &session, &global);
        assert!(matches!(
            InstanceGuard::acquire_named(&session, &global),
            Err(InstanceError::AlreadyRunning)
        ));
        stop_child(child, stdout);

        let (child, stdout) = start_child("new", &session, &global);
        assert!(matches!(
            HeldMutex::create(&session),
            Err(InstanceError::AlreadyRunning)
        ));
        stop_child(child, stdout);
        assert!(InstanceGuard::acquire_named(&session, &global).is_ok());
    }

    #[test]
    fn global_lock_blocks_new_instances_in_other_sessions_and_releases_both_locks() {
        let (session_a, session_b, global) = names();
        let (child, stdout) = start_child("new", &session_a, &global);
        assert!(matches!(
            InstanceGuard::acquire_named(&session_b, &global),
            Err(InstanceError::AlreadyRunning)
        ));
        // A failed global acquisition must not leave session B locked.
        assert!(HeldMutex::create(&session_b).is_ok());
        stop_child(child, stdout);
        assert!(InstanceGuard::acquire_named(&session_b, &global).is_ok());
    }
}
