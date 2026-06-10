use std::collections::{BTreeMap, VecDeque};
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tracing::{debug, warn};

/// Cap on the per-process raw byte ring buffer used to replay scrollback into
/// a fresh `vt100::Parser` on resize. Eviction is line-aligned so we never
/// split an ANSI escape sequence at the buffer boundary.
const RAW_CAP: usize = 1 << 20; // 1 MiB

/// Per-process scrollback row count for vt100. New parsers built on replay
/// inherit this; the buffered raw bytes act as the source of truth.
const SCROLLBACK: usize = 2000;

use crate::config::ProcessConfig;
use crate::env;
use crate::logfile::AnsiLogWriter;

#[derive(Debug, Clone)]
pub enum Status {
    Starting,
    Running,
    Exited(ExitInfo),
    SpawnFailed(String),
}

#[derive(Debug, Clone)]
pub struct ExitInfo {
    pub code: u32,
    pub signal: Option<String>,
}

impl std::fmt::Display for ExitInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.signal {
            Some(s) => write!(f, "signal {s}"),
            None => write!(f, "exit {}", self.code),
        }
    }
}

pub struct Process {
    pub name: String,
    /// Copied from `ProcessConfig::long_lived`. UI uses this to decide whether
    /// a clean `exit 0` is success (false) or an unexpected stop (true).
    pub long_lived: bool,
    /// Snapshot of the resolved env passed to the child at spawn time. Sorted
    /// for deterministic display. Source of truth for "what did this PID see"
    /// even after the `.env` files on disk change.
    pub env: BTreeMap<String, String>,
    pub parser: Arc<Mutex<vt100::Parser>>,
    /// Tee of every byte read from the PTY, capped at RAW_CAP. Used to replay
    /// scrollback into a fresh parser on resize so wrap toggles and window
    /// grows visually reflow / refill existing output.
    raw: Arc<Mutex<VecDeque<u8>>>,
    pub status_rx: watch::Receiver<Status>,
    status_tx: watch::Sender<Status>,
    writer_tx: mpsc::UnboundedSender<Vec<u8>>,
    master: Arc<Mutex<Box<dyn MasterPty + Send>>>,
    killer: Mutex<Box<dyn portable_pty::ChildKiller + Send + Sync>>,
    pid: u32,
    _read_task: JoinHandle<()>,
    _write_task: JoinHandle<()>,
    _wait_task: JoinHandle<()>,
}

impl Process {
    pub fn spawn(cfg: &ProcessConfig, size: PtySize, log_path: Option<&std::path::Path>) -> Result<Self> {
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(size)
            .with_context(|| format!("opening pty for {}", cfg.name))?;

        let mut cmd = CommandBuilder::new(&cfg.command);
        cmd.args(&cfg.args);
        cmd.cwd(&cfg.cwd);

        // Clear inherited env so we control what the child sees.
        cmd.env_clear();
        let child_env = env::build_env(&cfg.cwd, &cfg.global_env_files, &cfg.env_files, &cfg.env)
            .with_context(|| format!("building env for {}", cfg.name))?;
        for (k, v) in &child_env {
            cmd.env(k, v);
        }
        // Tell the child we're an xterm-256color terminal (typical default).
        if !child_env.contains_key("TERM") {
            cmd.env("TERM", "xterm-256color");
        }
        // Snapshot the resolved env (including the TERM fallback) for the
        // details overlay. BTreeMap gives alphabetical order for free.
        let mut env_snapshot: BTreeMap<String, String> = child_env
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        env_snapshot
            .entry("TERM".to_string())
            .or_insert_with(|| "xterm-256color".to_string());

        let child = pair
            .slave
            .spawn_command(cmd)
            .with_context(|| format!("spawning {} ({})", cfg.name, cfg.command))?;
        // Slave is no longer needed in the parent; release it so the only
        // reference is held by the child process.
        drop(pair.slave);

        let pid = child.process_id().ok_or_else(|| anyhow!("no pid"))?;
        let killer = child.clone_killer();

        let parser = Arc::new(Mutex::new(vt100::Parser::new(
            size.rows,
            size.cols,
            SCROLLBACK,
        )));
        let raw: Arc<Mutex<VecDeque<u8>>> = Arc::new(Mutex::new(VecDeque::with_capacity(8192)));

        let reader = pair
            .master
            .try_clone_reader()
            .with_context(|| format!("cloning pty reader for {}", cfg.name))?;
        let writer = pair
            .master
            .take_writer()
            .with_context(|| format!("taking pty writer for {}", cfg.name))?;

        let master: Arc<Mutex<Box<dyn MasterPty + Send>>> =
            Arc::new(Mutex::new(pair.master));

        let (status_tx, status_rx) = watch::channel(Status::Starting);
        let _ = status_tx.send(Status::Running);

        // Session log capture is strictly best-effort: any failure here warns
        // and proceeds without a log file. Spawn never fails because of it.
        let log_writer = log_path.and_then(|path| {
            match std::fs::OpenOptions::new().create(true).append(true).open(path) {
                Ok(mut file) => {
                    // A non-empty file means this slot respawned within the
                    // session; mark the boundary. Written pre-stripper so it
                    // never interleaves mid-escape.
                    if file.metadata().map(|m| m.len() > 0).unwrap_or(false) {
                        let _ = writeln!(file, "\n----- restarted at {} -----", now_hms());
                    }
                    Some(AnsiLogWriter::new(file, cfg.name.clone()))
                }
                Err(e) => {
                    warn!(name = %cfg.name, path = %path.display(), error = %e,
                        "could not open session log; capture disabled for this process");
                    None
                }
            }
        });

        let read_task = spawn_read_task(
            reader,
            Arc::clone(&parser),
            Arc::clone(&raw),
            cfg.name.clone(),
            log_writer,
        );
        let (writer_tx, writer_rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let write_task = spawn_write_task(writer, writer_rx, cfg.name.clone());
        let wait_task = spawn_wait_task(child, status_tx.clone(), cfg.name.clone());

        Ok(Self {
            name: cfg.name.clone(),
            long_lived: cfg.long_lived,
            env: env_snapshot,
            parser,
            raw,
            status_rx,
            status_tx,
            writer_tx,
            master,
            killer: Mutex::new(killer),
            pid,
            _read_task: read_task,
            _write_task: write_task,
            _wait_task: wait_task,
        })
    }

    pub fn status(&self) -> Status {
        self.status_rx.borrow().clone()
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// Snapshot of the raw byte ring buffer for things like log-regex matching.
    pub fn raw_snapshot(&self) -> Vec<u8> {
        self.raw.lock().unwrap().iter().copied().collect()
    }

    pub fn is_running(&self) -> bool {
        matches!(self.status(), Status::Starting | Status::Running)
    }

    pub fn write_input(&self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        if let Err(e) = self.writer_tx.send(bytes.to_vec()) {
            debug!(name = %self.name, error = %e, "writer channel closed");
        }
    }

    pub fn resize(&self, size: PtySize) {
        if let Err(e) = self.master.lock().unwrap().resize(size) {
            warn!(name = %self.name, error = %e, "pty resize failed");
        }
        // Rebuild the parser from the raw byte history at the new size so
        // wrap toggles and window grows visually reflow / refill existing
        // output. Lock order matches the read task: raw → parser.
        let raw_snapshot: Vec<u8> = {
            let r = self.raw.lock().unwrap();
            r.iter().copied().collect()
        };
        let mut parser_guard = self.parser.lock().unwrap();
        let prev_scrollback = parser_guard.screen().scrollback();
        let mut fresh = vt100::Parser::new(size.rows, size.cols, SCROLLBACK);
        fresh.process(&raw_snapshot);
        // Preserve the user's scrollback position across the rebuild (vt100
        // clamps internally if the new history has fewer rows).
        fresh.screen_mut().set_scrollback(prev_scrollback);
        *parser_guard = fresh;
    }

    /// Send SIGTERM. After `grace`, send SIGKILL if still running.
    pub fn terminate(&self, grace: Duration) {
        if !self.is_running() {
            return;
        }
        unsafe { libc::kill(self.pid as i32, libc::SIGTERM) };
        // Schedule a SIGKILL fallback. We can't capture &self into a task
        // (lifetime), so capture what we need.
        let pid = self.pid;
        let mut status_rx = self.status_rx.clone();
        tokio::spawn(async move {
            let _ = tokio::time::timeout(grace, async {
                while matches!(*status_rx.borrow(), Status::Starting | Status::Running) {
                    if status_rx.changed().await.is_err() {
                        break;
                    }
                }
            })
            .await;
            if matches!(*status_rx.borrow(), Status::Starting | Status::Running) {
                warn!(pid, "SIGTERM grace expired; sending SIGKILL");
                unsafe { libc::kill(pid as i32, libc::SIGKILL) };
            }
        });
    }

    /// Wait for the process to exit (either naturally or after `terminate`).
    pub async fn wait_for_exit(&self) {
        let mut rx = self.status_rx.clone();
        while matches!(*rx.borrow(), Status::Starting | Status::Running) {
            if rx.changed().await.is_err() {
                break;
            }
        }
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        // Best-effort: if still running when dropped, send SIGTERM so we don't
        // orphan children when the orchestrator quits or restarts.
        if self.is_running() {
            unsafe { libc::kill(self.pid as i32, libc::SIGTERM) };
        }
        // Hard fallback: ask the killer (which sends SIGKILL on unix via the
        // underlying std::process::Child::kill).
        let _ = self.killer.lock().unwrap().kill();
        let _ = self.status_tx.send(self.status());
    }
}

fn spawn_read_task(
    mut reader: Box<dyn Read + Send>,
    parser: Arc<Mutex<vt100::Parser>>,
    raw: Arc<Mutex<VecDeque<u8>>>,
    name: String,
    mut log: Option<AnsiLogWriter>,
) -> JoinHandle<()> {
    tokio::task::spawn_blocking(move || {
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => {
                    debug!(name = %name, "pty reader EOF");
                    break;
                }
                Ok(n) => {
                    // Lock order: raw before parser. Same order as `resize`
                    // below, which prevents deadlock and ensures bytes are
                    // visible to a concurrent replay.
                    if let Ok(mut r) = raw.lock() {
                        r.extend(buf[..n].iter().copied());
                        evict_to_cap(&mut r, RAW_CAP);
                    }
                    if let Ok(mut p) = parser.lock() {
                        p.process(&buf[..n]);
                    }
                    // Session log tee: lives on this thread only, no locks.
                    if let Some(l) = log.as_mut() {
                        l.write_chunk(&buf[..n]);
                    }
                }
                Err(e) => {
                    debug!(name = %name, error = %e, "pty reader error");
                    break;
                }
            }
        }
        if let Some(mut l) = log.take() {
            l.flush();
        }
    })
}

/// Trim a VecDeque<u8> down to `cap`, dropping at '\n' boundaries so we
/// never split an ANSI escape sequence across the eviction point.
fn evict_to_cap(buf: &mut VecDeque<u8>, cap: usize) {
    while buf.len() > cap {
        match buf.iter().position(|&b| b == b'\n') {
            Some(pos) => {
                for _ in 0..=pos {
                    buf.pop_front();
                }
            }
            None => {
                // No newline anywhere — drop everything rather than keep a
                // pathological super-long line that exceeds cap on its own.
                buf.clear();
                break;
            }
        }
    }
}

fn spawn_write_task(
    mut writer: Box<dyn Write + Send>,
    mut rx: mpsc::UnboundedReceiver<Vec<u8>>,
    name: String,
) -> JoinHandle<()> {
    tokio::task::spawn_blocking(move || {
        while let Some(chunk) = rx.blocking_recv() {
            if let Err(e) = writer.write_all(&chunk) {
                debug!(name = %name, error = %e, "pty writer error");
                break;
            }
            if let Err(e) = writer.flush() {
                debug!(name = %name, error = %e, "pty writer flush error");
                break;
            }
        }
    })
}

fn spawn_wait_task(
    mut child: Box<dyn portable_pty::Child + Send + Sync>,
    status_tx: watch::Sender<Status>,
    name: String,
) -> JoinHandle<()> {
    tokio::task::spawn_blocking(move || {
        match child.wait() {
            Ok(status) => {
                let info = ExitInfo {
                    code: status.exit_code(),
                    signal: status.signal().map(|s| s.to_string()),
                };
                debug!(name = %name, ?info, "child exited");
                let _ = status_tx.send(Status::Exited(info));
            }
            Err(e) => {
                warn!(name = %name, error = %e, "child wait failed");
                let _ = status_tx.send(Status::SpawnFailed(e.to_string()));
            }
        }
    })
}

// ============================================================================
// ProcessManager (Slot model with async dependency watchers)
// ============================================================================

use std::collections::HashMap;
use std::sync::Mutex as StdMutex;

use crate::config::ReadinessCondition;

/// State of a single process slot. Cloneable so it can ride a `watch::channel`.
#[derive(Clone)]
pub enum Slot {
    /// No spawn attempted yet, or a previous spawn was wiped on restart.
    Waiting,
    /// One or more dependencies failed or never became ready. The watcher
    /// stays alive watching for dep recovery; if a dep recovers, the slot
    /// transitions back through Waiting → (eventually) Process.
    Blocked(String),
    /// Process has been spawned. May be Running or Exited; check `p.status()`.
    Process(Arc<Process>),
    /// `Process::spawn` itself failed (bad command, permissions, etc.).
    SpawnFailed(String),
}

impl std::fmt::Debug for Slot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Slot::Waiting => write!(f, "Waiting"),
            Slot::Blocked(r) => write!(f, "Blocked({r})"),
            Slot::Process(p) => write!(f, "Process({}, pid={})", p.name, p.pid),
            Slot::SpawnFailed(e) => write!(f, "SpawnFailed({e})"),
        }
    }
}

pub struct ProcessManager {
    inner: Arc<ManagerInner>,
}

struct ManagerInner {
    configs: Vec<ProcessConfig>,
    /// One watch::Sender per slot. Subscribers read the current Slot value.
    slots: Vec<watch::Sender<Slot>>,
    /// In-flight watcher task per slot, aborted on restart so we don't
    /// double-spawn.
    watchers: StdMutex<Vec<Option<JoinHandle<()>>>>,
    /// Per-slot diagnostic ring (cap DIAG_CAP). Watcher pushes human-readable
    /// progress; UI renders this in the body area while Waiting / Blocked.
    diagnostics: Vec<StdMutex<VecDeque<String>>>,
    name_to_idx: HashMap<String, usize>,
    /// Target PTY size per slot, used when (re)spawning that slot. Kept in sync
    /// with the live display by `App::resize_one` via `set_size`, so a restarted
    /// or dependency-delayed process spawns at the current, wrap-aware width.
    sizes: Vec<StdMutex<PtySize>>,
    /// Session log file per slot, fixed for the manager's lifetime. Respawns
    /// reuse the same path, which is what gives restart-append semantics.
    /// All None when session log capture is disabled.
    log_paths: Vec<Option<std::path::PathBuf>>,
}

const DIAG_CAP: usize = 200;

impl ProcessManager {
    pub fn new(
        configs: Vec<ProcessConfig>,
        size: PtySize,
        session_dir: Option<std::path::PathBuf>,
    ) -> Self {
        let name_to_idx = configs
            .iter()
            .enumerate()
            .map(|(i, c)| (c.name.clone(), i))
            .collect();
        let slots: Vec<_> = configs
            .iter()
            .map(|_| watch::channel(Slot::Waiting).0)
            .collect();
        let watchers = (0..configs.len()).map(|_| None).collect();
        let diagnostics = (0..configs.len())
            .map(|_| StdMutex::new(VecDeque::new()))
            .collect();
        let sizes = (0..configs.len())
            .map(|_| StdMutex::new(size))
            .collect();
        let log_paths = match &session_dir {
            Some(dir) => crate::logfile::assign_log_paths(&configs, dir),
            None => vec![None; configs.len()],
        };
        let inner = Arc::new(ManagerInner {
            configs,
            slots,
            watchers: StdMutex::new(watchers),
            diagnostics,
            name_to_idx,
            sizes,
            log_paths,
        });
        let mgr = Self { inner };
        mgr.start_all();
        mgr
    }

    pub fn count(&self) -> usize {
        self.inner.configs.len()
    }

    pub fn configs(&self) -> &[ProcessConfig] {
        &self.inner.configs
    }

    pub fn slot(&self, idx: usize) -> Slot {
        self.inner.slots[idx].borrow().clone()
    }

    /// Snapshot of the per-slot watcher diagnostic log. UI renders this for
    /// Waiting / Blocked slots so the user can see why a process hasn't
    /// spawned yet without tailing the log file.
    pub fn diagnostics(&self, idx: usize) -> Vec<String> {
        self.inner.diagnostics[idx]
            .lock()
            .unwrap()
            .iter()
            .cloned()
            .collect()
    }

    /// Session log file for a slot, if capture is enabled. Manager-level (not
    /// on `Process`) so the UI can show it even while the slot is Waiting /
    /// Blocked / SpawnFailed.
    pub fn log_path(&self, idx: usize) -> Option<&std::path::Path> {
        self.inner.log_paths[idx].as_deref()
    }

    pub fn process(&self, idx: usize) -> Option<Arc<Process>> {
        match self.slot(idx) {
            Slot::Process(p) => Some(p),
            _ => None,
        }
    }

    pub fn start_all(&self) {
        for i in 0..self.count() {
            self.start_internal(i, None);
        }
    }

    fn start_internal(&self, idx: usize, wait_for_old: Option<Arc<Process>>) {
        if let Some(h) = self.inner.watchers.lock().unwrap()[idx].take() {
            h.abort();
        }
        // Fresh watcher = fresh diagnostic log.
        self.inner.diagnostics[idx].lock().unwrap().clear();
        self.inner.slots[idx].send_replace(Slot::Waiting);
        let inner = Arc::clone(&self.inner);
        let handle = tokio::spawn(watch_slot(idx, inner, wait_for_old));
        self.inner.watchers.lock().unwrap()[idx] = Some(handle);
    }

    pub fn restart(&self, idx: usize) {
        let old = if let Slot::Process(p) = self.slot(idx) {
            p.terminate(Duration::from_secs(3));
            Some(p)
        } else {
            None
        };
        self.start_internal(idx, old);
    }

    pub fn kill(&self, idx: usize) {
        if let Slot::Process(p) = self.slot(idx) {
            p.terminate(Duration::from_secs(3));
        }
    }

    pub fn restart_all(&self) {
        for i in 0..self.count() {
            self.restart(i);
        }
    }

    pub fn kill_all(&self) {
        for i in 0..self.count() {
            self.kill(i);
        }
    }

    /// Record the target PTY size for a slot (used at its next spawn) and, if a
    /// process is currently live in that slot, resize it immediately.
    pub fn set_size(&self, idx: usize, size: PtySize) {
        *self.inner.sizes[idx].lock().unwrap() = size;
        if let Slot::Process(p) = self.slot(idx) {
            p.resize(size);
        }
    }

    /// Abort all in-flight dependency watchers so none of them respawn a
    /// process while we're shutting down or restarting everything.
    fn abort_watchers(&self) {
        for h in self.inner.watchers.lock().unwrap().iter_mut() {
            if let Some(h) = h.take() {
                h.abort();
            }
        }
    }

    /// Begin a non-blocking shutdown: stop watchers (so nothing respawns), then
    /// SIGTERM every running process with a scheduled SIGKILL fallback. Returns
    /// immediately; callers poll `all_exited` to learn when it has finished.
    pub fn begin_shutdown(&self) {
        self.abort_watchers();
        self.kill_all();
    }

    /// True when no slot holds a still-running process.
    pub fn all_exited(&self) -> bool {
        (0..self.count()).all(|i| match self.slot(i) {
            Slot::Process(p) => !p.is_running(),
            _ => true,
        })
    }

    /// Non-blocking SIGKILL sweep over anything still alive. Used as the
    /// force-quit backstop so we never orphan a child on immediate exit.
    pub fn force_kill_all(&self) {
        self.abort_watchers();
        for i in 0..self.count() {
            if let Slot::Process(p) = self.slot(i) {
                if p.is_running() {
                    unsafe { libc::kill(p.pid as i32, libc::SIGKILL) };
                }
            }
        }
    }

    /// Blocking shutdown: SIGTERM all, await up to `timeout`, then SIGKILL
    /// stragglers. The interactive TUI uses the non-blocking `begin_shutdown` +
    /// `all_exited` poll instead; this remains as a synchronous teardown for the
    /// integration tests (`tests/pty_smoke.rs`).
    #[allow(dead_code)]
    pub async fn shutdown(&self, timeout: Duration) {
        // Abort watchers first so they don't spawn fresh processes mid-shutdown.
        self.abort_watchers();
        for i in 0..self.count() {
            if let Slot::Process(p) = self.slot(i) {
                if p.is_running() {
                    unsafe { libc::kill(p.pid as i32, libc::SIGTERM) };
                }
            }
        }
        let deadline = tokio::time::Instant::now() + timeout;
        for i in 0..self.count() {
            if let Slot::Process(p) = self.slot(i) {
                let remaining =
                    deadline.saturating_duration_since(tokio::time::Instant::now());
                let _ = tokio::time::timeout(remaining, p.wait_for_exit()).await;
            }
        }
        for i in 0..self.count() {
            if let Slot::Process(p) = self.slot(i) {
                if p.is_running() {
                    unsafe { libc::kill(p.pid as i32, libc::SIGKILL) };
                }
            }
        }
    }
}

fn push_diag(inner: &ManagerInner, idx: usize, msg: impl Into<String>) {
    let line = format!("[{}] {}", now_hms(), msg.into());
    let mut d = inner.diagnostics[idx].lock().unwrap();
    d.push_back(line);
    while d.len() > DIAG_CAP {
        d.pop_front();
    }
}

fn now_hms() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let h = (now / 3600) % 24;
    let m = (now / 60) % 60;
    let s = now % 60;
    format!("{h:02}:{m:02}:{s:02}")
}

async fn watch_slot(
    idx: usize,
    inner: Arc<ManagerInner>,
    wait_for_old: Option<Arc<Process>>,
) {
    // On restart, wait for the previous process to fully exit before spawning
    // a fresh one (avoids port-in-use races during quick restarts).
    if let Some(old) = wait_for_old {
        push_diag(&inner, idx, "waiting for previous instance to exit");
        old.wait_for_exit().await;
        push_diag(&inner, idx, "previous instance exited");
    }

    let cfg = inner.configs[idx].clone();
    if cfg.depends_on.is_empty() {
        push_diag(&inner, idx, "no dependencies, spawning");
    } else {
        let names: Vec<&str> = cfg.depends_on.iter().map(|d| d.name.as_str()).collect();
        push_diag(
            &inner,
            idx,
            format!("waiting on dependencies: {}", names.join(", ")),
        );
    }
    let slot_tx = &inner.slots[idx];

    'outer: loop {
        let mut blocked_this_pass = false;
        for dep in &cfg.depends_on {
            let dep_idx = match inner.name_to_idx.get(&dep.name) {
                Some(i) => *i,
                None => {
                    slot_tx.send_replace(Slot::SpawnFailed(format!(
                        "unknown dependency '{}'",
                        dep.name
                    )));
                    return;
                }
            };
            let mut dep_rx = inner.slots[dep_idx].subscribe();
            // Wait until this dep is Process(_) and meets its readiness condition.
            // If the dep is Blocked/SpawnFailed/Waiting, transition self to Blocked
            // and wait for the dep's state to change before re-checking.
            loop {
                let snap = dep_rx.borrow().clone();
                match snap {
                    Slot::Process(p) => {
                        push_diag(
                            &inner,
                            idx,
                            format!("checking dep '{}' ({:?})", dep.name, dep.until),
                        );
                        match wait_until_with_diag(&p, &dep.until, &inner, idx).await {
                            Ok(()) => {
                                push_diag(
                                    &inner,
                                    idx,
                                    format!("dep '{}' ready", dep.name),
                                );
                                break;
                            }
                            Err(e) => {
                                push_diag(
                                    &inner,
                                    idx,
                                    format!("dep '{}' failed: {}", dep.name, e),
                                );
                                slot_tx.send_replace(Slot::Blocked(format!(
                                    "dep {}: {}",
                                    dep.name, e
                                )));
                                blocked_this_pass = true;
                                if dep_rx.changed().await.is_err() {
                                    return;
                                }
                            }
                        }
                    }
                    Slot::Blocked(reason) => {
                        push_diag(
                            &inner,
                            idx,
                            format!("dep '{}' is blocked: {}", dep.name, reason),
                        );
                        slot_tx.send_replace(Slot::Blocked(format!(
                            "dep {} blocked ({})",
                            dep.name, reason
                        )));
                        blocked_this_pass = true;
                        if dep_rx.changed().await.is_err() {
                            return;
                        }
                    }
                    Slot::SpawnFailed(e) => {
                        push_diag(
                            &inner,
                            idx,
                            format!("dep '{}' spawn failed: {}", dep.name, e),
                        );
                        slot_tx.send_replace(Slot::Blocked(format!(
                            "dep {} spawn failed: {}",
                            dep.name, e
                        )));
                        blocked_this_pass = true;
                        if dep_rx.changed().await.is_err() {
                            return;
                        }
                    }
                    Slot::Waiting => {
                        push_diag(
                            &inner,
                            idx,
                            format!("dep '{}' not spawned yet", dep.name),
                        );
                        if dep_rx.changed().await.is_err() {
                            return;
                        }
                    }
                }
            }
        }

        if blocked_this_pass {
            // We may have transitioned through Blocked; re-check all deps from the
            // top to make sure earlier ones haven't regressed in the meantime.
            continue 'outer;
        }

        // All deps cleared; spawn this process.
        push_diag(&inner, idx, "all dependencies ready; spawning");
        let size = *inner.sizes[idx].lock().unwrap();
        match Process::spawn(&cfg, size, inner.log_paths[idx].as_deref()) {
            Ok(p) => {
                push_diag(&inner, idx, format!("spawned (pid {})", p.pid));
                slot_tx.send_replace(Slot::Process(Arc::new(p)));
                return;
            }
            Err(e) => {
                push_diag(&inner, idx, format!("spawn failed: {e}"));
                slot_tx.send_replace(Slot::SpawnFailed(e.to_string()));
                return;
            }
        }
    }
}

async fn wait_until_with_diag(
    proc: &Arc<Process>,
    cond: &ReadinessCondition,
    inner: &Arc<ManagerInner>,
    idx: usize,
) -> Result<()> {
    debug!(name = %proc.name, cond = ?cond, "wait_until: begin");
    let result = match cond {
        ReadinessCondition::Port(p) => wait_for_port(*p, proc, inner, idx).await,
        ReadinessCondition::Log(rx) => wait_for_log(rx, proc, inner, idx).await,
        ReadinessCondition::Exit(c) => wait_for_exit_code(*c, proc, inner, idx).await,
    };
    match &result {
        Ok(()) => debug!(name = %proc.name, cond = ?cond, "wait_until: ready"),
        Err(e) => debug!(name = %proc.name, cond = ?cond, error = %e, "wait_until: failed"),
    }
    result
}

async fn wait_for_port(
    port: u16,
    proc: &Arc<Process>,
    inner: &Arc<ManagerInner>,
    idx: usize,
) -> Result<()> {
    let mut backoff = Duration::from_millis(50);
    let mut attempt: u32 = 0;
    loop {
        attempt += 1;
        if !proc.is_running() {
            debug!(name = %proc.name, port, attempt, "wait_for_port: dep exited");
            push_diag(
                inner,
                idx,
                format!("port {port}: dep '{}' exited", proc.name),
            );
            return Err(anyhow!("exited before port {} accepted", port));
        }
        let v4 = try_connect("127.0.0.1", port).await;
        if v4 {
            debug!(name = %proc.name, port, attempt, "wait_for_port: 127.0.0.1 OK");
            push_diag(inner, idx, format!("port {port}: 127.0.0.1 OK"));
            return Ok(());
        }
        let v6 = try_connect("::1", port).await;
        if v6 {
            debug!(name = %proc.name, port, attempt, "wait_for_port: ::1 OK");
            push_diag(inner, idx, format!("port {port}: ::1 OK"));
            return Ok(());
        }
        debug!(
            name = %proc.name,
            port,
            attempt,
            backoff_ms = backoff.as_millis() as u64,
            "wait_for_port: no connect (v4=false v6=false)"
        );
        // Only emit a diag every few attempts so the in-tab log stays readable.
        if attempt == 1 || attempt % 3 == 0 {
            push_diag(
                inner,
                idx,
                format!(
                    "port {port}: no connect (attempt {attempt}, retry in {}ms)",
                    backoff.as_millis()
                ),
            );
        }
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(Duration::from_secs(2));
    }
}

async fn try_connect(host: &str, port: u16) -> bool {
    tokio::time::timeout(
        Duration::from_millis(200),
        tokio::net::TcpStream::connect((host, port)),
    )
    .await
    .ok()
    .and_then(|r| r.ok())
    .is_some()
}

async fn wait_for_log(
    pattern: &str,
    proc: &Arc<Process>,
    inner: &Arc<ManagerInner>,
    idx: usize,
) -> Result<()> {
    let re = regex::Regex::new(pattern).map_err(|e| anyhow!("bad regex: {e}"))?;
    let mut attempt: u32 = 0;
    let mut last_diag_bytes: usize = 0;
    loop {
        attempt += 1;
        let bytes = proc.raw_snapshot();
        let len = bytes.len();
        if !bytes.is_empty() {
            let text = String::from_utf8_lossy(&bytes);
            if re.is_match(&text) {
                debug!(name = %proc.name, pattern, attempt, bytes = len, "wait_for_log: match");
                push_diag(inner, idx, format!("log /{}/ matched", pattern));
                return Ok(());
            }
        }
        if !proc.is_running() {
            let bytes = proc.raw_snapshot();
            let text = String::from_utf8_lossy(&bytes);
            if re.is_match(&text) {
                debug!(name = %proc.name, pattern, "wait_for_log: match after exit");
                push_diag(inner, idx, format!("log /{}/ matched (after exit)", pattern));
                return Ok(());
            }
            debug!(name = %proc.name, pattern, "wait_for_log: exited without match");
            push_diag(
                inner,
                idx,
                format!("log /{}/: dep exited without match", pattern),
            );
            return Err(anyhow!("exited before log matched /{}/", pattern));
        }
        debug!(name = %proc.name, pattern, attempt, bytes = len, "wait_for_log: no match yet");
        // Throttle the in-tab diag: emit when we see a non-trivial chunk of
        // new bytes (>=512) or every 30 attempts (~3s).
        if attempt == 1 || (len.saturating_sub(last_diag_bytes) >= 512) || attempt % 30 == 0 {
            push_diag(
                inner,
                idx,
                format!("log /{}/: no match yet ({} bytes scanned)", pattern, len),
            );
            last_diag_bytes = len;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn wait_for_exit_code(
    expected: i32,
    proc: &Arc<Process>,
    inner: &Arc<ManagerInner>,
    idx: usize,
) -> Result<()> {
    let mut rx = proc.status_rx.clone();
    push_diag(
        inner,
        idx,
        format!("waiting for '{}' to exit with code {}", proc.name, expected),
    );
    loop {
        let snap = rx.borrow().clone();
        match snap {
            Status::Exited(info) => {
                if info.code as i32 == expected {
                    debug!(name = %proc.name, expected, code = info.code, "wait_for_exit_code: match");
                    push_diag(inner, idx, format!("'{}' exited {}", proc.name, info.code));
                    return Ok(());
                }
                debug!(name = %proc.name, expected, code = info.code, "wait_for_exit_code: mismatch");
                push_diag(
                    inner,
                    idx,
                    format!(
                        "'{}' exited {} (expected {})",
                        proc.name, info.code, expected
                    ),
                );
                return Err(anyhow!(
                    "exited with code {} (expected {})",
                    info.code,
                    expected
                ));
            }
            Status::SpawnFailed(e) => {
                debug!(name = %proc.name, error = %e, "wait_for_exit_code: dep spawn failed");
                push_diag(
                    inner,
                    idx,
                    format!("'{}' spawn failed: {}", proc.name, e),
                );
                return Err(anyhow!("dep spawn failed: {}", e));
            }
            other => {
                debug!(name = %proc.name, expected, status = ?other, "wait_for_exit_code: waiting for exit");
            }
        }
        if rx.changed().await.is_err() {
            return Err(anyhow!("dep status channel closed"));
        }
    }
}

