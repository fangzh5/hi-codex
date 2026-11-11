//! Refuse credential/state writes while a desktop or CLI can still own them.
use std::mem::{size_of, zeroed};
use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_NO_MORE_FILES, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};

fn is_account_client(name: &str) -> bool {
    // Current desktop builds use ChatGPT.exe; earlier builds used Codex.exe.
    name.eq_ignore_ascii_case("codex.exe") || name.eq_ignore_ascii_case("chatgpt.exe")
}

pub fn require_stopped() -> Result<(), String> {
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            return Err("Could not check running Codex clients; nothing was changed.".into());
        }
        let mut process: PROCESSENTRY32W = zeroed();
        process.dwSize = size_of::<PROCESSENTRY32W>() as u32;
        let mut found = Vec::new();
        let mut more = Process32FirstW(snapshot, &mut process);
        if more == 0 {
            CloseHandle(snapshot);
            return Err("Could not enumerate Codex clients; nothing was changed.".into());
        }
        while more != 0 {
            let length = process
                .szExeFile
                .iter()
                .position(|c| *c == 0)
                .unwrap_or(260);
            let name = String::from_utf16_lossy(&process.szExeFile[..length]);
            if is_account_client(&name) {
                found.push(process.th32ProcessID);
            }
            more = Process32NextW(snapshot, &mut process);
        }
        let enumeration_error = GetLastError();
        CloseHandle(snapshot);
        if enumeration_error != ERROR_NO_MORE_FILES {
            return Err("The Codex process check was incomplete; nothing was changed.".into());
        }
        if !found.is_empty() {
            return Err(format!(
                "ChatGPT/Codex is still running ({} processes). Fully quit the desktop app and close Codex CLI/IDE sessions, then retry. No credentials or project history were changed.",
                found.len()
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recognizes_current_and_older_clients_without_matching_our_meter() {
        for name in ["ChatGPT.exe", "CODEX.EXE", "codex.exe"] {
            assert!(is_account_client(name));
        }
        for name in ["HiCodex.exe", "codex-code-mode-host.exe", "explorer.exe"] {
            assert!(!is_account_client(name));
        }
    }
}
