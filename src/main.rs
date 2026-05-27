mod app;
mod config;
mod env;
mod keys;
mod process;
mod status_color;
mod ui;

use std::io::{self, Write};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use crossterm::event::{Event, EventStream, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use futures::StreamExt;
use portable_pty::PtySize;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use tracing::warn;

use crate::app::App;
use crate::config::Config;
use crate::process::ProcessManager;

// Layout: 3-row tab bar + body (with its own top+bottom border = 2 rows) +
// 1-row status line. So body inner rows = terminal_height - 3 - 2 - 1 = -6.
const UI_CHROME_ROWS: u16 = 6;
// Body block left+right borders = 2 cols.
const UI_CHROME_COLS: u16 = 2;

fn body_inner_size(width: u16, height: u16) -> (u16, u16) {
    (
        height.saturating_sub(UI_CHROME_ROWS).max(1),
        width.saturating_sub(UI_CHROME_COLS).max(1),
    )
}

fn init_tracing() {
    let dir = dirs_data_local()
        .unwrap_or_else(|| std::env::temp_dir())
        .join("rumor");
    let _ = std::fs::create_dir_all(&dir);
    let file_appender = tracing_appender::rolling::never(&dir, "rumor.log");
    let subscriber = tracing_subscriber::fmt()
        .with_writer(file_appender)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("RUMOR_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_ansi(false)
        .finish();
    let _ = tracing::subscriber::set_global_default(subscriber);
}

fn dirs_data_local() -> Option<PathBuf> {
    if let Ok(home) = std::env::var("HOME") {
        #[cfg(target_os = "macos")]
        return Some(PathBuf::from(home).join("Library/Logs"));
        #[cfg(not(target_os = "macos"))]
        return Some(PathBuf::from(home).join(".local/share"));
    }
    None
}

fn restore_terminal() {
    let mut stdout = io::stdout();
    let _ = disable_raw_mode();
    let _ = execute!(stdout, LeaveAlternateScreen);
    let _ = stdout.flush();
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 2 {
        eprintln!("usage: rumor <config.json>");
        eprintln!();
        eprintln!("Logs are written to {}/rumor/rumor.log",
            dirs_data_local().map(|p| p.display().to_string()).unwrap_or_else(|| "<tmp>".into()));
        eprintln!("Set RUMOR_LOG=debug to trace dependency readiness checks.");
        std::process::exit(2);
    }
    let config_path = PathBuf::from(&args[1]);

    init_tracing();
    let loaded = Config::load(&config_path).context("loading config")?;

    // Install a panic hook so a panic during the TUI loop doesn't leave the
    // terminal in raw / alternate-screen mode.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_terminal();
        default_hook(info);
    }));

    enable_raw_mode().context("enabling raw mode")?;
    {
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen).context("entering alt screen")?;
    }

    let result = run(loaded.config.processes).await;

    restore_terminal();

    if let Err(e) = &result {
        eprintln!("rumor: {e:#}");
    }
    result
}

async fn run(processes: Vec<crate::config::ProcessConfig>) -> Result<()> {
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend).context("creating terminal")?;
    let term_size = terminal.size().context("reading terminal size")?;
    let (body_rows, body_cols) = body_inner_size(term_size.width, term_size.height);
    let initial_pty = PtySize {
        rows: body_rows,
        cols: body_cols,
        pixel_width: 0,
        pixel_height: 0,
    };

    let mgr = ProcessManager::new(processes, initial_pty);
    let mut app = App::new(mgr, body_rows, body_cols);

    let mut events = EventStream::new();
    let mut tick = tokio::time::interval(Duration::from_millis(50));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        terminal
            .draw(|f| ui::draw(f, &mut app))
            .context("drawing frame")?;

        tokio::select! {
            biased;
            maybe_event = events.next() => match maybe_event {
                Some(Ok(Event::Key(k))) => {
                    // On most platforms only Press events arrive in raw mode,
                    // but filter explicitly for safety.
                    if k.kind == KeyEventKind::Press {
                        app.handle_key(k);
                    }
                }
                Some(Ok(Event::Resize(w, h))) => {
                    let (rows, cols) = body_inner_size(w, h);
                    app.apply_resize(rows, cols);
                }
                Some(Ok(_)) => {}
                Some(Err(e)) => warn!("event stream error: {e}"),
                None => break,
            },
            _ = tick.tick() => {}
        }

        if app.should_quit {
            break;
        }
    }

    app.mgr.shutdown(Duration::from_secs(3)).await;

    Ok(())
}
