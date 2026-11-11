use crate::quota::{QuotaWindow, UsageSnapshot};
use crate::{
    account_usage, accounts, diagnostics, panel_layout, rpc, startup, system_usage, taskbar,
    widget_layout,
};
use std::ffi::c_void;
use std::mem::{size_of, zeroed};
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use windows_sys::Win32::Foundation::{
    GetLastError, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM,
};
use windows_sys::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWA_WINDOW_CORNER_PREFERENCE,
    DWMWCP_ROUND,
};
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, CreateCompatibleDC, CreateDIBSection, CreateFontW, CreatePen, CreateRoundRectRgn,
    CreateSolidBrush, DeleteDC, DeleteObject, DrawTextW, EndPaint, FillRect, GetDC,
    GetMonitorInfoW, GetStockObject, GetTextExtentPoint32W, InvalidateRect, MonitorFromWindow,
    ReleaseDC, RoundRect, SelectObject, SetBkMode, SetTextColor, SetWindowRgn, AC_SRC_ALPHA,
    AC_SRC_OVER, ANTIALIASED_QUALITY, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, BLENDFUNCTION,
    CLEARTYPE_NATURAL_QUALITY, CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET, DEFAULT_PITCH, DIB_RGB_COLORS,
    DT_END_ELLIPSIS, DT_LEFT, DT_NOPREFIX, DT_RIGHT, DT_SINGLELINE, DT_VCENTER, FF_DONTCARE,
    FW_NORMAL, FW_SEMIBOLD, HBITMAP, HDC, HFONT, MONITORINFO, MONITOR_DEFAULTTONEAREST, NULL_BRUSH,
    OUT_DEFAULT_PRECIS, PAINTSTRUCT, PS_SOLID, TRANSPARENT,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Controls::Dialogs::{
    CommDlgExtendedError, GetOpenFileNameW, GetSaveFileNameW, OFN_FILEMUSTEXIST, OFN_NOCHANGEDIR,
    OFN_PATHMUSTEXIST, OPENFILENAMEW,
};
use windows_sys::Win32::UI::Controls::{WM_MOUSEHOVER, WM_MOUSELEAVE};
use windows_sys::Win32::UI::HiDpi::{GetDpiForWindow, SetProcessDpiAwarenessContext};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture, TrackMouseEvent, TME_HOVER, TME_LEAVE, TRACKMOUSEEVENT,
};
use windows_sys::Win32::UI::Shell::ShellExecuteW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DestroyWindow,
    DispatchMessageW, FindWindowExW, FindWindowW, GetClassNameW, GetClientRect, GetCursorPos,
    GetForegroundWindow, GetMessageW, GetWindow, GetWindowLongPtrW, GetWindowRect, IsWindowVisible,
    KillTimer, LoadCursorW, MessageBoxW, PostMessageW, PostQuitMessage, RegisterClassW,
    SetForegroundWindow, SetLayeredWindowAttributes, SetTimer, SetWindowLongPtrW, SetWindowPos,
    ShowWindow, TrackPopupMenu, TranslateMessage, UpdateLayeredWindow, CS_HREDRAW, CS_VREDRAW,
    CW_USEDEFAULT, GWL_EXSTYLE, GW_HWNDPREV, HMENU, HWND_TOPMOST, IDC_ARROW, IDYES, LWA_ALPHA,
    MA_NOACTIVATE, MB_DEFBUTTON2, MB_ICONINFORMATION, MB_ICONWARNING, MB_YESNO, MF_CHECKED,
    MF_GRAYED, MF_POPUP, MF_SEPARATOR, MF_STRING, MSG, SWP_NOACTIVATE, SWP_SHOWWINDOW, SW_HIDE,
    SW_SHOW, SW_SHOWNOACTIVATE, TPM_RETURNCMD, TPM_RIGHTBUTTON, ULW_ALPHA, WM_APP, WM_DESTROY,
    WM_DISPLAYCHANGE, WM_DPICHANGED, WM_ERASEBKGND, WM_MOUSEACTIVATE, WM_MOUSEMOVE, WM_PAINT,
    WM_RBUTTONDOWN, WM_RBUTTONUP, WM_SETTINGCHANGE, WM_TIMER, WNDCLASSW, WS_EX_LAYERED,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP, WS_VISIBLE,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{IDNO, MB_DEFBUTTON3, MB_YESNOCANCEL};

const WIDGET_CLASS_NAME: &str = "HiCodex.TaskbarWidget";
const FLYOUT_CLASS_NAME: &str = "HiCodex.Flyout";

const TIMER_POSITION: usize = 1;
const TIMER_REFRESH: usize = 2;
const TIMER_HIDE_FLYOUT: usize = 3;
const TIMER_HOVER_POLL: usize = 4;
const TIMER_SYSTEM_USAGE: usize = 5;
const TIMER_ACCOUNT_FEEDBACK: usize = 6;
const TIMER_LAYOUT_SETTLE: usize = 7;
const TIMER_LAYOUT_UPDATE: usize = 8;
const LAYOUT_MIN_INTERVAL_MS: u32 = 50;
const ACCOUNT_FEEDBACK_MS: u32 = 8_000;
const POSITION_INTERVAL_MS: u32 = 2_000;
const SYSTEM_USAGE_INTERVAL_MS: u32 = 2_000;
const REFRESH_INTERVAL_MS: u32 = 120_000;
const HOVER_DELAY_MS: u32 = 300;
const HIDE_DELAY_MS: u32 = 250;
const HOVER_POLL_INTERVAL_MS: u32 = 100;
const HOVER_SHOW_TICKS: u32 = 3;
const HOVER_HIDE_TICKS: u32 = 3;
const WM_REFRESHED: u32 = WM_APP + 1;
const WM_ACCOUNT_DONE: u32 = WM_APP + 2;
const WM_TASKBAR_LAYOUT: u32 = WM_APP + 3;
const WM_MULTI_UPDATED: u32 = WM_APP + 4;

const MENU_LANGUAGE_EN: i32 = 1018;
const MENU_LANGUAGE_ZH: i32 = 1019;
const MENU_REFRESH: i32 = 1001;
const MENU_OPEN_USAGE: i32 = 1002;
const MENU_STARTUP: i32 = 1003;
const MENU_CENTER_TASKBAR: i32 = 1004;
const MENU_ACRYLIC_TASKBAR: i32 = 1005;
const MENU_EXIT: i32 = 1006;
const MENU_SYSTEM_USAGE: i32 = 1007;
const MENU_ABOUT: i32 = 1008;
const MENU_SAVE_ACCOUNT: i32 = 1010;
const MENU_IMPORT_ACCOUNT: i32 = 1011;
const MENU_EXPORT_ACCOUNT: i32 = 1012;
const MENU_DELETE_FIRST: i32 = 3000;
const MENU_RESTORE_ACCOUNT: i32 = 1013;
const MENU_SIGN_IN_ACCOUNT: i32 = 1014;
const MENU_CANCEL_SIGN_IN: i32 = 1015;
const MENU_REPAIR_HISTORY: i32 = 1016;
const MENU_ACCOUNT_STORAGE_ERROR: i32 = 1017;
const MENU_ACCOUNT_FIRST: i32 = 2000;

static STATE: OnceLock<Arc<Mutex<AppState>>> = OnceLock::new();
static WINDOW: AtomicIsize = AtomicIsize::new(0);
static FLYOUT_WINDOW: AtomicIsize = AtomicIsize::new(0);
static FLYOUT_GLASS: AtomicBool = AtomicBool::new(false);
static REFRESHING: AtomicBool = AtomicBool::new(false);
static REFRESH_PENDING: AtomicBool = AtomicBool::new(false);
static ACCOUNT_OPERATION: AtomicBool = AtomicBool::new(false);
static ACCOUNT_LOGIN_REQUIRED: AtomicBool = AtomicBool::new(false);
static ACCOUNT_LOGIN_ACTIVE: AtomicBool = AtomicBool::new(false);
static ACCOUNT_LOGIN_CANCEL: AtomicBool = AtomicBool::new(false);
// Serialize credential mutations against this process's App Server readers.
static AUTH_GATE: Mutex<()> = Mutex::new(());
static ACCOUNT_NOTICE: Mutex<Option<(String, bool)>> = Mutex::new(None);
static HOVER_TICKS: AtomicU32 = AtomicU32::new(0);
static OUTSIDE_TICKS: AtomicU32 = AtomicU32::new(0);
static HOVER_SUPPRESSED: AtomicBool = AtomicBool::new(false);
static TASKBAR_CENTERED: AtomicBool = AtomicBool::new(false);
static TASKBAR_ACRYLIC: AtomicBool = AtomicBool::new(false);
static SYSTEM_USAGE_ENABLED: AtomicBool = AtomicBool::new(false);
static SWITCH_MODE: AtomicBool = AtomicBool::new(false);
static ACCOUNT_VISIBLE: AtomicBool = AtomicBool::new(true);
static ACCOUNT_BUTTON_PRESSED: AtomicBool = AtomicBool::new(false);
static MULTI_REFRESHING: AtomicBool = AtomicBool::new(false);
static MULTI_GENERATION: AtomicU64 = AtomicU64::new(0);
static MULTI_LAST_REFRESH: Mutex<Option<Instant>> = Mutex::new(None);
static ACCOUNT_SCROLL: AtomicUsize = AtomicUsize::new(0);
static PANEL_ROWS: AtomicUsize = AtomicUsize::new(0);
static ACCOUNT_STORAGE_ERROR: Mutex<Option<String>> = Mutex::new(None);
static SYSTEM_USAGE_SAMPLER: OnceLock<Mutex<system_usage::SystemUsageSampler>> = OnceLock::new();
static LAYOUT_GATE: Mutex<LayoutGate> = Mutex::new(LayoutGate {
    last_finished: None,
    active: false,
});

struct LayoutGate {
    last_finished: Option<Instant>,
    active: bool,
}

impl LayoutGate {
    fn begin(&mut self, now: Instant) -> u32 {
        let interval = Duration::from_millis(LAYOUT_MIN_INTERVAL_MS as u64);
        let remaining = if self.active {
            interval
        } else {
            self.last_finished.map_or(Duration::ZERO, |last| {
                interval.saturating_sub(now.saturating_duration_since(last))
            })
        };
        let delay = remaining.as_nanos().div_ceil(1_000_000) as u32;
        if delay == 0 {
            self.active = true;
        }
        delay
    }

    fn finish(&mut self, now: Instant) {
        self.last_finished = Some(now);
        self.active = false;
    }
}

#[derive(Clone, Debug)]
enum Status {
    Loading,
    Ready,
    Error(String),
}

#[derive(Clone, Debug)]
struct AppState {
    status: Status,
    usage: UsageSnapshot,
    account: rpc::AccountSummary,
    updated_at: Option<i64>,
    system_usage: system_usage::SystemUsage,
    account_feedback: Option<String>,
    saved_accounts: Vec<account_usage::Row>,
    saved_accounts_error: Option<String>,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            status: Status::Loading,
            usage: UsageSnapshot::default(),
            account: rpc::AccountSummary::default(),
            updated_at: None,
            system_usage: system_usage::SystemUsage::default(),
            account_feedback: None,
            saved_accounts: Vec::new(),
            saved_accounts_error: None,
        }
    }
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

const fn rgb(red: u8, green: u8, blue: u8) -> u32 {
    red as u32 | ((green as u32) << 8) | ((blue as u32) << 16)
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

fn trigger_refresh() {
    if ACCOUNT_OPERATION.load(Ordering::Acquire) || REFRESHING.swap(true, Ordering::AcqRel) {
        REFRESH_PENDING.store(true, Ordering::Release);
        return;
    }

    std::thread::spawn(|| {
        let Ok(_gate) = AUTH_GATE.lock() else {
            REFRESHING.store(false, Ordering::Release);
            return;
        };
        let result = rpc::fetch_usage_cancellable(&ACCOUNT_OPERATION);
        if ACCOUNT_OPERATION.load(Ordering::Acquire) {
            // The account worker is waiting for AUTH_GATE, so its completion
            // cannot consume this retry before we have recorded it.
            REFRESH_PENDING.store(true, Ordering::Release);
        } else if let Some(state) = STATE.get() {
            if let Ok(mut state) = state.lock() {
                match result {
                    Ok(result) => {
                        state.usage = result.usage;
                        state.account = result.account;
                        state.updated_at = Some(now_unix());
                        state.status = Status::Ready;
                    }
                    Err(error) => {
                        state.status = Status::Error(error);
                    }
                }
            }
        }

        REFRESHING.store(false, Ordering::Release);
        let hwnd = WINDOW.load(Ordering::Acquire);
        if hwnd != 0 {
            unsafe {
                PostMessageW(hwnd, WM_REFRESHED, 0, 0);
            }
        }
    });
}

fn notify_multi_update() {
    let hwnd = WINDOW.load(Ordering::Acquire);
    if hwnd != 0 {
        unsafe {
            PostMessageW(hwnd, WM_MULTI_UPDATED, 0, 0);
        }
    }
}

fn trigger_saved_refresh(force: bool) {
    if ACCOUNT_OPERATION.load(Ordering::Acquire) {
        return;
    }
    if let Ok(last) = MULTI_LAST_REFRESH.lock() {
        if !force && last.is_some_and(|time| time.elapsed() < Duration::from_secs(300)) {
            return;
        }
    }
    if MULTI_REFRESHING.swap(true, Ordering::AcqRel) {
        return;
    }
    if let Ok(mut last) = MULTI_LAST_REFRESH.lock() {
        *last = Some(Instant::now());
    }
    let generation = MULTI_GENERATION.load(Ordering::Acquire);
    std::thread::spawn(move || {
        let run = || -> Result<(), String> {
            // Account management cancels our pending RPC before waiting for this
            // gate, so its switch cannot race our child or rotated-token save.
            let _gate = AUTH_GATE
                .lock()
                .map_err(|_| "Account refresh unavailable")?;
            if ACCOUNT_OPERATION.load(Ordering::Acquire) {
                return Ok(());
            }
            let manager = accounts::AccountManager::current()?;
            let list = manager.list()?;
            let keys: Vec<_> = list
                .accounts
                .iter()
                .filter(|account| list.active_key.as_deref() != Some(&account.key))
                .map(|account| account.key.clone())
                .collect();
            if let Some(mut state) = STATE.get().and_then(|state| state.lock().ok()) {
                if MULTI_GENERATION.load(Ordering::Acquire) == generation {
                    account_usage::reconcile(&mut state.saved_accounts, list);
                    state.saved_accounts_error = None;
                }
            }
            notify_multi_update();
            for key in keys {
                if ACCOUNT_OPERATION.load(Ordering::Acquire)
                    || MULTI_GENERATION.load(Ordering::Acquire) != generation
                {
                    break;
                }
                if let Some(mut state) = STATE.get().and_then(|state| state.lock().ok()) {
                    if let Some(row) = state
                        .saved_accounts
                        .iter_mut()
                        .find(|row| row.account.key == key)
                    {
                        row.refreshing = true;
                    }
                }
                notify_multi_update();
                let result = manager.fetch_saved_usage(&key, &ACCOUNT_OPERATION);
                if MULTI_GENERATION.load(Ordering::Acquire) != generation {
                    break;
                }
                if let Some(mut state) = STATE.get().and_then(|state| state.lock().ok()) {
                    if let Some(row) = state
                        .saved_accounts
                        .iter_mut()
                        .find(|row| row.account.key == key)
                    {
                        row.refreshing = false;
                        match result {
                            Ok(result) => {
                                row.usage = Some(result.usage);
                                row.updated_at = Some(now_unix());
                                row.error = None;
                            }
                            Err(error) => {
                                row.error = Some(error);
                            }
                        }
                    }
                }
                notify_multi_update();
            }
            Ok(())
        };
        if let Err(error) = run() {
            if let Some(mut state) = STATE.get().and_then(|state| state.lock().ok()) {
                if MULTI_GENERATION.load(Ordering::Acquire) == generation {
                    state.saved_accounts_error = Some(error);
                }
            }
        }
        if let Some(mut state) = STATE.get().and_then(|state| state.lock().ok()) {
            for row in &mut state.saved_accounts {
                row.refreshing = false;
            }
        }
        MULTI_REFRESHING.store(false, Ordering::Release);
        notify_multi_update();
    });
}

enum AccountAction {
    Save,
    SignIn,
    Import(std::path::PathBuf),
    Export(std::path::PathBuf),
    Delete(String),
    Switch(String),
    Restore,
    RepairHistory,
}

fn begin_account_action(action: AccountAction) {
    if ACCOUNT_OPERATION.swap(true, Ordering::AcqRel) {
        return;
    }
    MULTI_GENERATION.fetch_add(1, Ordering::AcqRel);
    if let Ok(mut last) = MULTI_LAST_REFRESH.lock() {
        *last = None;
    }
    ACCOUNT_LOGIN_REQUIRED.store(false, Ordering::Release);
    let signing_in = matches!(&action, AccountAction::SignIn);
    if signing_in {
        ACCOUNT_LOGIN_CANCEL.store(false, Ordering::Release);
        ACCOUNT_LOGIN_ACTIVE.store(true, Ordering::Release);
    }
    std::thread::spawn(move || {
        let changes_auth = matches!(&action, AccountAction::Switch(_) | AccountAction::Restore);
        let result = (|| {
            let _gate = AUTH_GATE
                .lock()
                .map_err(|_| "Account operation lock is unavailable".to_owned())?;
            let manager = accounts::AccountManager::current()?;
            match action {
                AccountAction::Save => match manager.save_current()? {
                    accounts::SaveCurrent::Saved => Ok(("Account saved.".to_owned(), true)),
                    accounts::SaveCurrent::LoginRequired => {
                        ACCOUNT_LOGIN_REQUIRED.store(true, Ordering::Release);
                        Ok((String::new(), true))
                    }
                },
                AccountAction::SignIn => {
                    manager.sign_in_and_save(&ACCOUNT_LOGIN_CANCEL, |home| {
                        rpc::sign_in(
                            home,
                            |url| unsafe {
                                let opened = ShellExecuteW(
                                    0,
                                    wide("open").as_ptr(),
                                    wide(url).as_ptr(),
                                    null(),
                                    null(),
                                    SW_SHOW,
                                );
                                if opened > 32 {
                                    Ok(())
                                } else {
                                    Err("Could not open the browser for sign-in".into())
                                }
                            },
                            &ACCOUNT_LOGIN_CANCEL,
                        )
                    })?;
                    Ok((
                        "Account saved. Select it in Accounts to switch.".to_owned(),
                        false,
                    ))
                }
                AccountAction::Import(path) => {
                    let count = manager.import_file(&path)?;
                    Ok((format!("Added {count}. Current account unchanged."), false))
                }
                AccountAction::Export(path) => {
                    manager.export_json(&path)?;
                    Ok(("Accounts exported.".into(), false))
                }
                AccountAction::Delete(key) => {
                    manager.remove_saved(&key)?;
                    Ok(("Saved account removed.".into(), false))
                }
                AccountAction::Switch(key) => {
                    let list = manager.list()?;
                    if list.active_key.as_deref() == Some(&key) {
                        return Ok(("Already selected.".into(), false));
                    }
                    if !list.accounts.iter().any(|a| a.key == key) {
                        return Err("Saved account no longer exists".into());
                    }
                    manager.preflight_switch(&key)?;
                    let desktop = crate::desktop_handoff::Desktop::discover()?;
                    let changed = crate::desktop_handoff::sequence(
                        || {
                            desktop.close(|| unsafe {
                            let message=wide("Codex is still running. Force close may interrupt tasks and lose unsaved work.\n\nYes: Force close Codex Desktop\nNo: Wait another 15 seconds\nCancel: Stop switching");
                            match MessageBoxW(WINDOW.load(Ordering::Acquire),message.as_ptr(),wide("Codex is still running").as_ptr(),MB_YESNOCANCEL | MB_DEFBUTTON3 | MB_ICONWARNING) {
                                IDYES => crate::desktop_handoff::CloseChoice::Force,
                                IDNO => crate::desktop_handoff::CloseChoice::Wait,
                                _ => crate::desktop_handoff::CloseChoice::Cancel,
                            }
                        })
                        },
                        || manager.switch(&key),
                        || {
                            if manager.list()?.active_key.as_deref() == Some(&key) {
                                Ok(())
                            } else {
                                Err("Switched account could not be verified. Codex was not reopened.".into())
                            }
                        },
                        || desktop.open(),
                        |stage| {
                            if let Some(mut state) = STATE.get().and_then(|s| s.lock().ok()) {
                                state.account_feedback = Some(stage.into());
                            }
                            notify_multi_update();
                        },
                    )?;
                    Ok(("Codex reopened.".into(), changed))
                }
                AccountAction::Restore => {
                    manager.restore()?;
                    Ok(("Account restored. Reopen Codex.".to_owned(), true))
                }
                AccountAction::RepairHistory => {
                    let changed = manager.repair_local_history()?;
                    Ok((
                        if changed {
                            "Local project navigation repaired. Reopen Codex."
                        } else {
                            "Local project navigation is already consistent."
                        }
                        .to_owned(),
                        false,
                    ))
                }
            }
        })();
        let (message, changed, failed) = match result {
            Ok((message, changed)) => (message, changed, false),
            // A post-commit verification failure may have changed auth. Always
            // re-read after attempted writes instead of showing the old quota.
            Err(error) => (
                error,
                changes_auth,
                !(signing_in && ACCOUNT_LOGIN_CANCEL.load(Ordering::Acquire)),
            ),
        };
        ACCOUNT_LOGIN_ACTIVE.store(false, Ordering::Release);
        if let Ok(mut notice) = ACCOUNT_NOTICE.lock() {
            *notice = Some((message, failed));
        }
        let hwnd = WINDOW.load(Ordering::Acquire);
        if hwnd != 0 {
            unsafe {
                PostMessageW(hwnd, WM_ACCOUNT_DONE, usize::from(changed), 0);
            }
        } else {
            ACCOUNT_OPERATION.store(false, Ordering::Release);
        }
    });
}

unsafe fn confirm_switch(hwnd: HWND, target: &str) -> bool {
    let message = format!(
        "Switch to {target}?\n\nFully quit ChatGPT/Codex and close CLI/IDE sessions first.\nLocal project navigation will be backed up and kept across saved accounts. Cloud conversations remain account-specific.\nReopen Codex afterward."
    );
    MessageBoxW(
        hwnd,
        wide(&message).as_ptr(),
        wide("Switch account").as_ptr(),
        MB_YESNO | MB_DEFBUTTON2 | MB_ICONWARNING,
    ) == IDYES
}

unsafe fn confirm_restore(hwnd: HWND) -> bool {
    MessageBoxW(hwnd, wide("Restore the previous account?\n\nClose Codex and other switchers first.\nReopen Codex afterward.").as_ptr(),
        wide("Restore account").as_ptr(), MB_YESNO | MB_DEFBUTTON2 | MB_ICONWARNING) == IDYES
}

unsafe fn select_auth_file(hwnd: HWND) -> Option<std::path::PathBuf> {
    let mut buffer = [0u16; 32_768];
    let filter = wide("Auth JSON files\0*.json\0All files\0*.*\0");
    let title = wide("Import a Codex auth JSON file (credentials stay on this PC)");
    let mut dialog: OPENFILENAMEW = zeroed();
    dialog.lStructSize = size_of::<OPENFILENAMEW>() as u32;
    dialog.hwndOwner = hwnd;
    dialog.lpstrFile = buffer.as_mut_ptr();
    dialog.nMaxFile = buffer.len() as u32;
    dialog.lpstrFilter = filter.as_ptr();
    dialog.lpstrTitle = title.as_ptr();
    dialog.Flags = OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST | OFN_NOCHANGEDIR;
    if GetOpenFileNameW(&mut dialog) != 0 {
        use std::os::windows::ffi::OsStringExt;
        let length = buffer
            .iter()
            .position(|value| *value == 0)
            .unwrap_or(buffer.len());
        Some(std::ffi::OsString::from_wide(&buffer[..length]).into())
    } else {
        if CommDlgExtendedError() != 0 {
            MessageBoxW(
                hwnd,
                wide("Could not open the file picker. No account was imported.").as_ptr(),
                wide("HiCodex").as_ptr(),
                MB_ICONWARNING,
            );
        }
        None
    }
}

unsafe fn select_export_file(hwnd: HWND) -> Option<std::path::PathBuf> {
    let mut buffer = [0u16; 32768];
    let name = wide("hicodex-accounts.json");
    buffer[..name.len()].copy_from_slice(&name);
    let filter = wide("JSON files\0*.json\0\0");
    let title = wide("Export credentials — choose a new private file");
    let extension = wide("json");
    let mut dialog: OPENFILENAMEW = zeroed();
    dialog.lStructSize = size_of::<OPENFILENAMEW>() as u32;
    dialog.hwndOwner = hwnd;
    dialog.lpstrFile = buffer.as_mut_ptr();
    dialog.nMaxFile = buffer.len() as u32;
    dialog.lpstrFilter = filter.as_ptr();
    dialog.lpstrTitle = title.as_ptr();
    dialog.lpstrDefExt = extension.as_ptr();
    dialog.Flags = OFN_PATHMUSTEXIST | OFN_NOCHANGEDIR;
    if GetSaveFileNameW(&mut dialog) == 0 {
        return None;
    }
    use std::os::windows::ffi::OsStringExt;
    let length = buffer.iter().position(|v| *v == 0)?;
    Some(std::ffi::OsString::from_wide(&buffer[..length]).into())
}

unsafe fn append_account_menu(menu: HMENU) -> Vec<accounts::Account> {
    let submenu = CreatePopupMenu();
    if submenu == 0 {
        return Vec::new();
    }
    let busy = ACCOUNT_OPERATION.load(Ordering::Acquire);
    let flags = MF_STRING | if busy { MF_GRAYED } else { 0 };
    AppendMenuW(
        submenu,
        flags,
        MENU_SIGN_IN_ACCOUNT as usize,
        wide(crate::locale::text("Sign in...")).as_ptr(),
    );
    if ACCOUNT_LOGIN_ACTIVE.load(Ordering::Acquire) {
        AppendMenuW(
            submenu,
            MF_STRING,
            MENU_CANCEL_SIGN_IN as usize,
            wide(crate::locale::text("Cancel sign-in")).as_ptr(),
        );
    }
    AppendMenuW(
        submenu,
        flags,
        MENU_IMPORT_ACCOUNT as usize,
        wide(crate::locale::text("Import JSON...")).as_ptr(),
    );
    AppendMenuW(
        submenu,
        flags,
        MENU_EXPORT_ACCOUNT as usize,
        wide(crate::locale::text("Export JSON...")).as_ptr(),
    );
    AppendMenuW(
        menu,
        MF_POPUP,
        submenu as usize,
        wide(crate::locale::text("Accounts")).as_ptr(),
    );
    let result = accounts::AccountManager::current().and_then(|manager| manager.list());
    let accounts = match result {
        Ok(list) => {
            AppendMenuW(submenu, MF_SEPARATOR, 0, null());
            let delete_menu = CreatePopupMenu();
            if delete_menu != 0 {
                for (index, account) in list.accounts.iter().enumerate() {
                    AppendMenuW(
                        delete_menu,
                        flags,
                        MENU_DELETE_FIRST as usize + index,
                        wide(&account.label(ACCOUNT_VISIBLE.load(Ordering::Acquire), index))
                            .as_ptr(),
                    );
                }
                AppendMenuW(
                    submenu,
                    MF_POPUP
                        | if list.accounts.is_empty() {
                            MF_GRAYED
                        } else {
                            0
                        },
                    delete_menu as usize,
                    wide(crate::locale::text("Remove account")).as_ptr(),
                );
            }
            if list.accounts.is_empty() {
                AppendMenuW(
                    submenu,
                    MF_STRING | MF_GRAYED,
                    0,
                    wide(crate::locale::text("No saved accounts")).as_ptr(),
                );
            }
            list.accounts
        }
        Err(error) => {
            // Account storage failure must never change quota status.
            if let Ok(mut notice) = ACCOUNT_STORAGE_ERROR.lock() {
                *notice = Some(error);
            }
            AppendMenuW(
                submenu,
                MF_STRING,
                MENU_ACCOUNT_STORAGE_ERROR as usize,
                wide(crate::locale::text("Storage error...")).as_ptr(),
            );
            Vec::new()
        }
    };
    accounts
}

fn quota_value(quota: Option<&QuotaWindow>) -> String {
    quota
        .map(|quota| format!("{}%", quota.remaining_percent()))
        .unwrap_or_else(|| "--".to_owned())
}

fn quota_reset_compact(quota: Option<&QuotaWindow>) -> String {
    let Some(quota) = quota else {
        return String::new();
    };
    let seconds = (quota.resets_at - now_unix()).max(0);
    match seconds {
        0 => "now".to_owned(),
        1..=3_599 => format!("{}m", (seconds + 59) / 60),
        3_600..=172_799 => format!("{}h", (seconds + 3_599) / 3_600),
        _ => format!("{}d", (seconds + 86_399) / 86_400),
    }
}

fn color_for(quota: Option<&QuotaWindow>) -> u32 {
    match quota.map(QuotaWindow::remaining_percent) {
        Some(0..=20) => rgb(255, 115, 115),
        Some(21..=50) => rgb(242, 198, 109),
        Some(_) => rgb(120, 217, 139),
        None => rgb(146, 153, 165),
    }
}

fn system_usage_value(value: Option<u8>) -> String {
    value
        .map(|percent| format!("{percent}%"))
        .unwrap_or_else(|| "--".to_owned())
}

fn color_for_system_load(value: Option<u8>) -> u32 {
    match value {
        Some(0..=24) => rgb(70, 130, 220),
        Some(25..=49) => rgb(120, 217, 139),
        Some(50..=74) => rgb(242, 198, 109),
        Some(_) => rgb(255, 115, 115),
        None => rgb(146, 153, 165),
    }
}

unsafe fn refresh_system_usage(hwnd: HWND) {
    if !SYSTEM_USAGE_ENABLED.load(Ordering::Acquire) {
        return;
    }

    let sampler = SYSTEM_USAGE_SAMPLER
        .get_or_init(|| Mutex::new(system_usage::SystemUsageSampler::default()));
    let sample = sampler.lock().ok().map(|mut sampler| sampler.sample());
    if let (Some(sample), Some(state)) = (sample, STATE.get()) {
        if let Ok(mut state) = state.lock() {
            state.system_usage = sample;
        }
    }
    InvalidateRect(hwnd, null(), 0);
}

unsafe fn create_font_with_quality(
    dpi: u32,
    logical_pixels: i32,
    weight: u32,
    quality: u32,
    explicit_semibold: bool,
) -> HFONT {
    let height = -((logical_pixels * dpi as i32) / 96).max(logical_pixels);
    // Name the installed face explicitly to avoid inconsistent GDI weight matching.
    let face = wide(if explicit_semibold && weight == FW_SEMIBOLD {
        "Segoe UI Semibold"
    } else {
        "Segoe UI"
    });
    CreateFontW(
        height,
        0,
        0,
        0,
        weight as i32,
        0,
        0,
        0,
        DEFAULT_CHARSET.into(),
        OUT_DEFAULT_PRECIS.into(),
        CLIP_DEFAULT_PRECIS.into(),
        quality,
        (DEFAULT_PITCH | FF_DONTCARE).into(),
        face.as_ptr(),
    )
}

unsafe fn create_font(dpi: u32, logical_pixels: i32, weight: u32) -> HFONT {
    create_font_with_quality(dpi, logical_pixels, weight, CLEARTYPE_NATURAL_QUALITY, true)
}

unsafe fn create_widget_font(dpi: u32, logical_pixels: i32, weight: u32) -> HFONT {
    create_font_with_quality(
        dpi,
        logical_pixels,
        weight,
        ANTIALIASED_QUALITY.into(),
        false,
    )
}

unsafe fn paint_text(dc: HDC, font: HFONT, color: u32, text: &str, rect: &mut RECT, flags: u32) {
    let old_font = SelectObject(dc, font);
    SetTextColor(dc, color);
    let value = wide(text);
    DrawTextW(dc, value.as_ptr(), -1, rect, flags);
    SelectObject(dc, old_font);
}

unsafe fn text_width(dc: HDC, font: HFONT, text: &str) -> i32 {
    let old_font = SelectObject(dc, font);
    let value = wide(text);
    let mut size: SIZE = zeroed();
    let length = value.len().saturating_sub(1) as i32;
    let width = if GetTextExtentPoint32W(dc, value.as_ptr(), length, &mut size) != 0 {
        size.cx
    } else {
        0
    };
    SelectObject(dc, old_font);
    width
}

fn blend_pixel(output: &mut [u8], index: usize, color: u32, alpha: u8) {
    if alpha == 0 {
        return;
    }

    let source_alpha = alpha as u32;
    let inverse_alpha = 255 - source_alpha;
    let red = color & 0xff;
    let green = (color >> 8) & 0xff;
    let blue = (color >> 16) & 0xff;
    let channels = [blue, green, red];

    for (offset, channel) in channels.into_iter().enumerate() {
        let source = (channel * source_alpha + 127) / 255;
        let destination = output[index + offset] as u32;
        output[index + offset] =
            (source + (destination * inverse_alpha + 127) / 255).min(255) as u8;
    }

    let destination_alpha = output[index + 3] as u32;
    output[index + 3] =
        (source_alpha + (destination_alpha * inverse_alpha + 127) / 255).min(255) as u8;
}

struct WidgetLayer<'a> {
    mask_dc: HDC,
    mask_bits: *mut c_void,
    output: &'a mut [u8],
    width: i32,
    height: i32,
    shadow_offset: i32,
}

impl WidgetLayer<'_> {
    fn composite_mask(
        &mut self,
        mask: &[u8],
        offset_x: i32,
        offset_y: i32,
        color: u32,
        opacity: u8,
    ) {
        for source_y in 0..self.height {
            let destination_y = source_y + offset_y;
            if !(0..self.height).contains(&destination_y) {
                continue;
            }
            for source_x in 0..self.width {
                let destination_x = source_x + offset_x;
                if !(0..self.width).contains(&destination_x) {
                    continue;
                }

                let source_index = ((source_y * self.width + source_x) * 4) as usize;
                let coverage = mask[source_index]
                    .max(mask[source_index + 1])
                    .max(mask[source_index + 2]);
                if coverage == 0 {
                    continue;
                }
                let alpha = ((coverage as u16 * opacity as u16 + 127) / 255) as u8;
                let destination_index = ((destination_y * self.width + destination_x) * 4) as usize;
                blend_pixel(self.output, destination_index, color, alpha);
            }
        }
    }

    unsafe fn draw_text(
        &mut self,
        font: HFONT,
        color: u32,
        text: &str,
        mut rect: RECT,
        flags: u32,
    ) {
        let byte_count = (self.width * self.height * 4) as usize;
        std::ptr::write_bytes(self.mask_bits, 0, byte_count);

        let old_font = SelectObject(self.mask_dc, font);
        SetBkMode(self.mask_dc, TRANSPARENT as i32);
        SetTextColor(self.mask_dc, rgb(255, 255, 255));
        let value = wide(text);
        DrawTextW(self.mask_dc, value.as_ptr(), -1, &mut rect, flags);
        SelectObject(self.mask_dc, old_font);

        let mask = std::slice::from_raw_parts(self.mask_bits as *const u8, byte_count);
        self.composite_mask(
            mask,
            self.shadow_offset,
            self.shadow_offset,
            rgb(0, 0, 0),
            165,
        );
        self.composite_mask(mask, 0, 0, color, 255);
    }
}

unsafe fn create_argb_dib(dc: HDC, width: i32, height: i32, bits: &mut *mut c_void) -> HBITMAP {
    let mut info: BITMAPINFO = zeroed();
    info.bmiHeader = BITMAPINFOHEADER {
        biSize: size_of::<BITMAPINFOHEADER>() as u32,
        biWidth: width,
        biHeight: -height,
        biPlanes: 1,
        biBitCount: 32,
        biCompression: BI_RGB,
        biSizeImage: (width * height * 4) as u32,
        biXPelsPerMeter: 0,
        biYPelsPerMeter: 0,
        biClrUsed: 0,
        biClrImportant: 0,
    };
    CreateDIBSection(dc, &info, DIB_RGB_COLORS, bits, 0, 0)
}

unsafe fn render_widget_layered(hwnd: HWND) {
    let mut bounds: RECT = zeroed();
    if GetClientRect(hwnd, &mut bounds) == 0 {
        return;
    }
    let width = bounds.right - bounds.left;
    let height = bounds.bottom - bounds.top;
    if width <= 0 || height <= 0 {
        return;
    }

    let screen_dc = GetDC(0);
    let output_dc = CreateCompatibleDC(screen_dc);
    let mask_dc = CreateCompatibleDC(screen_dc);
    if screen_dc == 0 || output_dc == 0 || mask_dc == 0 {
        if output_dc != 0 {
            DeleteDC(output_dc);
        }
        if mask_dc != 0 {
            DeleteDC(mask_dc);
        }
        if screen_dc != 0 {
            ReleaseDC(0, screen_dc);
        }
        return;
    }

    let mut output_bits: *mut c_void = null_mut();
    let mut mask_bits: *mut c_void = null_mut();
    let output_bitmap = create_argb_dib(output_dc, width, height, &mut output_bits);
    let mask_bitmap = create_argb_dib(mask_dc, width, height, &mut mask_bits);
    if output_bitmap == 0 || mask_bitmap == 0 || output_bits.is_null() || mask_bits.is_null() {
        if output_bitmap != 0 {
            DeleteObject(output_bitmap);
        }
        if mask_bitmap != 0 {
            DeleteObject(mask_bitmap);
        }
        DeleteDC(output_dc);
        DeleteDC(mask_dc);
        ReleaseDC(0, screen_dc);
        return;
    }

    let old_output_bitmap = SelectObject(output_dc, output_bitmap);
    let old_mask_bitmap = SelectObject(mask_dc, mask_bitmap);
    let byte_count = (width * height * 4) as usize;
    std::ptr::write_bytes(output_bits, 0, byte_count);
    let output = std::slice::from_raw_parts_mut(output_bits as *mut u8, byte_count);
    for pixel in output.as_chunks_mut::<4>().0 {
        pixel[3] = 1;
    }

    let dpi = GetDpiForWindow(hwnd).max(96);
    let label_font = create_widget_font(dpi, 15, FW_SEMIBOLD);
    let value_font = create_widget_font(dpi, 16, FW_SEMIBOLD);
    let snapshot = STATE
        .get()
        .and_then(|state| state.lock().ok())
        .map(|state| state.clone())
        .unwrap_or_default();

    let label_color = rgb(232, 235, 241);
    let value_muted = rgb(196, 201, 210);
    let half = height / 2;
    let padding = ((5 * dpi as i32) / 96).max(4);
    let show_system_usage = SYSTEM_USAGE_ENABLED.load(Ordering::Acquire);
    let quota_left = if show_system_usage {
        ((92 * dpi as i32) / 96).max(90)
    } else {
        0
    };
    let quota_value_left = quota_left + ((54 * dpi as i32) / 96).max(51);
    let system_value_left = ((49 * dpi as i32) / 96).max(46);
    let system_right = ((90 * dpi as i32) / 96).max(86);
    let common_flags = DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX | DT_END_ELLIPSIS;
    let shadow_offset = ((dpi as i32) / 96).max(1);
    let mut layer = WidgetLayer {
        mask_dc,
        mask_bits,
        output,
        width,
        height,
        shadow_offset,
    };

    if show_system_usage {
        let cpu_label = RECT {
            left: padding,
            top: 0,
            right: system_value_left,
            bottom: half,
        };
        let ram_label = RECT {
            left: padding,
            top: half,
            right: system_value_left,
            bottom: height,
        };
        let cpu_value = RECT {
            left: system_value_left,
            top: 0,
            right: system_right,
            bottom: half,
        };
        let ram_value = RECT {
            left: system_value_left,
            top: half,
            right: system_right,
            bottom: height,
        };
        layer.draw_text(label_font, label_color, "CPU:", cpu_label, common_flags);
        layer.draw_text(label_font, label_color, "RAM:", ram_label, common_flags);
        layer.draw_text(
            value_font,
            color_for_system_load(snapshot.system_usage.cpu_percent),
            &system_usage_value(snapshot.system_usage.cpu_percent),
            cpu_value,
            common_flags,
        );
        layer.draw_text(
            value_font,
            color_for_system_load(snapshot.system_usage.memory_percent),
            &system_usage_value(snapshot.system_usage.memory_percent),
            ram_value,
            common_flags,
        );
    }

    let week_label = RECT {
        left: quota_left + padding,
        top: 0,
        right: quota_value_left,
        bottom: half,
    };
    let hour_label = RECT {
        left: quota_left + padding,
        top: half,
        right: quota_value_left,
        bottom: height,
    };
    layer.draw_text(label_font, label_color, "Week:", week_label, common_flags);
    layer.draw_text(label_font, label_color, "Hour:", hour_label, common_flags);

    let (week_text, week_reset, hour_text, hour_reset, week_color, hour_color) =
        match snapshot.status {
            Status::Loading => (
                "\u{2026}".to_owned(),
                String::new(),
                "\u{2026}".to_owned(),
                String::new(),
                value_muted,
                value_muted,
            ),
            Status::Error(_) => (
                "!".to_owned(),
                String::new(),
                "!".to_owned(),
                String::new(),
                rgb(255, 115, 115),
                rgb(255, 115, 115),
            ),
            Status::Ready => (
                quota_value(snapshot.usage.weekly.as_ref()),
                quota_reset_compact(snapshot.usage.weekly.as_ref()),
                quota_value(snapshot.usage.five_hour.as_ref()),
                quota_reset_compact(snapshot.usage.five_hour.as_ref()),
                color_for(snapshot.usage.weekly.as_ref()),
                color_for(snapshot.usage.five_hour.as_ref()),
            ),
        };

    let reset_gap = ((4 * dpi as i32) / 96).max(4);
    let quota_reset_left = quota_value_left
        + text_width(mask_dc, value_font, &week_text)
            .max(text_width(mask_dc, value_font, &hour_text))
        + reset_gap;

    let week_value = RECT {
        left: quota_value_left,
        top: 0,
        right: width - padding,
        bottom: half,
    };
    let week_reset_rect = RECT {
        left: quota_reset_left,
        top: 0,
        right: width - padding,
        bottom: half,
    };
    let hour_value = RECT {
        left: quota_value_left,
        top: half,
        right: width - padding,
        bottom: height,
    };
    let hour_reset_rect = RECT {
        left: quota_reset_left,
        top: half,
        right: width - padding,
        bottom: height,
    };
    layer.draw_text(value_font, week_color, &week_text, week_value, common_flags);
    layer.draw_text(
        value_font,
        value_muted,
        &week_reset,
        week_reset_rect,
        common_flags,
    );
    layer.draw_text(value_font, hour_color, &hour_text, hour_value, common_flags);
    layer.draw_text(
        value_font,
        value_muted,
        &hour_reset,
        hour_reset_rect,
        common_flags,
    );

    DeleteObject(label_font);
    DeleteObject(value_font);

    let source = POINT { x: 0, y: 0 };
    let size = SIZE {
        cx: width,
        cy: height,
    };
    let blend = BLENDFUNCTION {
        BlendOp: AC_SRC_OVER as u8,
        BlendFlags: 0,
        SourceConstantAlpha: 255,
        AlphaFormat: AC_SRC_ALPHA as u8,
    };
    let rendered = UpdateLayeredWindow(
        hwnd,
        screen_dc,
        null(),
        &size,
        output_dc,
        &source,
        0,
        &blend,
        ULW_ALPHA,
    );

    diagnostics::record(
        diagnostics::Event::Render,
        if rendered == 0 { GetLastError() } else { 0 },
    );

    SelectObject(output_dc, old_output_bitmap);
    SelectObject(mask_dc, old_mask_bitmap);
    DeleteObject(output_bitmap);
    DeleteObject(mask_bitmap);
    DeleteDC(output_dc);
    DeleteDC(mask_dc);
    ReleaseDC(0, screen_dc);
}

unsafe fn draw_widget(hwnd: HWND) {
    let mut paint: PAINTSTRUCT = zeroed();
    let dc = BeginPaint(hwnd, &mut paint);
    if dc != 0 {
        EndPaint(hwnd, &paint);
    }
    render_widget_layered(hwnd);
}

unsafe fn widget_width(tray: HWND, dpi: u32) -> i32 {
    if !SYSTEM_USAGE_ENABLED.load(Ordering::Acquire) {
        return ((122 * dpi as i32) / 96).max(116);
    }

    let snapshot = STATE
        .get()
        .and_then(|state| state.lock().ok())
        .map(|state| state.clone())
        .unwrap_or_default();
    let (week_text, week_reset, hour_text, hour_reset) = match snapshot.status {
        Status::Loading => (
            "\u{2026}".to_owned(),
            String::new(),
            "\u{2026}".to_owned(),
            String::new(),
        ),
        Status::Error(_) => ("!".to_owned(), String::new(), "!".to_owned(), String::new()),
        Status::Ready => (
            quota_value(snapshot.usage.weekly.as_ref()),
            quota_reset_compact(snapshot.usage.weekly.as_ref()),
            quota_value(snapshot.usage.five_hour.as_ref()),
            quota_reset_compact(snapshot.usage.five_hour.as_ref()),
        ),
    };

    let dc = GetDC(tray);
    if dc == 0 {
        return ((218 * dpi as i32) / 96).max(210);
    }
    let font = create_widget_font(dpi, 16, FW_SEMIBOLD);
    let value_width = text_width(dc, font, &week_text).max(text_width(dc, font, &hour_text));
    let reset_width = text_width(dc, font, &week_reset).max(text_width(dc, font, &hour_reset));
    DeleteObject(font);
    ReleaseDC(tray, dc);

    let padding = ((5 * dpi as i32) / 96).max(4);
    let quota_left = ((92 * dpi as i32) / 96).max(90);
    let quota_value_left = quota_left + ((54 * dpi as i32) / 96).max(51);
    let reset_gap = if reset_width > 0 {
        ((4 * dpi as i32) / 96).max(4)
    } else {
        0
    };
    (quota_value_left + value_width + reset_gap + reset_width + padding)
        .max(((176 * dpi as i32) / 96).max(170))
}

unsafe fn position_widget(hwnd: HWND) {
    let shell_class = wide("Shell_TrayWnd");
    let tray = FindWindowW(shell_class.as_ptr(), null());
    if tray == 0 {
        ShowWindow(hwnd, SW_HIDE);
        return;
    }

    // Never parent/own this window across processes: Windows would attach our
    // input queue to Explorer, defeating SendMessageTimeout's timeout.
    if IsWindowVisible(tray) == 0 {
        ShowWindow(hwnd, SW_HIDE);
        return;
    }

    let mut tray_rect: RECT = zeroed();
    if GetWindowRect(tray, &mut tray_rect) == 0 {
        return;
    }

    // A standalone overlay must follow auto-hide and fullscreen behavior itself.
    let monitor = MonitorFromWindow(tray, MONITOR_DEFAULTTONEAREST);
    let mut info: MONITORINFO = zeroed();
    info.cbSize = size_of::<MONITORINFO>() as u32;
    let foreground = GetForegroundWindow();
    let mut front_rect: RECT = zeroed();
    let mut class = [0u16; 64];
    let length = GetClassNameW(foreground, class.as_mut_ptr(), class.len() as i32).max(0) as usize;
    let desktop = matches!(
        String::from_utf16_lossy(&class[..length]).as_str(),
        "Progman" | "WorkerW"
    );
    let has_monitor = GetMonitorInfoW(monitor, &mut info) != 0;
    let covered = has_monitor
        && foreground != hwnd
        && foreground != tray
        && !desktop
        && GetWindowRect(foreground, &mut front_rect) != 0
        && widget_layout::covered_by_fullscreen(info.rcMonitor, front_rect);
    if covered || (has_monitor && widget_layout::offscreen(tray_rect, info.rcMonitor)) {
        ShowWindow(hwnd, SW_HIDE);
        return;
    }

    let dpi = GetDpiForWindow(tray).max(96);
    let width = widget_width(tray, dpi);
    let notify = FindWindowExW(tray, 0, wide("TrayNotifyWnd").as_ptr(), null());
    let mut notify_rect: RECT = zeroed();
    let notify_left =
        (notify != 0 && GetWindowRect(notify, &mut notify_rect) != 0).then_some(notify_rect.left);
    let Some(target) = widget_layout::position(tray_rect, notify_left, dpi, width) else {
        ShowWindow(hwnd, SW_HIDE);
        return;
    };
    let height = target.bottom - target.top;
    let mut current: RECT = zeroed();
    if GetWindowRect(hwnd, &mut current) == 0
        || current.left != target.left
        || current.top != target.top
        || current.right - current.left != width
        || current.bottom - current.top != height
        || IsWindowVisible(hwnd) == 0
        || GetWindowLongPtrW(hwnd, GWL_EXSTYLE) & WS_EX_TOPMOST as isize == 0
        || GetWindow(hwnd, GW_HWNDPREV) != 0
    {
        let positioned = SetWindowPos(
            hwnd,
            HWND_TOPMOST,
            target.left,
            target.top,
            width,
            height,
            SWP_NOACTIVATE | SWP_SHOWWINDOW,
        );
        diagnostics::record(
            diagnostics::Event::Position,
            if positioned == 0 { GetLastError() } else { 0 },
        );
    }

    let flyout = FLYOUT_WINDOW.load(Ordering::Acquire);
    if flyout != 0 && IsWindowVisible(flyout) != 0 {
        position_flyout(hwnd, flyout);
    }
}

unsafe fn update_taskbar_centering(hwnd: HWND) {
    if TASKBAR_CENTERED.load(Ordering::Acquire) {
        taskbar::watch_layout(hwnd, WM_TASKBAR_LAYOUT);
        taskbar::apply_centered();
    } else {
        KillTimer(hwnd, TIMER_LAYOUT_UPDATE);
        KillTimer(hwnd, TIMER_LAYOUT_SETTLE);
        taskbar::stop_layout_watch();
        taskbar::restore_left();
    }
}

unsafe fn refresh_taskbar_layout(hwnd: HWND, settle: bool) {
    KillTimer(hwnd, TIMER_LAYOUT_UPDATE);
    if !TASKBAR_CENTERED.load(Ordering::Acquire) {
        taskbar::acknowledge_layout_change();
        return;
    }
    let Ok(mut gate) = LAYOUT_GATE.lock() else {
        taskbar::acknowledge_layout_change();
        return;
    };
    let delay = gate.begin(Instant::now());
    drop(gate);
    if delay != 0 {
        // Keep the event watcher queued until this timer runs. A sustained
        // Explorer event stream must not refill the UI queue between passes.
        if SetTimer(hwnd, TIMER_LAYOUT_UPDATE, delay, None) == 0 {
            taskbar::acknowledge_layout_change();
        }
        return;
    }
    taskbar::acknowledge_layout_change();
    position_widget(hwnd);
    update_taskbar_centering(hwnd);
    if let Ok(mut gate) = LAYOUT_GATE.lock() {
        gate.finish(Instant::now());
    }
    if settle && TASKBAR_CENTERED.load(Ordering::Acquire) {
        // One trailing pass catches delayed MSAA button geometry. This is
        // event-driven; idle operation keeps only the two-second recovery pass.
        SetTimer(hwnd, TIMER_LAYOUT_SETTLE, 120, None);
    }
}

fn format_reset(timestamp: i64) -> String {
    if timestamp <= 0 {
        return "Reset unavailable".to_owned();
    }
    let seconds = timestamp - now_unix();
    if seconds <= 0 {
        "Resets now".to_owned()
    } else if seconds < 7_200 {
        format!("Resets in {} min", (seconds + 59) / 60)
    } else if seconds < 172_800 {
        format!("Resets in {} hr", (seconds + 3_599) / 3_600)
    } else {
        format!("Resets in {} days", (seconds + 86_399) / 86_400)
    }
}

fn format_updated(timestamp: Option<i64>) -> String {
    let Some(timestamp) = timestamp else {
        return "Updating…".to_owned();
    };
    let seconds = (now_unix() - timestamp).max(0);
    if seconds < 60 {
        "Updated just now".to_owned()
    } else if seconds < 3_600 {
        format!("Updated {}m ago", seconds / 60)
    } else {
        format!("Updated {}h ago", seconds / 3_600)
    }
}

fn plan_badge(state: &AppState) -> String {
    state
        .account
        .plan_type
        .as_deref()
        .map(|plan| {
            let lower = plan.to_lowercase();
            let mut chars = lower.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().chain(chars).collect(),
                None => "Codex".to_owned(),
            }
        })
        .unwrap_or_else(|| "Codex".to_owned())
}

unsafe fn rounded_bar(dc: HDC, rect: RECT, color: u32, radius: i32) {
    let brush = CreateSolidBrush(color);
    let pen = CreatePen(PS_SOLID, 1, color);
    let old_brush = SelectObject(dc, brush);
    let old_pen = SelectObject(dc, pen);
    RoundRect(
        dc,
        rect.left,
        rect.top,
        rect.right,
        rect.bottom,
        radius,
        radius,
    );
    SelectObject(dc, old_pen);
    SelectObject(dc, old_brush);
    DeleteObject(pen);
    DeleteObject(brush);
}

unsafe fn draw_progress(
    dc: HDC,
    left: i32,
    top: i32,
    right: i32,
    quota: Option<&QuotaWindow>,
    dpi: i32,
) {
    let height = (7 * dpi / 96).max(5);
    let radius = height;
    let track = RECT {
        left,
        top,
        right,
        bottom: top + height,
    };
    rounded_bar(dc, track, rgb(61, 67, 77), radius);

    if let Some(quota) = quota {
        let available = right - left;
        let fill_width = (available * quota.remaining_percent() as i32 / 100).max(0);
        if fill_width > 0 {
            let fill = RECT {
                left,
                top,
                right: left + fill_width,
                bottom: top + height,
            };
            rounded_bar(dc, fill, color_for(Some(quota)), radius);
        }
    }
}

fn multi_panel() -> bool {
    STATE
        .get()
        .and_then(|state| state.lock().ok())
        .is_some_and(|state| {
            !state.saved_accounts.is_empty() || state.saved_accounts_error.is_some()
        })
}

unsafe fn draw_flyout(hwnd: HWND) {
    if !multi_panel() {
        draw_legacy_flyout(hwnd);
        return;
    }
    let snapshot = STATE
        .get()
        .and_then(|state| state.lock().ok())
        .map(|state| state.clone())
        .unwrap_or_default();
    let mut paint: PAINTSTRUCT = zeroed();
    let dc = BeginPaint(hwnd, &mut paint);
    if dc == 0 {
        return;
    }
    let mut bounds: RECT = zeroed();
    GetClientRect(hwnd, &mut bounds);
    paint_account_panel(dc, bounds, &snapshot, GetDpiForWindow(hwnd).max(96));
    EndPaint(hwnd, &paint);
}

// The bundled toolbar SVGs use only absolute M/L/C commands, with spaced tokens.
// Read the same SVG paths used by the HTML preview, preserving DPI-scaled vectors.
unsafe fn paint_toolbar_svg(dc: HDC, svg: &str, x: i32, y: i32, dpi: u32, color: u32) {
    use windows_sys::Win32::Graphics::Gdi::{
        BeginPath, EndPath, LineTo, MoveToEx, PolyBezierTo, StrokePath,
    };
    let scale = dpi as f32 / 96.0;
    let pen = CreatePen(
        PS_SOLID,
        (1.8 * 18.0 / 24.0 * scale).round().max(1.0) as i32,
        color,
    );
    let old = SelectObject(dc, pen);
    for path in svg.split("d=\"").skip(1) {
        let mut tokens = path.split('"').next().unwrap_or("").split_whitespace();
        BeginPath(dc);
        while let Some(command) = tokens.next() {
            let count = if command == "C" { 3 } else { 1 };
            let mut points = [POINT { x: 0, y: 0 }; 3];
            for point in points.iter_mut().take(count) {
                let px = tokens
                    .next()
                    .and_then(|s| s.parse::<f32>().ok())
                    .unwrap_or(0.0);
                let py = tokens
                    .next()
                    .and_then(|s| s.parse::<f32>().ok())
                    .unwrap_or(0.0);
                point.x = ((x as f32 + px * 18.0 / 24.0) * scale).round() as i32;
                point.y = ((y as f32 + py * 18.0 / 24.0) * scale).round() as i32;
            }
            match command {
                "M" => {
                    MoveToEx(dc, points[0].x, points[0].y, null_mut());
                }
                "L" => {
                    LineTo(dc, points[0].x, points[0].y);
                }
                "C" => {
                    PolyBezierTo(dc, points.as_ptr(), 3);
                }
                _ => break,
            }
        }
        EndPath(dc);
        StrokePath(dc);
    }
    SelectObject(dc, old);
    DeleteObject(pen);
}

unsafe fn paint_account_panel(dc: HDC, bounds: RECT, snapshot: &AppState, dpi: u32) {
    let bg = CreateSolidBrush(rgb(32, 36, 44));
    FillRect(dc, &bounds, bg);
    DeleteObject(bg);
    SetBkMode(dc, TRANSPARENT as i32);
    let scale = |v: i32| v * dpi as i32 / 96;
    let width = bounds.right * 96 / dpi as i32;
    let height = bounds.bottom * 96 / dpi as i32;
    let title = create_font(dpi, 18, FW_SEMIBOLD);
    let body = create_font(dpi, 15, FW_SEMIBOLD);
    let small = create_font(dpi, 13, FW_SEMIBOLD);
    let number = create_font(dpi, 19, FW_SEMIBOLD);
    let fg = rgb(231, 234, 240);
    let muted = rgb(169, 179, 195);
    let text =
        |font: HFONT, color: u32, value: &str, x: i32, y: i32, w: i32, h: i32, right: bool| {
            let mut r = RECT {
                left: scale(x),
                top: scale(y),
                right: scale(x + w),
                bottom: scale(y + h),
            };
            paint_text(
                dc,
                font,
                color,
                value,
                &mut r,
                DT_VCENTER
                    | DT_SINGLELINE
                    | DT_NOPREFIX
                    | DT_END_ELLIPSIS
                    | if right { DT_RIGHT } else { DT_LEFT },
            );
        };
    text(title, fg, "HiCodex", 20, 12, 180, 28, false);
    text(
        small,
        muted,
        crate::locale::text("Accounts"),
        20,
        46,
        220,
        20,
        false,
    );
    text(
        small,
        muted,
        crate::locale::text("Week"),
        width - 176,
        46,
        70,
        20,
        true,
    );
    text(
        small,
        muted,
        crate::locale::text("5 hours"),
        width - 90,
        46,
        70,
        20,
        true,
    );
    let count = snapshot.saved_accounts.len();
    let visible_rows = PANEL_ROWS.load(Ordering::Acquire).min(count);
    let offset =
        panel_layout::scroll_offset(ACCOUNT_SCROLL.load(Ordering::Acquire), count, visible_rows);
    for (index, row) in snapshot
        .saved_accounts
        .iter()
        .enumerate()
        .skip(offset)
        .take(visible_rows)
    {
        let y = panel_layout::ROW_TOP + (index - offset) as i32 * panel_layout::ROW_HEIGHT;
        if row.active {
            rounded_bar(
                dc,
                RECT {
                    left: scale(12),
                    top: scale(y),
                    right: scale(width - 12),
                    bottom: scale(y + panel_layout::ROW_HEIGHT - 2),
                },
                rgb(43, 51, 61),
                scale(7),
            );
        }
        let label = if ACCOUNT_VISIBLE.load(Ordering::Acquire) {
            if row.account.alias.is_empty() {
                row.account.email.clone()
            } else {
                row.account.alias.clone()
            }
        } else {
            format!("Account {}", index + 1)
        };
        text(
            body,
            if row.active { rgb(164, 223, 187) } else { fg },
            &label,
            20,
            y + 6,
            width - 210,
            23,
            false,
        );
        let usage = if row.active {
            Some(&snapshot.usage)
        } else {
            row.usage.as_ref()
        };
        let failed =
            row.error.is_some() || (row.active && matches!(snapshot.status, Status::Error(_)));
        let status = if failed {
            crate::locale::text("Failed").to_owned()
        } else if row.refreshing {
            crate::locale::text("Refreshing...").to_owned()
        } else {
            let updated = if row.active {
                snapshot.updated_at
            } else {
                row.updated_at
            };
            match updated {
                Some(at) if now_unix().saturating_sub(at) < 60 => {
                    crate::locale::text("Just now").into()
                }
                Some(at) => format!("{}m ago", now_unix().saturating_sub(at) / 60),
                None => "-".into(),
            }
        };
        let credits = usage
            .and_then(|u| u.reset_credits)
            .map(|n| format!("Reset x{n}"))
            .unwrap_or_else(|| "Reset -".into());
        text(small, muted, &credits, 20, y + 32, 68, 20, false);
        text(small, muted, &status, 96, y + 32, width - 286, 20, false);
        for (x, window) in [
            (width - 176, usage.and_then(|u| u.weekly.as_ref())),
            (width - 90, usage.and_then(|u| u.five_hour.as_ref())),
        ] {
            let value = window
                .map(|q| format!("{}%", q.remaining_percent()))
                .unwrap_or_else(|| "-".into());
            text(
                number,
                if failed { muted } else { color_for(window) },
                &value,
                x,
                y + 5,
                70,
                25,
                true,
            );
            let reset = if window.is_some() {
                quota_reset_compact(window)
            } else {
                "-".into()
            };
            text(small, muted, &reset, x, y + 32, 70, 20, true);
        }
    }
    if count == 0 {
        text(
            small,
            muted,
            crate::locale::text("No saved accounts"),
            20,
            76,
            width - 40,
            22,
            false,
        );
    }
    let footer = height - 44;
    let line = CreateSolidBrush(rgb(53, 59, 69));
    FillRect(
        dc,
        &RECT {
            left: scale(20),
            top: scale(footer),
            right: scale(width - 20),
            bottom: scale(footer + 1),
        },
        line,
    );
    DeleteObject(line);
    let switching = SWITCH_MODE.load(Ordering::Acquire);
    let switch_color = if switching { rgb(164, 223, 187) } else { muted };
    paint_toolbar_svg(
        dc,
        include_str!("../assets/icons/toolbar-switch.svg"),
        20,
        footer + 13,
        dpi,
        switch_color,
    );
    text(
        body,
        switch_color,
        snapshot
            .account_feedback
            .as_deref()
            .unwrap_or_else(|| crate::locale::text(if switching { "Cancel" } else { "Switch" })),
        44,
        footer + 8,
        90,
        28,
        false,
    );
    let visible = ACCOUNT_VISIBLE.load(Ordering::Acquire);
    paint_toolbar_svg(
        dc,
        if visible {
            include_str!("../assets/icons/toolbar-visibility.svg")
        } else {
            include_str!("../assets/icons/toolbar-visibility-off.svg")
        },
        146,
        footer + 13,
        dpi,
        muted,
    );
    text(
        body,
        muted,
        crate::locale::text(if visible { "Hide" } else { "Show" }),
        170,
        footer + 8,
        62,
        28,
        false,
    );
    let refresh_label = crate::locale::text(if MULTI_REFRESHING.load(Ordering::Acquire) {
        "Refreshing..."
    } else {
        "Refresh"
    });
    let refresh_width = (text_width(dc, body, refresh_label) * 96 + dpi as i32 - 1) / dpi as i32;
    paint_toolbar_svg(
        dc,
        include_str!("../assets/icons/toolbar-refresh.svg"),
        width - 20 - refresh_width - 24,
        footer + 13,
        dpi,
        muted,
    );
    text(
        body,
        muted,
        refresh_label,
        width - 20 - refresh_width,
        footer + 8,
        refresh_width,
        28,
        true,
    );

    for font in [title, body, small, number] {
        DeleteObject(font);
    }
}

unsafe fn draw_legacy_flyout(hwnd: HWND) {
    let mut paint: PAINTSTRUCT = zeroed();
    let dc = BeginPaint(hwnd, &mut paint);
    if dc == 0 {
        return;
    }

    let mut bounds: RECT = zeroed();
    GetClientRect(hwnd, &mut bounds);
    let glass = FLYOUT_GLASS.load(Ordering::Acquire);
    let background = if glass { rgb(0, 0, 0) } else { rgb(28, 32, 39) };
    let background_brush = CreateSolidBrush(background);
    FillRect(dc, &bounds, background_brush);
    DeleteObject(background_brush);
    SetBkMode(dc, TRANSPARENT as i32);

    let dpi = GetDpiForWindow(hwnd).max(96);
    let scale = |value: i32| (value * dpi as i32 / 96).max(1);
    let title_font = create_font(dpi, 16, FW_SEMIBOLD);
    let section_font = create_font(dpi, 14, FW_NORMAL);
    let value_font = create_font(dpi, 16, FW_SEMIBOLD);
    let secondary_font = create_font(dpi, 14, FW_NORMAL);
    let reset_font = create_font(dpi, 14, FW_NORMAL);
    let snapshot = STATE
        .get()
        .and_then(|state| state.lock().ok())
        .map(|state| state.clone())
        .unwrap_or_default();

    let left = scale(18);
    let right = bounds.right - scale(18);
    let title_color = rgb(245, 247, 250);
    let muted = rgb(154, 163, 176);
    let section = rgb(174, 183, 196);
    let body = rgb(220, 225, 232);
    let common = DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX | DT_END_ELLIPSIS;

    let mut title_rect = RECT {
        left,
        top: scale(11),
        right,
        bottom: scale(37),
    };
    paint_text(
        dc,
        title_font,
        title_color,
        "HiCodex",
        &mut title_rect,
        common,
    );

    let updated = match &snapshot.status {
        Status::Error(_) => "Update failed".to_owned(),
        _ => format_updated(snapshot.updated_at),
    };
    let mut updated_rect = RECT {
        left: scale(130),
        top: scale(12),
        right,
        bottom: scale(36),
    };
    paint_text(
        dc,
        secondary_font,
        muted,
        &updated,
        &mut updated_rect,
        DT_RIGHT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX | DT_END_ELLIPSIS,
    );

    let mut week_section = RECT {
        left,
        top: scale(42),
        right,
        bottom: scale(62),
    };
    paint_text(dc, section_font, section, "Week", &mut week_section, common);
    let week_value = snapshot
        .usage
        .weekly
        .as_ref()
        .map(|quota| format!("{}%", quota.remaining_percent()))
        .unwrap_or_else(|| "--".to_owned());
    let mut week_value_rect = RECT {
        left,
        top: scale(60),
        right: scale(175),
        bottom: scale(84),
    };
    paint_text(
        dc,
        value_font,
        color_for(snapshot.usage.weekly.as_ref()),
        &week_value,
        &mut week_value_rect,
        common,
    );
    let week_reset = snapshot
        .usage
        .weekly
        .as_ref()
        .map(|quota| format_reset(quota.resets_at))
        .unwrap_or_else(|| "Not available".to_owned());
    let mut week_reset_rect = RECT {
        left: scale(150),
        top: scale(61),
        right,
        bottom: scale(88),
    };
    paint_text(
        dc,
        reset_font,
        muted,
        &week_reset,
        &mut week_reset_rect,
        DT_RIGHT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX | DT_END_ELLIPSIS,
    );
    draw_progress(
        dc,
        left,
        scale(88),
        right,
        snapshot.usage.weekly.as_ref(),
        dpi as i32,
    );

    let mut hour_section = RECT {
        left,
        top: scale(104),
        right,
        bottom: scale(124),
    };
    paint_text(
        dc,
        section_font,
        section,
        "5-Hour",
        &mut hour_section,
        common,
    );
    let hour_value = snapshot
        .usage
        .five_hour
        .as_ref()
        .map(|quota| format!("{}%", quota.remaining_percent()))
        .unwrap_or_else(|| "--".to_owned());
    let mut hour_value_rect = RECT {
        left,
        top: scale(122),
        right: scale(175),
        bottom: scale(146),
    };
    paint_text(
        dc,
        value_font,
        color_for(snapshot.usage.five_hour.as_ref()),
        &hour_value,
        &mut hour_value_rect,
        common,
    );
    let hour_reset = snapshot
        .usage
        .five_hour
        .as_ref()
        .map(|quota| format_reset(quota.resets_at))
        .unwrap_or_else(|| "Not available".to_owned());
    let mut hour_reset_rect = RECT {
        left: scale(150),
        top: scale(123),
        right,
        bottom: scale(145),
    };
    paint_text(
        dc,
        reset_font,
        muted,
        &hour_reset,
        &mut hour_reset_rect,
        DT_RIGHT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX | DT_END_ELLIPSIS,
    );
    draw_progress(
        dc,
        left,
        scale(150),
        right,
        snapshot.usage.five_hour.as_ref(),
        dpi as i32,
    );

    let (footer, reset_badge) = if let Some(message) = &snapshot.account_feedback {
        (message.clone(), String::new())
    } else {
        match &snapshot.status {
            Status::Error(error) => (error.clone(), String::new()),
            _ => (
                plan_badge(&snapshot),
                snapshot
                    .usage
                    .reset_credits
                    .map(|count| format!("Reset x{count}"))
                    .unwrap_or_default(),
            ),
        }
    };
    let mut footer_rect = RECT {
        left,
        top: scale(162),
        right: if snapshot.account_feedback.is_some() {
            right
        } else {
            scale(150)
        },
        bottom: scale(181),
    };
    paint_text(dc, secondary_font, body, &footer, &mut footer_rect, common);
    let mut reset_badge_rect = RECT {
        left: scale(150),
        top: scale(162),
        right,
        bottom: scale(181),
    };
    paint_text(
        dc,
        secondary_font,
        body,
        &reset_badge,
        &mut reset_badge_rect,
        DT_RIGHT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX | DT_END_ELLIPSIS,
    );

    let account_label = if matches!(snapshot.status, Status::Error(_)) {
        "Last account"
    } else {
        "Account"
    };
    let visible = ACCOUNT_VISIBLE.load(Ordering::Acquire);
    let account_text = format!(
        "{account_label}: {}",
        account_display(&snapshot.account, visible)
    );
    let mut account_rect = RECT {
        left,
        top: scale(184),
        right: right - scale(32),
        bottom: scale(205),
    };
    paint_text(
        dc,
        secondary_font,
        muted,
        &account_text,
        &mut account_rect,
        common,
    );

    let button = account_button_rect(hwnd);
    let pen = CreatePen(PS_SOLID, scale(1), muted);
    let previous_pen = SelectObject(dc, pen);
    let previous_brush = SelectObject(dc, GetStockObject(NULL_BRUSH));
    // Keep these paths in sync with assets/icons/eye-{open,closed}.svg.
    let point = |x, y| POINT {
        x: button.left + scale(x),
        y: button.top + scale(y),
    };
    if visible {
        let outline = [
            point(4, 12),
            point(9, 5),
            point(17, 5),
            point(22, 12),
            point(17, 19),
            point(9, 19),
            point(4, 12),
        ];
        windows_sys::Win32::Graphics::Gdi::PolyBezier(dc, outline.as_ptr(), outline.len() as u32);
        windows_sys::Win32::Graphics::Gdi::Ellipse(
            dc,
            button.left + scale(10),
            button.top + scale(9),
            button.left + scale(16),
            button.top + scale(15),
        );
    } else {
        let eyelid = [point(4, 10), point(9, 17), point(17, 17), point(22, 10)];
        windows_sys::Win32::Graphics::Gdi::PolyBezier(dc, eyelid.as_ptr(), eyelid.len() as u32);
        for (start, end) in [
            (point(7, 13), point(5, 16)),
            (point(13, 15), point(13, 19)),
            (point(19, 13), point(21, 16)),
        ] {
            windows_sys::Win32::Graphics::Gdi::MoveToEx(dc, start.x, start.y, null_mut());
            windows_sys::Win32::Graphics::Gdi::LineTo(dc, end.x, end.y);
        }
    }
    SelectObject(dc, previous_brush);
    SelectObject(dc, previous_pen);
    DeleteObject(pen);

    let border_pen = CreatePen(PS_SOLID, 1, rgb(73, 79, 89));
    let old_pen = SelectObject(dc, border_pen);
    let old_brush = SelectObject(dc, GetStockObject(NULL_BRUSH));
    RoundRect(
        dc,
        1,
        1,
        bounds.right - 2,
        bounds.bottom - 2,
        scale(24),
        scale(24),
    );
    SelectObject(dc, old_brush);
    SelectObject(dc, old_pen);
    DeleteObject(border_pen);

    DeleteObject(title_font);
    DeleteObject(section_font);
    DeleteObject(value_font);
    DeleteObject(secondary_font);
    DeleteObject(reset_font);
    EndPaint(hwnd, &paint);
}

unsafe fn apply_flyout_glass(hwnd: HWND) -> bool {
    let dark_mode: i32 = 1;
    DwmSetWindowAttribute(
        hwnd,
        DWMWA_USE_IMMERSIVE_DARK_MODE as u32,
        &dark_mode as *const _ as *const c_void,
        size_of::<i32>() as u32,
    );

    let corner = DWMWCP_ROUND;
    DwmSetWindowAttribute(
        hwnd,
        DWMWA_WINDOW_CORNER_PREFERENCE as u32,
        &corner as *const _ as *const c_void,
        size_of_val(&corner) as u32,
    );

    SetWindowLongPtrW(
        hwnd,
        GWL_EXSTYLE,
        (WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE) as isize,
    );
    SetLayeredWindowAttributes(hwnd, 0, 252, LWA_ALPHA);
    false
}

unsafe fn apply_flyout_region(hwnd: HWND, width: i32, height: i32, dpi: i32) {
    let corner_diameter = (24 * dpi / 96).max(16);
    let region = CreateRoundRectRgn(
        0,
        0,
        width + 1,
        height + 1,
        corner_diameter,
        corner_diameter,
    );
    if region != 0 && SetWindowRgn(hwnd, region, 1) == 0 {
        DeleteObject(region);
    }
}
unsafe fn position_flyout(widget: HWND, flyout: HWND) {
    let mut widget_rect: RECT = zeroed();
    if GetWindowRect(widget, &mut widget_rect) == 0 {
        return;
    }

    let dpi = GetDpiForWindow(widget).max(96) as i32;
    let multi = multi_panel();
    let mut width = if multi { panel_layout::WIDTH } else { 320 } * dpi / 96;
    if !multi
        && (ACCOUNT_VISIBLE.load(Ordering::Acquire)
            || STATE
                .get()
                .and_then(|state| state.lock().ok())
                .is_some_and(|state| state.account_feedback.is_some()))
    {
        if let Some(state) = STATE.get().and_then(|state| state.lock().ok()) {
            let account_text = format!(
                "Last account: {}",
                account_display(&state.account, ACCOUNT_VISIBLE.load(Ordering::Acquire))
            );
            let text = wide(state.account_feedback.as_deref().unwrap_or(&account_text));
            let dc = GetDC(widget);
            let font = create_font(dpi as u32, 14, FW_NORMAL);
            let old_font = SelectObject(dc, font);
            let mut size: SIZE = zeroed();
            if GetTextExtentPoint32W(dc, text.as_ptr(), (text.len() - 1) as i32, &mut size) != 0 {
                width = width.max(size.cx + 76 * dpi / 96);
            }
            SelectObject(dc, old_font);
            DeleteObject(font);
            ReleaseDC(widget, dc);
        }
    }
    let mut height = 214 * dpi / 96;
    let gap = (9 * dpi / 96).max(6);

    let monitor = MonitorFromWindow(widget, MONITOR_DEFAULTTONEAREST);
    let mut monitor_info: MONITORINFO = zeroed();
    monitor_info.cbSize = size_of::<MONITORINFO>() as u32;
    if GetMonitorInfoW(monitor, &mut monitor_info) == 0 {
        return;
    }

    let work = monitor_info.rcWork;
    if multi {
        let count = STATE
            .get()
            .and_then(|state| state.lock().ok())
            .map_or(0, |state| state.saved_accounts.len());
        let visible = panel_layout::visible_rows(count, (work.bottom - work.top) * 96 / dpi - 18);
        PANEL_ROWS.store(visible, Ordering::Release);
        height = panel_layout::height(visible) * dpi / 96;
    }
    height = height.min(work.bottom - work.top);
    width = width.min(work.right - work.left);
    let max_x = (work.right - width).max(work.left);
    let x = (widget_rect.right - width).clamp(work.left, max_x);
    let above = widget_rect.top - height - gap;
    let y = if above >= work.top {
        above
    } else {
        (widget_rect.bottom + gap).min(work.bottom - height)
    };

    SetWindowPos(
        flyout,
        HWND_TOPMOST,
        x,
        y,
        width,
        height,
        SWP_NOACTIVATE | SWP_SHOWWINDOW,
    );
    apply_flyout_region(flyout, width, height, dpi);
}

unsafe fn show_flyout(widget: HWND) {
    let flyout = FLYOUT_WINDOW.load(Ordering::Acquire);
    if flyout == 0 {
        return;
    }
    trigger_saved_refresh(false);
    KillTimer(widget, TIMER_HIDE_FLYOUT);
    position_flyout(widget, flyout);
    InvalidateRect(flyout, null(), 1);
    ShowWindow(flyout, SW_SHOWNOACTIVATE);
}

unsafe fn schedule_hide_flyout() {
    let widget = WINDOW.load(Ordering::Acquire);
    if widget != 0 {
        SetTimer(widget, TIMER_HIDE_FLYOUT, HIDE_DELAY_MS, None);
    }
}

unsafe fn cancel_hide_flyout() {
    let widget = WINDOW.load(Ordering::Acquire);
    if widget != 0 {
        KillTimer(widget, TIMER_HIDE_FLYOUT);
    }
}

unsafe fn point_inside_window(point: POINT, hwnd: HWND) -> bool {
    if hwnd == 0 || IsWindowVisible(hwnd) == 0 {
        return false;
    }
    let mut rect: RECT = zeroed();
    GetWindowRect(hwnd, &mut rect) != 0
        && point.x >= rect.left
        && point.x < rect.right
        && point.y >= rect.top
        && point.y < rect.bottom
}

unsafe fn hide_flyout_if_outside() {
    let widget = WINDOW.load(Ordering::Acquire);
    if widget != 0 {
        KillTimer(widget, TIMER_HIDE_FLYOUT);
    }
    // Keep successful operation feedback readable without a confirmation click.
    if STATE
        .get()
        .and_then(|state| state.lock().ok())
        .is_some_and(|state| state.account_feedback.is_some())
    {
        return;
    }

    let flyout = FLYOUT_WINDOW.load(Ordering::Acquire);
    let mut cursor: POINT = zeroed();
    if GetCursorPos(&mut cursor) == 0 {
        SWITCH_MODE.store(false, Ordering::Release);
        ShowWindow(flyout, SW_HIDE);
        return;
    }

    if !point_inside_window(cursor, widget) && !point_inside_window(cursor, flyout) {
        SWITCH_MODE.store(false, Ordering::Release);
        ShowWindow(flyout, SW_HIDE);
    }
}

unsafe fn poll_hover(widget: HWND) {
    let mut cursor: POINT = zeroed();
    if GetCursorPos(&mut cursor) == 0 {
        return;
    }

    let flyout = FLYOUT_WINDOW.load(Ordering::Acquire);
    let inside_widget = point_inside_window(cursor, widget);
    let inside_flyout = point_inside_window(cursor, flyout);

    if !inside_widget {
        HOVER_SUPPRESSED.store(false, Ordering::Release);
    }

    if inside_widget && !HOVER_SUPPRESSED.load(Ordering::Acquire) {
        OUTSIDE_TICKS.store(0, Ordering::Relaxed);
        let hover_ticks = HOVER_TICKS.fetch_add(1, Ordering::Relaxed) + 1;
        if hover_ticks >= HOVER_SHOW_TICKS && (flyout == 0 || IsWindowVisible(flyout) == 0) {
            show_flyout(widget);
        }
    } else if inside_flyout {
        HOVER_TICKS.store(0, Ordering::Relaxed);
        OUTSIDE_TICKS.store(0, Ordering::Relaxed);
        cancel_hide_flyout();
    } else {
        HOVER_TICKS.store(0, Ordering::Relaxed);
        let outside_ticks = OUTSIDE_TICKS.fetch_add(1, Ordering::Relaxed) + 1;
        if outside_ticks >= HOVER_HIDE_TICKS && flyout != 0 && IsWindowVisible(flyout) != 0 {
            hide_flyout_if_outside();
        }
    }
}
unsafe fn track_mouse(hwnd: HWND, with_hover: bool) {
    let mut tracking = TRACKMOUSEEVENT {
        cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
        dwFlags: TME_LEAVE | if with_hover { TME_HOVER } else { 0 },
        hwndTrack: hwnd,
        dwHoverTime: if with_hover { HOVER_DELAY_MS } else { 0 },
    };
    TrackMouseEvent(&mut tracking);
}

unsafe fn open_usage_dashboard(hwnd: HWND) {
    let verb = wide("open");
    let url = wide("https://chatgpt.com/codex/settings/usage");
    ShellExecuteW(hwnd, verb.as_ptr(), url.as_ptr(), null(), null(), SW_SHOW);
}

unsafe fn show_about(hwnd: HWND) {
    let project = env!("CARGO_PKG_REPOSITORY");
    let message = format!(
        "HiCodex v{}\n\nA lightweight taskbar meter for Codex limits.\n\nGitHub: {}\n\nOpen the project page?",
        env!("CARGO_PKG_VERSION"),
        project,
    );
    if MessageBoxW(
        hwnd,
        wide(&message).as_ptr(),
        wide(crate::locale::text("About HiCodex")).as_ptr(),
        MB_YESNO | MB_ICONINFORMATION | MB_DEFBUTTON2,
    ) == IDYES
    {
        ShellExecuteW(
            hwnd,
            wide("open").as_ptr(),
            wide(project).as_ptr(),
            null(),
            null(),
            SW_SHOW,
        );
    }
}

unsafe fn show_context_menu(hwnd: HWND) {
    let flyout = FLYOUT_WINDOW.load(Ordering::Acquire);
    if flyout != 0 {
        SWITCH_MODE.store(false, Ordering::Release);
        ShowWindow(flyout, SW_HIDE);
    }

    let menu = CreatePopupMenu();
    if menu == 0 {
        return;
    }

    let refresh = wide(crate::locale::text("Refresh all"));
    let open_usage = wide(crate::locale::text("Usage dashboard"));
    let startup_label = wide(crate::locale::text("Start with Windows"));
    let system_usage_label = wide(crate::locale::text("Show CPU and RAM"));
    let center_taskbar_label = wide(crate::locale::text("Center taskbar icons"));

    let about_label = wide(crate::locale::text("About HiCodex"));
    let exit = wide(crate::locale::text("Exit HiCodex"));

    AppendMenuW(menu, MF_STRING, MENU_REFRESH as usize, refresh.as_ptr());
    AppendMenuW(
        menu,
        MF_STRING,
        MENU_OPEN_USAGE as usize,
        open_usage.as_ptr(),
    );
    let account_choices = append_account_menu(menu);
    AppendMenuW(menu, MF_SEPARATOR, 0, null());
    let settings = CreatePopupMenu();
    if settings != 0 {
        AppendMenuW(
            settings,
            MF_STRING | if startup::is_enabled() { MF_CHECKED } else { 0 },
            MENU_STARTUP as usize,
            startup_label.as_ptr(),
        );
        AppendMenuW(
            settings,
            MF_STRING
                | if SYSTEM_USAGE_ENABLED.load(Ordering::Acquire) {
                    MF_CHECKED
                } else {
                    0
                },
            MENU_SYSTEM_USAGE as usize,
            system_usage_label.as_ptr(),
        );
        AppendMenuW(
            settings,
            MF_STRING
                | if TASKBAR_CENTERED.load(Ordering::Acquire) {
                    MF_CHECKED
                } else {
                    0
                },
            MENU_CENTER_TASKBAR as usize,
            center_taskbar_label.as_ptr(),
        );
        let languages = CreatePopupMenu();
        if languages != 0 {
            for (id, label, checked) in [
                (MENU_LANGUAGE_EN, "English", !crate::locale::is_chinese()),
                (MENU_LANGUAGE_ZH, "简体中文", crate::locale::is_chinese()),
            ] {
                AppendMenuW(
                    languages,
                    MF_STRING | if checked { MF_CHECKED } else { 0 },
                    id as usize,
                    wide(label).as_ptr(),
                );
            }
            AppendMenuW(settings, MF_SEPARATOR, 0, null());
            AppendMenuW(
                settings,
                MF_POPUP,
                languages as usize,
                wide(crate::locale::text("Language")).as_ptr(),
            );
        }
        AppendMenuW(
            menu,
            MF_POPUP,
            settings as usize,
            wide(crate::locale::text("Preferences")).as_ptr(),
        );
    }
    AppendMenuW(menu, MF_SEPARATOR, 0, null());
    AppendMenuW(menu, MF_STRING, MENU_ABOUT as usize, about_label.as_ptr());
    AppendMenuW(
        menu,
        MF_STRING
            | if ACCOUNT_OPERATION.load(Ordering::Acquire) {
                MF_GRAYED
            } else {
                0
            },
        MENU_EXIT as usize,
        exit.as_ptr(),
    );

    let mut cursor: POINT = zeroed();
    GetCursorPos(&mut cursor);
    SetForegroundWindow(hwnd);
    let command = TrackPopupMenu(
        menu,
        TPM_RIGHTBUTTON | TPM_RETURNCMD,
        cursor.x,
        cursor.y,
        0,
        hwnd,
        null(),
    );
    DestroyMenu(menu);

    match command as i32 {
        MENU_LANGUAGE_EN | MENU_LANGUAGE_ZH => {
            if let Err(error) = crate::locale::set(command as i32 == MENU_LANGUAGE_ZH) {
                MessageBoxW(hwnd, wide(&error).as_ptr(), wide("HiCodex").as_ptr(), MB_ICONWARNING);
            }
            InvalidateRect(hwnd, null(), 1);
            let flyout = FLYOUT_WINDOW.load(Ordering::Acquire);
            if flyout != 0 { InvalidateRect(flyout, null(), 1); }
        }

        MENU_ACCOUNT_STORAGE_ERROR => {
            let error=ACCOUNT_STORAGE_ERROR.lock().ok().and_then(|error|error.clone()).unwrap_or_else(||"Account storage could not be read".into());
            let home=accounts::codex_home().map(|home|home.display().to_string()).unwrap_or_else(|_|"Unknown CODEX_HOME".into());
            MessageBoxW(hwnd,wide(&format!("{error}\n\nAccount directory: {home}\\hi-codex\n\nNo accounts were removed. If this instance was launched from a restricted development environment, quit it and start HiCodex normally from File Explorer. Do not recreate the account store or change its permissions.")).as_ptr(),wide("Account storage unavailable").as_ptr(),MB_ICONWARNING);
        }
        MENU_REPAIR_HISTORY => {
            if MessageBoxW(hwnd, wide("Recover local project navigation from saved desktop account views?\n\nFully quit ChatGPT/Codex and close CLI/IDE sessions first. An encrypted checkpoint will be saved. Cloud conversations and conversation ownership will not be changed.\nReopen Codex afterward.").as_ptr(), wide("Repair local project history").as_ptr(), MB_YESNO | MB_DEFBUTTON2 | MB_ICONWARNING) == IDYES {
                begin_account_action(AccountAction::RepairHistory);
            }
        }
        MENU_SAVE_ACCOUNT => begin_account_action(AccountAction::Save),
        MENU_SIGN_IN_ACCOUNT => begin_account_action(AccountAction::SignIn),
        MENU_CANCEL_SIGN_IN => ACCOUNT_LOGIN_CANCEL.store(true, Ordering::Release),
        MENU_EXPORT_ACCOUNT => {
            if MessageBoxW(hwnd, wide("Export saved accounts? The JSON contains login credentials. Keep it private.").as_ptr(), wide("Export JSON").as_ptr(), MB_YESNO | MB_DEFBUTTON2 | MB_ICONWARNING) == IDYES {
                if let Some(path) = select_export_file(hwnd) { begin_account_action(AccountAction::Export(path)); }
            }
        }
        id if id >= MENU_DELETE_FIRST && (id - MENU_DELETE_FIRST) < account_choices.len() as i32 => {
            let index = (id - MENU_DELETE_FIRST) as usize;
            let account = &account_choices[index];
            let label = account.label(ACCOUNT_VISIBLE.load(Ordering::Acquire), index).replace("&&", "&");
            if MessageBoxW(hwnd, wide(&format!("Remove {label} from saved accounts? Local chats and the current login will be kept.")).as_ptr(), wide(crate::locale::text("Remove account")).as_ptr(), MB_YESNO | MB_DEFBUTTON2 | MB_ICONWARNING) == IDYES {
                begin_account_action(AccountAction::Delete(account.key.clone()));
            }
        }
        MENU_IMPORT_ACCOUNT => {
            if let Some(path) = select_auth_file(hwnd) {
                begin_account_action(AccountAction::Import(path));
            }
        }
        MENU_RESTORE_ACCOUNT => {
            if confirm_restore(hwnd) {
                begin_account_action(AccountAction::Restore);
            }
        }
        id if id >= MENU_ACCOUNT_FIRST
            && (id - MENU_ACCOUNT_FIRST) < account_choices.len() as i32 =>
        {
            let index = (id - MENU_ACCOUNT_FIRST) as usize;
            let account = &account_choices[index];
            let target = account
                .label(ACCOUNT_VISIBLE.load(Ordering::Acquire), index)
                .replace("&&", "&");
            if confirm_switch(hwnd, &target) {
                begin_account_action(AccountAction::Switch(account.key.clone()));
            }
        }
        MENU_REFRESH => { trigger_refresh(); trigger_saved_refresh(true); },
        MENU_OPEN_USAGE => open_usage_dashboard(hwnd),
        MENU_ABOUT => show_about(hwnd),
        MENU_STARTUP => {
            let _ = startup::set_enabled(!startup::is_enabled());
        }
        MENU_CENTER_TASKBAR => {
            let enabled = !TASKBAR_CENTERED.load(Ordering::Acquire);
            if enabled && !taskbar::native_centering_available() {
                MessageBoxW(
                    hwnd,
                    wide("Could not access the native Windows taskbar layout. Centering remains off. Run HiCodex in the same desktop session as Explorer and retry.").as_ptr(),
                    wide("Taskbar centering unavailable").as_ptr(),
                    MB_ICONWARNING,
                );
            } else if taskbar::set_preference(enabled).is_ok() {
                TASKBAR_CENTERED.store(enabled, Ordering::Release);
                update_taskbar_centering(hwnd);
            }
        }
        MENU_ACRYLIC_TASKBAR => {
            let enabled = !TASKBAR_ACRYLIC.load(Ordering::Acquire);
            if enabled {
                if taskbar::apply_acrylic() && taskbar::set_acrylic_preference(true).is_ok() {
                    TASKBAR_ACRYLIC.store(true, Ordering::Release);
                } else {
                    taskbar::restore_acrylic();
                }
            } else if taskbar::set_acrylic_preference(false).is_ok() {
                TASKBAR_ACRYLIC.store(false, Ordering::Release);
                taskbar::restore_acrylic();
            }
        }
        MENU_SYSTEM_USAGE => {
            let enabled = !SYSTEM_USAGE_ENABLED.load(Ordering::Acquire);
            if system_usage::set_preference(enabled).is_ok() {
                SYSTEM_USAGE_ENABLED.store(enabled, Ordering::Release);
                let sampler = SYSTEM_USAGE_SAMPLER
                    .get_or_init(|| Mutex::new(system_usage::SystemUsageSampler::default()));
                if let Ok(mut sampler) = sampler.lock() {
                    sampler.reset();
                }

                if enabled {
                    SetTimer(hwnd, TIMER_SYSTEM_USAGE, SYSTEM_USAGE_INTERVAL_MS, None);
                    refresh_system_usage(hwnd);
                } else {
                    KillTimer(hwnd, TIMER_SYSTEM_USAGE);
                    if let Some(state) = STATE.get() {
                        if let Ok(mut state) = state.lock() {
                            state.system_usage = system_usage::SystemUsage::default();
                        }
                    }
                }
                position_widget(hwnd);
                InvalidateRect(hwnd, null(), 0);
            }
        }
        MENU_EXIT => {
            DestroyWindow(hwnd);
        }
        _ => {}
    }
}

unsafe extern "system" fn widget_window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        windows_sys::Win32::UI::WindowsAndMessaging::WM_LBUTTONUP => {
            let flyout = FLYOUT_WINDOW.load(Ordering::Acquire);
            if flyout != 0 && IsWindowVisible(flyout) != 0 {
                SWITCH_MODE.store(false, Ordering::Release);
                ShowWindow(flyout, SW_HIDE);
                HOVER_SUPPRESSED.store(true, Ordering::Release);
            } else {
                show_flyout(hwnd);
            }
            0
        }
        WM_MULTI_UPDATED => {
            let flyout = FLYOUT_WINDOW.load(Ordering::Acquire);
            if flyout != 0 {
                if IsWindowVisible(flyout) != 0 {
                    position_flyout(hwnd, flyout);
                }
                InvalidateRect(flyout, null(), 1);
            }
            0
        }
        WM_PAINT => {
            draw_widget(hwnd);
            0
        }
        WM_ERASEBKGND => 1,
        WM_MOUSEMOVE => {
            cancel_hide_flyout();
            track_mouse(hwnd, true);
            0
        }
        WM_MOUSEHOVER => {
            if !HOVER_SUPPRESSED.load(Ordering::Acquire) {
                show_flyout(hwnd);
            }
            0
        }
        WM_MOUSELEAVE => {
            schedule_hide_flyout();
            0
        }
        WM_RBUTTONDOWN => {
            SetCapture(hwnd);
            HOVER_SUPPRESSED.store(true, Ordering::Release);
            HOVER_TICKS.store(0, Ordering::Relaxed);
            OUTSIDE_TICKS.store(0, Ordering::Relaxed);
            let flyout = FLYOUT_WINDOW.load(Ordering::Acquire);
            if flyout != 0 {
                SWITCH_MODE.store(false, Ordering::Release);
                ShowWindow(flyout, SW_HIDE);
            }
            0
        }
        WM_RBUTTONUP => {
            ReleaseCapture();
            show_context_menu(hwnd);
            0
        }
        WM_TIMER if wparam == TIMER_POSITION => {
            position_widget(hwnd);
            update_taskbar_centering(hwnd);
            if TASKBAR_ACRYLIC.load(Ordering::Acquire) {
                taskbar::apply_acrylic();
            } else {
                // Retry failed restores without a new worker or timer.
                taskbar::restore_acrylic();
            }
            0
        }
        WM_TASKBAR_LAYOUT => {
            refresh_taskbar_layout(hwnd, true);
            0
        }
        WM_TIMER if wparam == TIMER_LAYOUT_UPDATE => {
            refresh_taskbar_layout(hwnd, true);
            0
        }
        WM_TIMER if wparam == TIMER_LAYOUT_SETTLE => {
            KillTimer(hwnd, TIMER_LAYOUT_SETTLE);
            refresh_taskbar_layout(hwnd, false);
            0
        }
        WM_TIMER if wparam == TIMER_REFRESH => {
            trigger_refresh();
            trigger_saved_refresh(false);
            0
        }
        WM_TIMER if wparam == TIMER_SYSTEM_USAGE => {
            refresh_system_usage(hwnd);
            0
        }
        WM_TIMER if wparam == TIMER_HOVER_POLL => {
            poll_hover(hwnd);
            0
        }
        WM_TIMER if wparam == TIMER_HIDE_FLYOUT => {
            hide_flyout_if_outside();
            0
        }
        WM_TIMER if wparam == TIMER_ACCOUNT_FEEDBACK => {
            KillTimer(hwnd, TIMER_ACCOUNT_FEEDBACK);
            if let Some(state) = STATE.get().and_then(|state| state.lock().ok()) {
                let mut state = state;
                state.account_feedback = None;
            }
            let flyout = FLYOUT_WINDOW.load(Ordering::Acquire);
            if flyout != 0 {
                InvalidateRect(flyout, null(), 1);
                if IsWindowVisible(flyout) != 0 {
                    position_flyout(hwnd, flyout);
                }
            }
            hide_flyout_if_outside();
            0
        }
        WM_DISPLAYCHANGE | WM_SETTINGCHANGE | WM_DPICHANGED => {
            taskbar::invalidate_measurement();
            position_widget(hwnd);
            InvalidateRect(hwnd, null(), 0);
            update_taskbar_centering(hwnd);
            if TASKBAR_ACRYLIC.load(Ordering::Acquire) {
                taskbar::apply_acrylic();
            }
            0
        }
        WM_REFRESHED => {
            InvalidateRect(hwnd, null(), 1);
            let flyout = FLYOUT_WINDOW.load(Ordering::Acquire);
            if flyout != 0 && IsWindowVisible(flyout) != 0 {
                InvalidateRect(flyout, null(), 1);
            }
            position_widget(hwnd);
            update_taskbar_centering(hwnd);
            if TASKBAR_ACRYLIC.load(Ordering::Acquire) {
                taskbar::apply_acrylic();
            }
            if REFRESH_PENDING.swap(false, Ordering::AcqRel) {
                trigger_refresh();
            }
            0
        }
        WM_ACCOUNT_DONE => {
            if wparam != 0 {
                if let Some(state) = STATE.get() {
                    if let Ok(mut state) = state.lock() {
                        state.status = Status::Loading;
                        state.usage = UsageSnapshot::default();
                        state.account = rpc::AccountSummary::default();
                        state.updated_at = None;
                    }
                }
                // Preserve the user's visibility choice when switching accounts.
                InvalidateRect(hwnd, null(), 1);
                let flyout = FLYOUT_WINDOW.load(Ordering::Acquire);
                if flyout != 0 {
                    InvalidateRect(flyout, null(), 1);
                }
            }
            ACCOUNT_OPERATION.store(false, Ordering::Release);
            // Keep the current panel until the refresh reconciles saved accounts.
            // Clearing here briefly selected the obsolete single-account flyout.
            let notice = ACCOUNT_NOTICE
                .lock()
                .ok()
                .and_then(|mut notice| notice.take());
            if ACCOUNT_LOGIN_REQUIRED.swap(false, Ordering::AcqRel) {
                let answer = MessageBoxW(
                    hwnd,
                    wide("Codex has no saved file credentials for this login. A desktop sign-in may still be active.\n\nSign in in your browser to save this account in HiCodex? Choose the account you want to add. Your desktop login and existing saved accounts will be kept.\n\nYou can cancel from Accounts while the browser is open.").as_ptr(),
                    wide("Save account with browser sign-in").as_ptr(),
                    MB_YESNO | MB_ICONWARNING,
                );
                if answer == IDYES {
                    begin_account_action(AccountAction::SignIn);
                }
            } else if let Some((message, failed)) = notice {
                if failed {
                    MessageBoxW(
                        hwnd,
                        wide(&message).as_ptr(),
                        wide("HiCodex accounts").as_ptr(),
                        MB_ICONWARNING,
                    );
                } else {
                    if let Some(state) = STATE.get().and_then(|state| state.lock().ok()) {
                        let mut state = state;
                        state.account_feedback = Some(message);
                    }
                    SetTimer(hwnd, TIMER_ACCOUNT_FEEDBACK, ACCOUNT_FEEDBACK_MS, None);
                    show_flyout(hwnd);
                }
            }
            let pending = REFRESH_PENDING.swap(false, Ordering::AcqRel);
            if wparam != 0 || pending {
                trigger_refresh();
            }
            0
        }
        WM_DESTROY => {
            ACCOUNT_OPERATION.store(true, Ordering::Release);
            ACCOUNT_LOGIN_CANCEL.store(true, Ordering::Release);
            taskbar::stop_layout_watch();
            taskbar::restore_left();
            taskbar::restore_acrylic();
            let flyout = FLYOUT_WINDOW.swap(0, Ordering::AcqRel);
            if flyout != 0 {
                DestroyWindow(flyout);
            }
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcW(hwnd, message, wparam, lparam),
    }
}

fn account_display(account: &rpc::AccountSummary, visible: bool) -> &str {
    if visible {
        account.email.as_deref().unwrap_or("--")
    } else {
        "*****"
    }
}

unsafe fn account_button_rect(hwnd: HWND) -> RECT {
    let mut bounds: RECT = zeroed();
    GetClientRect(hwnd, &mut bounds);
    let dpi = GetDpiForWindow(hwnd).max(96) as i32;
    RECT {
        left: if multi_panel() {
            140 * dpi / 96
        } else {
            bounds.right - 44 * dpi / 96
        },
        top: if multi_panel() {
            bounds.bottom - 35 * dpi / 96
        } else {
            183 * dpi / 96
        },
        right: if multi_panel() {
            232 * dpi / 96
        } else {
            bounds.right - 18 * dpi / 96
        },
        bottom: if multi_panel() {
            bounds.bottom - 7 * dpi / 96
        } else {
            207 * dpi / 96
        },
    }
}

unsafe fn account_button_hit(hwnd: HWND, lparam: LPARAM) -> bool {
    let x = lparam as u16 as i16 as i32;
    let y = (lparam >> 16) as u16 as i16 as i32;
    let rect = account_button_rect(hwnd);
    x >= rect.left && x < rect.right && y >= rect.top && y < rect.bottom
}

unsafe extern "system" fn flyout_window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        windows_sys::Win32::UI::WindowsAndMessaging::WM_LBUTTONDOWN => {
            if account_button_hit(hwnd, lparam) {
                ACCOUNT_BUTTON_PRESSED.store(true, Ordering::Release);
                SetCapture(hwnd);
            }
            0
        }
        windows_sys::Win32::UI::WindowsAndMessaging::WM_LBUTTONUP => {
            let pressed = ACCOUNT_BUTTON_PRESSED.swap(false, Ordering::AcqRel);
            if pressed {
                ReleaseCapture();
                if account_button_hit(hwnd, lparam) {
                    ACCOUNT_VISIBLE.fetch_xor(true, Ordering::AcqRel);
                    position_flyout(WINDOW.load(Ordering::Acquire), hwnd);
                    InvalidateRect(hwnd, null(), 1);
                }
            } else if multi_panel() {
                let dpi = GetDpiForWindow(hwnd).max(96) as i32;
                let x = (lparam as u16 as i16 as i32) * 96 / dpi;
                let y = ((lparam >> 16) as u16 as i16 as i32) * 96 / dpi;
                let mut bounds: RECT = zeroed();
                GetClientRect(hwnd, &mut bounds);
                let width = bounds.right * 96 / dpi;
                let height = bounds.bottom * 96 / dpi;
                if (16..124).contains(&x) && (height - 36..height - 4).contains(&y) {
                    if !ACCOUNT_OPERATION.load(Ordering::Acquire) {
                        SWITCH_MODE.fetch_xor(true, Ordering::AcqRel);
                    }
                } else if (width - 144..width - 12).contains(&x)
                    && (height - 36..height - 4).contains(&y)
                {
                    trigger_saved_refresh(true);
                    trigger_refresh();
                } else {
                    let count = STATE
                        .get()
                        .and_then(|state| state.lock().ok())
                        .map_or(0, |state| state.saved_accounts.len());
                    let visible = PANEL_ROWS.load(Ordering::Acquire);
                    let offset = panel_layout::scroll_offset(
                        ACCOUNT_SCROLL.load(Ordering::Acquire),
                        count,
                        visible,
                    );
                    if let Some(index) = panel_layout::row_at(y, offset, count, visible) {
                        let target =
                            STATE
                                .get()
                                .and_then(|state| state.lock().ok())
                                .and_then(|state| {
                                    state
                                        .saved_accounts
                                        .get(index)
                                        .filter(|r| !r.active)
                                        .map(|r| {
                                            (
                                                r.account.key.clone(),
                                                account_usage::display_label(
                                                    r,
                                                    index,
                                                    ACCOUNT_VISIBLE.load(Ordering::Acquire),
                                                ),
                                            )
                                        })
                                });
                        if let Some((key, _label)) = target {
                            if !ACCOUNT_OPERATION.load(Ordering::Acquire)
                                && SWITCH_MODE.swap(false, Ordering::AcqRel)
                            {
                                begin_account_action(AccountAction::Switch(key));
                            }
                        }
                    }
                }
                InvalidateRect(hwnd, null(), 1);
            }
            0
        }
        windows_sys::Win32::UI::WindowsAndMessaging::WM_CAPTURECHANGED => {
            ACCOUNT_BUTTON_PRESSED.store(false, Ordering::Release);
            0
        }
        windows_sys::Win32::UI::WindowsAndMessaging::WM_MOUSEWHEEL => {
            let count = STATE
                .get()
                .and_then(|state| state.lock().ok())
                .map_or(0, |state| state.saved_accounts.len());
            let visible = PANEL_ROWS.load(Ordering::Acquire);
            let offset = ACCOUNT_SCROLL.load(Ordering::Acquire);
            let next = if (wparam >> 16) as u16 as i16 > 0 {
                offset.saturating_sub(1)
            } else {
                offset.saturating_add(1)
            };
            ACCOUNT_SCROLL.store(
                panel_layout::scroll_offset(next, count, visible),
                Ordering::Release,
            );
            InvalidateRect(hwnd, null(), 1);
            0
        }
        windows_sys::Win32::UI::WindowsAndMessaging::WM_SETCURSOR => {
            let mut point: POINT = zeroed();
            GetCursorPos(&mut point);
            windows_sys::Win32::Graphics::Gdi::ScreenToClient(hwnd, &mut point);
            let rect = account_button_rect(hwnd);
            if point.x >= rect.left
                && point.x < rect.right
                && point.y >= rect.top
                && point.y < rect.bottom
            {
                windows_sys::Win32::UI::WindowsAndMessaging::SetCursor(LoadCursorW(
                    0,
                    windows_sys::Win32::UI::WindowsAndMessaging::IDC_HAND,
                ));
                1
            } else {
                DefWindowProcW(hwnd, message, wparam, lparam)
            }
        }
        WM_PAINT => {
            draw_flyout(hwnd);
            0
        }
        WM_ERASEBKGND => 1,
        WM_MOUSEMOVE => {
            cancel_hide_flyout();
            track_mouse(hwnd, false);
            0
        }
        WM_MOUSELEAVE => {
            schedule_hide_flyout();
            0
        }
        WM_MOUSEACTIVATE => MA_NOACTIVATE as isize,
        WM_DESTROY => {
            FLYOUT_WINDOW.store(0, Ordering::Release);
            0
        }
        _ => DefWindowProcW(hwnd, message, wparam, lparam),
    }
}

unsafe fn register_window_class(
    instance: isize,
    class_name: &[u16],
    proc: unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT,
) -> bool {
    let window_class = WNDCLASSW {
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(proc),
        cbClsExtra: 0,
        cbWndExtra: 0,
        hInstance: instance,
        hIcon: 0,
        hCursor: LoadCursorW(0, IDC_ARROW),
        hbrBackground: 0,
        lpszMenuName: null(),
        lpszClassName: class_name.as_ptr(),
    };
    RegisterClassW(&window_class) != 0 || GetLastError() == 1410
}

pub fn run() {
    crate::locale::load();
    STATE.get_or_init(|| Arc::new(Mutex::new(AppState::default())));
    TASKBAR_CENTERED.store(taskbar::preference_enabled(), Ordering::Release);
    // Acrylic is retired; Windows owns taskbar appearance. Never reactivate old preferences.
    TASKBAR_ACRYLIC.store(false, Ordering::Release);
    SYSTEM_USAGE_ENABLED.store(system_usage::preference_enabled(), Ordering::Release);

    unsafe {
        SetProcessDpiAwarenessContext(-4isize);
        taskbar::load_recovery();
        taskbar::restore_left();

        let instance = GetModuleHandleW(null());
        let widget_class = wide(WIDGET_CLASS_NAME);
        let flyout_class = wide(FLYOUT_CLASS_NAME);
        if !register_window_class(instance, &widget_class, widget_window_proc)
            || !register_window_class(instance, &flyout_class, flyout_window_proc)
        {
            return;
        }

        let title = wide("HiCodex");
        let hwnd = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TOPMOST,
            widget_class.as_ptr(),
            title.as_ptr(),
            WS_POPUP | WS_VISIBLE,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            124,
            42,
            0,
            0 as HMENU,
            instance,
            null_mut(),
        );
        if hwnd == 0 {
            return;
        }

        WINDOW.store(hwnd, Ordering::Release);

        let flyout = CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            flyout_class.as_ptr(),
            title.as_ptr(),
            WS_POPUP,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            320,
            214,
            0,
            0 as HMENU,
            instance,
            null_mut(),
        );
        if flyout != 0 {
            FLYOUT_WINDOW.store(flyout, Ordering::Release);
            FLYOUT_GLASS.store(apply_flyout_glass(flyout), Ordering::Release);
        }

        SetTimer(hwnd, TIMER_POSITION, POSITION_INTERVAL_MS, None);
        SetTimer(hwnd, TIMER_REFRESH, REFRESH_INTERVAL_MS, None);
        SetTimer(hwnd, TIMER_HOVER_POLL, HOVER_POLL_INTERVAL_MS, None);
        if SYSTEM_USAGE_ENABLED.load(Ordering::Acquire) {
            SetTimer(hwnd, TIMER_SYSTEM_USAGE, SYSTEM_USAGE_INTERVAL_MS, None);
            refresh_system_usage(hwnd);
        }
        position_widget(hwnd);
        update_taskbar_centering(hwnd);
        if TASKBAR_ACRYLIC.load(Ordering::Acquire) {
            // Explorer can be mid-rebuild during startup/RDP reconnect. Keep
            // the preference and let the existing timer retry transient failures.
            taskbar::apply_acrylic();
        }
        trigger_refresh();
        #[cfg(debug_assertions)]
        if std::env::var_os("HICODEX_PREVIEW").is_some() {
            show_flyout(hwnd);
        }

        let mut message: MSG = zeroed();
        while GetMessageW(&mut message, 0, 0, 0) > 0 {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
        WINDOW.store(0, Ordering::Release);
        taskbar::shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn continuous_layout_events_are_rate_limited_without_delaying_the_first_pass() {
        let mut gate = LayoutGate {
            last_finished: None,
            active: false,
        };
        let start = Instant::now();
        let mut passes = 0;
        // Ten thousand notifications per second, spread across a full minute.
        for micros in (0..60_000_000).step_by(100) {
            let now = start + Duration::from_micros(micros);
            if gate.begin(now) == 0 {
                passes += 1;
                gate.finish(now);
            }
        }
        assert_eq!(passes, 1200);
        assert_eq!(gate.begin(start + Duration::from_secs(61)), 0);
    }

    #[test]
    fn reentrant_layout_events_wait_for_the_active_pass_and_a_full_interval() {
        let start = Instant::now();
        let mut gate = LayoutGate {
            last_finished: None,
            active: false,
        };
        assert_eq!(gate.begin(start), 0);
        assert_eq!(gate.begin(start + Duration::from_secs(10)), 50);
        gate.finish(start + Duration::from_secs(10));
        assert_eq!(gate.begin(start + Duration::from_secs(10)), 50);
        assert_eq!(
            gate.begin(start + Duration::from_millis(10_049) + Duration::from_nanos(1)),
            1
        );
        assert_eq!(gate.begin(start + Duration::from_millis(10_050)), 0);
    }

    #[test]
    fn hidden_account_never_exposes_identity_even_after_refresh() {
        for email in [
            None,
            Some("alice123@gmail.com"),
            Some("another@example.com"),
        ] {
            let account = rpc::AccountSummary {
                email: email.map(str::to_owned),
                ..Default::default()
            };
            assert_eq!(account_display(&account, false), "*****");
        }
    }

    #[test]
    fn visible_account_preserves_full_email_and_handles_missing_identity() {
        let account = rpc::AccountSummary {
            email: Some("alice123@gmail.com".to_owned()),
            ..Default::default()
        };
        assert_eq!(account_display(&account, true), "alice123@gmail.com");
        assert_eq!(account_display(&rpc::AccountSummary::default(), true), "--");
        assert!(!format!("{account:?}").contains("alice123@gmail.com"));
    }
}

#[cfg(test)]
mod resource_tests {
    use super::*;
    use windows_sys::Win32::System::ProcessStatus::{
        K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX,
    };
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcess, GetGuiResources, GetProcessHandleCount, GR_GDIOBJECTS, GR_USEROBJECTS,
    };

    #[derive(Debug)]
    struct Resources {
        gdi: u32,
        user: u32,
        handles: u32,
        private_bytes: usize,
    }

    unsafe fn resources() -> Resources {
        let process = GetCurrentProcess();
        let mut memory: PROCESS_MEMORY_COUNTERS_EX = zeroed();
        let size = size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32;
        memory.cb = size;
        assert_ne!(
            K32GetProcessMemoryInfo(
                process,
                (&mut memory as *mut PROCESS_MEMORY_COUNTERS_EX).cast(),
                size
            ),
            0
        );
        let mut handles = 0;
        assert_ne!(GetProcessHandleCount(process, &mut handles), 0);
        Resources {
            gdi: GetGuiResources(process, GR_GDIOBJECTS),
            user: GetGuiResources(process, GR_USEROBJECTS),
            handles,
            private_bytes: memory.PrivateUsage,
        }
    }

    struct FixtureWindow(HWND);

    impl FixtureWindow {
        unsafe fn new(layered: bool, width: i32, height: i32) -> Self {
            // Hidden, standalone stock windows: no taskbar parenting, hooks,
            // timers, credential reads, background service or registry writes.
            let hwnd = CreateWindowExW(
                if layered { WS_EX_LAYERED } else { 0 },
                wide("STATIC").as_ptr(),
                wide("HiCodex graphics resource fixture").as_ptr(),
                WS_POPUP,
                -10_000,
                -10_000,
                width,
                height,
                0,
                0,
                GetModuleHandleW(null()),
                null(),
            );
            assert_ne!(hwnd, 0);
            Self(hwnd)
        }
    }

    impl Drop for FixtureWindow {
        fn drop(&mut self) {
            unsafe {
                DestroyWindow(self.0);
            }
        }
    }

    #[test]
    #[ignore = "exports synthetic native UI previews without launching a meter or reading credentials"]
    fn render_multi_account_panel_preview() {
        let usage = UsageSnapshot {
            five_hour: Some(QuotaWindow {
                used_percent: 19.0,
                window_duration_minutes: 300,
                resets_at: now_unix() + 7200,
            }),
            weekly: Some(QuotaWindow {
                used_percent: 38.0,
                window_duration_minutes: 10080,
                resets_at: now_unix() + 345600,
            }),
            ..Default::default()
        };
        let mut state = AppState {
            usage: usage.clone(),
            updated_at: Some(now_unix()),
            status: Status::Ready,
            ..Default::default()
        };
        for i in 0..3 {
            state.saved_accounts.push(account_usage::Row {
                account: accounts::Account {
                    key: format!("fixture-{i}::workspace"),
                    email: format!("fixture-{i}@example.invalid"),
                    alias: String::new(),
                    plan: "Plus".into(),
                },
                active: i == 0,
                usage: if i < 2 { Some(usage.clone()) } else { None },
                updated_at: if i < 2 { Some(now_unix() - 180) } else { None },
                error: if i == 2 {
                    Some("Synthetic sign-in failure".into())
                } else {
                    None
                },
                refreshing: false,
            });
        }
        state.saved_accounts[1]
            .usage
            .as_mut()
            .unwrap()
            .five_hour
            .as_mut()
            .unwrap()
            .used_percent = 65.0;
        state.saved_accounts[1]
            .usage
            .as_mut()
            .unwrap()
            .weekly
            .as_mut()
            .unwrap()
            .used_percent = 10.0;
        PANEL_ROWS.store(3, Ordering::Release);
        ACCOUNT_VISIBLE.store(false, Ordering::Release);
        unsafe {
            for dpi in [96u32, 144] {
                let width = panel_layout::WIDTH * dpi as i32 / 96;
                let height = panel_layout::height(3) * dpi as i32 / 96;
                let dc = CreateCompatibleDC(0);
                let mut info: BITMAPINFO = zeroed();
                info.bmiHeader = BITMAPINFOHEADER {
                    biSize: size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: width,
                    biHeight: -height,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB,
                    ..zeroed()
                };
                let mut bits = null_mut();
                let bitmap = CreateDIBSection(dc, &info, DIB_RGB_COLORS, &mut bits, 0, 0);
                assert!(dc != 0 && bitmap != 0 && !bits.is_null());
                let old = SelectObject(dc, bitmap);
                let bounds = RECT {
                    left: 0,
                    top: 0,
                    right: width,
                    bottom: height,
                };
                paint_account_panel(dc, bounds, &state, dpi);
                // Warm font fallback caches, then require no growth across 1000 paints.
                for _ in 0..100 {
                    paint_account_panel(dc, bounds, &state, dpi);
                }
                // Flush GDI batches before comparing object counts.
                windows_sys::Win32::Graphics::Gdi::GdiFlush();
                let baseline = resources();
                for _ in 0..1000 {
                    paint_account_panel(dc, bounds, &state, dpi);
                }
                windows_sys::Win32::Graphics::Gdi::GdiFlush();
                let after = resources();
                assert!(
                    after.gdi <= baseline.gdi,
                    "GDI baseline={} after={}",
                    baseline.gdi,
                    after.gdi
                );
                let bytes = (width * height * 4) as usize;
                let mut output = Vec::with_capacity(bytes + 54);
                output.extend_from_slice(b"BM");
                output.extend_from_slice(&((bytes + 54) as u32).to_le_bytes());
                output.extend_from_slice(&[0u8; 4]);
                output.extend_from_slice(&54u32.to_le_bytes());
                output.extend_from_slice(std::slice::from_raw_parts(
                    (&info.bmiHeader as *const BITMAPINFOHEADER).cast::<u8>(),
                    40,
                ));
                output.extend_from_slice(std::slice::from_raw_parts(bits.cast::<u8>(), bytes));
                std::fs::write(format!("target/multi-account-panel-{dpi}.bmp"), output).unwrap();
                SelectObject(dc, old);
                DeleteObject(bitmap);
                DeleteDC(dc);
            }
        }
    }

    #[test]
    #[ignore = "graphics resource stress probe; run separately with --nocapture"]
    fn repeated_rendering_keeps_graphics_handles_and_memory_bounded() {
        let state = STATE.get_or_init(|| Arc::new(Mutex::new(AppState::default())));
        *state.lock().unwrap() = AppState {
            status: Status::Ready,
            usage: UsageSnapshot {
                five_hour: Some(QuotaWindow {
                    used_percent: 25.0,
                    window_duration_minutes: 300,
                    resets_at: now_unix() + 3600,
                }),
                weekly: Some(QuotaWindow {
                    used_percent: 50.0,
                    window_duration_minutes: 10_080,
                    resets_at: now_unix() + 86_400,
                }),
                ..Default::default()
            },
            account: rpc::AccountSummary {
                email: Some("synthetic@example.invalid".to_owned()),
                plan_type: Some("plus".to_owned()),
            },
            system_usage: system_usage::SystemUsage {
                cpu_percent: Some(42),
                memory_percent: Some(63),
            },
            ..Default::default()
        };
        SYSTEM_USAGE_ENABLED.store(true, Ordering::Release);
        unsafe {
            let widget = FixtureWindow::new(true, 280, 42);
            let flyout = FixtureWindow::new(false, 400, 214);
            let render = |cycle| {
                ACCOUNT_VISIBLE.store(cycle % 2 == 0, Ordering::Release);
                render_widget_layered(widget.0);
                apply_flyout_region(flyout.0, 400, 214, 96);
                InvalidateRect(flyout.0, null(), 0);
                draw_flyout(flyout.0);
                // Cover font allocation in dynamic width measurement as well.
                assert!(widget_width(widget.0, 96) > 0);
            };
            // Windows initializes some process resources several seconds after
            // the first font/window use. Both the old and refactored binaries
            // show that delayed step; sample after a timed warmup rather than
            // relaxing the subsequent handle-growth bound.
            let warmup = Instant::now();
            let mut cycle = 0;
            while warmup.elapsed() < Duration::from_secs(10) {
                render(cycle);
                cycle += 1;
            }
            let baseline = resources();
            println!("graphics baseline: {baseline:?}");
            for batch in 1..=20 {
                for cycle in 0..250 {
                    render(cycle);
                }
                let current = resources();
                println!("graphics after {} renders: {current:?}", batch * 250);
                assert!(current.gdi <= baseline.gdi);
                assert!(current.user <= baseline.user);
                assert!(current.handles <= baseline.handles + 2);
                // Allow allocator/native cache high water without accepting
                // linear growth across thousands of complete paint cycles.
                assert!(current.private_bytes <= baseline.private_bytes + 2 * 1024 * 1024);
            }
        }
        SYSTEM_USAGE_ENABLED.store(false, Ordering::Release);
        ACCOUNT_VISIBLE.store(false, Ordering::Release);
    }
}
