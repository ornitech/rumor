use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Tabs};
use ratatui::Frame;
use tui_term::widget::PseudoTerminal;

use crate::app::{App, Mode};
use crate::process::{Slot, Status};
use crate::status_color::exited_color;

pub fn draw(frame: &mut Frame, app: &mut App) {
    let area = frame.area();

    if app.shutting_down {
        draw_shutdown(frame, app, area);
        return;
    }

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
}

fn draw_tabs(frame: &mut Frame, app: &mut App, area: Rect) {
    let block = Block::default().borders(Borders::ALL).title("rumor");
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

    let titles: Vec<Line> = app.mgr.configs()[start..end]
        .iter()
        .enumerate()
        .map(|(offset, cfg)| {
            let i = start + offset;
            let dot_style = Style::default().fg(tab_dot_color(app, i));
            Line::from(vec![
                Span::styled("● ", dot_style),
                Span::raw(cfg.name.clone()),
            ])
        })
        .collect();

    let highlight = if app.mode == Mode::Focus {
        Style::default()
            .fg(Color::Black)
            .bg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .fg(Color::Black)
            .bg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    };

    let selected_local = app.selected.saturating_sub(start);
    let tabs = Tabs::new(titles)
        .select(selected_local)
        .highlight_style(highlight)
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
            let parser = proc.parser.lock().unwrap();
            let term = PseudoTerminal::new(parser.screen()).block(block);
            frame.render_widget(term, area);
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
        Mode::Nav => "  ←/→ tabs · ↑/↓ scroll · Enter focus · r restart · k kill · w wrap · y log path · d details · ^R/^K all · q quit",
        Mode::Focus => "  Esc leave focus · all other keys go to the process",
        Mode::Details => "  ↑/↓ scroll · PgUp/PgDn ×10 · Home top · y copy log path · d/Esc close",
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
    spans.push(Span::styled(hints, Style::default().fg(Color::DarkGray)));
    let line = Line::from(spans);

    frame.render_widget(Paragraph::new(line), area);
}

fn tab_dot_color(app: &App, idx: usize) -> Color {
    match app.mgr.slot(idx) {
        Slot::Waiting => Color::Yellow,
        Slot::Blocked(_) => Color::Magenta,
        Slot::SpawnFailed(_) => Color::Red,
        Slot::Process(p) => match p.status() {
            Status::Starting => Color::Yellow,
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
        Slot::Waiting => Span::styled("waiting", Style::default().fg(Color::Yellow)),
        Slot::Blocked(reason) => {
            Span::styled(format!("blocked: {reason}"), Style::default().fg(Color::Magenta))
        }
        Slot::SpawnFailed(err) => {
            Span::styled(format!("spawn failed: {err}"), Style::default().fg(Color::Red))
        }
        Slot::Process(p) => match p.status() {
            Status::Starting => Span::styled("starting", Style::default().fg(Color::Yellow)),
            Status::Running => Span::styled("running", Style::default().fg(Color::Green)),
            Status::Exited(info) => {
                let color = exited_color(info.code, info.signal.is_some(), p.long_lived);
                Span::styled(format!("exited ({info})"), Style::default().fg(color))
            }
            Status::SpawnFailed(err) => Span::styled(
                format!("spawn failed: {err}"),
                Style::default().fg(Color::Magenta),
            ),
        },
    }
}

fn draw_details_body(frame: &mut Frame, app: &App, area: Rect) {
    let idx = app.selected;
    let cfg = match app.mgr.configs().get(idx) {
        Some(c) => c,
        None => return,
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

    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" details: {} ", cfg.name));

    let paragraph = Paragraph::new(lines)
        .block(block)
        .scroll((app.details_scroll, 0));
    frame.render_widget(paragraph, area);
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

