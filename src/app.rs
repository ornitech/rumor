use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use portable_pty::PtySize;

use crate::keys::encode_key;
use crate::process::ProcessManager;

/// When wrap is disabled, set the PTY this wide so the child writes long
/// lines as a single row in the vt100 grid. The display widget clips the
/// right side; toggle wrap back on (or shrink the line) to see it.
const NOWRAP_COLS: u16 = 1000;

/// Grace given to each process during shutdown before SIGKILL. Matches the
/// grace baked into `Process::terminate` (via `kill_all`).
const SHUTDOWN_GRACE: Duration = Duration::from_secs(3);

/// Slack on top of `SHUTDOWN_GRACE` after which we stop rendering the shutdown
/// screen and quit regardless - a safety net for a process that ignores even
/// SIGKILL (e.g. stuck in uninterruptible sleep).
const SHUTDOWN_DEADLINE_SLACK: Duration = Duration::from_millis(500);

/// How long a transient details-screen notice ("log path copied") stays up.
const NOTICE_TTL: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Nav,
    Focus,
    Details,
}

pub struct App {
    pub mgr: ProcessManager,
    pub selected: usize,
    pub mode: Mode,
    pub should_quit: bool,
    /// Set once the user quits: the render loop keeps drawing a full-screen
    /// shutdown progress view until every process has exited.
    pub shutting_down: bool,
    /// When the shutdown phase began, used for the spinner and hard deadline.
    pub shutdown_started: Option<Instant>,
    pub wraps: Vec<bool>,
    pub display_rows: u16,
    pub display_cols: u16,
    pub tab_offset: usize,
    pub details_scroll: u16,
    /// Hotkey help overlay, drawn on top of whatever mode is active.
    pub help_visible: bool,
    /// Scroll offset for the help overlay when it doesn't fit the terminal.
    pub help_scroll: u16,
    /// Transient status-line feedback (e.g. clipboard copy result).
    notice: Option<(String, Instant)>,
}

impl App {
    pub fn new(mgr: ProcessManager, display_rows: u16, display_cols: u16) -> Self {
        let wraps = vec![true; mgr.count()];
        Self {
            mgr,
            selected: 0,
            mode: Mode::Nav,
            should_quit: false,
            shutting_down: false,
            shutdown_started: None,
            wraps,
            display_rows,
            display_cols,
            tab_offset: 0,
            details_scroll: 0,
            help_visible: false,
            help_scroll: 0,
            notice: None,
        }
    }

    /// Current status-line notice, if it hasn't expired yet.
    pub fn notice(&self) -> Option<&str> {
        match &self.notice {
            Some((msg, at)) if at.elapsed() < NOTICE_TTL => Some(msg),
            _ => None,
        }
    }

    /// Copy the selected process's session log path to the clipboard and
    /// surface the result in the status line.
    fn copy_log_path(&mut self) {
        let msg = match self.mgr.log_path(self.selected) {
            Some(p) => {
                if crate::clipboard::copy(&p.display().to_string()) {
                    "log path copied"
                } else {
                    "copy failed"
                }
            }
            None => "no log file (capture disabled)",
        };
        self.notice = Some((msg.to_string(), Instant::now()));
    }

    pub fn wrap_of(&self, idx: usize) -> bool {
        self.wraps.get(idx).copied().unwrap_or(true)
    }

    pub fn ensure_selected_visible(&mut self, available_width: u16) {
        let names: Vec<&str> = self.mgr.configs().iter().map(|c| c.name.as_str()).collect();
        self.tab_offset =
            adjust_tab_offset(&names, self.selected, self.tab_offset, available_width);
    }

    pub fn visible_tab_range(&self, available_width: u16) -> (usize, usize) {
        let names: Vec<&str> = self.mgr.configs().iter().map(|c| c.name.as_str()).collect();
        visible_tab_range(&names, self.tab_offset, available_width)
    }

    pub fn apply_resize(&mut self, rows: u16, cols: u16) {
        self.display_rows = rows;
        self.display_cols = cols;
        for i in 0..self.mgr.count() {
            self.resize_one(i);
        }
    }

    fn resize_one(&self, idx: usize) {
        let size = PtySize {
            rows: self.display_rows,
            cols: if self.wrap_of(idx) {
                self.display_cols
            } else {
                self.display_cols.max(NOWRAP_COLS)
            },
            pixel_width: 0,
            pixel_height: 0,
        };
        // Record the target size so a not-yet-spawned (restarting / dependency-
        // delayed) process spawns at the current width, and resize it now if live.
        self.mgr.set_size(idx, size);
    }

    pub fn handle_key(&mut self, key: KeyEvent) {
        if self.shutting_down {
            // While shutting down, the only thing the user can do is force-quit.
            let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
            if matches!(key.code, KeyCode::Char('q')) && !ctrl
                || matches!(key.code, KeyCode::Char('c')) && ctrl
            {
                self.should_quit = true;
            }
            return;
        }
        if self.help_visible {
            // Swallow everything; only close and scroll keys do anything.
            // The draw code clamps the offset to the actual content height.
            match key.code {
                KeyCode::Esc | KeyCode::Char('h') | KeyCode::Char('q') => {
                    self.help_visible = false;
                }
                KeyCode::Up => self.help_scroll = self.help_scroll.saturating_sub(1),
                KeyCode::Down => self.help_scroll = self.help_scroll.saturating_add(1),
                KeyCode::PageUp => self.help_scroll = self.help_scroll.saturating_sub(10),
                KeyCode::PageDown => self.help_scroll = self.help_scroll.saturating_add(10),
                KeyCode::Home => self.help_scroll = 0,
                _ => {}
            }
            return;
        }
        match self.mode {
            Mode::Nav => self.handle_nav_key(key),
            Mode::Focus => self.handle_focus_key(key),
            Mode::Details => self.handle_details_key(key),
        }
    }

    /// Enter the shutdown phase: stop watchers and SIGTERM everything, keeping
    /// the TUI alive to show progress. If nothing is running, quit immediately.
    fn begin_shutdown(&mut self) {
        if self.mgr.all_exited() {
            self.should_quit = true;
            return;
        }
        self.shutting_down = true;
        self.shutdown_started = Some(Instant::now());
        self.mgr.begin_shutdown();
    }

    /// True once shutdown should end: every process exited, or the hard
    /// deadline elapsed.
    pub fn shutdown_complete(&self) -> bool {
        if !self.shutting_down {
            return false;
        }
        if self.mgr.all_exited() {
            return true;
        }
        match self.shutdown_started {
            Some(started) => started.elapsed() >= SHUTDOWN_GRACE + SHUTDOWN_DEADLINE_SLACK,
            None => true,
        }
    }

    fn handle_nav_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('q') if !ctrl => self.begin_shutdown(),
            KeyCode::Char('c') if ctrl => self.begin_shutdown(),
            KeyCode::Left => self.prev_tab(),
            KeyCode::Right => self.next_tab(),
            KeyCode::Up => self.scroll_by(1),
            KeyCode::Down => self.scroll_by(-1),
            KeyCode::Enter => {
                if self.processes_has_selected() {
                    self.mode = Mode::Focus;
                    self.set_scroll(0);
                }
            }
            KeyCode::Char('r') if !ctrl => {
                self.mgr.restart(self.selected);
            }
            KeyCode::Char('k') if !ctrl => self.mgr.kill(self.selected),
            KeyCode::Char('r') if ctrl => {
                self.mgr.restart_all();
            }
            KeyCode::Char('k') if ctrl => self.mgr.kill_all(),
            KeyCode::Char('w') if !ctrl => self.toggle_wrap(),
            KeyCode::Char('y') if !ctrl => self.copy_log_path(),
            KeyCode::Char('h') if !ctrl => self.open_help(),
            KeyCode::Char('d') if !ctrl => {
                self.mode = Mode::Details;
                self.details_scroll = 0;
                self.notice = None;
            }
            KeyCode::PageUp => self.scroll_by(10),
            KeyCode::PageDown => self.scroll_by(-10),
            KeyCode::Home => self.scroll_to_top(),
            KeyCode::End => self.set_scroll(0),
            _ => {}
        }
    }

    fn handle_details_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc | KeyCode::Char('d') | KeyCode::Char('q') => {
                self.mode = Mode::Nav;
            }
            KeyCode::Char('y') => self.copy_log_path(),
            KeyCode::Char('h') => self.open_help(),
            KeyCode::Up => {
                self.details_scroll = self.details_scroll.saturating_sub(1);
            }
            KeyCode::Down => {
                self.details_scroll = self.details_scroll.saturating_add(1);
            }
            KeyCode::PageUp => {
                self.details_scroll = self.details_scroll.saturating_sub(10);
            }
            KeyCode::PageDown => {
                self.details_scroll = self.details_scroll.saturating_add(10);
            }
            KeyCode::Home => self.details_scroll = 0,
            _ => {}
        }
    }

    fn open_help(&mut self) {
        self.help_visible = true;
        self.help_scroll = 0;
    }

    fn handle_focus_key(&mut self, key: KeyEvent) {
        if matches!(key.code, KeyCode::Esc) && key.modifiers.is_empty() {
            self.mode = Mode::Nav;
            return;
        }
        if let Some(bytes) = encode_key(key) {
            if let Some(p) = self.mgr.process(self.selected) {
                p.write_input(&bytes);
            }
        }
    }

    fn processes_has_selected(&self) -> bool {
        self.mgr.process(self.selected).is_some()
    }

    pub fn next_tab(&mut self) {
        if self.mgr.count() == 0 {
            return;
        }
        self.selected = (self.selected + 1) % self.mgr.count();
    }

    pub fn prev_tab(&mut self) {
        if self.mgr.count() == 0 {
            return;
        }
        if self.selected == 0 {
            self.selected = self.mgr.count() - 1;
        } else {
            self.selected -= 1;
        }
    }

    fn toggle_wrap(&mut self) {
        if let Some(w) = self.wraps.get_mut(self.selected) {
            *w = !*w;
        }
        self.resize_one(self.selected);
    }

    fn scroll_by(&mut self, delta: i32) {
        if let Some(p) = self.mgr.process(self.selected) {
            let mut parser = p.parser.lock().unwrap();
            let cur = parser.screen().scrollback() as i32;
            let new = (cur + delta).max(0) as usize;
            parser.screen_mut().set_scrollback(new);
        }
    }

    fn set_scroll(&mut self, n: usize) {
        if let Some(p) = self.mgr.process(self.selected) {
            p.parser.lock().unwrap().screen_mut().set_scrollback(n);
        }
    }

    fn scroll_to_top(&mut self) {
        self.set_scroll(usize::MAX / 2);
    }
}

// ---- pure helpers (tested in isolation) ----

/// Rendered width of one tab as drawn by ratatui's `Tabs`:
/// padding_left(1) + "● "(2) + name + padding_right(1) = name + 4.
/// Divider (1 col) between adjacent tabs is added separately.
pub fn tab_width(name: &str) -> u16 {
    (name.chars().count() as u16).saturating_add(4)
}

pub fn visible_tab_range(names: &[&str], offset: usize, available_width: u16) -> (usize, usize) {
    let n = names.len();
    if n == 0 {
        return (0, 0);
    }
    let start = offset.min(n - 1);
    let mut used: u16 = 0;
    let mut end = start;
    for i in start..n {
        let mut needed = tab_width(names[i]);
        if i > start {
            needed = needed.saturating_add(1); // divider
        }
        if used.saturating_add(needed) > available_width {
            break;
        }
        used = used.saturating_add(needed);
        end = i + 1;
    }
    if end == start {
        end = start + 1;
    }
    (start, end)
}

pub fn adjust_tab_offset(
    names: &[&str],
    selected: usize,
    mut offset: usize,
    available_width: u16,
) -> usize {
    let n = names.len();
    if n == 0 || available_width == 0 {
        return 0;
    }
    let selected = selected.min(n - 1);
    if offset > selected {
        offset = selected;
    }
    loop {
        let (start, end) = visible_tab_range(names, offset, available_width);
        if selected < end || start == selected {
            return offset;
        }
        offset += 1;
        if offset >= n {
            return n - 1;
        }
    }
}
