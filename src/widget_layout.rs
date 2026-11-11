//! Screen-coordinate decisions only; no Explorer calls or window mutations.
use windows_sys::Win32::Foundation::RECT;

pub fn covered_by_fullscreen(monitor: RECT, foreground: RECT) -> bool {
    foreground.left <= monitor.left
        && foreground.top <= monitor.top
        && foreground.right >= monitor.right
        && foreground.bottom >= monitor.bottom
}

pub fn offscreen(tray: RECT, monitor: RECT) -> bool {
    i64::from(tray.bottom.min(monitor.bottom)) - i64::from(tray.top.max(monitor.top)) <= 2
        || i64::from(tray.right.min(monitor.right)) - i64::from(tray.left.max(monitor.left)) <= 2
}

pub fn position(tray: RECT, notify_left: Option<i32>, dpi: u32, width: i32) -> Option<RECT> {
    let tray_width = tray.right.checked_sub(tray.left)?;
    let tray_height = tray.bottom.checked_sub(tray.top)?;
    if width <= 0 || tray_width < width || tray_height <= 0 {
        return None;
    }
    let scale = i32::try_from(dpi.max(96)).ok()?;
    let height = (42i32.checked_mul(scale)? / 96).min(tray_height.saturating_sub(2).max(1));
    let boundary = notify_left.unwrap_or(tray.right.checked_sub(220i32.checked_mul(scale)? / 96)?);
    let x = boundary
        .checked_sub(width)?
        .checked_sub(2i32.checked_mul(scale)? / 96)?
        .clamp(tray.left, tray.right.checked_sub(width)?);
    let top = tray.top.checked_add((tray_height - height) / 2)?;
    Some(RECT {
        left: x,
        top,
        right: x.checked_add(width)?,
        bottom: top.checked_add(height)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn rect(l: i32, t: i32, r: i32, b: i32) -> RECT {
        RECT {
            left: l,
            top: t,
            right: r,
            bottom: b,
        }
    }
    #[test]
    fn fullscreen_requires_the_monitor_not_just_the_taskbar() {
        let monitor = rect(0, 0, 1920, 1080);
        assert!(covered_by_fullscreen(monitor, monitor));
        assert!(!covered_by_fullscreen(monitor, rect(0, 1000, 1920, 1080)));
        assert!(!covered_by_fullscreen(monitor, rect(0, 0, 1920, 1040)));
    }
    #[test]
    fn negative_monitor_and_hidden_taskbar_are_handled() {
        let monitor = rect(-1920, -1080, 0, 0);
        assert!(!offscreen(rect(-1920, -40, 0, 0), monitor));
        assert!(offscreen(rect(-1920, -2, 0, 38), monitor));
        let widget = position(rect(-1920, -40, 0, 0), Some(-200), 96, 220).unwrap();
        assert_eq!(
            (widget.left, widget.top, widget.right, widget.bottom),
            (-422, -39, -202, -1)
        );
    }
    #[test]
    fn small_taskbars_and_large_dpi_stay_inside_bounds() {
        let widget = position(rect(0, 0, 800, 20), Some(700), 192, 300).unwrap();
        assert!(widget.top >= 0 && widget.bottom <= 20 && widget.right <= 700);
        assert!(position(rect(0, 0, 100, 40), None, 96, 220).is_none());
        assert!(position(rect(0, 0, 100, 0), None, 96, 50).is_none());
    }
}
