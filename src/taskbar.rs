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
use windows_sys::Win32::UI::Accessibility::AccessibleObjectFromWindow;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    FindWindowExW, FindWindowW, GetWindowRect, SetWindowPos, OBJID_CLIENT, SWP_ASYNCWINDOWPOS,
    SWP_NOACTIVATE, SWP_NOSENDCHANGING, SWP_NOSIZE, SWP_NOZORDER,
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
static ORIGINAL_TASKBAR_POLICIES: OnceLock<Mutex<Vec<(HWND, AccentPolicy)>>> = OnceLock::new();

#[repr(C)]
#[derive(Clone, Copy)]
struct AccentPolicy {
    state: i32,
    flags: i32,
    gradient_color: u32,
    animation_id: i32,
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
    if COM_INITIALIZED.swap(false, Ordering::AcqRel) {
        CoUninitialize();
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
    for hwnd in taskbar_windows() {
        if !originals.iter().any(|(saved, _)| *saved == hwnd) {
            let original = read_accent_policy(hwnd, get).unwrap_or(AccentPolicy {
                state: ACCENT_DISABLED,
                flags: 0,
                gradient_color: 0,
                animation_id: 0,
            });
            originals.push((hwnd, original));
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
        applied |= success;
    }
    applied
}

pub unsafe fn restore_acrylic() -> bool {
    let Some((set, _)) = composition_functions() else {
        return false;
    };
    let originals = ORIGINAL_TASKBAR_POLICIES.get_or_init(|| Mutex::new(Vec::new()));
    let Ok(mut originals) = originals.lock() else {
        return false;
    };

    let mut restored = originals.is_empty();
    for (hwnd, mut policy) in originals.drain(..) {
        restored |= write_accent_policy(hwnd, set, &mut policy);
    }
    restored
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
            && left >= taskbar_rect.left
            && left < taskbar_rect.right
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

    let Some((first_button, last_button)) = task_button_bounds(task_list, taskbar_rect) else {
        return false;
    };
    let group_width = last_button - first_button;
    let leading_margin = first_button - list_rect.left;

    let available_left = switch_rect.left;
    let mut available_right = taskbar_rect.right;
    reduce_right_boundary(
        taskbar,
        "TrayNotifyWnd",
        available_left,
        &mut available_right,
    );
    reduce_right_boundary(
        taskbar,
        "HiCodex.TaskbarWidget",
        available_left,
        &mut available_right,
    );
    reduce_right_boundary(
        taskbar,
        "DynamicContent2",
        available_left,
        &mut available_right,
    );

    let available_width = available_right - available_left;
    if group_width <= 0 || available_width <= group_width + 16 {
        return restore_left();
    }

    let taskbar_width = taskbar_rect.right - taskbar_rect.left;
    let visual_center_left = taskbar_rect.left + (taskbar_width - group_width) / 2;
    let target_group_left = if visual_center_left >= available_left
        && visual_center_left + group_width <= available_right
    {
        visual_center_left
    } else {
        available_left + (available_width - group_width) / 2
    };
    let target_list_left = target_group_left - leading_margin;
    let local_x = (target_list_left - switch_rect.left).max(0);
    SetWindowPos(
        task_list,
        0,
        local_x,
        0,
        0,
        0,
        SWP_NOSIZE | SWP_ASYNCWINDOWPOS | SWP_NOACTIVATE | SWP_NOZORDER | SWP_NOSENDCHANGING,
    ) != 0
}

pub unsafe fn restore_left() -> bool {
    let Some((_, _, task_list)) = find_task_list() else {
        return false;
    };
    SetWindowPos(
        task_list,
        0,
        0,
        0,
        0,
        0,
        SWP_NOSIZE | SWP_ASYNCWINDOWPOS | SWP_NOACTIVATE | SWP_NOZORDER | SWP_NOSENDCHANGING,
    ) != 0
}
