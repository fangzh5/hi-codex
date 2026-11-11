//! Graceful desktop handoff. Never terminate a process by executable name.
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::*,
    System::Threading::*,
    UI::{Shell::ShellExecuteW, WindowsAndMessaging::*},
};

struct Target {
    process: HANDLE,
    path: PathBuf,
}
impl Drop for Target {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.process);
        }
    }
}

fn supported_path(path: &str) -> bool {
    let p = path.replace('/', "\\").to_ascii_lowercase();
    p.contains("\\windowsapps\\openai.codex_")
        && (p.ends_with("\\app\\chatgpt.exe") || p.ends_with("\\app\\codex.exe"))
}

unsafe extern "system" fn collect(hwnd: HWND, parameter: LPARAM) -> BOOL {
    if IsWindowVisible(hwnd) == 0 {
        return 1;
    }
    let mut pid = 0;
    GetWindowThreadProcessId(hwnd, &mut pid);
    let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | 0x00100000, 0, pid);
    if handle == 0 {
        return 1;
    }
    let mut buffer = vec![0u16; 32768];
    let mut len = buffer.len() as u32;
    if QueryFullProcessImageNameW(handle, 0, buffer.as_mut_ptr(), &mut len) != 0 {
        let path = String::from_utf16_lossy(&buffer[..len as usize]);
        if supported_path(&path) {
            let found = &mut *(parameter as *mut Vec<(HWND, Target)>);
            found.push((
                hwnd,
                Target {
                    process: handle,
                    path: PathBuf::from(path),
                },
            ));
            return 1;
        }
    }
    CloseHandle(handle);
    1
}

fn is_quit_accelerator(label: &str) -> bool {
    label
        .rsplit_once('\t')
        .is_some_and(|(_, key)| key.replace(' ', "").eq_ignore_ascii_case("Ctrl+Q"))
}

unsafe fn quit_menu_command(menu: HMENU) -> Option<u32> {
    if menu == 0 {
        return None;
    }
    for index in 0..GetMenuItemCount(menu) {
        if let Some(id) = quit_menu_command(GetSubMenu(menu, index)) {
            return Some(id);
        }
        let mut label = [0u16; 512];
        let len = GetMenuStringW(
            menu,
            index as u32,
            label.as_mut_ptr(),
            label.len() as i32,
            MF_BYPOSITION,
        );
        if len > 0 && is_quit_accelerator(&String::from_utf16_lossy(&label[..len as usize])) {
            let id = GetMenuItemID(menu, index);
            if id != u32::MAX {
                return Some(id);
            }
        }
    }
    None
}

fn request_quit(window: HWND, target: &Target) -> Result<(), String> {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;
    unsafe {
        let mut pid = 0;
        GetWindowThreadProcessId(window, &mut pid);
        if pid != GetProcessId(target.process)
            || WaitForSingleObject(target.process, 0) != WAIT_TIMEOUT
        {
            return Err("Codex window changed. No account was changed.".into());
        }
        // Prefer the actual Quit menu command: no global keyboard input is needed.
        if let Some(command) = quit_menu_command(GetMenu(window)) {
            if PostMessageW(window, WM_COMMAND, command as WPARAM, 0) != 0 {
                return Ok(());
            }
            return Err("Could not request Codex to quit. No account was changed.".into());
        }
        // Custom title bars may not expose a Win32 menu. Use the verified Ctrl+Q
        // accelerator only after confirming that the target window has focus.
        if IsIconic(window) != 0 {
            ShowWindow(window, SW_RESTORE);
        }
        SetForegroundWindow(window);
        let end = Instant::now() + Duration::from_secs(2);
        loop {
            let modifiers_up = [VK_CONTROL, VK_SHIFT, VK_MENU, VK_LWIN, VK_RWIN, 0x51]
                .iter()
                .all(|key| GetAsyncKeyState(*key as i32) >= 0);
            if GetForegroundWindow() == window && modifiers_up {
                break;
            }
            if Instant::now() >= end {
                return Err("Could not focus Codex safely. Release shortcut keys and retry. No account was changed.".into());
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        let keys = [
            (VK_CONTROL, 0),
            (0x51, 0),
            (0x51, KEYEVENTF_KEYUP),
            (VK_CONTROL, KEYEVENTF_KEYUP),
        ];
        let inputs = keys.map(|(key, flags)| INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: key,
                    wScan: 0,
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        });
        if GetForegroundWindow() != window {
            return Err("Focus changed. No account was changed.".into());
        }
        if SendInput(
            inputs.len() as u32,
            inputs.as_ptr(),
            std::mem::size_of::<INPUT>() as i32,
        ) != inputs.len() as u32
        {
            // Release our keys if Windows accepted only part of the sequence.
            SendInput(2, inputs[2..].as_ptr(), std::mem::size_of::<INPUT>() as i32);
            return Err(
                "Windows could not send the Codex quit shortcut. No account was changed.".into(),
            );
        }
    }
    Ok(())
}

pub struct Desktop {
    windows: Vec<(HWND, Target)>,
    app_id: String,
}
impl Desktop {
    pub fn discover() -> Result<Self, String> {
        let mut windows: Vec<(HWND, Target)> = Vec::new();
        if unsafe { EnumWindows(Some(collect), &mut windows as *mut _ as LPARAM) } == 0 {
            return Err("Could not inspect Codex windows. Nothing changed.".into());
        }
        if windows.is_empty() {
            return Err("No supported Codex Desktop window found. Open Codex and retry.".into());
        }
        if windows.iter().any(|(_, t)| t.path != windows[0].1.path) {
            return Err(
                "Multiple Codex installations are open. Close the extra installation first.".into(),
            );
        }
        // Capture the registered app identity while the original process still exists.
        let mut id = vec![0u16; 512];
        let mut length = id.len() as u32;
        let result = unsafe {
            windows_sys::Win32::Storage::Packaging::Appx::GetApplicationUserModelId(
                windows[0].1.process,
                &mut length,
                id.as_mut_ptr(),
            )
        };
        if result != 0 || length < 2 || length as usize > id.len() {
            return Err(
                "Could not resolve the Windows app launch identity. No account was changed.".into(),
            );
        }
        let app_id = String::from_utf16_lossy(&id[..length as usize - 1]);
        Ok(Self { windows, app_id })
    }
    pub fn close(&self, mut timeout: impl FnMut() -> CloseChoice) -> Result<(), String> {
        // Quit is application-wide; sending it once avoids duplicate quit dialogs.
        let (window, target) = &self.windows[0];
        request_quit(*window, target)?;
        let mut deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if self
                .windows
                .iter()
                .all(|(_, t)| unsafe { WaitForSingleObject(t.process, 0) } == WAIT_OBJECT_0)
                && crate::process_guard::require_stopped().is_ok()
            {
                return Ok(());
            }
            // Renderer and app-server children can take longer than the UI.
            if Instant::now() >= deadline {
                match timeout() {
                    CloseChoice::Wait => deadline = Instant::now() + Duration::from_secs(15),
                    CloseChoice::Cancel => {
                        return Err("Switch cancelled. No account was changed.".into())
                    }
                    CloseChoice::Force => {
                        self.force_close_roots()?;
                        // Do not kill unrelated CLI/IDE clients. The global write guard
                        // must still succeed before any credentials are replaced.
                        let end = Instant::now() + Duration::from_secs(10);
                        loop {
                            if crate::process_guard::require_stopped().is_ok() {
                                return Ok(());
                            }
                            if Instant::now() >= end {
                                return Err("Desktop close was requested, but Codex clients remain. No account was changed. Close remaining CLI/IDE sessions and retry.".into());
                            }
                            std::thread::sleep(Duration::from_millis(150));
                        }
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(150));
        }
    }
    fn force_close_roots(&self) -> Result<(), String> {
        for (_, target) in &self.windows {
            unsafe {
                if WaitForSingleObject(target.process, 0) == WAIT_OBJECT_0 {
                    continue;
                }
                let pid = GetProcessId(target.process);
                let handle = OpenProcess(
                    PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION,
                    0,
                    pid,
                );
                if handle == 0 {
                    return Err(
                        "Windows refused permission to close Codex. No account changed.".into(),
                    );
                }
                // Original process handle is retained; never act if it exited.
                let mut a: FILETIME = std::mem::zeroed();
                let mut b = a;
                let mut exit = a;
                let mut kernel = a;
                let mut user = a;
                let same =
                    GetProcessTimes(target.process, &mut a, &mut exit, &mut kernel, &mut user) != 0
                        && GetProcessTimes(handle, &mut b, &mut exit, &mut kernel, &mut user) != 0
                        && a.dwLowDateTime == b.dwLowDateTime
                        && a.dwHighDateTime == b.dwHighDateTime
                        && WaitForSingleObject(target.process, 0) == WAIT_TIMEOUT;
                let closed = same && TerminateProcess(handle, 1) != 0;
                CloseHandle(handle);
                if !closed {
                    return Err("Codex changed while closing. No account was changed.".into());
                }
            }
        }
        Ok(())
    }
    pub fn open(&self) -> Result<(), String> {
        let path: Vec<u16> = format!("shell:AppsFolder\\{}", self.app_id)
            .encode_utf16()
            .chain(Some(0))
            .collect();
        let verb: Vec<u16> = "open".encode_utf16().chain(Some(0)).collect();
        let result = unsafe {
            ShellExecuteW(
                0,
                verb.as_ptr(),
                path.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                SW_SHOWNORMAL,
            )
        };
        if result <= 32 {
            return Err("The account was switched, but Windows could not reopen Codex. Open it from Start; do not switch again.".into());
        }
        let end = Instant::now() + Duration::from_secs(20);
        loop {
            if Self::discover().is_ok_and(|d| {
                d.windows
                    .iter()
                    .any(|(_, t)| t.path == self.windows[0].1.path)
            }) {
                return Ok(());
            }
            if Instant::now() >= end {
                return Err("The account was switched, but no Codex window appeared. Open Codex from Start; do not switch again.".into());
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }
}

pub enum CloseChoice {
    Wait,
    Force,
    Cancel,
}

pub fn sequence(
    close: impl FnOnce() -> Result<(), String>,
    switch: impl FnOnce() -> Result<bool, String>,
    verify: impl FnOnce() -> Result<(), String>,
    open: impl FnOnce() -> Result<(), String>,
    mut progress: impl FnMut(&str),
) -> Result<bool, String> {
    progress("Closing…");
    close()?;
    progress("Switching…");
    let changed = switch()?;
    progress("Verifying…");
    verify()?;
    progress("Opening…");
    open()?;
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn quit_command_matches_accelerator_not_translated_label() {
        assert!(is_quit_accelerator("Quit Codex\tCtrl+Q"));
        assert!(is_quit_accelerator("退出\tCtrl + Q"));
        assert!(!is_quit_accelerator("Close window\tCtrl+W"));
        assert!(!is_quit_accelerator("Ctrl+Q"));
    }
    #[test]
    fn only_supported_desktop_paths_are_selected() {
        assert!(supported_path(
            r"C:\Program Files\WindowsApps\OpenAI.Codex_1_x64\app\ChatGPT.exe"
        ));
        assert!(!supported_path(r"C:\tools\codex.exe"));
        assert!(!supported_path(
            r"C:\Program Files\WindowsApps\OpenAI.ChatGPT_1\app\ChatGPT.exe"
        ));
    }
    #[test]
    fn close_failure_never_changes_auth() {
        assert!(sequence(
            || Err("busy".into()),
            || panic!("must not switch"),
            || panic!(),
            || panic!(),
            |_| {}
        )
        .is_err());
    }
    #[test]
    fn verification_failure_never_reopens() {
        assert!(sequence(
            || Ok(()),
            || Ok(true),
            || Err("wrong identity".into()),
            || panic!("must not reopen"),
            |_| {}
        )
        .is_err());
    }
    #[test]
    fn successful_flow_has_ordered_progress() {
        let mut stages = Vec::new();
        assert!(sequence(
            || Ok(()),
            || Ok(true),
            || Ok(()),
            || Ok(()),
            |s| stages.push(s.to_owned())
        )
        .unwrap());
        assert_eq!(stages, ["Closing…", "Switching…", "Verifying…", "Opening…"]);
    }
}
