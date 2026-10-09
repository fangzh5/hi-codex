#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod accounts;
mod credential_storage;
mod quota;
mod rpc;
mod startup;
mod system_usage;
mod taskbar;
mod ui;

use std::ptr::null;
use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS};
use windows_sys::Win32::System::Threading::CreateMutexW;

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

fn main() {
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

    let mutex_name = wide("Local\\HiCodex.SingleInstance");
    let mutex = unsafe { CreateMutexW(null(), 0, mutex_name.as_ptr()) };
    if mutex == 0 {
        return;
    }

    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        unsafe { CloseHandle(mutex) };
        return;
    }

    ui::run();
    unsafe { CloseHandle(mutex) };
}
