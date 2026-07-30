use crate::quota::{QuotaWindow, UsageSnapshot};
use crate::{rpc, startup, taskbar};
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
use windows_sys::Win32::UI::Controls::{WM_MOUSEHOVER, WM_MOUSELEAVE};
use windows_sys::Win32::UI::HiDpi::{GetDpiForWindow, SetProcessDpiAwarenessContext};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    TrackMouseEvent, TME_HOVER, TME_LEAVE, TRACKMOUSEEVENT,
};
use windows_sys::Win32::UI::Shell::ShellExecuteW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DestroyWindow,
    DispatchMessageW, FindWindowExW, FindWindowW, GetClientRect, GetCursorPos, GetMessageW,
    GetParent, GetWindowRect, IsWindowVisible, KillTimer, LoadCursorW, PostMessageW,
    PostQuitMessage, RegisterClassW, SetForegroundWindow, SetLayeredWindowAttributes, SetParent,
    SetTimer, SetWindowLongPtrW, SetWindowPos, ShowWindow, TrackPopupMenu, TranslateMessage,
    UpdateLayeredWindow, CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT, GWL_EXSTYLE, GWL_STYLE, HMENU,
    HWND_TOP, HWND_TOPMOST, IDC_ARROW, LWA_ALPHA, MA_NOACTIVATE, MF_CHECKED, MF_SEPARATOR,
    MF_STRING, MSG, SWP_NOACTIVATE, SWP_SHOWWINDOW, SW_HIDE, SW_SHOW, SW_SHOWNOACTIVATE,
    TPM_RETURNCMD, TPM_RIGHTBUTTON, ULW_ALPHA, WM_APP, WM_DESTROY, WM_DISPLAYCHANGE, WM_ERASEBKGND,
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
const POSITION_INTERVAL_MS: u32 = 2_000;
const REFRESH_INTERVAL_MS: u32 = 120_000;
const HOVER_DELAY_MS: u32 = 300;
const HIDE_DELAY_MS: u32 = 250;
const HOVER_POLL_INTERVAL_MS: u32 = 100;
const HOVER_SHOW_TICKS: u32 = 3;
const HOVER_HIDE_TICKS: u32 = 3;
const WM_REFRESHED: u32 = WM_APP + 1;

const MENU_REFRESH: i32 = 1001;
const MENU_OPEN_USAGE: i32 = 1002;
const MENU_STARTUP: i32 = 1003;
const MENU_CENTER_TASKBAR: i32 = 1004;
const MENU_EXIT: i32 = 1005;

static STATE: OnceLock<Arc<Mutex<AppState>>> = OnceLock::new();
static WINDOW: AtomicIsize = AtomicIsize::new(0);
static FLYOUT_WINDOW: AtomicIsize = AtomicIsize::new(0);
static FLYOUT_GLASS: AtomicBool = AtomicBool::new(false);
static REFRESHING: AtomicBool = AtomicBool::new(false);
static HOVER_TICKS: AtomicU32 = AtomicU32::new(0);
static OUTSIDE_TICKS: AtomicU32 = AtomicU32::new(0);
static HOVER_SUPPRESSED: AtomicBool = AtomicBool::new(false);
static TASKBAR_CENTERED: AtomicBool = AtomicBool::new(false);

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
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            status: Status::Loading,
            usage: UsageSnapshot::default(),
            account: rpc::AccountSummary::default(),
            updated_at: None,
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
    if REFRESHING.swap(true, Ordering::AcqRel) {
        return;
    }

    std::thread::spawn(|| {
        let result = rpc::fetch_usage();
        if let Some(state) = STATE.get() {
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

fn quota_value(quota: Option<&QuotaWindow>) -> String {
    quota
        .map(|quota| format!("{}%", quota.remaining_percent()))
        .unwrap_or_else(|| "--".to_owned())
}

fn quota_days(quota: Option<&QuotaWindow>) -> String {
    quota
        .map(|quota| format!("{}d", quota.days_until_reset()))
        .unwrap_or_default()
}

fn color_for(quota: Option<&QuotaWindow>) -> u32 {
    match quota.map(QuotaWindow::remaining_percent) {
        Some(0..=20) => rgb(255, 115, 115),
        Some(21..=50) => rgb(242, 198, 109),
        Some(_) => rgb(120, 217, 139),
        None => rgb(146, 153, 165),
    }
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
    let value_left = ((54 * dpi as i32) / 96).max(50);
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

    let week_label = RECT {
        left: padding,
        top: 0,
        right: value_left,
        bottom: half,
    };
    let hour_label = RECT {
        left: padding,
        top: half,
        right: value_left,
        bottom: height,
    };
    layer.draw_text(label_font, label_color, "Week:", week_label, common_flags);
    layer.draw_text(label_font, label_color, "Hour:", hour_label, common_flags);

    let (week_text, week_days, hour_text, week_color, hour_color) = match snapshot.status {
        Status::Loading => (
            "\u{2026}".to_owned(),
            String::new(),
            "\u{2026}".to_owned(),
            value_muted,
            value_muted,
        ),
        Status::Error(_) => (
            "!".to_owned(),
            String::new(),
            "!".to_owned(),
            rgb(255, 115, 115),
            rgb(255, 115, 115),
        ),
        Status::Ready => (
            quota_value(snapshot.usage.weekly.as_ref()),
            quota_days(snapshot.usage.weekly.as_ref()),
            quota_value(snapshot.usage.five_hour.as_ref()),
            color_for(snapshot.usage.weekly.as_ref()),
            color_for(snapshot.usage.five_hour.as_ref()),
        ),
    };

    let week_value = RECT {
        left: value_left,
        top: 0,
        right: width - padding,
        bottom: half,
    };
    let week_days_left =
        value_left + text_width(mask_dc, value_font, &week_text) + ((3 * dpi as i32) / 96).max(2);
    let week_days_rect = RECT {
        left: week_days_left,
        top: 0,
        right: width - padding,
        bottom: half,
    };
    let hour_value = RECT {
        left: value_left,
        top: half,
        right: width - padding,
        bottom: height,
    };
    layer.draw_text(value_font, week_color, &week_text, week_value, common_flags);
    layer.draw_text(
        value_font,
        value_muted,
        &week_days,
        week_days_rect,
        common_flags,
    );
    layer.draw_text(value_font, hour_color, &hour_text, hour_value, common_flags);

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
    let width = (122 * dpi / 96).max(116);
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

    let (footer, reset_badge) = match &snapshot.status {
        Status::Error(error) => (error.clone(), String::new()),
        _ => (
            plan_badge(&snapshot),
            snapshot
                .usage
                .reset_credits
                .map(|count| format!("Reset x{count}"))
                .unwrap_or_default(),
        ),
    };
    let mut footer_rect = RECT {
        left,
        top: scale(162),
        right: scale(150),
        bottom: bounds.bottom - scale(7),
    };
    paint_text(dc, secondary_font, body, &footer, &mut footer_rect, common);
    let mut reset_badge_rect = RECT {
        left: scale(150),
        top: scale(162),
        right,
        bottom: bounds.bottom - scale(7),
    };
    paint_text(
        dc,
        secondary_font,
        body,
        &reset_badge,
        &mut reset_badge_rect,
        DT_RIGHT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX | DT_END_ELLIPSIS,
    );

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
    let width = 320 * dpi / 96;
    let height = 188 * dpi / 96;
    let gap = (9 * dpi / 96).max(6);

    let monitor = MonitorFromWindow(widget, MONITOR_DEFAULTTONEAREST);
    let mut monitor_info: MONITORINFO = zeroed();
    monitor_info.cbSize = size_of::<MONITORINFO>() as u32;
    if GetMonitorInfoW(monitor, &mut monitor_info) == 0 {
        return;
    }

    let work = monitor_info.rcWork;
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
    let center_taskbar_label = wide("Center taskbar icons");
    let exit = wide("Exit HiCodex");

    AppendMenuW(menu, MF_STRING, MENU_REFRESH as usize, refresh.as_ptr());
    AppendMenuW(
        menu,
        MF_STRING,
        MENU_OPEN_USAGE as usize,
        open_usage.as_ptr(),
    );
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
            | if TASKBAR_CENTERED.load(Ordering::Acquire) {
                MF_CHECKED
            } else {
                0
            },
        MENU_CENTER_TASKBAR as usize,
        center_taskbar_label.as_ptr(),
    );
    AppendMenuW(menu, MF_SEPARATOR, 0, null());
    AppendMenuW(menu, MF_STRING, MENU_EXIT as usize, exit.as_ptr());

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
                }
            } else if taskbar::set_preference(false).is_ok() {
                TASKBAR_CENTERED.store(false, Ordering::Release);
                taskbar::restore_left();
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
            show_context_menu(hwnd);
            0
        }
        WM_TIMER if wparam == TIMER_POSITION => {
            position_widget(hwnd);
            if TASKBAR_CENTERED.load(Ordering::Acquire) {
                taskbar::apply_centered();
            }
            0
        }
        WM_TIMER if wparam == TIMER_REFRESH => {
            trigger_refresh();
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
        WM_DISPLAYCHANGE | WM_SETTINGCHANGE => {
            position_widget(hwnd);
            if TASKBAR_CENTERED.load(Ordering::Acquire) {
                taskbar::apply_centered();
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
            0
        }
        WM_DESTROY => {
            taskbar::restore_left();
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

unsafe extern "system" fn flyout_window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
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
            188,
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
        position_widget(hwnd);
        if TASKBAR_CENTERED.load(Ordering::Acquire) {
            taskbar::apply_centered();
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
