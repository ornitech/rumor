//! Unit tests for tab-bar horizontal scroll math.

#[path = "../src/app.rs"]
mod app;

// app.rs pulls in keys/process/config via `use crate::...`; we stub those
// minimally so this test target compiles without the full dependency graph.
#[path = "../src/keys.rs"]
mod keys;
#[path = "../src/config.rs"]
mod config;
#[path = "../src/env.rs"]
mod env;
#[path = "../src/template.rs"]
mod template;
#[path = "../src/process.rs"]
mod process;

use app::{adjust_tab_offset, tab_width, visible_tab_range};

#[test]
fn tab_width_includes_padding_dot_and_name() {
    // padding(1) + "● "(2) + name + padding(1) = name + 4
    assert_eq!(tab_width(""), 4);
    assert_eq!(tab_width("api"), 7);
    assert_eq!(tab_width("counter"), 11);
}

#[test]
fn visible_range_fits_everything_when_wide() {
    let names = ["a", "b", "c"];
    let (start, end) = visible_tab_range(&names, 0, 200);
    assert_eq!((start, end), (0, 3));
}

#[test]
fn visible_range_truncates_when_narrow() {
    // "a"=5, "b"=5 (with divider in between: 5+1+5=11). Allow 11 → fits 2.
    // "c" would need another +1+5 = 17 total.
    let names = ["a", "b", "c"];
    let (start, end) = visible_tab_range(&names, 0, 11);
    assert_eq!((start, end), (0, 2));
}

#[test]
fn visible_range_respects_offset() {
    let names = ["a", "b", "c", "d"];
    let (start, end) = visible_tab_range(&names, 2, 200);
    assert_eq!((start, end), (2, 4));
}

#[test]
fn visible_range_returns_one_tab_when_overflow_at_offset() {
    // tab needs at least 5 cols; with available_width=2, nothing fits, but we
    // still report at least one tab visible to avoid an empty render.
    let names = ["x"];
    let (start, end) = visible_tab_range(&names, 0, 2);
    assert_eq!((start, end), (0, 1));
}

#[test]
fn adjust_offset_keeps_selected_visible_when_walking_right() {
    let names = ["a", "b", "c", "d", "e"];
    // Each tab = 5 cols, with 1-col divider between. Width 12 fits 2 tabs.
    let w = 12;
    assert_eq!(adjust_tab_offset(&names, 0, 0, w), 0);
    assert_eq!(adjust_tab_offset(&names, 1, 0, w), 0); // 0,1 visible
    assert_eq!(adjust_tab_offset(&names, 2, 0, w), 1); // slide → 1,2
    assert_eq!(adjust_tab_offset(&names, 3, 1, w), 2); // slide → 2,3
    assert_eq!(adjust_tab_offset(&names, 4, 2, w), 3); // slide → 3,4
}

#[test]
fn adjust_offset_snaps_left_when_selected_before_window() {
    let names = ["a", "b", "c", "d", "e"];
    let w = 12;
    // Walking back from 4 → 0 should jump the offset left to keep selected visible.
    assert_eq!(adjust_tab_offset(&names, 0, 3, w), 0);
    assert_eq!(adjust_tab_offset(&names, 1, 3, w), 1);
}

#[test]
fn adjust_offset_handles_zero_width_and_empty() {
    let empty: [&str; 0] = [];
    assert_eq!(adjust_tab_offset(&empty, 0, 0, 100), 0);
    let names = ["a", "b"];
    assert_eq!(adjust_tab_offset(&names, 1, 0, 0), 0);
}

#[test]
fn adjust_offset_handles_selected_past_end() {
    let names = ["a", "b", "c"];
    // selected clamped to last
    assert_eq!(adjust_tab_offset(&names, 99, 0, 200), 0);
}
