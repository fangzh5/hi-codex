//! Bounded, asynchronous Explorer accessibility reads. Never call COM on the UI thread.
use std::ffi::c_void;
use std::mem::{transmute, zeroed};
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use windows_sys::core::GUID;
use windows_sys::Win32::Foundation::{HWND, RECT};
use windows_sys::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};
use windows_sys::Win32::System::Variant::{VARIANT, VT_I4};
use windows_sys::Win32::UI::Accessibility::AccessibleObjectFromWindow;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetWindowRect, GetWindowThreadProcessId, OBJID_CLIENT,
};

static GENERATION: AtomicU64 = AtomicU64::new(0);

pub fn invalidate() {
    GENERATION.fetch_add(1, Ordering::Relaxed);
}

const IID_IACCESSIBLE: GUID = GUID::from_u128(0x618736e0_3c3d_11cf_810c_00aa00389b71);
// Covers the existing two-second UI recovery interval, but rejects late replies.
const MAX_RESULT_AGE: Duration = Duration::from_secs(5);
const MEASUREMENT_BUDGET: Duration = Duration::from_millis(250);

unsafe fn window_process(window: HWND) -> u32 {
    let mut process = 0;
    GetWindowThreadProcessId(window, &mut process);
    process
}

struct Apartment;
impl Apartment {
    unsafe fn enter() -> Option<Self> {
        if CoInitializeEx(null(), COINIT_APARTMENTTHREADED as u32) >= 0 {
            Some(Self)
        } else {
            None
        }
    }
}
impl Drop for Apartment {
    fn drop(&mut self) {
        unsafe {
            CoUninitialize();
        }
    }
}

type ReleaseFn = unsafe extern "system" fn(*mut c_void) -> u32;
type GetChildCountFn = unsafe extern "system" fn(*mut c_void, *mut i32) -> i32;
type AccLocationFn =
    unsafe extern "system" fn(*mut c_void, *mut i32, *mut i32, *mut i32, *mut i32, VARIANT) -> i32;

// One outstanding request bounds both memory and thread use even if the
// accessibility provider never returns. The UI only polls; shutdown never joins.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct MeasurementRequest {
    window: HWND,
    process: u32,
    rect: [i32; 4],
    list_rect: [i32; 4],
    generation: u64,
}

struct ButtonReader {
    requests: std::sync::mpsc::SyncSender<MeasurementRequest>,
    results: std::sync::mpsc::Receiver<(MeasurementRequest, Option<(i32, i32)>)>,
    pending: Option<Instant>,
}

impl ButtonReader {
    fn spawn(
        measure: impl Fn(MeasurementRequest) -> Option<(i32, i32)> + Send + 'static,
    ) -> Option<Self> {
        let (requests, input) = std::sync::mpsc::sync_channel(1);
        let (output, results) = std::sync::mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("taskbar-accessibility".into())
            .spawn(move || {
                while let Ok(request) = input.recv() {
                    if output.send((request, measure(request))).is_err() {
                        break;
                    }
                }
            })
            .ok()?;
        Some(Self {
            requests,
            results,
            pending: None,
        })
    }

    fn poll(&mut self, request: MeasurementRequest) -> Option<(i32, i32)> {
        self.poll_at(request, Instant::now())
    }

    fn poll_at(&mut self, request: MeasurementRequest, now: Instant) -> Option<(i32, i32)> {
        let mut measured = None;
        if let Ok((completed, result)) = self.results.try_recv() {
            let fresh = self
                .pending
                .take()
                .is_some_and(|started| now.saturating_duration_since(started) <= MAX_RESULT_AGE);
            let code = if !fresh {
                1
            } else if completed != request {
                2
            } else if result.is_none() {
                3
            } else {
                0
            };
            crate::diagnostics::record(crate::diagnostics::Event::Measurement, code);
            if fresh && completed == request {
                measured = result;
            }
        }
        if self
            .pending
            .is_some_and(|started| now.saturating_duration_since(started) > MAX_RESULT_AGE)
        {
            crate::diagnostics::record(crate::diagnostics::Event::Measurement, 1);
        }
        // An expired request stays in flight until it actually returns. Never
        // replace blocked workers or accumulate requests in response to a timeout.
        if self.pending.is_none() && self.requests.try_send(request).is_ok() {
            self.pending = Some(now);
        }
        measured
    }
}

pub unsafe fn bounds(window: HWND, rect: RECT, process: u32) -> Option<(i32, i32)> {
    static READER: OnceLock<Mutex<Option<ButtonReader>>> = OnceLock::new();
    let reader = READER.get_or_init(|| {
        Mutex::new(ButtonReader::spawn(|request| unsafe {
            let _apartment = Apartment::enter()?;
            let rect = RECT {
                left: request.rect[0],
                top: request.rect[1],
                right: request.rect[2],
                bottom: request.rect[3],
            };
            if window_process(request.window) == request.process {
                let mut before: RECT = zeroed();
                let mut after: RECT = zeroed();
                let geometry = |r: RECT| [r.left, r.top, r.right, r.bottom];
                if GetWindowRect(request.window, &mut before) == 0
                    || geometry(before) != request.list_rect
                {
                    None
                } else {
                    let measured = measure_task_button_bounds(request.window, rect);
                    if GetWindowRect(request.window, &mut after) != 0
                        && geometry(after) == request.list_rect
                    {
                        measured
                    } else {
                        None
                    }
                }
            } else {
                None
            }
        }))
    });
    let mut list_rect: RECT = zeroed();
    if GetWindowRect(window, &mut list_rect) == 0 {
        return None;
    }
    reader.try_lock().ok()?.as_mut()?.poll(MeasurementRequest {
        window,
        process,
        generation: GENERATION.load(Ordering::Relaxed),
        rect: [rect.left, rect.top, rect.right, rect.bottom],
        list_rect: [
            list_rect.left,
            list_rect.top,
            list_rect.right,
            list_rect.bottom,
        ],
    })
}

unsafe fn measure_task_button_bounds(task_list: HWND, taskbar_rect: RECT) -> Option<(i32, i32)> {
    let started = Instant::now();
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
    // An invalid accessibility provider must not drive an unbounded COM loop.
    // Skip the whole measurement rather than centering a truncated icon group.
    if get_child_count(object, &mut count) < 0 || !(1..=512).contains(&count) {
        release(object);
        return None;
    }

    let mut first = i32::MAX;
    let mut last = i32::MIN;
    for id in 1..=count {
        if started.elapsed() >= MEASUREMENT_BUDGET {
            release(object);
            return None;
        }
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

    if first < last && started.elapsed() < MEASUREMENT_BUDGET {
        Some((first, last))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn accessibility_results_from_old_geometry_or_explorer_are_rejected() {
        use super::{ButtonReader, MeasurementRequest};
        use std::sync::mpsc::sync_channel;
        let request = MeasurementRequest {
            window: 1,
            process: 2,
            rect: [0; 4],
            list_rect: [0; 4],
            generation: 0,
        };
        for current in [
            MeasurementRequest {
                process: 3,
                ..request
            },
            MeasurementRequest {
                generation: 1,
                ..request
            },
            MeasurementRequest {
                window: 9,
                ..request
            },
            MeasurementRequest {
                list_rect: [1; 4],
                ..request
            },
        ] {
            let (requests, _input) = sync_channel(1);
            let (output, results) = sync_channel(1);
            let mut reader = ButtonReader {
                requests,
                results,
                pending: Some(std::time::Instant::now()),
            };
            output.send((request, Some((10, 90)))).unwrap();
            assert_eq!(reader.poll(current), None);
        }
    }

    #[test]
    fn blocked_accessibility_provider_never_blocks_or_queues_more_ui_requests() {
        use super::{ButtonReader, MeasurementRequest};
        use std::sync::mpsc::channel;
        use std::time::{Duration, Instant};
        let (started, ready) = channel();
        let (release, blocked) = channel();
        let mut reader = ButtonReader::spawn(move |_| {
            started.send(()).unwrap();
            blocked.recv().unwrap();
            Some((10, 90))
        })
        .unwrap();
        let request = MeasurementRequest {
            window: 1,
            process: 2,
            rect: [0; 4],
            list_rect: [0; 4],
            generation: 0,
        };
        assert_eq!(reader.poll(request), None);
        ready.recv_timeout(Duration::from_secs(2)).unwrap();
        let before = Instant::now();
        for _ in 0..10000 {
            assert_eq!(reader.poll(request), None);
        }
        assert!(before.elapsed() < Duration::from_secs(1));
        assert!(ready.try_recv().is_err());
        release.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        // The original request completes once the provider resumes.
        let (completed, result) = reader
            .results
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .unwrap();
        assert_eq!(completed, request);
        assert_eq!(result, Some((10, 90)));
        drop(reader);
    }

    #[test]
    fn late_results_are_rejected_even_if_window_and_geometry_are_unchanged() {
        let (requests, input) = std::sync::mpsc::sync_channel(1);
        let (output, results) = std::sync::mpsc::sync_channel(1);
        let started = Instant::now();
        let request = MeasurementRequest {
            window: 1,
            process: 2,
            rect: [0; 4],
            list_rect: [0; 4],
            generation: 0,
        };
        let mut reader = ButtonReader {
            requests,
            results,
            pending: Some(started),
        };
        output.send((request, Some((10, 90)))).unwrap();
        assert_eq!(
            reader.poll_at(request, started + MAX_RESULT_AGE + Duration::from_millis(1)),
            None
        );
        assert_eq!(input.try_recv().unwrap(), request);
    }

    #[test]
    fn dropping_reader_does_not_join_a_blocked_provider() {
        let (release, blocked) = std::sync::mpsc::channel();
        let (finished, finish) = std::sync::mpsc::channel();
        let mut reader = ButtonReader::spawn(move |_| {
            blocked.recv().unwrap();
            finished.send(()).unwrap();
            None
        })
        .unwrap();
        reader.poll(MeasurementRequest {
            window: 1,
            process: 2,
            rect: [0; 4],
            list_rect: [0; 4],
            generation: 0,
        });
        let start = Instant::now();
        drop(reader);
        assert!(start.elapsed() < Duration::from_millis(100));
        release.send(()).unwrap();
        finish.recv_timeout(Duration::from_secs(2)).unwrap();
    }
}
