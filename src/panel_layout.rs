//! All flyout coordinates are logical pixels; native painting scales once.
pub const WIDTH: i32 = 450;
pub const ROW_TOP: i32 = 72;
pub const ROW_HEIGHT: i32 = 62;
pub const MAX_ROWS: usize = 5;
pub const FOOTER: i32 = 48;

pub fn visible_rows(count: usize, available_height: i32) -> usize {
    count
        .min(MAX_ROWS)
        .min(((available_height - ROW_TOP - FOOTER).max(0) / ROW_HEIGHT) as usize)
}

pub fn height(rows: usize) -> i32 {
    ROW_TOP + rows as i32 * ROW_HEIGHT + FOOTER
}

pub fn scroll_offset(offset: usize, count: usize, visible: usize) -> usize {
    offset.min(count.saturating_sub(visible))
}

pub fn row_at(y: i32, offset: usize, count: usize, visible: usize) -> Option<usize> {
    let relative = y.checked_sub(ROW_TOP)?;
    if relative < 0 {
        return None;
    }
    let index = relative as usize / ROW_HEIGHT as usize;
    (index < visible && index + offset < count).then_some(index + offset)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn viewport_and_hit_testing_do_not_select_hidden_rows_or_the_footer() {
        assert_eq!(visible_rows(100, 1080), 5);
        assert_eq!(visible_rows(100, 340), 3);
        assert_eq!(scroll_offset(99, 8, 5), 3);
        assert_eq!(row_at(ROW_TOP + ROW_HEIGHT, 3, 8, 5), Some(4));
        assert_eq!(row_at(ROW_TOP - 1, 3, 8, 5), None);
        assert_eq!(row_at(ROW_TOP + 5 * ROW_HEIGHT, 3, 8, 5), None);
    }
}
