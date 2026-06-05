mod app;
mod config;
mod env;
mod keys;
mod process;
mod status_color;
mod template;
mod ui;

use std::io::{self, Write};
use std::path::{Path, PathBuf};
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

const DEFAULT_CONFIG: &str = "rumor.json";

/// Resolve which config file to load from the CLI args.
/// Returns `None` when usage is invalid (caller prints usage + exits 2).
fn resolve_config_path(args: &[String]) -> Option<PathBuf> {
    match args.len() {
        2 => Some(PathBuf::from(&args[1])),
        1 if Path::new(DEFAULT_CONFIG).exists() => Some(PathBuf::from(DEFAULT_CONFIG)),
        _ => None,
    }
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let config_path = match resolve_config_path(&args) {
        Some(p) => p,
        None => {
            eprintln!("usage: rumor [config.json]");
            eprintln!();
            eprintln!("With no argument, rumor loads ./rumor.json from the current directory.");
            eprintln!();
            eprintln!("Logs are written to {}/rumor/rumor.log",
                dirs_data_local().map(|p| p.display().to_string()).unwrap_or_else(|| "<tmp>".into()));
            eprintln!("Set RUMOR_LOG=debug to trace dependency readiness checks.");
            std::process::exit(2);
        }
    };

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

        // Force-quit (second `q`) or the shutdown phase finished.
        if app.should_quit || app.shutdown_complete() {
            break;
        }
    }

    // Backstop: SIGKILL anything still alive (e.g. force-quit before the grace
    // elapsed) so we never orphan a child. No-op on a clean shutdown.
    app.mgr.force_kill_all();

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn explicit_path_is_used_verbatim() {
        assert_eq!(
            resolve_config_path(&args(&["rumor", "custom.json"])),
            Some(PathBuf::from("custom.json"))
        );
    }

    #[test]
    fn too_many_args_is_invalid() {
        assert_eq!(resolve_config_path(&args(&["rumor", "a", "b"])), None);
    }

    // Both bare-invocation cases depend on the process-global cwd, so they live
    // in one test to avoid a data race between parallel test threads.
    #[test]
    fn bare_invocation_uses_rumor_json_only_when_present() {
        let dir = std::env::temp_dir().join(format!("rumor-cfg-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        let prev = std::env::current_dir().unwrap();
        std::env::set_current_dir(&dir).unwrap();

        // No rumor.json present -> invalid usage.
        let without = resolve_config_path(&args(&["rumor"]));

        // rumor.json present -> defaults to it.
        std::fs::write(dir.join(DEFAULT_CONFIG), "{}").unwrap();
        let with = resolve_config_path(&args(&["rumor"]));

        std::env::set_current_dir(&prev).unwrap();
        std::fs::remove_dir_all(&dir).ok();

        assert_eq!(without, None);
        assert_eq!(with, Some(PathBuf::from(DEFAULT_CONFIG)));
    }
}
