//! Unit tests for the hotkey help overlay (open/close/key-swallowing).

#[path = "../src/app.rs"]
mod app;

// app.rs pulls in keys/process/config via `use crate::...`; we stub those
// minimally so this test target compiles without the full dependency graph.
#[path = "../src/keys.rs"]
mod keys;
#[path = "../src/clipboard.rs"]
mod clipboard;
#[path = "../src/config.rs"]
mod config;
#[path = "../src/env.rs"]
mod env;
#[path = "../src/logfile.rs"]
mod logfile;
#[path = "../src/template.rs"]
mod template;
#[path = "../src/ports.rs"]
mod ports;
#[path = "../src/process.rs"]
mod process;
#[path = "../src/search.rs"]
mod search;
#[path = "../src/status_color.rs"]
mod status_color;
#[path = "../src/ui.rs"]
mod ui;

use app::{App, Mode};
use crossterm::event::{KeyCode, KeyEvent};
use portable_pty::PtySize;
use process::ProcessManager;

fn test_app() -> App {
    let size = PtySize {
        rows: 24,
        cols: 80,
        pixel_width: 0,
        pixel_height: 0,
    };
    App::new(ProcessManager::new(vec![], size, None), 24, 80)
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::from(code)
}

#[test]
fn h_opens_help_in_nav() {
    let mut app = test_app();
    app.handle_key(key(KeyCode::Char('h')));
    assert!(app.help_visible);
    assert_eq!(app.mode, Mode::Nav);
}

#[test]
fn esc_h_and_q_close_help() {
    for close in [KeyCode::Esc, KeyCode::Char('h'), KeyCode::Char('q')] {
        let mut app = test_app();
        app.handle_key(key(KeyCode::Char('h')));
        assert!(app.help_visible);
        app.handle_key(key(close));
        assert!(!app.help_visible, "{close:?} should close help");
        // q closed the overlay, it must not have started a shutdown
        assert!(!app.shutting_down);
        assert!(!app.should_quit);
    }
}

#[test]
fn other_keys_are_swallowed_while_help_open() {
    let mut app = test_app();
    app.handle_key(key(KeyCode::Char('h')));
    app.handle_key(key(KeyCode::Char('d')));
    assert!(app.help_visible, "d should be swallowed, not enter details");
    assert_eq!(app.mode, Mode::Nav);
}

#[test]
fn scroll_keys_move_help_offset() {
    let mut app = test_app();
    app.handle_key(key(KeyCode::Char('h')));
    assert_eq!(app.help_scroll, 0);
    app.handle_key(key(KeyCode::Down));
    app.handle_key(key(KeyCode::Down));
    assert_eq!(app.help_scroll, 2);
    app.handle_key(key(KeyCode::Up));
    assert_eq!(app.help_scroll, 1);
    app.handle_key(key(KeyCode::PageDown));
    assert_eq!(app.help_scroll, 11);
    app.handle_key(key(KeyCode::Home));
    assert_eq!(app.help_scroll, 0);
    // saturates at the top instead of underflowing
    app.handle_key(key(KeyCode::Up));
    assert_eq!(app.help_scroll, 0);
    assert!(app.help_visible);
}

#[test]
fn reopening_help_resets_scroll() {
    let mut app = test_app();
    app.handle_key(key(KeyCode::Char('h')));
    app.handle_key(key(KeyCode::Down));
    assert_eq!(app.help_scroll, 1);
    app.handle_key(key(KeyCode::Esc));
    app.handle_key(key(KeyCode::Char('h')));
    assert_eq!(app.help_scroll, 0);
}

#[test]
fn h_does_not_open_help_in_focus() {
    let mut app = test_app();
    app.mode = Mode::Focus;
    app.handle_key(key(KeyCode::Char('h')));
    assert!(!app.help_visible);
    assert_eq!(app.mode, Mode::Focus);
}

#[test]
fn help_from_details_returns_to_details() {
    let mut app = test_app();
    app.handle_key(key(KeyCode::Char('d')));
    assert_eq!(app.mode, Mode::Details);
    app.handle_key(key(KeyCode::Char('h')));
    assert!(app.help_visible);
    app.handle_key(key(KeyCode::Esc));
    assert!(!app.help_visible);
    assert_eq!(app.mode, Mode::Details);
}
