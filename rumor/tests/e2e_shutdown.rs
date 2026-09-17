//! End-to-end shutdown tests against the real `rumor` binary.
//!
//! Each test launches the built binary (TUI mode hosted in a real PTY, or
//! `--raw` as a plain child), waits until every configured child has written
//! its process-group id and inner pids to disk, fires a trigger (an OS signal
//! to rumor, a keypress, closing the PTY), and then asserts that rumor exited
//! within its shutdown budget and that no member of any child process group
//! survived. Liveness is checked with `ps`, not with the pidfiles alone, so a
//! grandchild rumor never knew about is still caught.
//!
//! Sync tests on purpose (std threads, no tokio): a hung runtime in the test
//! must never mask a hung rumor.
//!
//! The harness SIGKILLs every recorded group and rumor itself on drop, so a
//! failing test does not leak busy orphans onto the developer's machine.

#![cfg(unix)]

use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::os::fd::FromRawFd;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};

const BIN: &str = env!("CARGO_BIN_EXE_rumor");

/// rumor's own shutdown grace (3s) plus slack for SIGKILL delivery, runtime
/// teardown, and a slow CI box. A clean shutdown finishes well inside this.
const EXIT_BUDGET: Duration = Duration::from_secs(7);
/// How long a group may linger after rumor has exited. SIGKILL is
/// asynchronous but fast; anything slower than this is an orphan.
const CLEANUP_BUDGET: Duration = Duration::from_secs(2);
/// A run consisting only of a TERM-honouring child must not sit out the full
/// grace; this proves the TERM path works and shutdown does not always wait.
const FAST_EXIT_BUDGET: Duration = Duration::from_millis(1500);
/// Debug binary startup + node/python startup on a loaded CI runner.
const READY_BUDGET: Duration = Duration::from_secs(20);

// ---------------------------------------------------------------------------
// Child shapes
// ---------------------------------------------------------------------------

/// The process trees rumor has to tear down. Each writes `<name>.pgid` (the
/// `$$` of the outer `sh`, which portable-pty `setsid`s into its own group, so
/// pgid == that pid) and `<name>.<n>.pid` for every inner process.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Shape {
    /// Leader ignores TERM/HUP/INT and loops. Needs the grace -> KILL path.
    Stubborn,
    /// `sh -c "inner & wait"`: the `npm run dev` shape. Leader alive,
    /// grandchild ignores TERM.
    Wrapper,
    /// Leader backgrounds a stubborn grandchild and exits 0 as soon as the
    /// grandchild has installed its traps (it waits for the pidfile: exiting
    /// earlier would HUP the not-yet-trapping grandchild along with the
    /// session), so the group has no leader by the time shutdown starts.
    ExitedLeader,
    /// Three nested `sh` levels, innermost stubborn.
    Deep,
    /// `exec sleep`: dies on TERM. Used for the fast-exit assertion.
    Polite,
    /// `exec node` with TERM/HUP/INT handlers installed, printing continuously.
    /// This is the shape that pins a core on EIO when orphaned. Skipped when
    /// `node` is not on PATH.
    NoisyNode,
    /// Exits 1 immediately with an aggressive retry policy. Proves nothing is
    /// respawned during or after teardown.
    Retrying,
    /// A python child that `setsid`s into a new session (outside rumor's reach
    /// by design) but keeps the PTY slave open. rumor must still exit on time.
    /// Skipped when `python3` is not on PATH.
    Escapee,
}

impl Shape {
    fn name(self) -> &'static str {
        match self {
            Shape::Stubborn => "stubborn",
            Shape::Wrapper => "wrapper",
            Shape::ExitedLeader => "exited-leader",
            Shape::Deep => "deep",
            Shape::Polite => "polite",
            Shape::NoisyNode => "noisy-node",
            Shape::Retrying => "retrying",
            Shape::Escapee => "escapee",
        }
    }

    fn available(self) -> bool {
        match self {
            Shape::NoisyNode => on_path("node"),
            Shape::Escapee => on_path("python3"),
            _ => true,
        }
    }

    /// Inner pidfiles this shape writes (beyond `<name>.pgid`).
    fn inner_pidfiles(self) -> &'static [&'static str] {
        match self {
            Shape::Wrapper => &["wrapper.1.pid"],
            Shape::ExitedLeader => &["exited-leader.1.pid"],
            Shape::Deep => &["deep.1.pid"],
            Shape::NoisyNode => &["noisy-node.1.pid"],
            Shape::Escapee => &["escapee.1.pid"],
            _ => &[],
        }
    }

    fn script(self) -> &'static str {
        match self {
            Shape::Stubborn => {
                "trap '' TERM HUP INT\n\
                 echo $$ > \"$PIDS/stubborn.pgid\"\n\
                 while :; do sleep 1; done\n"
            }
            Shape::Wrapper => {
                "trap '' TERM HUP INT\n\
                 echo $$ > \"$PIDS/wrapper.pgid\"\n\
                 sh \"$SCRIPTS/inner.sh\" \"$PIDS/wrapper.1.pid\" &\n\
                 wait\n"
            }
            Shape::ExitedLeader => {
                "echo $$ > \"$PIDS/exited-leader.pgid\"\n\
                 sh \"$SCRIPTS/inner.sh\" \"$PIDS/exited-leader.1.pid\" &\n\
                 while [ ! -s \"$PIDS/exited-leader.1.pid\" ]; do sleep 0.05; done\n\
                 exit 0\n"
            }
            Shape::Deep => {
                "trap '' TERM HUP INT\n\
                 echo $$ > \"$PIDS/deep.pgid\"\n\
                 sh \"$SCRIPTS/deep2.sh\" &\n\
                 wait\n"
            }
            Shape::Polite => {
                "echo $$ > \"$PIDS/polite.pgid\"\n\
                 exec sleep 1000\n"
            }
            Shape::NoisyNode => {
                "echo $$ > \"$PIDS/noisy-node.pgid\"\n\
                 exec node -e '\
                 process.on(\"SIGTERM\", () => {}); \
                 process.on(\"SIGHUP\", () => {}); \
                 process.on(\"SIGINT\", () => {}); \
                 require(\"fs\").writeFileSync(process.env.PIDS + \"/noisy-node.1.pid\", String(process.pid)); \
                 setInterval(() => console.log(\"tick\"), 20);'\n"
            }
            Shape::Retrying => {
                "echo $$ >> \"$PIDS/retrying.pgids\"\n\
                 exit 1\n"
            }
            Shape::Escapee => {
                "echo $$ > \"$PIDS/escapee.pgid\"\n\
                 python3 -c '\
                 import os, signal, time\n\
                 os.setsid()\n\
                 signal.signal(signal.SIGTERM, signal.SIG_IGN)\n\
                 signal.signal(signal.SIGHUP, signal.SIG_IGN)\n\
                 signal.signal(signal.SIGINT, signal.SIG_IGN)\n\
                 open(os.environ[\"PIDS\"] + \"/escapee.1.pid\", \"w\").write(str(os.getpid()))\n\
                 time.sleep(1000)\n\
                 ' &\n\
                 wait\n"
            }
        }
    }

    fn config(self, scripts: &Path) -> serde_json::Value {
        let mut v = serde_json::json!({
            "name": self.name(),
            "command": "sh",
            "args": [scripts.join(format!("{}.sh", self.name()))],
            "cwd": scripts.parent().unwrap(),
        });
        if self == Shape::Retrying {
            v["retry"] = serde_json::json!({ "maxRetries": 50, "delayMs": 200 });
        }
        v
    }
}

/// The everyday matrix: every stubborn shape plus the polite and retrying
/// ones, and the node dev-server shape when node is installed.
fn all_shapes() -> Vec<Shape> {
    let mut v = vec![
        Shape::Stubborn,
        Shape::Wrapper,
        Shape::ExitedLeader,
        Shape::Deep,
        Shape::Polite,
        Shape::Retrying,
    ];
    if Shape::NoisyNode.available() {
        v.push(Shape::NoisyNode);
    } else {
        eprintln!("note: `node` not on PATH; skipping the noisy-node shape");
    }
    v
}

fn on_path(bin: &str) -> bool {
    Command::new("sh")
        .args(["-c", &format!("command -v {bin} >/dev/null 2>&1")])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// Process liveness oracle (independent of the pidfiles)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct PsRow {
    pid: i32,
    pgid: i32,
    cpu: String,
    command: String,
}

/// Same flags on macOS and procps.
fn ps_rows() -> Vec<PsRow> {
    let out = Command::new("ps")
        .args(["-axo", "pid=,pgid=,%cpu=,command="])
        .output()
        .expect("run ps");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let pid = it.next()?.parse().ok()?;
            let pgid = it.next()?.parse().ok()?;
            let cpu = it.next()?.to_string();
            let command = it.collect::<Vec<_>>().join(" ");
            Some(PsRow {
                pid,
                pgid,
                cpu,
                command,
            })
        })
        .collect()
}

fn group_members(pgids: &HashSet<i32>) -> Vec<PsRow> {
    ps_rows()
        .into_iter()
        .filter(|r| pgids.contains(&r.pgid))
        .collect()
}

fn alive(pid: i32) -> bool {
    unsafe { libc::kill(pid, 0) == 0 }
}

fn kill_group(pgid: i32, sig: i32) {
    unsafe {
        libc::kill(-pgid, sig);
    }
}

fn poll_until(budget: Duration, mut pred: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + budget;
    loop {
        if pred() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(Duration::from_millis(25));
    }
}

fn read_pid(path: &Path) -> Option<i32> {
    fs::read_to_string(path).ok()?.trim().parse().ok()
}

fn read_pids(path: &Path) -> Vec<i32> {
    fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.trim().parse().ok())
        .collect()
}

// ---------------------------------------------------------------------------
// Hosts: TUI in a PTY, or --raw as a plain child
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    Tui,
    Raw,
}

struct TuiHost {
    child: Box<dyn portable_pty::Child + Send + Sync>,
    master: Option<Box<dyn MasterPty + Send>>,
    writer: Option<Box<dyn Write + Send>>,
    stop: Arc<AtomicBool>,
    drain: Option<JoinHandle<()>>,
}

impl TuiHost {
    fn spawn(config: &Path, ws: &Path, envs: &[(&str, String)]) -> Self {
        let pty = native_pty_system();
        let pair = pty
            .openpty(PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("openpty");
        let mut cmd = CommandBuilder::new(BIN);
        cmd.arg(config);
        cmd.cwd(ws);
        for (k, v) in envs {
            cmd.env(k, v);
        }
        let child = pair.slave.spawn_command(cmd).expect("spawn rumor in pty");
        drop(pair.slave);

        // Drain the master on a thread: the TUI redraws every 50ms and would
        // block on a full PTY buffer. Poll with a stop flag so `close_pty` can
        // retire this fd too; the hangup only reaches rumor once every master
        // descriptor is closed.
        let stop = Arc::new(AtomicBool::new(false));
        let raw_fd = pair.master.as_raw_fd().expect("master fd");
        let dup = unsafe { libc::dup(raw_fd) };
        assert!(dup >= 0, "dup master fd");
        let mut file = unsafe { File::from_raw_fd(dup) };
        let stop2 = Arc::clone(&stop);
        let drain = thread::spawn(move || {
            let mut buf = [0u8; 8192];
            while !stop2.load(Ordering::SeqCst) {
                let mut pfd = libc::pollfd {
                    fd: dup,
                    events: libc::POLLIN,
                    revents: 0,
                };
                let n = unsafe { libc::poll(&mut pfd, 1, 50) };
                if n < 0 {
                    break;
                }
                if n == 0 {
                    continue;
                }
                match file.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
            }
        });
        let writer = pair.master.take_writer().expect("take writer");
        TuiHost {
            child,
            master: Some(pair.master),
            writer: Some(writer),
            stop,
            drain: Some(drain),
        }
    }

    fn pid(&self) -> i32 {
        self.child.process_id().expect("rumor pid") as i32
    }

    fn keys(&mut self, bytes: &[u8]) {
        let w = self.writer.as_mut().expect("pty already closed");
        w.write_all(bytes).expect("write keys");
        w.flush().expect("flush keys");
    }

    /// Close every descriptor on the master side: the real "terminal tab
    /// closed" event. rumor, as the PTY's session leader, receives SIGHUP.
    fn close_pty(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(h) = self.drain.take() {
            let _ = h.join();
        }
        self.writer = None;
        self.master = None;
    }

    fn exited(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(Some(_)))
    }
}

impl Drop for TuiHost {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(h) = self.drain.take() {
            let _ = h.join();
        }
    }
}

struct RawHost {
    child: Child,
}

impl RawHost {
    fn spawn(config: &Path, ws: &Path, envs: &[(&str, String)]) -> Self {
        let stderr = File::create(ws.join("rumor.stderr")).unwrap();
        let mut cmd = Command::new(BIN);
        cmd.arg("--raw")
            .arg(config)
            .current_dir(ws)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(stderr);
        for (k, v) in envs {
            cmd.env(k, v);
        }
        RawHost {
            child: cmd.spawn().expect("spawn rumor --raw"),
        }
    }

    fn pid(&self) -> i32 {
        self.child.id() as i32
    }

    fn exited(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(Some(_)))
    }
}

enum Host {
    Tui(TuiHost),
    Raw(RawHost),
}

// ---------------------------------------------------------------------------
// The harness
// ---------------------------------------------------------------------------

struct Rumor {
    ws: PathBuf,
    pids: PathBuf,
    shapes: Vec<Shape>,
    host: Host,
    pid: i32,
    exited: bool,
}

fn workspace() -> PathBuf {
    static SEQ: AtomicUsize = AtomicUsize::new(0);
    let p = std::env::temp_dir().join(format!(
        "rumor-e2e-{}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&p).unwrap();
    p.canonicalize().unwrap()
}

impl Rumor {
    fn launch(mode: Mode, shapes: &[Shape]) -> Self {
        let shapes: Vec<Shape> = shapes.iter().copied().filter(|s| s.available()).collect();
        assert!(!shapes.is_empty(), "no available shapes to run");

        let ws = workspace();
        let scripts = ws.join("scripts");
        let pids = ws.join("pids");
        fs::create_dir_all(&scripts).unwrap();
        fs::create_dir_all(&pids).unwrap();

        // Shared inner scripts.
        fs::write(
            scripts.join("inner.sh"),
            "trap '' TERM HUP INT\necho $$ > \"$1\"\nwhile :; do sleep 1; done\n",
        )
        .unwrap();
        fs::write(
            scripts.join("deep2.sh"),
            "trap '' TERM HUP INT\nsh \"$SCRIPTS/inner.sh\" \"$PIDS/deep.1.pid\" &\nwait\n",
        )
        .unwrap();
        for s in &shapes {
            fs::write(scripts.join(format!("{}.sh", s.name())), s.script()).unwrap();
        }

        let config = ws.join("rumor.json");
        let processes: Vec<_> = shapes.iter().map(|s| s.config(&scripts)).collect();
        fs::write(
            &config,
            serde_json::to_string_pretty(&serde_json::json!({ "processes": processes })).unwrap(),
        )
        .unwrap();

        // HOME is redirected so rumor.log lands in the workspace, not in the
        // developer's ~/Library/Logs. Children inherit rumor's env, which is
        // how PIDS/SCRIPTS reach the scripts.
        let envs = vec![
            ("HOME", ws.display().to_string()),
            ("TERM", "xterm-256color".to_string()),
            ("RUMOR_NO_SESSION_LOGS", "1".to_string()),
            ("RUMOR_NO_UPDATE_CHECK", "1".to_string()),
            ("PIDS", pids.display().to_string()),
            ("SCRIPTS", scripts.display().to_string()),
        ];

        let host = match mode {
            Mode::Tui => Host::Tui(TuiHost::spawn(&config, &ws, &envs)),
            Mode::Raw => Host::Raw(RawHost::spawn(&config, &ws, &envs)),
        };
        let pid = match &host {
            Host::Tui(t) => t.pid(),
            Host::Raw(r) => r.pid(),
        };
        let mut r = Rumor {
            ws,
            pids,
            shapes,
            host,
            pid,
            exited: false,
        };
        r.wait_ready();
        r
    }

    fn pgid_file(&self, s: Shape) -> PathBuf {
        self.pids.join(format!("{}.pgid", s.name()))
    }

    /// Every group id any child ever wrote, including every retry attempt.
    fn pgids(&self) -> HashSet<i32> {
        let mut set = HashSet::new();
        for s in &self.shapes {
            if *s == Shape::Retrying {
                set.extend(read_pids(&self.pids.join("retrying.pgids")));
            } else if let Some(p) = read_pid(&self.pgid_file(*s)) {
                set.insert(p);
            }
        }
        set
    }

    fn inner_pids(&self) -> Vec<(String, i32)> {
        let mut v = Vec::new();
        for s in &self.shapes {
            for f in s.inner_pidfiles() {
                if let Some(p) = read_pid(&self.pids.join(f)) {
                    v.push((f.to_string(), p));
                }
            }
        }
        v
    }

    /// Block until every shape has recorded its group and inner pids, every
    /// recorded pid is alive, the exited-leader shape's leader is gone, and the
    /// retrying shape has been through at least two attempts.
    fn wait_ready(&mut self) {
        for s in self.shapes.clone() {
            if s == Shape::Retrying {
                let f = self.pids.join("retrying.pgids");
                assert!(
                    poll_until(READY_BUDGET, || read_pids(&f).len() >= 2),
                    "retrying shape never reached its second attempt"
                );
                continue;
            }
            let f = self.pgid_file(s);
            assert!(
                poll_until(READY_BUDGET, || read_pid(&f).is_some()),
                "{} never wrote its pgid file",
                s.name()
            );
            for inner in s.inner_pidfiles() {
                let f = self.pids.join(inner);
                assert!(
                    poll_until(READY_BUDGET, || read_pid(&f).is_some_and(alive)),
                    "{inner} never appeared alive"
                );
            }
            let leader = read_pid(&f).unwrap();
            if s == Shape::ExitedLeader {
                assert!(
                    poll_until(READY_BUDGET, || !alive(leader)),
                    "exited-leader's leader {leader} should have exited"
                );
            } else {
                assert!(
                    alive(leader),
                    "{} leader {leader} died before the trigger",
                    s.name()
                );
            }
            assert!(!self.exited(), "rumor exited before the trigger was fired");
        }
        // The retrying shape may be mid-backoff; let every other group show up
        // in ps at least once so the oracle is known to see them.
        let pgids: HashSet<i32> = self
            .shapes
            .iter()
            .filter(|s| **s != Shape::Retrying)
            .filter_map(|s| read_pid(&self.pgid_file(*s)))
            .collect();
        assert!(
            poll_until(READY_BUDGET, || {
                let seen: HashSet<i32> =
                    group_members(&pgids).into_iter().map(|r| r.pgid).collect();
                pgids.iter().all(|p| seen.contains(p))
            }),
            "not every child group is visible to ps: want {pgids:?}, have {:?}",
            group_members(&pgids)
        );
    }

    fn exited(&mut self) -> bool {
        if self.exited {
            return true;
        }
        self.exited = match &mut self.host {
            Host::Tui(t) => t.exited(),
            Host::Raw(r) => r.exited(),
        };
        self.exited
    }

    fn signal(&self, sig: i32) {
        unsafe {
            libc::kill(self.pid, sig);
        }
    }

    fn keys(&mut self, bytes: &[u8]) {
        match &mut self.host {
            Host::Tui(t) => t.keys(bytes),
            Host::Raw(_) => panic!("keys need the TUI host"),
        }
    }

    fn close_pty(&mut self) {
        match &mut self.host {
            Host::Tui(t) => t.close_pty(),
            Host::Raw(_) => panic!("close_pty needs the TUI host"),
        }
    }

    /// Wait for rumor to exit, returning how long it took.
    fn wait_exit(&mut self, budget: Duration) -> Option<Duration> {
        let start = Instant::now();
        if poll_until(budget, || self.exited()) {
            Some(start.elapsed())
        } else {
            None
        }
    }

    fn rumor_log(&self) -> String {
        let mac = self.ws.join("Library/Logs/rumor/rumor.log");
        let linux = self.ws.join(".local/share/rumor/rumor.log");
        fs::read_to_string(&mac)
            .or_else(|_| fs::read_to_string(&linux))
            .unwrap_or_else(|_| "<no rumor.log>".into())
    }

    /// The assertion every trigger shares: rumor exited in budget, every child
    /// group is empty, every inner pid is dead, nothing was respawned.
    fn assert_clean_shutdown(&mut self, exit_budget: Duration) -> Duration {
        let elapsed = match self.wait_exit(exit_budget) {
            Some(d) => d,
            None => {
                let pgids = self.pgids();
                panic!(
                    "rumor (pid {}) did not exit within {:?}\nsurviving group members:\n{}\nrumor.log:\n{}",
                    self.pid,
                    exit_budget,
                    fmt_rows(&group_members(&pgids)),
                    self.rumor_log()
                );
            }
        };

        let pgids = self.pgids();
        let inner = self.inner_pids();
        assert!(!pgids.is_empty(), "no child groups were recorded");
        let cleaned_up = poll_until(CLEANUP_BUDGET, || {
            group_members(&pgids).is_empty() && inner.iter().all(|(_, p)| !alive(*p))
        });
        if !cleaned_up {
            let rows = group_members(&pgids);
            let live_inner: Vec<_> = inner.iter().filter(|(_, p)| alive(*p)).collect();
            panic!(
                "orphans survived rumor's exit ({:?} after exit):\n{}\ninner pids still alive: {:?}\nrumor.log:\n{}",
                CLEANUP_BUDGET,
                fmt_rows(&rows),
                live_inner,
                self.rumor_log()
            );
        }

        if self.shapes.contains(&Shape::Retrying) {
            let f = self.pids.join("retrying.pgids");
            let before = read_pids(&f).len();
            thread::sleep(Duration::from_secs(1));
            let after = read_pids(&f).len();
            assert_eq!(
                before, after,
                "retrying shape was respawned after rumor exited"
            );
        }
        elapsed
    }
}

fn fmt_rows(rows: &[PsRow]) -> String {
    if rows.is_empty() {
        return "  (none)".into();
    }
    rows.iter()
        .map(|r| {
            format!(
                "  pid={} pgid={} cpu={} {}",
                r.pid, r.pgid, r.cpu, r.command
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

impl Drop for Rumor {
    fn drop(&mut self) {
        // Never leave orphans behind, whether the test passed or not.
        for pgid in self.pgids() {
            kill_group(pgid, libc::SIGKILL);
        }
        for (_, p) in self.inner_pids() {
            unsafe {
                libc::kill(p, libc::SIGKILL);
            }
        }
        if !self.exited() {
            unsafe {
                libc::kill(self.pid, libc::SIGKILL);
            }
            let _ = self.wait_exit(Duration::from_secs(2));
        }
        let _ = fs::remove_dir_all(&self.ws);
    }
}

// ---------------------------------------------------------------------------
// Triggers
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
enum Trigger {
    Signal(i32),
    Keys(&'static [u8]),
    /// `q` twice: the second press skips the grace and force-quits.
    ForceQuit,
    ClosePty,
}

fn fire(r: &mut Rumor, t: Trigger) {
    match t {
        Trigger::Signal(sig) => r.signal(sig),
        Trigger::Keys(k) => r.keys(k),
        Trigger::ForceQuit => {
            r.keys(b"q");
            thread::sleep(Duration::from_millis(300));
            r.keys(b"q");
        }
        Trigger::ClosePty => r.close_pty(),
    }
}

fn run_matrix(mode: Mode, trigger: Trigger) {
    let mut r = Rumor::launch(mode, &all_shapes());
    fire(&mut r, trigger);
    r.assert_clean_shutdown(EXIT_BUDGET);
}

// ---------------------------------------------------------------------------
// TUI mode
// ---------------------------------------------------------------------------

#[test]
fn tui_sigterm_cleans_up_every_group() {
    run_matrix(Mode::Tui, Trigger::Signal(libc::SIGTERM));
}

#[test]
fn tui_sigint_cleans_up_every_group() {
    run_matrix(Mode::Tui, Trigger::Signal(libc::SIGINT));
}

#[test]
fn tui_sighup_cleans_up_every_group() {
    run_matrix(Mode::Tui, Trigger::Signal(libc::SIGHUP));
}

#[test]
fn tui_pty_closed_cleans_up_every_group() {
    run_matrix(Mode::Tui, Trigger::ClosePty);
}

#[test]
fn tui_q_key_cleans_up_every_group() {
    run_matrix(Mode::Tui, Trigger::Keys(b"q"));
}

#[test]
fn tui_ctrl_c_key_cleans_up_every_group() {
    run_matrix(Mode::Tui, Trigger::Keys(b"\x03"));
}

#[test]
fn tui_force_quit_cleans_up_every_group() {
    run_matrix(Mode::Tui, Trigger::ForceQuit);
}

#[test]
fn tui_polite_child_exits_fast() {
    let mut r = Rumor::launch(Mode::Tui, &[Shape::Polite]);
    fire(&mut r, Trigger::Signal(libc::SIGTERM));
    let took = r.assert_clean_shutdown(EXIT_BUDGET);
    assert!(
        took < FAST_EXIT_BUDGET,
        "a TERM-honouring child should not wait out the grace; rumor took {took:?}"
    );
}

#[test]
fn tui_exits_on_time_when_an_escaped_child_holds_the_pty() {
    if !Shape::Escapee.available() {
        eprintln!("note: `python3` not on PATH; skipping");
        return;
    }
    let mut r = Rumor::launch(Mode::Tui, &[Shape::Escapee]);
    fire(&mut r, Trigger::Keys(b"q"));
    // The escapee left rumor's process group on purpose (a daemonising child);
    // rumor cannot clean it up and is not expected to. It must still exit on
    // time instead of blocking on the PTY the escapee keeps open.
    let took = r.wait_exit(EXIT_BUDGET);
    let escapee = read_pid(&r.pids.join("escapee.1.pid")).expect("escapee pid");
    unsafe {
        libc::kill(escapee, libc::SIGKILL);
    }
    assert!(
        took.is_some(),
        "rumor hung on exit while an escaped child held the PTY open"
    );
}

#[test]
#[ignore = "rumor cannot react to SIGKILL; needs the deferred cleanup helper"]
fn tui_sigkill_cleans_up_every_group() {
    run_matrix(Mode::Tui, Trigger::Signal(libc::SIGKILL));
}

// ---------------------------------------------------------------------------
// Raw mode
// ---------------------------------------------------------------------------

#[test]
fn raw_sigterm_cleans_up_every_group() {
    run_matrix(Mode::Raw, Trigger::Signal(libc::SIGTERM));
}

#[test]
fn raw_sigint_cleans_up_every_group() {
    run_matrix(Mode::Raw, Trigger::Signal(libc::SIGINT));
}

#[test]
fn raw_sighup_cleans_up_every_group() {
    run_matrix(Mode::Raw, Trigger::Signal(libc::SIGHUP));
}

#[test]
fn raw_polite_child_exits_fast() {
    let mut r = Rumor::launch(Mode::Raw, &[Shape::Polite]);
    fire(&mut r, Trigger::Signal(libc::SIGTERM));
    let took = r.assert_clean_shutdown(EXIT_BUDGET);
    assert!(
        took < FAST_EXIT_BUDGET,
        "a TERM-honouring child should not wait out the grace; rumor took {took:?}"
    );
}

#[test]
fn raw_exits_on_time_when_an_escaped_child_holds_the_pty() {
    if !Shape::Escapee.available() {
        eprintln!("note: `python3` not on PATH; skipping");
        return;
    }
    let mut r = Rumor::launch(Mode::Raw, &[Shape::Escapee]);
    fire(&mut r, Trigger::Signal(libc::SIGTERM));
    let took = r.wait_exit(EXIT_BUDGET);
    let escapee = read_pid(&r.pids.join("escapee.1.pid")).expect("escapee pid");
    unsafe {
        libc::kill(escapee, libc::SIGKILL);
    }
    assert!(
        took.is_some(),
        "rumor hung on exit while an escaped child held the PTY open"
    );
}

#[test]
#[ignore = "rumor cannot react to SIGKILL; needs the deferred cleanup helper"]
fn raw_sigkill_cleans_up_every_group() {
    run_matrix(Mode::Raw, Trigger::Signal(libc::SIGKILL));
}
