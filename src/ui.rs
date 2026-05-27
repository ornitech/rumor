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

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // tab bar
            Constraint::Min(1),    // body
            Constraint::Length(1), // status / hotkey line
        ])
        .split(area);

    draw_tabs(frame, app, chunks[0]);
    draw_body(frame, app, chunks[1]);
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
        Mode::Nav => "  ←/→ tabs · ↑/↓ scroll · PgUp/PgDn ×10 · Enter focus · r restart · k kill · w wrap · ^R restart-all · ^K kill-all · q quit",
        Mode::Focus => "  Esc leave focus · all other keys go to the process",
    };

    let line = Line::from(vec![
        mode_span,
        Span::raw(" "),
        status_span,
        Span::raw(" "),
        wrap_span,
        Span::styled(hints, Style::default().fg(Color::DarkGray)),
    ]);

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

