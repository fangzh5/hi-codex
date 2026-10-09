use std::ffi::c_void;
use std::mem::{size_of, transmute, zeroed};
use std::os::windows::process::CommandExt;
use std::process::Command;
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use windows_sys::core::GUID;
use windows_sys::Win32::Foundation::{HWND, POINT, RECT};
use windows_sys::Win32::Graphics::Gdi::{ClientToScreen, ScreenToClient};
use windows_sys::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};
use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;
use windows_sys::Win32::System::Variant::{VARIANT, VT_I4};
use windows_sys::Win32::UI::Accessibility::AccessibleObjectFromWindow;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    FindWindowExW, FindWindowW, GetClientRect, GetWindowRect, GetWindowThreadProcessId,
    SetWindowPos, OBJID_CLIENT, SWP_NOACTIVATE, SWP_NOSENDCHANGING, SWP_NOSIZE, SWP_NOZORDER,
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
static TASK_LIST_PLACEMENT: Mutex<Option<TaskListPlacement>> = Mutex::new(None);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TaskListPlacement {
    parent: HWND,
    window: HWND,
    original: (i32, i32),
    applied: (i32, i32),
}

impl TaskListPlacement {
    fn owns_position(&self, parent: HWND, window: HWND, current: (i32, i32)) -> bool {
        self.parent == parent && self.window == window && self.applied == current
    }

    fn after_move(
        previous: Option<Self>,
        parent: HWND,
        window: HWND,
        current: (i32, i32),
        applied: (i32, i32),
    ) -> Self {
        // If Explorer or another tool moved the list, preserve that newer
        // position as the baseline for our next move.
        let original = previous
            .filter(|saved| saved.owns_position(parent, window, current))
            .map_or(current, |saved| saved.original);
        Self {
            parent,
            window,
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

fn recovery_offset(saved: i32, extent: i32) -> i32 {
    // Do not clamp an obsolete RDP offset to the last visible pixel: that
    // would leave nearly the entire icon list outside its parent.
    if saved >= 0 && saved < extent {
        saved
    } else {
        0
    }
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
        restore_left();
        return false;
    };
    let group_width = last_button - first_button;
    let leading_margin = first_button - list_rect.left;

    let mut client: RECT = zeroed();
    let mut origin = POINT { x: 0, y: 0 };
    if GetClientRect(task_switch, &mut client) == 0 || ClientToScreen(task_switch, &mut origin) == 0
    {
        return false;
    }
    let available_left = origin.x.max(taskbar_rect.left);
    let mut available_right = origin
        .x
        .saturating_add(client.right)
        .min(taskbar_rect.right);
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

    let Some(target_group_left) = centered_group_left(
        taskbar_rect.left,
        taskbar_rect.right,
        available_left,
        available_right,
        group_width,
    ) else {
        return restore_left();
    };
    let target_list_left = target_group_left - leading_margin;
    let mut current = POINT {
        x: list_rect.left,
        y: list_rect.top,
    };
    let mut target = POINT {
        x: target_list_left,
        y: list_rect.top,
    };
    if ScreenToClient(task_switch, &mut current) == 0
        || ScreenToClient(task_switch, &mut target) == 0
    {
        return false;
    }
    let current = (current.x, current.y);
    // Reject, rather than silently shift, a target that would clip icons.
    if target.x < 0
        || target
            .x
            .saturating_add(leading_margin)
            .saturating_add(group_width)
            > client.right
    {
        return restore_left();
    }
    let target = (target.x, target.y);
    if current == target {
        return true;
    }
    let Ok(mut saved) = TASK_LIST_PLACEMENT.lock() else {
        return false;
    };
    if SetWindowPos(
        task_list,
        0,
        target.0,
        target.1,
        0,
        0,
        SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOZORDER | SWP_NOSENDCHANGING,
    ) == 0
    {
        return false;
    }
    *saved = Some(TaskListPlacement::after_move(
        *saved,
        task_switch,
        task_list,
        current,
        target,
    ));
    true
}

pub unsafe fn restore_left() -> bool {
    let Ok(mut saved) = TASK_LIST_PLACEMENT.lock() else {
        return false;
    };
    let Some(placement) = *saved else {
        return true;
    };
    let Some((_, task_switch, task_list)) = find_task_list() else {
        *saved = None;
        return true;
    };
    let mut rect: RECT = zeroed();
    if GetWindowRect(task_list, &mut rect) == 0 {
        return false;
    }
    let mut current = POINT {
        x: rect.left,
        y: rect.top,
    };
    if ScreenToClient(task_switch, &mut current) == 0 {
        return false;
    }
    if !placement.owns_position(task_switch, task_list, (current.x, current.y)) {
        *saved = None;
        return true;
    }
    let mut parent_rect: RECT = zeroed();
    if GetClientRect(task_switch, &mut parent_rect) == 0 {
        return false;
    }
    // A saved offset from a larger RDP desktop must not put the list outside
    // the resized parent. The native task list ordinarily starts at zero.
    let restore_x = recovery_offset(placement.original.0, parent_rect.right);
    let restore_y = recovery_offset(placement.original.1, parent_rect.bottom);
    if SetWindowPos(
        task_list,
        0,
        restore_x,
        restore_y,
        0,
        0,
        SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOZORDER | SWP_NOSENDCHANGING,
    ) == 0
    {
        return false;
    }
    *saved = None;
    true
}

#[cfg(test)]
mod placement_tests {
    use super::{
        centered_group_left, recovery_offset, AccentPlacement, AccentPolicy, TaskListPlacement,
    };

    #[test]
    fn obsolete_remote_desktop_restore_offsets_return_to_origin() {
        assert_eq!(recovery_offset(1400, 640), 0);
        assert_eq!(recovery_offset(640, 640), 0);
        assert_eq!(recovery_offset(-10, 640), 0);
        assert_eq!(recovery_offset(30, 0), 0);
        assert_eq!(recovery_offset(40, 640), 40);
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
    fn repeated_centering_preserves_original_position() {
        let first = TaskListPlacement::after_move(None, 1, 2, (40, 3), (300, 3));
        let next = TaskListPlacement::after_move(Some(first), 1, 2, (300, 3), (350, 3));
        assert_eq!(next.original, (40, 3));
        assert!(next.owns_position(1, 2, (350, 3)));
    }

    #[test]
    fn external_move_or_recreated_window_is_not_owned() {
        let saved = TaskListPlacement::after_move(None, 1, 2, (40, 3), (300, 3));
        assert!(!saved.owns_position(1, 2, (120, 3)));
        assert!(!saved.owns_position(1, 9, (300, 3)));
        assert!(!saved.owns_position(9, 2, (300, 3)));
        let next = TaskListPlacement::after_move(Some(saved), 1, 2, (120, 3), (350, 3));
        assert_eq!(next.original, (120, 3));
        let recreated = TaskListPlacement::after_move(Some(saved), 1, 9, (80, 0), (350, 0));
        assert_eq!(recreated.original, (80, 0));
    }
}
