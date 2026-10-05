use crate::quota::{QuotaWindow, UsageSnapshot};
use crate::{accounts, rpc, startup, system_usage, taskbar};
use std::ffi::c_void;
use std::mem::{size_of, zeroed};
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};
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
    CommDlgExtendedError, GetOpenFileNameW, OFN_FILEMUSTEXIST, OFN_NOCHANGEDIR, OFN_PATHMUSTEXIST,
    OPENFILENAMEW,
};
use windows_sys::Win32::UI::Controls::{WM_MOUSEHOVER, WM_MOUSELEAVE};
use windows_sys::Win32::UI::HiDpi::{GetDpiForWindow, SetProcessDpiAwarenessContext};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture, TrackMouseEvent, TME_HOVER, TME_LEAVE, TRACKMOUSEEVENT,
};
use windows_sys::Win32::UI::Shell::ShellExecuteW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DestroyWindow,
    DispatchMessageW, FindWindowExW, FindWindowW, GetClientRect, GetCursorPos, GetMessageW,
    GetParent, GetWindowRect, IsWindowVisible, KillTimer, LoadCursorW, MessageBoxW, PostMessageW,
    PostQuitMessage, RegisterClassW, SetForegroundWindow, SetLayeredWindowAttributes, SetParent,
    SetTimer, SetWindowLongPtrW, SetWindowPos, ShowWindow, TrackPopupMenu, TranslateMessage,
    UpdateLayeredWindow, CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT, GWL_EXSTYLE, GWL_STYLE, HMENU,
    HWND_TOP, HWND_TOPMOST, IDC_ARROW, IDYES, LWA_ALPHA, MA_NOACTIVATE, MB_DEFBUTTON2,
    MB_ICONWARNING, MB_YESNO, MF_CHECKED, MF_GRAYED, MF_POPUP, MF_SEPARATOR, MF_STRING, MSG,
    SWP_NOACTIVATE, SWP_SHOWWINDOW, SW_HIDE, SW_SHOW, SW_SHOWNOACTIVATE, TPM_RETURNCMD,
    TPM_RIGHTBUTTON, ULW_ALPHA, WM_APP, WM_DESTROY, WM_DISPLAYCHANGE, WM_ERASEBKGND,
    WM_MOUSEACTIVATE, WM_MOUSEMOVE, WM_PAINT, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_SETTINGCHANGE,
    WM_TIMER, WNDCLASSW, WS_CHILD, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_POPUP,
    WS_VISIBLE,
};

const WIDGET_CLASS_NAME: &str = "HiCodex.TaskbarWidget";
const FLYOUT_CLASS_NAME: &str = "HiCodex.Flyout";

const TIMER_POSITION: usize = 1;
const TIMER_REFRESH: usize = 2;
const TIMER_HIDE_FLYOUT: usize = 3;
const TIMER_HOVER_POLL: usize = 4;
const TIMER_SYSTEM_USAGE: usize = 5;
const TIMER_ACCOUNT_FEEDBACK: usize = 6;
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

const MENU_REFRESH: i32 = 1001;
const MENU_OPEN_USAGE: i32 = 1002;
const MENU_STARTUP: i32 = 1003;
const MENU_CENTER_TASKBAR: i32 = 1004;
const MENU_ACRYLIC_TASKBAR: i32 = 1005;
const MENU_EXIT: i32 = 1006;
const MENU_SYSTEM_USAGE: i32 = 1007;
const MENU_SAVE_ACCOUNT: i32 = 1010;
const MENU_IMPORT_ACCOUNT: i32 = 1011;
const MENU_IMPORT_CODEX_AUTH: i32 = 1012;
const MENU_RESTORE_ACCOUNT: i32 = 1013;
const MENU_ACCOUNT_FIRST: i32 = 2000;

static STATE: OnceLock<Arc<Mutex<AppState>>> = OnceLock::new();
static WINDOW: AtomicIsize = AtomicIsize::new(0);
static FLYOUT_WINDOW: AtomicIsize = AtomicIsize::new(0);
static FLYOUT_GLASS: AtomicBool = AtomicBool::new(false);
static REFRESHING: AtomicBool = AtomicBool::new(false);
static REFRESH_PENDING: AtomicBool = AtomicBool::new(false);
static ACCOUNT_OPERATION: AtomicBool = AtomicBool::new(false);
// Serialize credential mutations against this process's App Server readers.
static AUTH_GATE: Mutex<()> = Mutex::new(());
static ACCOUNT_NOTICE: Mutex<Option<(String, bool)>> = Mutex::new(None);
static HOVER_TICKS: AtomicU32 = AtomicU32::new(0);
static OUTSIDE_TICKS: AtomicU32 = AtomicU32::new(0);
static HOVER_SUPPRESSED: AtomicBool = AtomicBool::new(false);
static TASKBAR_CENTERED: AtomicBool = AtomicBool::new(false);
static TASKBAR_ACRYLIC: AtomicBool = AtomicBool::new(false);
static SYSTEM_USAGE_ENABLED: AtomicBool = AtomicBool::new(false);
static ACCOUNT_VISIBLE: AtomicBool = AtomicBool::new(false);
static ACCOUNT_BUTTON_PRESSED: AtomicBool = AtomicBool::new(false);
static SYSTEM_USAGE_SAMPLER: OnceLock<Mutex<system_usage::SystemUsageSampler>> = OnceLock::new();

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

enum AccountAction {
    Save,
    Import(std::path::PathBuf),
    ImportCodexAuth,
    Switch(String),
    Restore,
}

fn begin_account_action(action: AccountAction) {
    if ACCOUNT_OPERATION.swap(true, Ordering::AcqRel) {
        return;
    }
    std::thread::spawn(move || {
        let changes_auth = matches!(&action, AccountAction::Switch(_) | AccountAction::Restore);
        let result = (|| {
            let _gate = AUTH_GATE
                .lock()
                .map_err(|_| "Account operation lock is unavailable".to_owned())?;
            let manager = accounts::AccountManager::current()?;
            match action {
                AccountAction::Save => {
                    manager.save_current()?;
                    Ok(("Account saved.".to_owned(), false))
                }
                AccountAction::Import(path) => {
                    let count = manager.import_file(&path)?;
                    Ok((format!("Added {count}. Current account unchanged."), false))
                }
                AccountAction::ImportCodexAuth => {
                    let report = manager.import_codex_auth()?;
                    Ok((
                        format!(
                            "Added {}; skipped {}. Current account unchanged.",
                            report.added, report.skipped
                        ),
                        false,
                    ))
                }
                AccountAction::Switch(key) => {
                    let changed = manager.switch(&key)?;
                    Ok((
                        if changed {
                            "Account selected. Reopen Codex."
                        } else {
                            "This account is already selected."
                        }
                        .to_owned(),
                        changed,
                    ))
                }
                AccountAction::Restore => {
                    manager.restore()?;
                    Ok(("Account restored. Reopen Codex.".to_owned(), true))
                }
            }
        })();
        let (message, changed, failed) = match result {
            Ok((message, changed)) => (message, changed, false),
            // A post-commit verification failure may have changed auth. Always
            // re-read after attempted writes instead of showing the old quota.
            Err(error) => (error, changes_auth, true),
        };
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
        "Switch to {target}?\n\nClose Codex and other switchers first.\nReopen Codex afterward."
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
        MENU_SAVE_ACCOUNT as usize,
        wide("Save current account").as_ptr(),
    );
    AppendMenuW(
        submenu,
        flags,
        MENU_IMPORT_ACCOUNT as usize,
        wide("Import auth JSON...").as_ptr(),
    );
    AppendMenuW(
        submenu,
        flags,
        MENU_IMPORT_CODEX_AUTH as usize,
        wide("Import existing codex-auth accounts").as_ptr(),
    );
    AppendMenuW(submenu, MF_SEPARATOR, 0, null());
    let result = accounts::AccountManager::current().and_then(|manager| manager.list());
    let accounts = match result {
        Ok(list) => {
            for (index, account) in list.accounts.iter().enumerate() {
                let active = list.active_key.as_deref() == Some(&account.key);
                AppendMenuW(
                    submenu,
                    flags | if active { MF_CHECKED | MF_GRAYED } else { 0 },
                    MENU_ACCOUNT_FIRST as usize + index,
                    wide(&account.label(ACCOUNT_VISIBLE.load(Ordering::Acquire), index)).as_ptr(),
                );
            }
            if list.accounts.is_empty() {
                AppendMenuW(
                    submenu,
                    MF_STRING | MF_GRAYED,
                    0,
                    wide("No saved accounts").as_ptr(),
                );
            }
            AppendMenuW(submenu, MF_SEPARATOR, 0, null());
            AppendMenuW(
                submenu,
                MF_STRING
                    | if busy || !list.can_restore {
                        MF_GRAYED
                    } else {
                        0
                    },
                MENU_RESTORE_ACCOUNT as usize,
                wide("Restore last switch backup").as_ptr(),
            );
            list.accounts
        }
        Err(_) => {
            // Account storage failure must never change quota status.
            AppendMenuW(
                submenu,
                MF_STRING | MF_GRAYED,
                0,
                wide("Account storage unavailable").as_ptr(),
            );
            Vec::new()
        }
    };
    AppendMenuW(
        menu,
        MF_POPUP,
        submenu as usize,
        wide("Accounts (experimental)").as_ptr(),
    );
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
) -> HFONT {
    let height = -((logical_pixels * dpi as i32) / 96).max(logical_pixels);
    let face = wide("Segoe UI");
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
    create_font_with_quality(dpi, logical_pixels, weight, CLEARTYPE_NATURAL_QUALITY)
}

unsafe fn create_widget_font(dpi: u32, logical_pixels: i32, weight: u32) -> HFONT {
    create_font_with_quality(dpi, logical_pixels, weight, ANTIALIASED_QUALITY.into())
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
    UpdateLayeredWindow(
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

    if GetParent(hwnd) != tray {
        SetParent(hwnd, tray);
        SetWindowLongPtrW(hwnd, GWL_STYLE, (WS_CHILD | WS_VISIBLE) as isize);
        SetWindowLongPtrW(
            hwnd,
            GWL_EXSTYLE,
            (WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE) as isize,
        );
    }

    let mut tray_rect: RECT = zeroed();
    if GetWindowRect(tray, &mut tray_rect) == 0 {
        return;
    }

    let dpi = GetDpiForWindow(tray).max(96) as i32;
    let width = widget_width(tray, dpi as u32);
    let tray_height = tray_rect.bottom - tray_rect.top;
    let height = (42 * dpi / 96).min(tray_height.saturating_sub(2)).max(28);
    let y = ((tray_height - height) / 2).max(0);

    let notify_class = wide("TrayNotifyWnd");
    let notify = FindWindowExW(tray, 0, notify_class.as_ptr(), null());
    let x = if notify != 0 {
        let mut notify_rect: RECT = zeroed();
        if GetWindowRect(notify, &mut notify_rect) != 0 {
            notify_rect.left - tray_rect.left - width - (2 * dpi / 96)
        } else {
            tray_rect.right - tray_rect.left - width - (220 * dpi / 96)
        }
    } else {
        tray_rect.right - tray_rect.left - width - (220 * dpi / 96)
    }
    .max(0);

    SetWindowPos(
        hwnd,
        HWND_TOP,
        x,
        y,
        width,
        height,
        SWP_NOACTIVATE | SWP_SHOWWINDOW,
    );
    ShowWindow(hwnd, SW_SHOW);

    let flyout = FLYOUT_WINDOW.load(Ordering::Acquire);
    if flyout != 0 && IsWindowVisible(flyout) != 0 {
        position_flyout(hwnd, flyout);
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

unsafe fn draw_flyout(hwnd: HWND) {
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
    let mut width = 320 * dpi / 96;
    if ACCOUNT_VISIBLE.load(Ordering::Acquire)
        || STATE
            .get()
            .and_then(|state| state.lock().ok())
            .is_some_and(|state| state.account_feedback.is_some())
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
    let height = 214 * dpi / 96;
    let gap = (9 * dpi / 96).max(6);

    let monitor = MonitorFromWindow(widget, MONITOR_DEFAULTTONEAREST);
    let mut monitor_info: MONITORINFO = zeroed();
    monitor_info.cbSize = size_of::<MONITORINFO>() as u32;
    if GetMonitorInfoW(monitor, &mut monitor_info) == 0 {
        return;
    }

    let work = monitor_info.rcWork;
    width = width.min(work.right - work.left);
    let max_x = (work.right - width).max(work.left);
    let widget_center = widget_rect.left + (widget_rect.right - widget_rect.left) / 2;
    let x = (widget_center - width / 2).clamp(work.left, max_x);
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
        ShowWindow(flyout, SW_HIDE);
        return;
    }

    if !point_inside_window(cursor, widget) && !point_inside_window(cursor, flyout) {
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
            ShowWindow(flyout, SW_HIDE);
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

unsafe fn show_context_menu(hwnd: HWND) {
    let flyout = FLYOUT_WINDOW.load(Ordering::Acquire);
    if flyout != 0 {
        ShowWindow(flyout, SW_HIDE);
    }

    let menu = CreatePopupMenu();
    if menu == 0 {
        return;
    }

    let refresh = wide("Refresh now");
    let open_usage = wide("Open Usage dashboard");
    let startup_label = wide("Start with Windows");
    let system_usage_label = wide("Show CPU and RAM");
    let center_taskbar_label = wide("Center taskbar icons");
    let acrylic_taskbar_label = wide("Acrylic taskbar");
    let exit = wide("Exit HiCodex");

    AppendMenuW(menu, MF_STRING, MENU_REFRESH as usize, refresh.as_ptr());
    AppendMenuW(
        menu,
        MF_STRING,
        MENU_OPEN_USAGE as usize,
        open_usage.as_ptr(),
    );
    let account_choices = append_account_menu(menu);
    AppendMenuW(menu, MF_SEPARATOR, 0, null());
    AppendMenuW(
        menu,
        MF_STRING | if startup::is_enabled() { MF_CHECKED } else { 0 },
        MENU_STARTUP as usize,
        startup_label.as_ptr(),
    );
    AppendMenuW(
        menu,
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
        menu,
        MF_STRING
            | if TASKBAR_CENTERED.load(Ordering::Acquire) {
                MF_CHECKED
            } else {
                0
            },
        MENU_CENTER_TASKBAR as usize,
        center_taskbar_label.as_ptr(),
    );
    AppendMenuW(
        menu,
        MF_STRING
            | if TASKBAR_ACRYLIC.load(Ordering::Acquire) {
                MF_CHECKED
            } else {
                0
            },
        MENU_ACRYLIC_TASKBAR as usize,
        acrylic_taskbar_label.as_ptr(),
    );
    AppendMenuW(menu, MF_SEPARATOR, 0, null());
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
        MENU_SAVE_ACCOUNT => begin_account_action(AccountAction::Save),
        MENU_IMPORT_CODEX_AUTH => begin_account_action(AccountAction::ImportCodexAuth),
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
        MENU_REFRESH => trigger_refresh(),
        MENU_OPEN_USAGE => open_usage_dashboard(hwnd),
        MENU_STARTUP => {
            let _ = startup::set_enabled(!startup::is_enabled());
        }
        MENU_CENTER_TASKBAR => {
            let enabled = !TASKBAR_CENTERED.load(Ordering::Acquire);
            if enabled {
                if taskbar::apply_centered() && taskbar::set_preference(true).is_ok() {
                    TASKBAR_CENTERED.store(true, Ordering::Release);
                } else {
                    taskbar::restore_left();
                }
            } else if taskbar::set_preference(false).is_ok() {
                TASKBAR_CENTERED.store(false, Ordering::Release);
                taskbar::restore_left();
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
            if TASKBAR_CENTERED.load(Ordering::Acquire) {
                taskbar::apply_centered();
            }
            if TASKBAR_ACRYLIC.load(Ordering::Acquire) {
                taskbar::apply_acrylic();
            }
            0
        }
        WM_TIMER if wparam == TIMER_REFRESH => {
            trigger_refresh();
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
        WM_DISPLAYCHANGE | WM_SETTINGCHANGE => {
            position_widget(hwnd);
            if TASKBAR_CENTERED.load(Ordering::Acquire) {
                taskbar::apply_centered();
            }
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
            if TASKBAR_CENTERED.load(Ordering::Acquire) {
                taskbar::apply_centered();
            }
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
                // Newly selected identities start hidden again.
                ACCOUNT_VISIBLE.store(false, Ordering::Release);
                InvalidateRect(hwnd, null(), 1);
                let flyout = FLYOUT_WINDOW.load(Ordering::Acquire);
                if flyout != 0 {
                    InvalidateRect(flyout, null(), 1);
                }
            }
            ACCOUNT_OPERATION.store(false, Ordering::Release);
            let notice = ACCOUNT_NOTICE
                .lock()
                .ok()
                .and_then(|mut notice| notice.take());
            if let Some((message, failed)) = notice {
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
        left: bounds.right - 44 * dpi / 96,
        top: 183 * dpi / 96,
        right: bounds.right - 18 * dpi / 96,
        bottom: 207 * dpi / 96,
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
            }
            0
        }
        windows_sys::Win32::UI::WindowsAndMessaging::WM_CAPTURECHANGED => {
            ACCOUNT_BUTTON_PRESSED.store(false, Ordering::Release);
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
    STATE.get_or_init(|| Arc::new(Mutex::new(AppState::default())));
    TASKBAR_CENTERED.store(taskbar::preference_enabled(), Ordering::Release);
    TASKBAR_ACRYLIC.store(taskbar::acrylic_preference_enabled(), Ordering::Release);
    SYSTEM_USAGE_ENABLED.store(system_usage::preference_enabled(), Ordering::Release);

    unsafe {
        taskbar::initialize();
        SetProcessDpiAwarenessContext(-4isize);

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
            WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
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
        if TASKBAR_CENTERED.load(Ordering::Acquire) {
            taskbar::apply_centered();
        }
        if TASKBAR_ACRYLIC.load(Ordering::Acquire) && !taskbar::apply_acrylic() {
            TASKBAR_ACRYLIC.store(false, Ordering::Release);
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
mod account_visibility_tests {
    use super::*;

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
