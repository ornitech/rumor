//! Tests for interactive search: key handling (open/edit/confirm/cancel,
//! n/N stepping, help precedence) and end-to-end rendering of highlights
//! against a real PTY-backed process.

#[path = "../src/app.rs"]
mod app;

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

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use app::{App, Mode};
use config::ProcessConfig;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use portable_pty::PtySize;
use process::{Process, ProcessManager, Slot};

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::from(code)
}

fn type_str(app: &mut App, s: &str) {
    for c in s.chars() {
        app.handle_key(key(KeyCode::Char(c)));
    }
}

fn empty_app() -> App {
    let size = PtySize { rows: 24, cols: 80, pixel_width: 0, pixel_height: 0 };
    App::new(ProcessManager::new(vec![], size, None), 24, 80)
}

// ---- details search (no process needed) ----

#[test]
fn slash_opens_details_search_and_chars_edit_query() {
    let mut app = empty_app();
    app.handle_key(key(KeyCode::Char('d')));
    assert_eq!(app.mode, Mode::Details);
    app.handle_key(key(KeyCode::Char('/')));
    assert!(app.details_search.editing);
    assert!(app.details_search.active);
    type_str(&mut app, "path");
    assert_eq!(app.details_search.query, "path");
    app.handle_key(key(KeyCode::Backspace));
    assert_eq!(app.details_search.query, "pat");
}

#[test]
fn q_while_editing_types_a_q_instead_of_quitting() {
    let mut app = empty_app();
    app.handle_key(key(KeyCode::Char('d')));
    app.handle_key(key(KeyCode::Char('/')));
    app.handle_key(key(KeyCode::Char('q')));
    assert_eq!(app.details_search.query, "q");
    assert!(!app.should_quit);
    assert!(!app.shutting_down);
    assert_eq!(app.mode, Mode::Details, "d must not close details either");
}

#[test]
fn enter_confirms_and_esc_clears_details_search() {
    let mut app = empty_app();
    app.handle_key(key(KeyCode::Char('d')));
    app.handle_key(key(KeyCode::Char('/')));
    type_str(&mut app, "x");
    app.handle_key(key(KeyCode::Enter));
    assert!(!app.details_search.editing);
    assert!(app.details_search.active, "stays active after confirm");
    // First Esc clears the search, second closes details.
    app.handle_key(key(KeyCode::Esc));
    assert!(!app.details_search.active);
    assert_eq!(app.mode, Mode::Details);
    app.handle_key(key(KeyCode::Esc));
    assert_eq!(app.mode, Mode::Nav);
}

#[test]
fn confirming_empty_query_deactivates() {
    let mut app = empty_app();
    app.handle_key(key(KeyCode::Char('d')));
    app.handle_key(key(KeyCode::Char('/')));
    app.handle_key(key(KeyCode::Enter));
    assert!(!app.details_search.active);
    assert!(!app.details_search.editing);
}

#[test]
fn ctrl_u_clears_the_query() {
    let mut app = empty_app();
    app.handle_key(key(KeyCode::Char('d')));
    app.handle_key(key(KeyCode::Char('/')));
    type_str(&mut app, "abc");
    app.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
    assert_eq!(app.details_search.query, "");
    assert!(app.details_search.editing, "clearing keeps the bar open");
}

#[test]
fn help_overlay_swallows_slash() {
    let mut app = empty_app();
    app.handle_key(key(KeyCode::Char('h')));
    app.handle_key(key(KeyCode::Char('/')));
    assert!(!app.log_search.editing);
    assert!(!app.details_search.editing);
    assert!(app.help_visible);
}

#[test]
fn slash_without_process_shows_notice_not_search() {
    let mut app = empty_app();
    app.handle_key(key(KeyCode::Char('/')));
    assert!(!app.log_search.editing);
    assert!(!app.log_search.active);
    assert!(app.notice().is_some());
}

#[test]
fn reopening_details_clears_previous_search() {
    let mut app = empty_app();
    app.handle_key(key(KeyCode::Char('d')));
    app.handle_key(key(KeyCode::Char('/')));
    type_str(&mut app, "x");
    app.handle_key(key(KeyCode::Enter));
    app.handle_key(key(KeyCode::Char('d'))); // close
    app.handle_key(key(KeyCode::Char('d'))); // reopen
    assert!(!app.details_search.active);
    assert_eq!(app.details_search.query, "");
}

// ---- log search against a real process ----

async fn wait_for_process(mgr: &ProcessManager, idx: usize) -> Arc<Process> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        match mgr.slot(idx) {
            Slot::Process(p) => return p,
            Slot::SpawnFailed(e) => panic!("spawn failed: {e}"),
            Slot::Blocked(r) => panic!("blocked: {r}"),
            Slot::Waiting => {}
        }
        if tokio::time::Instant::now() > deadline {
            panic!("process never spawned");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// App with one exited bash process whose output is `script`, on a
/// rows x cols screen.
async fn app_with_output(script: &str, rows: u16, cols: u16) -> App {
    let cfg = ProcessConfig {
        name: "logs".into(),
        command: "bash".into(),
        args: vec!["-c".into(), script.into()],
        cwd: std::env::temp_dir(),
        env_files: vec![],
        global_env_files: vec![],
        env: HashMap::new(),
        dynamic_ports: HashMap::new(),
        depends_on: vec![],
        long_lived: false,
        tags: vec![],
    };
    let size = PtySize { rows, cols, pixel_width: 0, pixel_height: 0 };
    let mgr = ProcessManager::new(vec![cfg], size, None);
    let proc = wait_for_process(&mgr, 0).await;
    let _ = tokio::time::timeout(Duration::from_secs(5), proc.wait_for_exit()).await;
    App::new(mgr, rows, cols)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn log_search_finds_matches_and_steps() {
    let mut app = app_with_output(
        "for i in $(seq 1 40); do echo line $i; done; echo needle one; echo needle two",
        10,
        40,
    )
    .await;

    app.handle_key(key(KeyCode::Char('/')));
    assert!(app.log_search.editing);
    type_str(&mut app, "needle");
    app.handle_key(key(KeyCode::Enter));
    assert!(!app.log_search.editing);
    assert_eq!(app.log_search.matches.len(), 2);
    assert_eq!(app.log_search.current, 1, "starts at the newest match");

    // n = older, N = newer, both wrap.
    app.handle_key(key(KeyCode::Char('n')));
    assert_eq!(app.log_search.current, 0);
    app.handle_key(key(KeyCode::Char('n')));
    assert_eq!(app.log_search.current, 1, "wraps past the oldest");
    app.handle_key(key(KeyCode::Char('N')));
    assert_eq!(app.log_search.current, 0, "wraps past the newest");

    // Esc clears highlights and the query.
    app.handle_key(key(KeyCode::Esc));
    assert!(!app.log_search.active);
    assert!(app.log_search.matches.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn search_is_case_insensitive_literal() {
    let mut app = app_with_output("echo ERROR alpha; echo error beta", 10, 40).await;
    app.handle_key(key(KeyCode::Char('/')));
    type_str(&mut app, "error");
    assert_eq!(app.log_search.matches.len(), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn esc_while_editing_restores_scroll_position() {
    let mut app = app_with_output("for i in $(seq 1 60); do echo line $i; done", 10, 40).await;

    // Scroll up a few rows, then open and cancel a search.
    for _ in 0..5 {
        app.handle_key(key(KeyCode::Up));
    }
    let before = app
        .mgr
        .process(0)
        .unwrap()
        .parser
        .lock()
        .unwrap()
        .screen()
        .scrollback();
    assert!(before > 0);

    app.handle_key(key(KeyCode::Char('/')));
    type_str(&mut app, "line 2"); // jumps the view to a match
    app.handle_key(key(KeyCode::Esc));

    let after = app
        .mgr
        .process(0)
        .unwrap()
        .parser
        .lock()
        .unwrap()
        .screen()
        .scrollback();
    assert_eq!(after, before, "Esc must restore the pre-search scroll");
    assert!(!app.log_search.active);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn confirm_jumps_to_match_in_scrollback() {
    let mut app = app_with_output(
        "echo needle here; for i in $(seq 1 60); do echo line $i; done",
        10,
        40,
    )
    .await;

    app.handle_key(key(KeyCode::Char('/')));
    type_str(&mut app, "needle");
    app.handle_key(key(KeyCode::Enter));
    assert_eq!(app.log_search.matches.len(), 1);

    // The match is deep in scrollback; the view must have jumped to it.
    let s = app
        .mgr
        .process(0)
        .unwrap()
        .parser
        .lock()
        .unwrap()
        .screen()
        .scrollback();
    assert!(s > 0, "view should have scrolled back to the match");
}

// ---- rendering ----

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn highlights_and_indicator_render_into_the_buffer() {
    use ratatui::backend::TestBackend;
    use ratatui::style::Color;
    use ratatui::Terminal;

    // 60x16 terminal: body inner = 10 rows x 58 cols (UI chrome = 6 rows,
    // 2 cols), matching the PTY size below.
    let (width, height) = (60u16, 16u16);
    let mut app = app_with_output("echo a needle b", 10, 58).await;

    app.handle_key(key(KeyCode::Char('/')));
    type_str(&mut app, "needle");
    app.handle_key(key(KeyCode::Enter));
    assert_eq!(app.log_search.matches.len(), 1);

    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|f| ui::draw(f, &mut app)).unwrap();
    let buf = terminal.backend().buffer();

    // "a needle b" renders on the first body row (body y = 3 border + 1).
    // The match starts at display col 2, so buffer x = 1 (border) + 2.
    let row_text: String = (0..width).map(|x| buf[(x, 4)].symbol()).collect();
    assert!(row_text.contains("a needle b"), "body row: {row_text:?}");
    let needle_x = 1 + 2;
    for x in needle_x..needle_x + 6 {
        assert_eq!(
            buf[(x, 4)].style().bg,
            Some(Color::Yellow),
            "cell {x} should carry the current-match highlight"
        );
    }
    assert_ne!(
        buf[(1, 4)].style().bg,
        Some(Color::Yellow),
        "cells outside the match must not be highlighted"
    );

    // Status line (last row) shows the match indicator, and the hints are
    // swapped for search-mode keys while the search is active.
    let status: String = (0..width).map(|x| buf[(x, height - 1)].symbol()).collect();
    assert!(
        status.contains("(1/1) /needle"),
        "status line: {status:?}"
    );
    assert!(
        status.contains("n older"),
        "search-mode hints should replace nav hints: {status:?}"
    );

    // Clearing the search restores the normal nav hints.
    app.handle_key(key(KeyCode::Esc));
    terminal.draw(|f| ui::draw(f, &mut app)).unwrap();
    let buf = terminal.backend().buffer();
    let status: String = (0..width).map(|x| buf[(x, height - 1)].symbol()).collect();
    assert!(
        !status.contains("n older") && status.contains("tabs"),
        "nav hints should be back after Esc: {status:?}"
    );
}
