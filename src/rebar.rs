//! Native task-band layout. WM_USER control messages do not marshal pointers
//! across processes, so their small data-only buffer must live in the owner.
use std::ffi::c_void;
use std::mem::{size_of, zeroed};
use std::ptr::null;
use std::sync::Mutex;
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, HWND, WAIT_TIMEOUT};
use windows_sys::Win32::System::Diagnostics::Debug::{ReadProcessMemory, WriteProcessMemory};
use windows_sys::Win32::System::Memory::{
    VirtualAllocEx, VirtualFreeEx, MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_READWRITE,
};
use windows_sys::Win32::System::Threading::{
    IsWow64Process, OpenProcess, WaitForSingleObject, PROCESS_QUERY_LIMITED_INFORMATION,
    PROCESS_SYNCHRONIZE, PROCESS_VM_OPERATION, PROCESS_VM_READ, PROCESS_VM_WRITE,
};
use windows_sys::Win32::UI::Controls::{
    RBBIM_CHILD, RBBIM_CHILDSIZE, RBBIM_HEADERSIZE, RBBIM_ID, RBBIM_SIZE, RBBIM_STYLE,
    RB_GETBANDCOUNT, RB_GETBANDINFOW, RB_SETBANDINFOW, REBARBANDINFOW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetClassNameW, GetWindowThreadProcessId, SendMessageTimeoutW, SMTO_ABORTIFHUNG,
    SMTO_ERRORONEXIT,
};

static CLIENT: Mutex<Option<Client>> = Mutex::new(None);
// The header-size version of the SDK structure (REBARBANDINFO_V6_SIZE).
// Older common controls reject the later chevron fields' full structure size.
const BAND_INFO_SIZE: u32 =
    (std::mem::offset_of!(REBARBANDINFOW, cxHeader) + size_of::<u32>()) as u32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Band {
    pub window: HWND,
    pub process: u32,
    pub child: HWND,
    pub id: u32,
    pub header: u32,
    pub min_child: u32,
}

impl Band {
    pub fn same_owner(self, other: Self) -> bool {
        (self.window, self.process, self.child, self.id)
            == (other.window, other.process, other.child, other.id)
    }
}

struct Client {
    window: HWND,
    process_id: u32,
    process: HANDLE,
    // An address in the target process, never dereferenced in this process.
    buffer: usize,
    quarantined: bool,
}

impl Client {
    unsafe fn open(window: HWND) -> Option<Self> {
        if !cfg!(target_pointer_width = "64") {
            return None;
        }
        let mut name = [0u16; 64];
        let length = GetClassNameW(window, name.as_mut_ptr(), name.len() as i32);
        if length <= 0 || String::from_utf16_lossy(&name[..length as usize]) != "ReBarWindow32" {
            return None;
        }
        let mut process_id = 0;
        GetWindowThreadProcessId(window, &mut process_id);
        if process_id == 0 {
            return None;
        }
        let process = OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION
                | PROCESS_VM_OPERATION
                | PROCESS_VM_READ
                | PROCESS_VM_WRITE
                | PROCESS_SYNCHRONIZE,
            0,
            process_id,
        );
        if process == 0 {
            return None;
        }
        let mut wow64 = 0;
        if IsWow64Process(process, &mut wow64) == 0 || wow64 != 0 {
            CloseHandle(process);
            return None;
        }
        let buffer = VirtualAllocEx(
            process,
            null(),
            size_of::<REBARBANDINFOW>(),
            MEM_COMMIT | MEM_RESERVE,
            PAGE_READWRITE,
        );
        if buffer.is_null() {
            CloseHandle(process);
            return None;
        }
        Some(Self {
            window,
            process_id,
            process,
            buffer: buffer as usize,
            quarantined: false,
        })
    }

    unsafe fn matches(&self, window: HWND) -> bool {
        let mut process_id = 0;
        GetWindowThreadProcessId(window, &mut process_id);
        self.window == window
            && self.process_id == process_id
            && WaitForSingleObject(self.process, 0) == WAIT_TIMEOUT
    }

    unsafe fn send(&mut self, message: u32, index: usize, data: isize) -> Option<usize> {
        if self.quarantined || !self.matches(self.window) {
            return None;
        }
        let mut result = 0;
        if SendMessageTimeoutW(
            self.window,
            message,
            index,
            data,
            SMTO_ABORTIFHUNG | SMTO_ERRORONEXIT,
            250,
            &mut result,
        ) == 0
        {
            // A timed-out receiver may still be using the pointer. Do not free
            // or reuse it. Keep this one client blocked until its owner changes;
            // the tiny allocation is reclaimed when Explorer exits.
            self.quarantined = true;
            return None;
        }
        Some(result)
    }

    unsafe fn exchange(&mut self, message: u32, index: usize, info: &mut REBARBANDINFOW) -> bool {
        if self.quarantined {
            return false;
        }
        let size = size_of::<REBARBANDINFOW>();
        let mut copied = 0;
        if WriteProcessMemory(
            self.process,
            self.buffer as *const c_void,
            (info as *const REBARBANDINFOW).cast(),
            size,
            &mut copied,
        ) == 0
            || copied != size
        {
            return false;
        }
        if self
            .send(message, index, self.buffer as isize)
            .is_none_or(|result| result == 0)
        {
            return false;
        }
        ReadProcessMemory(
            self.process,
            self.buffer as *const c_void,
            (info as *mut REBARBANDINFOW).cast(),
            size,
            &mut copied,
        ) != 0
            && copied == size
    }

    unsafe fn band(&mut self, child: HWND) -> Option<(usize, Band)> {
        let count = self.send(RB_GETBANDCOUNT, 0, 0)?;
        if count > 64 {
            return None;
        }
        for index in 0..count {
            let mut info: REBARBANDINFOW = zeroed();
            info.cbSize = BAND_INFO_SIZE;
            info.fMask = RBBIM_CHILD
                | RBBIM_CHILDSIZE
                | RBBIM_HEADERSIZE
                | RBBIM_ID
                | RBBIM_SIZE
                | RBBIM_STYLE;
            if !self.exchange(RB_GETBANDINFOW, index, &mut info) {
                return None;
            }
            if info.hwndChild == child {
                return Some((
                    index,
                    Band {
                        window: self.window,
                        process: self.process_id,
                        child,
                        id: info.wID,
                        header: info.cxHeader,
                        min_child: info.cxMinChild,
                    },
                ));
            }
        }
        None
    }

    unsafe fn set_header(&mut self, expected: Band, header: u32) -> bool {
        let Some((index, current)) = self.band(expected.child) else {
            return false;
        };
        if !expected.same_owner(current) || expected.header != current.header {
            return false;
        }
        let mut info: REBARBANDINFOW = zeroed();
        info.cbSize = BAND_INFO_SIZE;
        info.fMask = RBBIM_HEADERSIZE;
        info.cxHeader = header;
        self.exchange(RB_SETBANDINFOW, index, &mut info)
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        unsafe {
            if !self.quarantined {
                VirtualFreeEx(self.process, self.buffer as *mut c_void, 0, MEM_RELEASE);
            }
            CloseHandle(self.process);
        }
    }
}

unsafe fn with_client<T>(window: HWND, action: impl FnOnce(&mut Client) -> Option<T>) -> Option<T> {
    let mut saved = CLIENT.try_lock().ok()?;
    if !saved.as_ref().is_some_and(|client| client.matches(window)) {
        *saved = Client::open(window);
    }
    action(saved.as_mut()?)
}

pub unsafe fn task_band(window: HWND, child: HWND) -> Option<Band> {
    with_client(window, |client| client.band(child).map(|(_, band)| band))
}

pub unsafe fn set_header(expected: Band, header: u32) -> bool {
    with_client(expected.window, |client| {
        Some(client.set_header(expected, header))
    })
    .unwrap_or(false)
}

pub fn release() {
    // A synchronous native-control call can deliver a reentrant window message.
    // Let the next recovery pass clean up if that call still owns the client.
    if let Ok(mut saved) = CLIENT.try_lock() {
        if saved.as_ref().is_some_and(|client| !client.quarantined) {
            *saved = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::os::windows::process::CommandExt;
    use std::process::{Child, Command, Stdio};
    use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
    use std::time::{Duration, Instant};
    use windows_sys::Win32::Foundation::{LPARAM, LRESULT, RECT, WPARAM};
    use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;
    use windows_sys::Win32::UI::Controls::{
        InitCommonControlsEx, ICC_BAR_CLASSES, ICC_COOL_CLASSES, INITCOMMONCONTROLSEX,
        RBBS_HIDETITLE, RBBS_NOGRIPPER, RB_INSERTBANDW,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CallWindowProcW, CreateWindowExW, DispatchMessageW, GetWindowRect, IsWindow, PeekMessageW,
        SetWindowLongPtrW, SetWindowPos, TranslateMessage, GWLP_WNDPROC, MSG, PM_REMOVE,
        SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, WM_APP, WNDPROC, WS_CHILD, WS_POPUP,
        WS_VISIBLE,
    };

    const ARM_DELAY: u32 = WM_APP + 77;
    static ORIGINAL_PROC: AtomicIsize = AtomicIsize::new(0);
    static DELAY_NEXT: AtomicBool = AtomicBool::new(false);

    #[test]
    fn release_does_not_wait_for_a_client_held_by_a_native_call() {
        let held = CLIENT.lock().unwrap();
        let (sender, receiver) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            release();
            sender.send(()).unwrap();
        });
        let returned_without_waiting = receiver.recv_timeout(Duration::from_millis(500)).is_ok();
        drop(held);
        worker.join().unwrap();
        assert!(
            returned_without_waiting,
            "reentrant cleanup must not wait for the active call"
        );
    }

    fn wide(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(Some(0)).collect()
    }

    unsafe extern "system" fn fixture_proc(
        hwnd: HWND,
        message: u32,
        w: WPARAM,
        l: LPARAM,
    ) -> LRESULT {
        if message == ARM_DELAY {
            DELAY_NEXT.store(true, Ordering::Release);
            return 1;
        }
        if message == RB_GETBANDINFOW && DELAY_NEXT.swap(false, Ordering::AcqRel) {
            std::thread::sleep(Duration::from_millis(750));
        }
        let procedure: WNDPROC = std::mem::transmute(ORIGINAL_PROC.load(Ordering::Acquire));
        CallWindowProcW(procedure, hwnd, message, w, l)
    }

    // Launched only by the integration tests below, in a separate test process.
    // All windows are hidden fixtures; Explorer is never changed by these tests.
    #[test]
    #[ignore = "fixture process for cross-process rebar tests"]
    fn remote_fixture() {
        unsafe {
            let controls = INITCOMMONCONTROLSEX {
                dwSize: size_of::<INITCOMMONCONTROLSEX>() as u32,
                dwICC: ICC_COOL_CLASSES | ICC_BAR_CLASSES,
            };
            assert_ne!(InitCommonControlsEx(&controls), 0);
            let host = CreateWindowExW(
                0,
                wide("STATIC").as_ptr(),
                null(),
                WS_POPUP,
                0,
                0,
                1500,
                40,
                0,
                0,
                0,
                null(),
            );
            let rebar = CreateWindowExW(
                0,
                wide("ReBarWindow32").as_ptr(),
                null(),
                WS_CHILD | WS_VISIBLE | 0x4 | 0x40,
                0,
                0,
                1500,
                40,
                host,
                0,
                0,
                null(),
            );
            let child = CreateWindowExW(
                0,
                wide("STATIC").as_ptr(),
                null(),
                WS_CHILD | WS_VISIBLE,
                0,
                0,
                1500,
                40,
                rebar,
                0,
                0,
                null(),
            );
            let list = CreateWindowExW(
                0,
                wide("STATIC").as_ptr(),
                null(),
                WS_CHILD | WS_VISIBLE,
                0,
                0,
                480,
                40,
                child,
                0,
                0,
                null(),
            );
            assert!(host != 0 && rebar != 0 && child != 0 && list != 0);
            let mut info: REBARBANDINFOW = zeroed();
            info.cbSize = BAND_INFO_SIZE;
            info.fMask = RBBIM_CHILD
                | RBBIM_CHILDSIZE
                | RBBIM_SIZE
                | RBBIM_STYLE
                | RBBIM_ID
                | RBBIM_HEADERSIZE;
            info.fStyle = RBBS_NOGRIPPER | RBBS_HIDETITLE;
            info.hwndChild = child;
            info.cxMinChild = 48;
            info.cyMinChild = 40;
            info.cx = 1500;
            info.wID = 7;
            let mut result = 0;
            assert_ne!(
                SendMessageTimeoutW(
                    rebar,
                    RB_INSERTBANDW,
                    0,
                    (&info as *const REBARBANDINFOW) as isize,
                    SMTO_ABORTIFHUNG,
                    250,
                    &mut result
                ),
                0
            );
            assert_ne!(result, 0);
            ORIGINAL_PROC.store(
                SetWindowLongPtrW(rebar, GWLP_WNDPROC, fixture_proc as *const () as isize),
                Ordering::Release,
            );
            println!("HICODEX_REBAR_FIXTURE {host} {rebar} {child} {list}");
            let deadline = Instant::now() + Duration::from_secs(15);
            while Instant::now() < deadline && IsWindow(host) != 0 {
                let mut message: MSG = zeroed();
                while PeekMessageW(&mut message, 0, 0, 0, PM_REMOVE) != 0 {
                    TranslateMessage(&message);
                    DispatchMessageW(&message);
                }
                std::thread::sleep(Duration::from_millis(1));
            }
        }
    }

    struct Fixture {
        process: Child,
        rebar: HWND,
        child: HWND,
        list: HWND,
    }

    impl Fixture {
        fn new() -> Self {
            let mut process = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "rebar::tests::remote_fixture",
                    "--ignored",
                    "--nocapture",
                ])
                .stdout(Stdio::piped())
                .stderr(Stdio::inherit())
                .creation_flags(CREATE_NO_WINDOW)
                .spawn()
                .unwrap();
            let output = process.stdout.take().unwrap();
            for line in BufReader::new(output).lines().map_while(Result::ok) {
                if let Some((_, values)) = line.split_once("HICODEX_REBAR_FIXTURE ") {
                    let handles: Vec<isize> = values
                        .split_whitespace()
                        .map(|value| value.parse().unwrap())
                        .collect();
                    return Self {
                        process,
                        rebar: handles[1],
                        child: handles[2],
                        list: handles[3],
                    };
                }
            }
            let status = process.wait().unwrap();
            panic!("native rebar fixture failed to start: {status}");
        }
        unsafe fn rect(&self, window: HWND) -> RECT {
            let mut rect: RECT = zeroed();
            assert_ne!(GetWindowRect(window, &mut rect), 0);
            rect
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = self.process.kill();
            let _ = self.process.wait();
        }
    }

    #[test]
    fn native_reservation_survives_child_reset_and_notification_area_resizes() {
        let fixture = Fixture::new();
        unsafe {
            let mut client = Client::open(fixture.rebar).unwrap();
            let band = client.band(fixture.child).unwrap().1;
            let base = fixture.rect(fixture.child).left;
            // Reproduce the old failure: moving the inner list is undone by a reset.
            assert_ne!(
                SetWindowPos(
                    fixture.list,
                    0,
                    400,
                    0,
                    0,
                    0,
                    SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE
                ),
                0
            );
            assert_eq!(fixture.rect(fixture.list).left, base + 400);
            assert_ne!(
                SetWindowPos(
                    fixture.list,
                    0,
                    0,
                    0,
                    0,
                    0,
                    SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE
                ),
                0
            );
            assert_eq!(fixture.rect(fixture.list).left, base);

            assert!(client.set_header(band, 400));
            for width in [1500, 1480, 1440, 1200, 1000, 1400, 1500]
                .into_iter()
                .cycle()
                .take(50)
            {
                assert_ne!(
                    SetWindowPos(
                        fixture.rebar,
                        0,
                        0,
                        0,
                        width,
                        40,
                        SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE
                    ),
                    0
                );
                assert_ne!(
                    SetWindowPos(
                        fixture.list,
                        0,
                        0,
                        0,
                        0,
                        0,
                        SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE
                    ),
                    0
                );
                // No correction callback or timer runs between reset and assertion.
                assert_eq!(fixture.rect(fixture.list).left, base + 400);
                assert_eq!(client.band(fixture.child).unwrap().1.header, 400);
            }
            let applied = client.band(fixture.child).unwrap().1;
            assert!(client.set_header(applied, band.header));
            assert_eq!(fixture.rect(fixture.list).left, base);
        }
    }

    #[test]
    fn native_write_does_not_overwrite_a_newer_band_header() {
        let fixture = Fixture::new();
        unsafe {
            let mut client = Client::open(fixture.rebar).unwrap();
            let original = client.band(fixture.child).unwrap().1;
            assert!(client.set_header(original, 400));
            let ours = client.band(fixture.child).unwrap().1;
            assert!(client.set_header(ours, 120));
            assert!(!client.set_header(ours, original.header));
            assert_eq!(client.band(fixture.child).unwrap().1.header, 120);
        }
    }

    #[test]
    fn timed_out_native_message_keeps_its_buffer_alive_and_stops_reusing_it() {
        let mut fixture = Fixture::new();
        unsafe {
            let mut client = Client::open(fixture.rebar).unwrap();
            assert!(client.send(ARM_DELAY, 0, 0).is_some());
            assert!(client.band(fixture.child).is_none());
            assert!(client.quarantined);
            assert!(client.band(fixture.child).is_none());
            let process_id = client.process_id;
            let address = client.buffer;
            drop(client);
            std::thread::sleep(Duration::from_millis(850));
            assert!(fixture.process.try_wait().unwrap().is_none());
            let process = OpenProcess(PROCESS_VM_READ, 0, process_id);
            assert_ne!(process, 0);
            let mut data: REBARBANDINFOW = zeroed();
            let readable = ReadProcessMemory(
                process,
                address as *const c_void,
                (&mut data as *mut REBARBANDINFOW).cast(),
                size_of::<REBARBANDINFOW>(),
                std::ptr::null_mut(),
            );
            CloseHandle(process);
            assert_ne!(readable, 0);
        }
    }
}
