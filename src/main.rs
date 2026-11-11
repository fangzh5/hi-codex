#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod account_usage;
mod accounts;
mod credential_storage;
mod desktop_handoff;
mod desktop_state;
mod diagnostics;
mod instance;
mod locale;
mod panel_layout;
mod process_guard;
mod quota;
mod rebar;
mod rpc;
mod startup;
mod system_usage;
mod taskbar;
mod taskbar_measurement;
mod taskbar_recovery;
mod ui;
mod widget_layout;

use windows_sys::Win32::UI::WindowsAndMessaging::{
    MessageBoxW, MB_ICONINFORMATION, MB_ICONWARNING, MB_OK,
};

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).is_some_and(|arg| arg == "--taskbar-guardian") {
        if let (Some(pid), Some(created), Some(event)) = (
            args.get(2).and_then(|s| s.parse().ok()),
            args.get(3).and_then(|s| s.parse().ok()),
            args.get(4),
        ) {
            taskbar_recovery::run_guardian(pid, created, event);
        }
        return;
    }

    if std::env::args().any(|argument| argument == "--accounts-diagnose") {
        match accounts::AccountManager::current().and_then(|manager| manager.list()) {
            Ok(list) => println!(
                "Saved accounts: {}; active saved account: {}. No account identities are included.",
                list.accounts.len(),
                list.active_key.is_some()
            ),
            Err(error) => {
                eprintln!("Account storage unavailable: {error}");
                std::process::exit(1);
            }
        }
        return;
    }
    if std::env::args().any(|argument| argument == "--switch-preflight") {
        match process_guard::require_stopped() {
            Ok(()) => println!("No ChatGPT/Codex client found."),
            Err(error) => {
                eprintln!("{error}");
                std::process::exit(1);
            }
        }
        return;
    }
    if std::env::args().any(|argument| argument == "--history-diagnose") {
        match accounts::codex_home().and_then(|home| desktop_state::diagnose(&home)) {
            Ok(report) => println!("{report}"),
            Err(error) => {
                eprintln!("{error}");
                std::process::exit(1);
            }
        }
        return;
    }
    if std::env::args().any(|argument| argument == "--taskbar-diagnose") {
        unsafe {
            windows_sys::Win32::UI::HiDpi::SetProcessDpiAwarenessContext(-4isize);
        }
        match unsafe { taskbar::diagnose_layout() } {
            Some(layout) => println!("{layout}"),
            None => {
                eprintln!("The native Windows taskbar band is unavailable");
                std::process::exit(1);
            }
        }
        return;
    }
    if std::env::args().any(|argument| argument == "--diagnose") {
        match rpc::fetch_usage() {
            Ok(result) => println!("{result:#?}"),
            Err(error) => {
                eprintln!("HiCodex diagnostic failed: {error}");
                std::process::exit(1);
            }
        }
        return;
    }

    let _instance = match instance::InstanceGuard::acquire() {
        Ok(instance) => instance,
        Err(error) => {
            let (message, icon) = match error {
                instance::InstanceError::AlreadyRunning => (
                    "HiCodex is already running. Close the existing version before starting another one.",
                    MB_ICONINFORMATION,
                ),
                instance::InstanceError::Unavailable => (
                    "HiCodex could not verify whether another instance is running. Please try again.",
                    MB_ICONWARNING,
                ),
            };
            unsafe {
                MessageBoxW(
                    0,
                    wide(message).as_ptr(),
                    wide("HiCodex").as_ptr(),
                    MB_OK | icon,
                );
            }
            return;
        }
    };

    ui::run();
}
