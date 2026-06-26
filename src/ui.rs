use std::ops::Range;

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Tabs};
use ratatui::Frame;
use tui_term::widget::PseudoTerminal;

use crate::app::{App, Mode};

/// Running version, sourced from Cargo.toml at compile time.
const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
use crate::process::{Slot, Status};
use crate::search;
use crate::status_color::{exited_color, PENDING};

/// Style for the search match the cursor is on.
const MATCH_CURRENT: Style = Style::new().fg(Color::Black).bg(Color::Yellow);
/// Style for all other search matches.
const MATCH_OTHER: Style = Style::new().fg(Color::White).bg(Color::DarkGray);

pub fn draw(frame: &mut Frame, app: &mut App) {
    let area = frame.area();

    if app.shutting_down {
        draw_shutdown(frame, app, area);
        return;
    }

    app.pre_draw();

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // tab bar
            Constraint::Min(1),    // body
            Constraint::Length(1), // status / hotkey line
        ])
        .split(area);

    draw_tabs(frame, app, chunks[0]);
    if app.mode == Mode::Details {
        draw_details_body(frame, app, chunks[1]);
    } else {
        draw_body(frame, app, chunks[1]);
    }
    draw_status(frame, app, chunks[2]);

    if app.help_visible {
        draw_help(frame, app, area);
    }
}

fn draw_tabs(frame: &mut Frame, app: &mut App, area: Rect) {
    // Version lives on the top border (left), out of the way of the tabs below.
    // When an update is found, a green badge is right-aligned on the same border.
    let mut block = Block::default()
        .borders(Borders::ALL)
        .title(format!("rumor v{APP_VERSION}"));
    if let Some(info) = &app.update_available {
        block = block.title(
            Line::from(Span::styled(
                format!(" ↑ {} · {} ", info.latest, info.action),
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ))
            .right_aligned(),
        );
    }
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Reserve 1 col on each side for ◀ / ▶ scroll indicators.
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(inner);
    let left_arrow_area = cols[0];
    let tabs_area = cols[1];
    let right_arrow_area = cols[2];

    app.ensure_selected_visible(tabs_area.width);
    let (start, end) = app.visible_tab_range(tabs_area.width);

    // Highlight only the name of the selected tab, not its status dot. The dot keeps
    // its true status color on a normal background so it stays readable (#31) and does
    // not lose contrast against the bright highlight (e.g. green-on-cyan).
    let highlight_bg = if app.mode == Mode::Focus {
        Color::Yellow
    } else {
        Color::Cyan
    };
    let titles: Vec<Line> = app.mgr.configs()[start..end]
        .iter()
        .enumerate()
        .map(|(offset, cfg)| {
            let i = start + offset;
            let dot_style = Style::default().fg(tab_dot_color(app, i));
            let name = if i == app.selected {
                Span::styled(
                    cfg.name.clone(),
                    Style::default()
                        .fg(Color::Black)
                        .bg(highlight_bg)
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                Span::raw(cfg.name.clone())
            };
            Line::from(vec![Span::styled("● ", dot_style), name])
        })
        .collect();

    let selected_local = app.selected.saturating_sub(start);
    // All selected styling lives on the spans above; neutralize the widget's default
    // reversed highlight so it does not repaint (and clobber) the dot.
    let tabs = Tabs::new(titles)
        .select(selected_local)
        .highlight_style(Style::default())
        .divider("│");
    frame.render_widget(tabs, tabs_area);

    if start > 0 {
        frame.render_widget(
            Paragraph::new("◀").style(Style::default().fg(Color::DarkGray)),
            left_arrow_area,
        );
    }
    if end < app.mgr.count() {
        frame.render_widget(
            Paragraph::new("▶").style(Style::default().fg(Color::DarkGray)),
            right_arrow_area,
        );
    }
}

fn draw_body(frame: &mut Frame, app: &App, area: Rect) {
    let selected = app.selected;
    let title = app
        .mgr
        .configs()
        .get(selected)
        .map(|c| c.name.clone())
        .unwrap_or_default();

    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {title} "));

    match app.mgr.slot(selected) {
        Slot::Process(proc) => {
            let mut parser = proc.parser.lock().unwrap();
            {
                let term = PseudoTerminal::new(parser.screen()).block(block);
                frame.render_widget(term, area);
            }
            if app.log_search.active && !app.log_search.matches.is_empty() {
                draw_log_highlights(frame, app, &mut parser, area);
            }
        }
        Slot::Waiting => {
            draw_diag_body(frame, app, selected, area, block, "waiting for dependencies", Color::Yellow);
        }
        Slot::Blocked(reason) => {
            draw_diag_body(
                frame,
                app,
                selected,
                area,
                block,
                &format!("blocked: {reason}"),
                Color::Magenta,
            );
        }
        Slot::SpawnFailed(err) => {
            let p = Paragraph::new(format!("(spawn failed: {err})"))
                .style(Style::default().fg(Color::Red))
                .block(block);
            frame.render_widget(p, area);
        }
    }
}

/// Restyle the frame-buffer cells of search matches visible in the log view.
/// tui-term draws visible screen cell (r, c) at (inner.x + c, inner.y + r),
/// and at scrollback offset `s` the window covers absolute rows
/// (total - s)..(total - s + height), so the mapping is direct.
fn draw_log_highlights(frame: &mut Frame, app: &App, parser: &mut vt100::Parser, area: Rect) {
    let inner = Rect {
        x: area.x + 1,
        y: area.y + 1,
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(2),
    };
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let s = parser.screen().scrollback();
    let total = search::probe_total(parser);
    let top = total - s; // absolute row shown on the first visible line
    let matches = &app.log_search.matches;
    let first = matches.partition_point(|m| m.row < top);
    let buf = frame.buffer_mut();
    for (i, m) in matches.iter().enumerate().skip(first) {
        let vis = m.row - top;
        if vis >= inner.height as usize {
            break; // sorted by row: nothing below is visible either
        }
        let col_start = m.col_start.min(inner.width);
        let col_end = m.col_end.min(inner.width);
        if col_start >= col_end {
            continue;
        }
        let style = if i == app.log_search.current {
            MATCH_CURRENT
        } else {
            MATCH_OTHER
        };
        buf.set_style(
            Rect {
                x: inner.x + col_start,
                y: inner.y + vis as u16,
                width: col_end - col_start,
                height: 1,
            },
            style,
        );
    }
}

fn draw_diag_body(
    frame: &mut Frame,
    app: &App,
    idx: usize,
    area: Rect,
    block: Block<'static>,
    header: &str,
    color: Color,
) {
    let diag = app.mgr.diagnostics(idx);
    let inner_h = area.height.saturating_sub(2) as usize; // borders
    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(Span::styled(
        format!("({header})"),
        Style::default().fg(color),
    )));
    lines.push(Line::raw(""));
    if diag.is_empty() {
        lines.push(Line::from(Span::styled(
            "  (no diagnostics yet)",
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        // Show the most recent (inner_h - 2) lines.
        let want = inner_h.saturating_sub(2).max(1);
        let start = diag.len().saturating_sub(want);
        for msg in &diag[start..] {
            lines.push(Line::from(Span::styled(
                format!("  {msg}"),
                Style::default().fg(Color::Gray),
            )));
        }
    }
    let p = Paragraph::new(lines).block(block);
    frame.render_widget(p, area);
}

fn draw_status(frame: &mut Frame, app: &App, area: Rect) {
    let mode_span = match app.mode {
        Mode::Nav => Span::styled(
            " NAV ",
            Style::default().fg(Color::Black).bg(Color::Cyan).add_modifier(Modifier::BOLD),
        ),
        Mode::Focus => Span::styled(
            " FOCUS ",
            Style::default().fg(Color::Black).bg(Color::Yellow).add_modifier(Modifier::BOLD),
        ),
        Mode::Details => Span::styled(
            " DETAILS ",
            Style::default().fg(Color::Black).bg(Color::Magenta).add_modifier(Modifier::BOLD),
        ),
    };

    let status_span = slot_status_span(app, app.selected);

    let wrap_label = if app.wrap_of(app.selected) {
        " wrap:on "
    } else {
        " wrap:off "
    };
    let wrap_span = Span::styled(
        wrap_label,
        Style::default().fg(Color::Black).bg(Color::Blue),
    );

    let hints = match app.mode {
        Mode::Nav => "  ←/→ tabs · ↑/↓ scroll · Enter focus · / search · r restart · k kill · c clear · w wrap · y log path · d details · ^R/^K all · h help · q quit",
        Mode::Focus => "  Esc leave focus · all other keys go to the process",
        Mode::Details => "  ↑/↓ scroll · PgUp/PgDn ×10 · Home top · / search · y copy log path · d/Esc close · h help",
    };

    let mut spans = vec![
        mode_span,
        Span::raw(" "),
        status_span,
        Span::raw(" "),
        wrap_span,
    ];
    if let Some(notice) = app.notice() {
        spans.push(Span::styled(
            format!(" {notice} "),
            Style::default().fg(Color::Black).bg(Color::Green),
        ));
    }

    // Search input bar / match indicator for the mode-appropriate search.
    let (editing, active, query, count, current) = match app.mode {
        Mode::Details => (
            app.details_search.editing,
            app.details_search.active,
            app.details_search.query.as_str(),
            app.details_search.matches.len(),
            app.details_search.current,
        ),
        _ => (
            app.log_search.editing,
            app.log_search.active,
            app.log_search.query.as_str(),
            app.log_search.matches.len(),
            app.log_search.current,
        ),
    };
    if editing {
        // Input bar replaces the hints; a reversed cell acts as the cursor.
        spans.push(Span::styled(
            format!(" /{query}"),
            Style::default().fg(Color::White),
        ));
        spans.push(Span::styled(" ", Style::default().add_modifier(Modifier::REVERSED)));
        spans.push(Span::styled(
            "  Enter confirm · Esc cancel",
            Style::default().fg(Color::DarkGray),
        ));
    } else if active {
        // A confirmed search swaps the hints for search-mode keys.
        let (text, style) = if count == 0 {
            (
                format!(" no matches /{query} "),
                Style::default().fg(Color::White).bg(Color::Red),
            )
        } else {
            (
                format!(" ({}/{count}) /{query} ", current + 1),
                Style::default().fg(Color::Black).bg(Color::Yellow),
            )
        };
        spans.push(Span::styled(text, style));
        // In Focus mode keys go to the child, so keep the Focus hints there.
        let search_hints = match app.mode {
            Mode::Details => "  n/N next/prev match · / new search · Esc clear · h help",
            Mode::Nav => "  n older · N newer match · / new search · Esc clear · h help",
            Mode::Focus => hints,
        };
        spans.push(Span::styled(search_hints, Style::default().fg(Color::DarkGray)));
    } else {
        spans.push(Span::styled(hints, Style::default().fg(Color::DarkGray)));
    }
    let line = Line::from(spans);

    frame.render_widget(Paragraph::new(line), area);
}

fn tab_dot_color(app: &App, idx: usize) -> Color {
    match app.mgr.slot(idx) {
        Slot::Waiting => PENDING,
        Slot::Blocked(_) => Color::Magenta,
        Slot::SpawnFailed(_) => Color::Red,
        Slot::Process(p) => match p.status() {
            Status::Starting => PENDING,
            Status::Running => Color::Green,
            Status::Exited(info) => {
                exited_color(info.code, info.signal.is_some(), p.long_lived)
            }
            Status::SpawnFailed(_) => Color::Magenta,
        },
    }
}

fn slot_status_span(app: &App, idx: usize) -> Span<'static> {
    match app.mgr.slot(idx) {
        Slot::Waiting => Span::styled("waiting", Style::default().fg(PENDING)),
        Slot::Blocked(reason) => {
            Span::styled(format!("blocked: {reason}"), Style::default().fg(Color::Magenta))
        }
        Slot::SpawnFailed(err) => {
            Span::styled(format!("spawn failed: {err}"), Style::default().fg(Color::Red))
        }
        Slot::Process(p) => match p.status() {
            Status::Starting => Span::styled("starting", Style::default().fg(PENDING)),
            Status::Running => Span::styled("running", Style::default().fg(Color::Green)),
            Status::Exited(info) => {
                let color = exited_color(info.code, info.signal.is_some(), p.long_lived);
                let suffix = if app.mgr.retries_exhausted(idx) {
                    " (retries exhausted)"
                } else {
                    ""
                };
                Span::styled(format!("exited ({info}){suffix}"), Style::default().fg(color))
            }
            Status::SpawnFailed(err) => Span::styled(
                format!("spawn failed: {err}"),
                Style::default().fg(Color::Magenta),
            ),
        },
    }
}

/// The details pane content, one `Line` per rendered row. Shared by the
/// renderer and the details search (which matches against the flattened
/// text), so the two can never drift apart.
pub fn details_lines(app: &App, idx: usize) -> Vec<Line<'static>> {
    let cfg = match app.mgr.configs().get(idx) {
        Some(c) => c,
        None => return Vec::new(),
    };
    let slot = app.mgr.slot(idx);

    let label_style = Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD);
    let value_style = Style::default().fg(Color::White);
    let dim_style = Style::default().fg(Color::DarkGray);

    let mut lines: Vec<Line> = Vec::new();

    lines.push(Line::from(Span::styled(
        cfg.name.clone(),
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::raw(""));

    let pid_text = match &slot {
        Slot::Process(p) => p.pid().to_string(),
        _ => "—".to_string(),
    };
    lines.push(Line::from(vec![
        Span::styled("PID:    ", label_style),
        Span::styled(pid_text, value_style),
    ]));
    lines.push(Line::from(vec![
        Span::styled("Status: ", label_style),
        slot_status_span(app, idx),
    ]));

    lines.push(Line::raw(""));
    lines.push(Line::from(vec![
        Span::styled("Command: ", label_style),
        Span::styled(cfg.command.clone(), value_style),
    ]));
    let args_str = if cfg.args.is_empty() {
        "(none)".to_string()
    } else {
        cfg.args.join(" ")
    };
    lines.push(Line::from(vec![
        Span::styled("Args:    ", label_style),
        Span::styled(args_str, value_style),
    ]));
    lines.push(Line::from(vec![
        Span::styled("CWD:     ", label_style),
        Span::styled(cfg.cwd.display().to_string(), value_style),
    ]));
    lines.push(Line::from(vec![
        Span::styled("Long-lived: ", label_style),
        Span::styled(cfg.long_lived.to_string(), value_style),
    ]));
    lines.push(Line::from(match app.mgr.log_path(idx) {
        Some(p) => vec![
            Span::styled("Log:     ", label_style),
            Span::styled(p.display().to_string(), value_style),
            Span::styled("  (y to copy)", dim_style),
        ],
        None => vec![
            Span::styled("Log:     ", label_style),
            Span::styled("(session log capture disabled)", dim_style),
        ],
    }));

    lines.push(Line::raw(""));
    if cfg.env_files.is_empty() {
        lines.push(Line::from(vec![
            Span::styled("Env files: ", label_style),
            Span::styled("(none)", dim_style),
        ]));
    } else {
        lines.push(Line::from(Span::styled("Env files:", label_style)));
        for ef in &cfg.env_files {
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(ef.display().to_string(), value_style),
            ]));
        }
    }

    if !cfg.depends_on.is_empty() {
        lines.push(Line::raw(""));
        lines.push(Line::from(Span::styled("Depends on:", label_style)));
        for dep in &cfg.depends_on {
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(dep.name.clone(), value_style),
                Span::styled(format!(" until {:?}", dep.until), dim_style),
            ]));
        }
    }

    lines.push(Line::raw(""));
    match &slot {
        Slot::Process(p) => {
            lines.push(Line::from(Span::styled(
                format!("Environment ({} vars):", p.env.len()),
                label_style,
            )));
            for (k, v) in &p.env {
                lines.push(Line::from(vec![
                    Span::styled(k.clone(), Style::default().fg(Color::Green)),
                    Span::raw("="),
                    Span::styled(v.clone(), value_style),
                ]));
            }
        }
        _ => {
            lines.push(Line::from(Span::styled(
                "Environment:",
                label_style,
            )));
            lines.push(Line::from(Span::styled(
                "  (not spawned — env not yet resolved)",
                dim_style,
            )));
            if !cfg.env.is_empty() {
                lines.push(Line::raw(""));
                lines.push(Line::from(Span::styled(
                    format!("Config env overrides ({}):", cfg.env.len()),
                    label_style,
                )));
                let mut keys: Vec<&String> = cfg.env.keys().collect();
                keys.sort();
                for k in keys {
                    let v = &cfg.env[k];
                    lines.push(Line::from(vec![
                        Span::styled(k.clone(), Style::default().fg(Color::Green)),
                        Span::raw("="),
                        Span::styled(v.clone(), value_style),
                    ]));
                }
            }
        }
    }

    lines
}

/// One line's text as the user sees it: span contents concatenated.
pub fn flatten_line(line: &Line) -> String {
    line.spans.iter().map(|s| s.content.as_ref()).collect()
}

/// Re-style the byte `ranges` (offsets into the flattened line text) of a
/// details line as search matches, splitting spans at range boundaries.
/// `ranges` must be sorted and non-overlapping (regex find order).
fn highlight_line(line: Line<'static>, ranges: &[(Range<usize>, bool)]) -> Line<'static> {
    let mut out: Vec<Span<'static>> = Vec::new();
    let mut span_start = 0usize;
    for span in line.spans {
        let text = span.content.into_owned();
        let span_end = span_start + text.len();
        let mut cursor = 0usize; // byte position within this span's text
        for (r, current) in ranges {
            let start = r.start.max(span_start);
            let end = r.end.min(span_end);
            if start >= end {
                continue;
            }
            let (ls, le) = (start - span_start, end - span_start);
            if ls > cursor {
                out.push(Span::styled(text[cursor..ls].to_string(), span.style));
            }
            let style = if *current { MATCH_CURRENT } else { MATCH_OTHER };
            out.push(Span::styled(text[ls..le].to_string(), style));
            cursor = le;
        }
        if cursor < text.len() {
            out.push(Span::styled(text[cursor..].to_string(), span.style));
        }
        span_start = span_end;
    }
    Line::from(out)
}

fn draw_details_body(frame: &mut Frame, app: &mut App, area: Rect) {
    let idx = app.selected;
    let mut lines = details_lines(app, idx);

    // The pane is small (at most a few hundred short lines), so the details
    // search recomputes every frame while active; this also keeps matches in
    // step with live content like the status line.
    if app.details_search.active {
        let flat: Vec<String> = lines.iter().map(flatten_line).collect();
        app.details_search.recompute(&flat);
        for (mi, (line_idx, range)) in app.details_search.matches.iter().enumerate() {
            let ranges = [(range.clone(), mi == app.details_search.current)];
            if let Some(line) = lines.get_mut(*line_idx) {
                *line = highlight_line(std::mem::take(line), &ranges);
            }
        }
    }

    let title = app
        .mgr
        .configs()
        .get(idx)
        .map(|c| c.name.clone())
        .unwrap_or_default();
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" details: {title} "));

    let paragraph = Paragraph::new(lines)
        .block(block)
        .scroll((app.details_scroll, 0));
    frame.render_widget(paragraph, area);
}

/// Centered overlay listing every hotkey per mode. Must mirror the bindings
/// in `app.rs` (`handle_nav_key` / `handle_focus_key` / `handle_details_key`).
/// Scrollable (↑/↓) when the terminal is too short to fit all of it.
fn draw_help(frame: &mut Frame, app: &mut App, area: Rect) {
    let header_style = Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD);
    let key_style = Style::default().fg(Color::White);
    let desc_style = Style::default().fg(Color::Gray);

    let entry = |key: &str, desc: &str| {
        Line::from(vec![
            Span::styled(format!("  {key:<12}"), key_style),
            Span::styled(desc.to_string(), desc_style),
        ])
    };

    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(Span::styled("Navigation", header_style)));
    lines.push(entry("←/→", "previous / next tab"));
    lines.push(entry("↑/↓", "scroll output"));
    lines.push(entry("PgUp/PgDn", "scroll ×10"));
    lines.push(entry("Home/End", "scroll to top / bottom"));
    lines.push(entry("Enter", "focus the process (keys go to it)"));
    lines.push(entry("/", "search output (see Search below)"));
    lines.push(entry("r / ^R", "restart process / all"));
    lines.push(entry("k / ^K", "kill process / all"));
    lines.push(entry("c", "clear log view (display only)"));
    lines.push(entry("w", "toggle line wrap"));
    lines.push(entry("y", "copy log path"));
    lines.push(entry("d", "process details"));
    lines.push(entry("h", "this help"));
    lines.push(entry("q / ^C", "quit"));
    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled("Focus", header_style)));
    lines.push(entry("Esc", "leave focus"));
    lines.push(entry("(other)", "sent to the process"));
    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled("Details", header_style)));
    lines.push(entry("↑/↓", "scroll"));
    lines.push(entry("PgUp/PgDn", "scroll ×10"));
    lines.push(entry("Home", "scroll to top"));
    lines.push(entry("/", "search details (see Search below)"));
    lines.push(entry("y", "copy log path"));
    lines.push(entry("h", "this help"));
    lines.push(entry("d/Esc/q", "close details"));
    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled("Search (output & details)", header_style)));
    lines.push(entry("/", "open search; typing searches as you go"));
    lines.push(entry("Enter", "confirm and keep the matches"));
    lines.push(entry("Esc", "cancel input (restores view)"));
    lines.push(entry("n / N", "older / newer match in output"));
    lines.push(entry("n / N", "next / previous match in details"));
    lines.push(entry("Esc", "clear a confirmed search"));
    lines.push(entry("^U", "clear the query while typing"));

    let width = (area.width).min(56);
    let height = (area.height).min(lines.len() as u16 + 2); // + borders
    let rect = Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    };

    // Clamp the scroll offset so the last content line stops at the bottom
    // border instead of scrolling off into blank space.
    let inner_height = rect.height.saturating_sub(2) as usize; // borders
    let max_scroll = lines.len().saturating_sub(inner_height) as u16;
    app.help_scroll = app.help_scroll.min(max_scroll);

    let mut block = Block::default()
        .borders(Borders::ALL)
        .title(" Hotkeys (Esc/h to close) ");
    if max_scroll > 0 {
        let above = app.help_scroll > 0;
        let below = app.help_scroll < max_scroll;
        let hint = match (above, below) {
            (true, true) => " ↑/↓ more ",
            (true, false) => " ↑ more ",
            _ => " ↓ more ",
        };
        block = block.title_bottom(
            Line::from(Span::styled(hint, Style::default().fg(Color::DarkGray)))
                .right_aligned(),
        );
    }
    frame.render_widget(Clear, rect);
    frame.render_widget(
        Paragraph::new(lines).block(block).scroll((app.help_scroll, 0)),
        rect,
    );
}

const SPINNER: [&str; 4] = ["|", "/", "-", "\\"];

/// Full-screen view shown while the orchestrator is shutting down: one line per
/// process with its live state, so the user can see that long-lived processes
/// are still being terminated in the background.
fn draw_shutdown(frame: &mut Frame, app: &App, area: Rect) {
    let count = app.mgr.count();
    let frame_idx = app
        .shutdown_started
        .map(|t| (t.elapsed().as_millis() / 100) as usize % SPINNER.len())
        .unwrap_or(0);
    let spinner = SPINNER[frame_idx];

    let mut stopped = 0usize;
    let mut rows: Vec<Line> = Vec::new();
    for i in 0..count {
        let name = app.mgr.configs()[i].name.clone();
        let (state_span, is_stopped) = match app.mgr.slot(i) {
            Slot::Process(p) => match p.status() {
                Status::Starting | Status::Running => (
                    Span::styled(
                        format!("{spinner} terminating..."),
                        Style::default().fg(Color::Yellow),
                    ),
                    false,
                ),
                Status::Exited(info) => {
                    let color = exited_color(info.code, info.signal.is_some(), p.long_lived);
                    (
                        Span::styled(format!("stopped ({info})"), Style::default().fg(color)),
                        true,
                    )
                }
                Status::SpawnFailed(err) => (
                    Span::styled(
                        format!("spawn failed: {err}"),
                        Style::default().fg(Color::Magenta),
                    ),
                    true,
                ),
            },
            _ => (
                Span::styled("not running", Style::default().fg(Color::DarkGray)),
                true,
            ),
        };
        if is_stopped {
            stopped += 1;
        }
        rows.push(Line::from(vec![
            Span::styled(
                format!("  {name:<20} "),
                Style::default().fg(Color::White),
            ),
            state_span,
        ]));
    }

    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled(
        format!("  Stopped {stopped}/{count} processes"),
        Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::raw(""));
    lines.extend(rows);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Shutting down (press q to force quit) ");
    let paragraph = Paragraph::new(lines).block(block);
    frame.render_widget(paragraph, area);
}

