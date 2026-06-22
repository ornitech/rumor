mod app;
mod clipboard;
mod config;
mod env;
mod keys;
mod logfile;
mod ports;
mod process;
mod search;
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

// vt100 0.16 underflows on a 1-row or 1-col screen the moment output wraps or
// scrolls (`size.cols - width` and `prev_pos.row -= scrolled` in grid.rs), which
// panics the process reader thread and poisons its parser lock. body_inner_size
// is the sole source of every PTY/parser dimension (initial spawn and resize),
// so clamp it to a size vt100 can actually handle, even when the real terminal
// is degenerate (0/1 rows) or headless. 2x2 is the smallest panic-free size.
const MIN_PTY_ROWS: u16 = 2;
const MIN_PTY_COLS: u16 = 2;

fn body_inner_size(width: u16, height: u16) -> (u16, u16) {
    (
        height.saturating_sub(UI_CHROME_ROWS).max(MIN_PTY_ROWS),
        width.saturating_sub(UI_CHROME_COLS).max(MIN_PTY_COLS),
    )
}

/// Base directory shared by rumor's own log file and the per-process session
/// logs: `~/Library/Logs/rumor` (macOS) / `~/.local/share/rumor` (Linux).
fn rumor_dir() -> PathBuf {
    dirs_data_local()
        .unwrap_or_else(std::env::temp_dir)
        .join("rumor")
}

fn init_tracing() {
    let dir = rumor_dir();
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

/// Parsed command line.
#[derive(Debug, PartialEq)]
struct CliArgs {
    config_path: PathBuf,
    /// Run only processes carrying any of these tags (plus deps). Empty = all.
    tags: Vec<String>,
    /// Raw mode: stream all output to one stdout instead of the TUI.
    raw: bool,
    /// Raw-mode print filter by process name. Empty = print all. Everything
    /// still runs; this only narrows what prints.
    only: Vec<String>,
    /// Raw mode: pass ANSI escapes through instead of stripping them.
    color: bool,
}

/// Greedily collect the comma/space-separated values following a list flag
/// (`-t`, `--only`), advancing `i` past them. Returns how many were collected;
/// `0` signals "flag given without a value" (invalid usage).
fn collect_list(args: &[String], i: &mut usize, out: &mut Vec<String>) -> usize {
    let mut collected = 0;
    *i += 1;
    while *i < args.len() && !args[*i].starts_with('-') {
        for t in args[*i].split(',') {
            let t = t.trim();
            if !t.is_empty() {
                out.push(t.to_string());
                collected += 1;
            }
        }
        *i += 1;
    }
    collected
}

/// Parse the CLI args. Returns `None` when usage is invalid (caller prints
/// usage + exits 2).
///
/// Grammar: `rumor [config.json] [-t|--tags TAG[,TAG...] ...] [--raw [--only
/// NAME[,NAME...] ...] [--color]]`. The `-t` and `--only` flags greedily
/// consume following non-flag tokens (each split on commas). `--only` and
/// `--color` are only valid together with `--raw`.
fn parse_args(args: &[String]) -> Option<CliArgs> {
    let mut config_path: Option<PathBuf> = None;
    let mut tags: Vec<String> = Vec::new();
    let mut only: Vec<String> = Vec::new();
    let mut raw = false;
    let mut color = false;

    let mut i = 1;
    while i < args.len() {
        let arg = &args[i];
        if arg == "-t" || arg == "--tags" {
            if collect_list(args, &mut i, &mut tags) == 0 {
                return None;
            }
        } else if arg == "--only" {
            if collect_list(args, &mut i, &mut only) == 0 {
                return None;
            }
        } else if arg == "--raw" {
            raw = true;
            i += 1;
        } else if arg == "--color" {
            color = true;
            i += 1;
        } else if arg.starts_with('-') {
            return None;
        } else if config_path.is_none() {
            config_path = Some(PathBuf::from(arg));
            i += 1;
        } else {
            return None;
        }
    }

    // --only / --color are meaningless without raw mode's combined stream.
    if !raw && (!only.is_empty() || color) {
        return None;
    }

    let config_path = match config_path {
        Some(p) => p,
        None if Path::new(DEFAULT_CONFIG).exists() => PathBuf::from(DEFAULT_CONFIG),
        None => return None,
    };
    Some(CliArgs {
        config_path,
        tags,
        raw,
        only,
        color,
    })
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let CliArgs {
        config_path,
        tags,
        raw,
        only,
        color,
    } = match parse_args(&args) {
        Some(p) => p,
        None => {
            eprintln!("usage: rumor [config.json] [-t|--tags TAG ...] [--raw [--only NAME ...] [--color]]");
            eprintln!();
            eprintln!("With no config argument, rumor loads ./rumor.json from the current directory.");
            eprintln!("--tags runs only processes carrying any of the given tags, plus their dependencies.");
            eprintln!();
            eprintln!("--raw  streams all process output to a single stdout (no TUI), one line per");
            eprintln!("       process prefixed with [name]. Intended for AI agents and piping.");
            eprintln!("--only restricts which processes' output prints in raw mode (by name); every");
            eprintln!("       process still runs. --color passes ANSI escapes through (default: strip).");
            eprintln!();
            eprintln!("Logs are written to {}/rumor/rumor.log",
                dirs_data_local().map(|p| p.display().to_string()).unwrap_or_else(|| "<tmp>".into()));
            eprintln!("Set RUMOR_LOG=debug to trace dependency readiness checks.");
            std::process::exit(2);
        }
    };

    init_tracing();
    let loaded = Config::load(&config_path).context("loading config")?;
    if !loaded.dynamic_ports.is_empty() {
        let mut assigned: Vec<String> = loaded
            .dynamic_ports
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect();
        assigned.sort();
        tracing::info!("dynamic ports: {}", assigned.join(" "));
    }

    let processes = if tags.is_empty() {
        loaded.config.processes
    } else {
        config::filter_by_tags(&loaded.config.processes, &tags)
            .context("filtering processes by tags")?
    };

    // Per-process session log capture. Always on; RUMOR_NO_SESSION_LOGS is the
    // only escape hatch. A None session_dir disables capture but never rumor.
    let sessions_root = rumor_dir().join("sessions");
    let session_dir: Option<PathBuf> = if std::env::var_os("RUMOR_NO_SESSION_LOGS").is_some() {
        None
    } else {
        let stem = config_path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "rumor".to_string());
        logfile::create_session_dir(&sessions_root, &stem)
    };
    if let Some(d) = &session_dir {
        tracing::info!(dir = %d.display(), "session logs");
    }
    // Best-effort cleanup of old sessions, off the startup path.
    tokio::task::spawn_blocking(move || {
        logfile::cleanup_old_sessions(&sessions_root, Duration::from_secs(7 * 86_400));
    });

    let result = if raw {
        // Raw mode: no TUI, no terminal raw/alt-screen, no panic hook. Output
        // goes to ordinary stdout; SIGINT/SIGTERM drive graceful shutdown.
        run_raw(processes, session_dir.clone(), only, color).await
    } else {
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

        let r = run(processes, session_dir.clone()).await;

        restore_terminal();
        r
    };

    // Printed after leaving the alternate screen so it stays selectable in the
    // terminal — easy to copy into an LLM chat or bug report.
    if let Some(d) = &session_dir {
        println!("Session logs: {}", d.display());
    }

    if let Err(e) = &result {
        eprintln!("rumor: {e:#}");
    }
    result
}

async fn run(
    processes: Vec<crate::config::ProcessConfig>,
    session_dir: Option<PathBuf>,
) -> Result<()> {
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

    let mgr = ProcessManager::new(processes, initial_pty, session_dir);
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

/// Grace period for children to exit after SIGTERM before we SIGKILL them in
/// raw mode. Mirrors the TUI's shutdown grace.
const RAW_SHUTDOWN_GRACE: Duration = Duration::from_secs(3);

/// Raw mode: run every process and stream their combined output to stdout, one
/// line at a time prefixed with `[name]`. No TUI. `only` (by process name)
/// restricts what prints; everything still runs. `color` passes ANSI through.
async fn run_raw(
    processes: Vec<crate::config::ProcessConfig>,
    session_dir: Option<PathBuf>,
    only: Vec<String>,
    color: bool,
) -> Result<()> {
    use std::collections::HashSet;
    use std::io::Write as _;
    use tokio::signal::unix::{signal, SignalKind};

    use crate::process::{RawLine, RawSink};

    // Fail fast on an --only that names no process (mirrors filter_by_tags).
    let names: HashSet<&str> = processes.iter().map(|p| p.name.as_str()).collect();
    for o in &only {
        if !names.contains(o.as_str()) {
            anyhow::bail!("--only: no process named '{o}' in this config");
        }
    }

    // No terminal to measure; pick a wide size so children wrap less. Raw mode
    // never resizes.
    let pty_size = PtySize {
        rows: 24,
        cols: 200,
        pixel_width: 0,
        pixel_height: 0,
    };

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<RawLine>();
    let sink = RawSink {
        tx,
        strip: !color,
    };
    let mgr = ProcessManager::new_raw(processes, pty_size, session_dir, sink);

    // Single printer task => each line is written atomically and prefixed
    // correctly; ordering is channel-send (≈ arrival) order across processes.
    let allow: Option<HashSet<String>> = if only.is_empty() {
        None
    } else {
        Some(only.into_iter().collect())
    };
    let printer = tokio::task::spawn_blocking(move || {
        let stdout = io::stdout();
        let mut out = io::BufWriter::new(stdout.lock());
        while let Some(RawLine { name, line }) = rx.blocking_recv() {
            if allow.as_ref().is_some_and(|a| !a.contains(&name)) {
                continue;
            }
            let _ = write!(out, "[{name}] ");
            let _ = out.write_all(&line);
            let _ = out.write_all(b"\n");
            let _ = out.flush();
        }
        let _ = out.flush();
    });

    // Run until a signal arrives or every process has exited on its own.
    let mut sigint = signal(SignalKind::interrupt()).context("installing SIGINT handler")?;
    let mut sigterm = signal(SignalKind::terminate()).context("installing SIGTERM handler")?;
    let mut tick = tokio::time::interval(Duration::from_millis(100));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = sigint.recv() => { mgr.begin_shutdown(); break; }
            _ = sigterm.recv() => { mgr.begin_shutdown(); break; }
            _ = tick.tick() => { if mgr.all_finished() { break; } }
        }
    }

    // Let children settle after SIGTERM, then SIGKILL stragglers.
    let deadline = tokio::time::Instant::now() + RAW_SHUTDOWN_GRACE;
    while !mgr.all_exited() && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    mgr.force_kill_all();

    // Drop the manager so its sink sender closes, letting the printer drain the
    // remaining buffered lines and finish. Bounded wait as a safety net.
    drop(mgr);
    let _ = tokio::time::timeout(Duration::from_secs(2), printer).await;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    /// CliArgs with the given config + tags and all raw-mode fields defaulted.
    fn cli(config: &str, tags: &[&str]) -> CliArgs {
        CliArgs {
            config_path: PathBuf::from(config),
            tags: tags.iter().map(|s| s.to_string()).collect(),
            raw: false,
            only: vec![],
            color: false,
        }
    }

    // vt100 panics on a 1-row/1-col parser when output wraps or scrolls, so the
    // PTY size derived from the terminal must never drop below 2x2 — including
    // for a degenerate (0x0 / 1x1) or headless terminal.
    #[test]
    fn body_inner_size_stays_above_vt100_minimum() {
        for (w, h) in [(0u16, 0u16), (1, 1), (2, 6), (7, 7), (8, 7), (200, 50)] {
            let (rows, cols) = body_inner_size(w, h);
            assert!(rows >= MIN_PTY_ROWS, "rows {rows} < {MIN_PTY_ROWS} for {w}x{h}");
            assert!(cols >= MIN_PTY_COLS, "cols {cols} < {MIN_PTY_COLS} for {w}x{h}");
        }
    }

    #[test]
    fn explicit_path_is_used_verbatim() {
        assert_eq!(
            parse_args(&args(&["rumor", "custom.json"])),
            Some(cli("custom.json", &[]))
        );
    }

    #[test]
    fn two_positional_args_is_invalid() {
        assert_eq!(parse_args(&args(&["rumor", "a", "b"])), None);
    }

    #[test]
    fn tags_collect_space_separated() {
        assert_eq!(
            parse_args(&args(&["rumor", "c.json", "-t", "backend", "api"])),
            Some(cli("c.json", &["backend", "api"]))
        );
    }

    #[test]
    fn tags_split_on_commas() {
        assert_eq!(
            parse_args(&args(&["rumor", "c.json", "--tags", "backend,api"])),
            Some(cli("c.json", &["backend", "api"]))
        );
    }

    #[test]
    fn repeated_tag_flag_accumulates() {
        assert_eq!(
            parse_args(&args(&["rumor", "c.json", "-t", "backend", "-t", "api"])),
            Some(cli("c.json", &["backend", "api"]))
        );
    }

    #[test]
    fn tag_flag_without_value_is_invalid() {
        assert_eq!(parse_args(&args(&["rumor", "c.json", "-t"])), None);
    }

    #[test]
    fn unknown_flag_is_invalid() {
        assert_eq!(parse_args(&args(&["rumor", "c.json", "-x"])), None);
    }

    #[test]
    fn raw_flag_sets_raw() {
        let parsed = parse_args(&args(&["rumor", "c.json", "--raw"])).unwrap();
        assert!(parsed.raw);
        assert!(parsed.only.is_empty());
        assert!(!parsed.color);
    }

    #[test]
    fn only_collects_like_tags() {
        let space = parse_args(&args(&["rumor", "c.json", "--raw", "--only", "a", "b"])).unwrap();
        let comma = parse_args(&args(&["rumor", "c.json", "--raw", "--only", "a,b"])).unwrap();
        let repeated =
            parse_args(&args(&["rumor", "c.json", "--raw", "--only", "a", "--only", "b"])).unwrap();
        let want = vec!["a".to_string(), "b".to_string()];
        assert_eq!(space.only, want);
        assert_eq!(comma.only, want);
        assert_eq!(repeated.only, want);
    }

    #[test]
    fn color_flag_requires_raw() {
        assert!(parse_args(&args(&["rumor", "c.json", "--raw", "--color"]))
            .unwrap()
            .color);
        // --color / --only without --raw is invalid usage.
        assert_eq!(parse_args(&args(&["rumor", "c.json", "--color"])), None);
        assert_eq!(parse_args(&args(&["rumor", "c.json", "--only", "a"])), None);
    }

    #[test]
    fn only_without_value_is_invalid() {
        assert_eq!(parse_args(&args(&["rumor", "c.json", "--raw", "--only"])), None);
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
        let without = parse_args(&args(&["rumor"]));

        // rumor.json present -> defaults to it.
        std::fs::write(dir.join(DEFAULT_CONFIG), "{}").unwrap();
        let with = parse_args(&args(&["rumor"]));

        std::env::set_current_dir(&prev).unwrap();
        std::fs::remove_dir_all(&dir).ok();

        assert_eq!(without, None);
        assert_eq!(with, Some(cli(DEFAULT_CONFIG, &[])));
    }
}
