use crate::rebar::{self, Band};
use std::ffi::c_void;
use std::mem::{size_of, transmute, zeroed};
use std::os::windows::process::CommandExt;
use std::process::Command;
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use windows_sys::core::GUID;
use windows_sys::Win32::Foundation::{HWND, RECT};
use windows_sys::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};
use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;
use windows_sys::Win32::System::Variant::{VARIANT, VT_I4};
use windows_sys::Win32::UI::Accessibility::{
    AccessibleObjectFromWindow, SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    FindWindowExW, FindWindowW, GetParent, GetWindowRect, GetWindowThreadProcessId, IsChild,
    PostMessageW, EVENT_OBJECT_CREATE, EVENT_OBJECT_DESTROY, EVENT_OBJECT_LOCATIONCHANGE,
    EVENT_OBJECT_REORDER, OBJID_CLIENT, OBJID_WINDOW, WINEVENT_OUTOFCONTEXT,
    WINEVENT_SKIPOWNPROCESS,
};

const SETTINGS_KEY: &str = r"HKCU\Software\HiCodex";
const CENTER_VALUE: &str = "CenterTaskbarIcons";
const ACRYLIC_VALUE: &str = "AcrylicTaskbar";
const IID_IACCESSIBLE: GUID = GUID::from_u128(0x618736e0_3c3d_11cf_810c_00aa00389b71);
const WCA_ACCENT_POLICY: i32 = 19;
const ACCENT_DISABLED: i32 = 0;
const ACCENT_ENABLE_BLUR_BEHIND: i32 = 3;
const ACCENT_ENABLE_ACRYLIC_BLUR_BEHIND: i32 = 4;
const TASKBAR_ACRYLIC_TINT: u32 = 0x9028_2220;

static COM_INITIALIZED: AtomicBool = AtomicBool::new(false);
static ORIGINAL_TASKBAR_POLICIES: OnceLock<Mutex<Vec<AccentPlacement>>> = OnceLock::new();
static BAND_PLACEMENT: Mutex<Option<BandPlacement>> = Mutex::new(None);
static BUTTON_SPAN: Mutex<Option<ButtonSpan>> = Mutex::new(None);
static LAYOUT_WATCH: Mutex<Option<LayoutWatch>> = Mutex::new(None);

struct LayoutWatch {
    taskbar: HWND,
    process: u32,
    receiver: HWND,
    message: u32,
    hooks: [HWINEVENTHOOK; 2],
    queued: bool,
}

#[derive(Clone, Copy)]
struct ButtonSpan {
    parent: HWND,
    window: HWND,
    process: u32,
    width: i32,
    leading: i32,
}

impl ButtonSpan {
    fn for_window(self, parent: HWND, window: HWND, process: u32) -> Option<Self> {
        (self.parent == parent && self.window == window && self.process == process).then_some(self)
    }
}

fn remember_button_span(
    cached: &mut Option<ButtonSpan>,
    parent: HWND,
    window: HWND,
    process: u32,
    measured: Option<ButtonSpan>,
) -> Option<ButtonSpan> {
    *cached = measured.or_else(|| cached.and_then(|span| span.for_window(parent, window, process)));
    *cached
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct BandPlacement {
    band: Band,
    original: u32,
    applied: u32,
}

impl BandPlacement {
    fn owns(&self, band: Band) -> bool {
        self.band.same_owner(band) && self.applied == band.header
    }

    fn after_change(previous: Option<Self>, band: Band, applied: u32) -> Self {
        let original = previous
            .filter(|saved| saved.owns(band))
            .map_or(band.header, |saved| saved.original);
        Self {
            band,
            original,
            applied,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct AccentPolicy {
    state: i32,
    flags: i32,
    gradient_color: u32,
    animation_id: i32,
}

#[derive(Clone, Copy)]
struct AccentPlacement {
    window: HWND,
    process: u32,
    original: AccentPolicy,
    applied: AccentPolicy,
}

impl AccentPlacement {
    fn owns(&self, process: u32, current: AccentPolicy) -> bool {
        self.process == process && self.applied == current
    }

    // Return whether this recovery record must be kept for another attempt.
    fn restore_with(
        &self,
        process: u32,
        current: Option<AccentPolicy>,
        restore: impl FnOnce(AccentPolicy) -> bool,
    ) -> bool {
        if self.process != process {
            return false;
        }
        let Some(current) = current else {
            return true;
        };
        self.owns(process, current) && !restore(self.original)
    }
}

unsafe fn window_process(hwnd: HWND) -> u32 {
    let mut process = 0;
    GetWindowThreadProcessId(hwnd, &mut process);
    process
}

// Screen coordinates may be negative; clamp the group to the actual parent
// client area rather than the full monitor/taskbar width.
fn centered_group_left(
    taskbar_left: i32,
    taskbar_right: i32,
    available_left: i32,
    available_right: i32,
    group_width: i32,
) -> Option<i32> {
    let width = available_right.checked_sub(available_left)?;
    if group_width <= 0 || width < group_width.checked_add(16)? {
        return None;
    }
    let visual = i64::from(taskbar_left)
        + (i64::from(taskbar_right) - i64::from(taskbar_left) - i64::from(group_width)) / 2;
    Some(visual.clamp(
        i64::from(available_left),
        i64::from(available_right - group_width),
    ) as i32)
}

fn centered_band_header(
    taskbar: (i32, i32),
    band: (i32, i32),
    reserved_right: i32,
    original: u32,
    min_child: u32,
    span: ButtonSpan,
) -> Option<u32> {
    let original = i32::try_from(original).ok()?;
    let min_child = i32::try_from(min_child).ok()?;
    let left = band.0.checked_add(original)?.checked_add(span.leading)?;
    // Keep both the buttons and the native child's minimum width reachable.
    let right = reserved_right.min(band.1).min(
        band.1
            .checked_sub(min_child)?
            .checked_add(span.leading)?
            .checked_add(span.width)?,
    );
    let target = centered_group_left(taskbar.0, taskbar.1, left, right, span.width)?;
    u32::try_from(target.checked_sub(span.leading)?.checked_sub(band.0)?).ok()
}

#[repr(C)]
struct WindowCompositionAttributeData {
    attribute: i32,
    data: *mut c_void,
    size: usize,
}

type SetWindowCompositionAttributeFn =
    unsafe extern "system" fn(HWND, *mut WindowCompositionAttributeData) -> i32;
type GetWindowCompositionAttributeFn =
    unsafe extern "system" fn(HWND, *mut WindowCompositionAttributeData) -> i32;

type ReleaseFn = unsafe extern "system" fn(*mut c_void) -> u32;
type GetChildCountFn = unsafe extern "system" fn(*mut c_void, *mut i32) -> i32;
type AccLocationFn =
    unsafe extern "system" fn(*mut c_void, *mut i32, *mut i32, *mut i32, *mut i32, VARIANT) -> i32;

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

fn reg_command() -> Command {
    let mut command = Command::new("reg.exe");
    command.creation_flags(CREATE_NO_WINDOW);
    command
}

fn preference_enabled_for(name: &str) -> bool {
    reg_command()
        .args(["query", SETTINGS_KEY, "/v", name])
        .output()
        .map(|output| {
            output.status.success()
                && String::from_utf8_lossy(&output.stdout)
                    .split_whitespace()
                    .any(|value| value.eq_ignore_ascii_case("0x1"))
        })
        .unwrap_or(false)
}

fn set_preference_for(name: &str, enabled: bool) -> Result<(), String> {
    let value = if enabled { "1" } else { "0" };
    let status = reg_command()
        .args([
            "add",
            SETTINGS_KEY,
            "/v",
            name,
            "/t",
            "REG_DWORD",
            "/d",
            value,
            "/f",
        ])
        .status()
        .map_err(|error| format!("Could not update taskbar preference: {error}"))?;

    if status.success() {
        Ok(())
    } else {
        Err("Windows rejected the taskbar preference change".to_owned())
    }
}

pub fn preference_enabled() -> bool {
    preference_enabled_for(CENTER_VALUE)
}

pub fn set_preference(enabled: bool) -> Result<(), String> {
    set_preference_for(CENTER_VALUE, enabled)
}

pub fn acrylic_preference_enabled() -> bool {
    preference_enabled_for(ACRYLIC_VALUE)
}

pub fn set_acrylic_preference(enabled: bool) -> Result<(), String> {
    set_preference_for(ACRYLIC_VALUE, enabled)
}

pub unsafe fn initialize() {
    let result = CoInitializeEx(null(), COINIT_APARTMENTTHREADED as u32);
    if result >= 0 {
        COM_INITIALIZED.store(true, Ordering::Release);
    }
}

pub unsafe fn shutdown() {
    stop_layout_watch();
    rebar::release();
    if COM_INITIALIZED.swap(false, Ordering::AcqRel) {
        CoUninitialize();
    }
}

unsafe extern "system" fn taskbar_layout_changed(
    _hook: HWINEVENTHOOK,
    event: u32,
    hwnd: HWND,
    object: i32,
    child: i32,
    _thread: u32,
    _time: u32,
) {
    // MSAA can reenter during measurement. Never measure or move Explorer from
    // this callback; coalesce its burst of events into one UI-thread message.
    let Ok(mut watch) = LAYOUT_WATCH.try_lock() else {
        return;
    };
    let Some(watch) = watch.as_mut() else {
        return;
    };
    let mut destroyed_list = false;
    if event == EVENT_OBJECT_DESTROY && object == OBJID_WINDOW && child == 0 {
        if let Ok(mut cached) = BUTTON_SPAN.try_lock() {
            if cached.is_some_and(|span| {
                span.window == hwnd || span.parent == hwnd || watch.taskbar == hwnd
            }) {
                *cached = None;
                destroyed_list = true;
            }
        }
    }
    if !watch.queued
        && hwnd != 0
        && (destroyed_list || hwnd == watch.taskbar || IsChild(watch.taskbar, hwnd) != 0)
    {
        watch.queued = PostMessageW(watch.receiver, watch.message, 0, 0) != 0;
    }
}

pub unsafe fn watch_layout(receiver: HWND, message: u32) {
    let taskbar = FindWindowW(wide("Shell_TrayWnd").as_ptr(), null());
    let process = window_process(taskbar);
    let Ok(mut saved) = LAYOUT_WATCH.lock() else {
        return;
    };
    if saved.as_ref().is_some_and(|watch| {
        watch.taskbar == taskbar && watch.process == process && watch.receiver == receiver
    }) {
        return;
    }
    if let Some(watch) = saved.take() {
        for hook in watch.hooks {
            UnhookWinEvent(hook);
        }
    }
    if taskbar == 0 || process == 0 {
        return;
    }
    let flags = WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS;
    let hooks = [
        SetWinEventHook(
            EVENT_OBJECT_CREATE,
            EVENT_OBJECT_REORDER,
            0,
            Some(taskbar_layout_changed),
            process,
            0,
            flags,
        ),
        SetWinEventHook(
            EVENT_OBJECT_LOCATIONCHANGE,
            EVENT_OBJECT_LOCATIONCHANGE,
            0,
            Some(taskbar_layout_changed),
            process,
            0,
            flags,
        ),
    ];
    if hooks.contains(&0) {
        for hook in hooks.into_iter().filter(|hook| *hook != 0) {
            UnhookWinEvent(hook);
        }
        // The existing slow timer still works and will retry registration.
        return;
    }
    *saved = Some(LayoutWatch {
        taskbar,
        process,
        receiver,
        message,
        hooks,
        queued: false,
    });
}

pub fn acknowledge_layout_change() {
    if let Ok(mut watch) = LAYOUT_WATCH.lock() {
        if let Some(watch) = watch.as_mut() {
            watch.queued = false;
        }
    }
}

pub unsafe fn stop_layout_watch() {
    if let Ok(mut saved) = LAYOUT_WATCH.lock() {
        if let Some(watch) = saved.take() {
            for hook in watch.hooks {
                UnhookWinEvent(hook);
            }
        }
    }
    if let Ok(mut span) = BUTTON_SPAN.lock() {
        *span = None;
    }
}

unsafe fn composition_functions() -> Option<(
    SetWindowCompositionAttributeFn,
    GetWindowCompositionAttributeFn,
)> {
    let user32 = GetModuleHandleW(wide("user32.dll").as_ptr());
    if user32 == 0 {
        return None;
    }

    let set = GetProcAddress(user32, c"SetWindowCompositionAttribute".as_ptr().cast())?;
    let get = GetProcAddress(user32, c"GetWindowCompositionAttribute".as_ptr().cast())?;
    Some((
        transmute::<unsafe extern "system" fn() -> isize, SetWindowCompositionAttributeFn>(set),
        transmute::<unsafe extern "system" fn() -> isize, GetWindowCompositionAttributeFn>(get),
    ))
}

unsafe fn taskbar_windows() -> Vec<HWND> {
    let mut windows = Vec::new();
    let primary = FindWindowW(wide("Shell_TrayWnd").as_ptr(), null());
    if primary != 0 {
        windows.push(primary);
    }

    let secondary_class = wide("Shell_SecondaryTrayWnd");
    let mut after = 0;
    loop {
        let window = FindWindowExW(0, after, secondary_class.as_ptr(), null());
        if window == 0 {
            break;
        }
        windows.push(window);
        after = window;
    }
    windows
}

unsafe fn read_accent_policy(
    hwnd: HWND,
    get: GetWindowCompositionAttributeFn,
) -> Option<AccentPolicy> {
    let mut policy = AccentPolicy {
        state: ACCENT_DISABLED,
        flags: 0,
        gradient_color: 0,
        animation_id: 0,
    };
    let mut data = WindowCompositionAttributeData {
        attribute: WCA_ACCENT_POLICY,
        data: &mut policy as *mut _ as *mut c_void,
        size: size_of::<AccentPolicy>(),
    };
    (get(hwnd, &mut data) != 0).then_some(policy)
}

unsafe fn write_accent_policy(
    hwnd: HWND,
    set: SetWindowCompositionAttributeFn,
    policy: &mut AccentPolicy,
) -> bool {
    let mut data = WindowCompositionAttributeData {
        attribute: WCA_ACCENT_POLICY,
        data: policy as *mut _ as *mut c_void,
        size: size_of::<AccentPolicy>(),
    };
    set(hwnd, &mut data) != 0
}

pub unsafe fn apply_acrylic() -> bool {
    let Some((set, get)) = composition_functions() else {
        return false;
    };
    let originals = ORIGINAL_TASKBAR_POLICIES.get_or_init(|| Mutex::new(Vec::new()));
    let Ok(mut originals) = originals.lock() else {
        return false;
    };

    let mut applied = false;
    let windows = taskbar_windows();
    originals.retain(|saved| {
        windows.contains(&saved.window) && window_process(saved.window) == saved.process
    });
    for hwnd in windows {
        let process = window_process(hwnd);
        let Some(current) = read_accent_policy(hwnd, get) else {
            // Never invent an original state: restoring it would damage the
            // user's theme. Retry on a later low-frequency check instead.
            continue;
        };
        if process == 0 {
            continue;
        }
        let previous = originals.iter().position(|saved| saved.window == hwnd);
        if previous.is_some_and(|index| originals[index].owns(process, current)) {
            applied = true;
            continue;
        }

        let mut policy = AccentPolicy {
            state: ACCENT_ENABLE_ACRYLIC_BLUR_BEHIND,
            flags: 2,
            gradient_color: TASKBAR_ACRYLIC_TINT,
            animation_id: 0,
        };
        let success = if write_accent_policy(hwnd, set, &mut policy) {
            true
        } else {
            policy.state = ACCENT_ENABLE_BLUR_BEHIND;
            write_accent_policy(hwnd, set, &mut policy)
        };
        if success {
            // Explorer/theme changes become the new baseline; never restore a
            // stale theme after a remote-session or display transition.
            let saved = AccentPlacement {
                window: hwnd,
                process,
                original: current,
                applied: policy,
            };
            if let Some(index) = previous {
                originals[index] = saved;
            } else {
                originals.push(saved);
            }
            applied = true;
        }
    }
    applied
}

pub unsafe fn restore_acrylic() -> bool {
    let Some((set, get)) = composition_functions() else {
        return false;
    };
    let originals = ORIGINAL_TASKBAR_POLICIES.get_or_init(|| Mutex::new(Vec::new()));
    let Ok(mut originals) = originals.lock() else {
        return false;
    };

    if originals.is_empty() {
        return true;
    }
    let windows = taskbar_windows();
    originals.retain_mut(|saved| {
        if !windows.contains(&saved.window) || window_process(saved.window) != saved.process {
            return false;
        }
        saved.restore_with(
            window_process(saved.window),
            read_accent_policy(saved.window, get),
            |mut policy| write_accent_policy(saved.window, set, &mut policy),
        )
    });
    originals.is_empty()
}

unsafe fn find_task_list() -> Option<(HWND, HWND, HWND)> {
    let taskbar = FindWindowW(wide("Shell_TrayWnd").as_ptr(), null());
    if taskbar == 0 {
        return None;
    }
    let rebar = FindWindowExW(taskbar, 0, wide("ReBarWindow32").as_ptr(), null());
    let task_switch = FindWindowExW(rebar, 0, wide("MSTaskSwWClass").as_ptr(), null());
    let task_list = FindWindowExW(task_switch, 0, wide("MSTaskListWClass").as_ptr(), null());
    if rebar == 0 || task_switch == 0 || task_list == 0 {
        None
    } else {
        Some((taskbar, task_switch, task_list))
    }
}

unsafe fn task_button_bounds(task_list: HWND, taskbar_rect: RECT) -> Option<(i32, i32)> {
    let mut object = null_mut();
    let result = AccessibleObjectFromWindow(
        task_list,
        OBJID_CLIENT as u32,
        &IID_IACCESSIBLE,
        &mut object,
    );
    if result < 0 || object.is_null() {
        return None;
    }

    let vtable = *(object as *mut *const usize);
    let release: ReleaseFn = transmute(*vtable.add(2));
    let get_child_count: GetChildCountFn = transmute(*vtable.add(8));
    let acc_location: AccLocationFn = transmute(*vtable.add(22));

    let mut count = 0;
    if get_child_count(object, &mut count) < 0 || count <= 0 {
        release(object);
        return None;
    }

    let mut first = i32::MAX;
    let mut last = i32::MIN;
    for id in 1..=count {
        let mut child: VARIANT = zeroed();
        child.Anonymous.Anonymous.vt = VT_I4;
        child.Anonymous.Anonymous.Anonymous.lVal = id;

        let mut left = 0;
        let mut top = 0;
        let mut width = 0;
        let mut height = 0;
        if acc_location(object, &mut left, &mut top, &mut width, &mut height, child) >= 0
            && width > 0
            && height > 0
            && top < taskbar_rect.bottom
            && top.saturating_add(height) > taskbar_rect.top
        {
            first = first.min(left);
            last = last.max(left.saturating_add(width));
        }
    }
    release(object);

    if first < last {
        Some((first, last))
    } else {
        None
    }
}

unsafe fn reduce_right_boundary(taskbar: HWND, class_name: &str, left: i32, right: &mut i32) {
    let window = FindWindowExW(taskbar, 0, wide(class_name).as_ptr(), null());
    let mut rect: RECT = zeroed();
    if window != 0
        && GetWindowRect(window, &mut rect) != 0
        && rect.right > rect.left
        && rect.left > left
    {
        *right = (*right).min(rect.left);
    }
}

pub unsafe fn apply_centered() -> bool {
    if !COM_INITIALIZED.load(Ordering::Acquire) {
        return false;
    }
    let Some((taskbar, task_switch, task_list)) = find_task_list() else {
        return false;
    };
    let Some(band) = rebar::task_band(GetParent(task_switch), task_switch) else {
        return false;
    };
    let mut taskbar_rect: RECT = zeroed();
    let mut switch_rect: RECT = zeroed();
    let mut list_rect: RECT = zeroed();
    if GetWindowRect(taskbar, &mut taskbar_rect) == 0
        || GetWindowRect(task_switch, &mut switch_rect) == 0
        || GetWindowRect(task_list, &mut list_rect) == 0
        || taskbar_rect.right - taskbar_rect.left <= taskbar_rect.bottom - taskbar_rect.top
    {
        return false;
    }

    let bounds = task_button_bounds(task_list, taskbar_rect);
    let mut latest_list: RECT = zeroed();
    let mut latest_switch: RECT = zeroed();
    if GetWindowRect(task_list, &mut latest_list) == 0
        || GetWindowRect(task_switch, &mut latest_switch) == 0
    {
        return false;
    }
    let stable = latest_list.left == list_rect.left
        && latest_list.right == list_rect.right
        && latest_list.top == list_rect.top
        && latest_list.bottom == list_rect.bottom
        && latest_switch.left == switch_rect.left
        && latest_switch.right == switch_rect.right;
    switch_rect = latest_switch;
    let measured = bounds
        .filter(|(first, _)| stable && *first >= switch_rect.left)
        .map(|(first, last)| ButtonSpan {
            parent: task_switch,
            window: task_list,
            process: band.process,
            width: last - first,
            leading: first - switch_rect.left,
        });
    let span = if let Ok(mut cached) = BUTTON_SPAN.lock() {
        remember_button_span(&mut cached, task_switch, task_list, band.process, measured)
    } else {
        measured
    };
    let Some(span) = span else {
        return false;
    };

    let Ok(mut saved) = BAND_PLACEMENT.try_lock() else {
        return false;
    };
    let original = saved
        .filter(|saved| saved.owns(band))
        .map_or(band.header, |saved| saved.original);
    let Ok(header) = i32::try_from(band.header) else {
        return false;
    };
    let Some(band_left) = switch_rect.left.checked_sub(header) else {
        return false;
    };
    let mut available_right = switch_rect.right.min(taskbar_rect.right);
    for class_name in ["TrayNotifyWnd", "HiCodex.TaskbarWidget", "DynamicContent2"] {
        reduce_right_boundary(taskbar, class_name, band_left, &mut available_right);
    }
    let target = centered_band_header(
        (taskbar_rect.left, taskbar_rect.right),
        (band_left, switch_rect.right),
        available_right,
        original,
        band.min_child,
        span,
    );
    let Some(target) = target else {
        drop(saved);
        return restore_left();
    };
    if target == band.header {
        return true;
    }
    let previous = *saved;
    // Record ownership before sending: a timed-out control may still apply it.
    *saved = Some(BandPlacement::after_change(previous, band, target));
    if rebar::set_header(band, target) {
        return true;
    }
    if rebar::task_band(band.window, band.child)
        .is_some_and(|current| current.same_owner(band) && current.header == band.header)
    {
        *saved = previous;
    }
    false
}

pub unsafe fn restore_left() -> bool {
    let Ok(mut saved) = BAND_PLACEMENT.try_lock() else {
        return false;
    };
    let Some(placement) = *saved else {
        rebar::release();
        return true;
    };
    if window_process(placement.band.window) != placement.band.process
        || GetParent(placement.band.child) != placement.band.window
    {
        *saved = None;
        rebar::release();
        return true;
    }
    let Some(current) = rebar::task_band(placement.band.window, placement.band.child) else {
        return false;
    };
    if !placement.owns(current) {
        *saved = None;
        rebar::release();
        return true;
    }
    if !rebar::set_header(current, placement.original) {
        return false;
    }
    *saved = None;
    rebar::release();
    true
}

pub unsafe fn diagnose_layout() -> Option<String> {
    let (taskbar, child, list) = find_task_list()?;
    let band = rebar::task_band(GetParent(child), child)?;
    let mut taskbar_rect: RECT = zeroed();
    let mut child_rect: RECT = zeroed();
    let mut list_rect: RECT = zeroed();
    if GetWindowRect(taskbar, &mut taskbar_rect) == 0
        || GetWindowRect(child, &mut child_rect) == 0
        || GetWindowRect(list, &mut list_rect) == 0
    {
        return None;
    }
    let result = format!(
        "Native task band: id={}, header={}, min_child={}; taskbar=({}, {}); band=({}, {}); list=({}, {})",
        band.id, band.header, band.min_child, taskbar_rect.left, taskbar_rect.right,
        child_rect.left, child_rect.right, list_rect.left, list_rect.right,
    );
    rebar::release();
    Some(result)
}

pub unsafe fn native_centering_available() -> bool {
    let Some((taskbar, child, _)) = find_task_list() else {
        return false;
    };
    let mut rect: RECT = zeroed();
    GetWindowRect(taskbar, &mut rect) != 0
        && rect.right - rect.left > rect.bottom - rect.top
        && rebar::task_band(GetParent(child), child).is_some()
}

#[cfg(test)]
mod placement_tests {
    use super::{
        centered_band_header, centered_group_left, remember_button_span, AccentPlacement,
        AccentPolicy, Band, BandPlacement, ButtonSpan,
    };

    #[test]
    fn temporary_missing_buttons_keep_last_geometry_until_window_is_recreated() {
        let span = ButtonSpan {
            parent: 1,
            window: 2,
            process: 42,
            width: 480,
            leading: 0,
        };
        let mut cached = None;
        remember_button_span(&mut cached, 1, 2, 42, Some(span));
        // Multiple failed reads during a tray relayout preserve the same target.
        for _ in 0..8 {
            let current = remember_button_span(&mut cached, 1, 2, 42, None).unwrap();
            assert_eq!(
                centered_group_left(0, 1920, 60, 1500, current.width),
                Some(720)
            );
        }
        let changed = ButtonSpan { width: 528, ..span };
        assert_eq!(
            remember_button_span(&mut cached, 1, 2, 42, Some(changed))
                .unwrap()
                .width,
            528
        );
        for (parent, window, process) in [(1, 2, 99), (1, 9, 42), (9, 2, 42)] {
            let mut cached = Some(span);
            assert!(remember_button_span(&mut cached, parent, window, process, None).is_none());
            assert!(cached.is_none());
        }
    }

    #[test]
    fn taskbar_layout_event_bursts_are_coalesced_and_rearmed() {
        use super::*;
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            CreateWindowExW, DestroyWindow, PeekMessageW, HWND_MESSAGE, MSG, PM_REMOVE, WM_APP,
        };
        unsafe {
            // A private message-only test window; never touches the real taskbar.
            let receiver = CreateWindowExW(
                0,
                wide("STATIC").as_ptr(),
                null(),
                0,
                0,
                0,
                0,
                0,
                HWND_MESSAGE,
                0,
                0,
                null(),
            );
            assert_ne!(receiver, 0);
            let message = WM_APP + 88;
            *LAYOUT_WATCH.lock().unwrap() = Some(LayoutWatch {
                taskbar: receiver,
                process: 42,
                receiver,
                message,
                hooks: [0, 0],
                queued: false,
            });
            for _ in 0..40 {
                taskbar_layout_changed(0, EVENT_OBJECT_LOCATIONCHANGE, receiver, 0, 0, 0, 0);
            }
            let mut received: MSG = zeroed();
            assert_ne!(
                PeekMessageW(&mut received, receiver, message, message, PM_REMOVE),
                0
            );
            assert_eq!(
                PeekMessageW(&mut received, receiver, message, message, PM_REMOVE),
                0
            );
            acknowledge_layout_change();
            taskbar_layout_changed(0, EVENT_OBJECT_REORDER, receiver, 0, 0, 0, 0);
            assert_ne!(
                PeekMessageW(&mut received, receiver, message, message, PM_REMOVE),
                0
            );
            *BUTTON_SPAN.lock().unwrap() = Some(ButtonSpan {
                parent: receiver,
                window: receiver,
                process: 42,
                width: 480,
                leading: 0,
            });
            // Removing a button must retain the containing window's cache.
            taskbar_layout_changed(0, EVENT_OBJECT_DESTROY, receiver, OBJID_CLIENT, 1, 0, 0);
            assert!(BUTTON_SPAN.lock().unwrap().is_some());
            taskbar_layout_changed(0, EVENT_OBJECT_DESTROY, receiver, OBJID_WINDOW, 0, 0, 0);
            assert!(BUTTON_SPAN.lock().unwrap().is_none());
            *LAYOUT_WATCH.lock().unwrap() = None;
            taskbar_layout_changed(0, EVENT_OBJECT_LOCATIONCHANGE, receiver, 0, 0, 0, 0);
            assert_eq!(
                PeekMessageW(&mut received, receiver, message, message, PM_REMOVE),
                0
            );
            DestroyWindow(receiver);
        }
    }

    #[test]
    fn native_header_bounds_preserve_child_minimums_and_handle_display_resizing() {
        let span = ButtonSpan {
            parent: 1,
            window: 2,
            process: 42,
            width: 480,
            leading: 0,
        };
        assert_eq!(
            centered_band_header((0, 1920), (60, 1700), 1500, 0, 48, span),
            Some(660)
        );
        assert_eq!(
            centered_band_header((-1920, 0), (-1860, -220), -420, 0, 48, span),
            Some(660)
        );
        assert_eq!(
            centered_band_header((0, 640), (60, 440), 420, 0, 48, span),
            None
        );
        // A native child may need more room than the measured icon buttons.
        assert_eq!(
            centered_band_header((0, 1920), (60, 1500), 1500, 0, 1000, span),
            Some(440)
        );
        assert_eq!(
            centered_band_header((0, 1920), (60, 1500), 1500, 0, u32::MAX, span),
            None
        );
    }

    #[test]
    fn centering_fits_resized_and_negative_coordinate_desktops() {
        for desktop_left in [-3840, -1920, 0, 1920] {
            for width in [640, 800, 1024, 1920, 2560, 3840] {
                for scale in [1, 2, 3] {
                    for buttons in [1, 4, 12, 30] {
                        let left = desktop_left + 60 * scale;
                        let right = desktop_left + width - 200 * scale;
                        let group = buttons * 48 * scale;
                        let result = centered_group_left(
                            desktop_left,
                            desktop_left + width,
                            left,
                            right,
                            group,
                        );
                        if right - left >= group + 16 {
                            let target = result.expect("fitting group should have a target");
                            assert!(target >= left);
                            assert!(target + group <= right);
                        } else {
                            assert_eq!(result, None);
                        }
                    }
                }
            }
        }
        assert_eq!(centered_group_left(0, 1920, 60, 800, 400), Some(400));
        assert_eq!(centered_group_left(0, 640, 60, 440, 500), None);
        assert_eq!(centered_group_left(0, 640, 60, 440, 0), None);
    }

    fn accent_fixture() -> AccentPlacement {
        AccentPlacement {
            window: 1,
            process: 42,
            original: AccentPolicy {
                state: 0,
                flags: 0,
                gradient_color: 0,
                animation_id: 0,
            },
            applied: AccentPolicy {
                state: 4,
                flags: 2,
                gradient_color: 123,
                animation_id: 0,
            },
        }
    }

    #[test]
    fn accent_restoration_retries_failures_and_retires_successes() {
        let saved = accent_fixture();
        assert!(saved.restore_with(42, None, |_| panic!("must not write on read failure")));
        assert!(saved.restore_with(42, Some(saved.applied), |_| false));
        assert!(!saved.restore_with(42, Some(saved.applied), |policy| {
            assert_eq!(policy, saved.original);
            true
        }));
    }

    #[test]
    fn accent_restoration_does_not_overwrite_external_styles_or_reused_handles() {
        let saved = accent_fixture();
        assert!(!saved.restore_with(42, Some(saved.original), |_| panic!("external style wins")));
        assert!(!saved.restore_with(99, Some(saved.applied), |_| panic!(
            "new Explorer process wins"
        )));
        assert!(!saved.restore_with(99, None, |_| panic!("dead owner must not be restored")));
        assert!(saved.owns(42, saved.applied));
        assert!(!saved.owns(99, saved.applied));
    }

    #[test]
    fn repeated_centering_preserves_original_band_header() {
        let band = Band {
            window: 1,
            process: 42,
            child: 2,
            id: 7,
            header: 8,
            min_child: 48,
        };
        let first = BandPlacement::after_change(None, band, 400);
        let current = Band {
            header: 400,
            ..band
        };
        let next = BandPlacement::after_change(Some(first), current, 440);
        assert_eq!(next.original, 8);
        assert!(next.owns(Band {
            header: 440,
            ..band
        }));
    }

    #[test]
    fn external_band_changes_and_recreated_windows_are_not_owned() {
        let band = Band {
            window: 1,
            process: 42,
            child: 2,
            id: 7,
            header: 8,
            min_child: 48,
        };
        let saved = BandPlacement::after_change(None, band, 400);
        for current in [
            Band {
                header: 120,
                ..band
            },
            Band {
                header: 400,
                process: 99,
                ..band
            },
            Band {
                header: 400,
                child: 9,
                ..band
            },
            Band {
                header: 400,
                id: 9,
                ..band
            },
        ] {
            assert!(!saved.owns(current));
            assert_eq!(
                BandPlacement::after_change(Some(saved), current, 440).original,
                current.header
            );
        }
    }
}
