//! End-to-end tests for the configurable retry / auto-restart logic in
//! `ProcessManager` / `watch_slot`. Each test drives real PTY children and
//! polls slot state with timeouts (respawns are async).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use portable_pty::PtySize;

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

use config::{BackoffStrategy, ProcessConfig, RetryConfig};
use process::{ProcessManager, Slot, Status};

const SIZE: PtySize = PtySize { rows: 24, cols: 80, pixel_width: 0, pixel_height: 0 };

fn tmpdir() -> PathBuf {
    static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let p = std::env::temp_dir().join(format!(
        "rumor-retry-{}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&p).unwrap();
    p.canonicalize().unwrap()
}

fn cfg(
    dir: &Path,
    name: &str,
    script: &str,
    long_lived: bool,
    retry: Option<RetryConfig>,
) -> ProcessConfig {
    ProcessConfig {
        name: name.into(),
        command: "bash".into(),
        args: vec!["-c".into(), script.into()],
        cwd: dir.to_path_buf(),
        env_files: vec![],
        global_env_files: vec![],
        env: HashMap::new(),
        dynamic_ports: HashMap::new(),
        depends_on: vec![],
        long_lived,
        tags: vec![],
        retry,
    }
}

fn fixed_retry(max_retries: u32, delay_ms: u64) -> RetryConfig {
    RetryConfig {
        max_retries,
        strategy: BackoffStrategy::Fixed,
        delay: Duration::from_millis(delay_ms),
        max_delay: None,
    }
}

/// Poll the slot, recording each distinct PID, until retries are exhausted or
/// the deadline passes. Returns the list of distinct PIDs in first-seen order.
async fn collect_pids_until_exhausted(
    mgr: &ProcessManager,
    idx: usize,
    timeout: Duration,
) -> Vec<u32> {
    let mut seen: Vec<u32> = Vec::new();
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if let Some(p) = mgr.process(idx) {
            let pid = p.pid();
            if !seen.contains(&pid) {
                seen.push(pid);
            }
        }
        if mgr.retries_exhausted(idx) {
            break;
        }
        if tokio::time::Instant::now() > deadline {
            break;
        }
        tokio::time::sleep(Duration::from_millis(3)).await;
    }
    seen
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retry_respawns_long_lived_until_exhausted() {
    let dir = tmpdir();
    // Long-lived process that exits immediately: every exit is a failure.
    let c = cfg(&dir, "flaky", "exit 0", true, Some(fixed_retry(2, 30)));
    let mgr = ProcessManager::new(vec![c], SIZE, None);

    let pids = collect_pids_until_exhausted(&mgr, 0, Duration::from_secs(5)).await;
    // 1 initial spawn + 2 retries = 3 distinct PIDs.
    assert_eq!(pids.len(), 3, "expected 3 spawns, saw PIDs: {pids:?}");
    assert!(mgr.retries_exhausted(0), "should be flagged exhausted");

    // Terminal state: slot still holds the last (exited) process.
    match mgr.slot(0) {
        Slot::Process(p) => match p.status() {
            Status::Exited(_) => {}
            other => panic!("expected Exited, got {other:?}"),
        },
        other => panic!("expected Slot::Process, got {other:?}"),
    }
    let diags = mgr.diagnostics(0).join("\n");
    assert!(diags.contains("retries exhausted"), "diags:\n{diags}");

    mgr.shutdown(Duration::from_secs(2)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_retry_block_spawns_once() {
    let dir = tmpdir();
    let c = cfg(&dir, "once", "exit 1", true, None);
    let mgr = ProcessManager::new(vec![c], SIZE, None);

    // Wait for the first spawn and capture its PID.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let first_pid = loop {
        if let Some(p) = mgr.process(0) {
            break p.pid();
        }
        assert!(tokio::time::Instant::now() < deadline, "never spawned");
        tokio::time::sleep(Duration::from_millis(5)).await;
    };

    // Give it time to (not) respawn; PID must remain stable.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!mgr.retries_exhausted(0));
    assert_eq!(mgr.process(0).unwrap().pid(), first_pid, "should not respawn");

    mgr.shutdown(Duration::from_secs(2)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn oneshot_exit_zero_not_retried() {
    let dir = tmpdir();
    // One-shot: exit 0 is success, not a failure.
    let c = cfg(&dir, "setup", "exit 0", false, Some(fixed_retry(3, 20)));
    let mgr = ProcessManager::new(vec![c], SIZE, None);

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let first_pid = loop {
        if let Some(p) = mgr.process(0) {
            break p.pid();
        }
        assert!(tokio::time::Instant::now() < deadline, "never spawned");
        tokio::time::sleep(Duration::from_millis(5)).await;
    };

    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!mgr.retries_exhausted(0));
    assert_eq!(mgr.process(0).unwrap().pid(), first_pid, "exit 0 should not retry");
    let diags = mgr.diagnostics(0).join("\n");
    assert!(diags.contains("exited successfully"), "diags:\n{diags}");

    mgr.shutdown(Duration::from_secs(2)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn oneshot_nonzero_retried() {
    let dir = tmpdir();
    // One-shot exiting non-zero is a failure and retries.
    let c = cfg(&dir, "setup", "exit 1", false, Some(fixed_retry(2, 30)));
    let mgr = ProcessManager::new(vec![c], SIZE, None);

    let pids = collect_pids_until_exhausted(&mgr, 0, Duration::from_secs(5)).await;
    assert_eq!(pids.len(), 3, "expected 3 spawns, saw PIDs: {pids:?}");
    assert!(mgr.retries_exhausted(0));

    mgr.shutdown(Duration::from_secs(2)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn kill_suppresses_retry() {
    let dir = tmpdir();
    // Long-lived service that would normally retry forever-ish on exit.
    let c = cfg(&dir, "svc", "sleep 30", true, Some(fixed_retry(5, 20)));
    let mgr = ProcessManager::new(vec![c], SIZE, None);

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let killed_pid = loop {
        if let Some(p) = mgr.process(0) {
            if p.is_running() {
                break p.pid();
            }
        }
        assert!(tokio::time::Instant::now() < deadline, "never started running");
        tokio::time::sleep(Duration::from_millis(5)).await;
    };

    mgr.kill(0);

    // After the kill-induced exit, no respawn should occur.
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(!mgr.retries_exhausted(0));
    match mgr.slot(0) {
        Slot::Process(p) => {
            assert_eq!(p.pid(), killed_pid, "should not have respawned after kill");
            assert!(!p.is_running(), "killed process should be stopped");
        }
        other => panic!("expected Slot::Process, got {other:?}"),
    }
    let diags = mgr.diagnostics(0).join("\n");
    assert!(diags.contains("terminated by request"), "diags:\n{diags}");

    mgr.shutdown(Duration::from_secs(2)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn restart_resets_retry_budget() {
    let dir = tmpdir();
    let c = cfg(&dir, "flaky", "exit 0", true, Some(fixed_retry(1, 30)));
    let mgr = ProcessManager::new(vec![c], SIZE, None);

    // First run: 1 initial + 1 retry = 2 spawns, then exhausted.
    let pids1 = collect_pids_until_exhausted(&mgr, 0, Duration::from_secs(5)).await;
    assert_eq!(pids1.len(), 2, "first run PIDs: {pids1:?}");
    assert!(mgr.retries_exhausted(0));

    // Manual restart resets the budget and the exhausted flag.
    mgr.restart(0);
    let pids2 = collect_pids_until_exhausted(&mgr, 0, Duration::from_secs(5)).await;
    assert_eq!(pids2.len(), 2, "second run PIDs: {pids2:?}");
    assert!(mgr.retries_exhausted(0));
    // The second run produced fresh PIDs not seen in the first.
    assert!(
        pids2.iter().any(|p| !pids1.contains(p)),
        "restart should spawn new processes: {pids1:?} vs {pids2:?}"
    );

    mgr.shutdown(Duration::from_secs(2)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_during_backoff_no_respawn() {
    let dir = tmpdir();
    // Immediate exit, then a long backoff so we can shut down mid-sleep.
    let c = cfg(&dir, "flaky", "exit 0", true, Some(fixed_retry(5, 2000)));
    let mgr = ProcessManager::new(vec![c], SIZE, None);

    // Wait until the first instance has exited and the watcher is in backoff
    // (a "retry .. in" diagnostic has been pushed).
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let first_pid = loop {
        let diags = mgr.diagnostics(0).join("\n");
        if diags.contains("retry 1/5 in") {
            break mgr.process(0).map(|p| p.pid());
        }
        assert!(tokio::time::Instant::now() < deadline, "never entered backoff");
        tokio::time::sleep(Duration::from_millis(5)).await;
    };

    mgr.begin_shutdown();

    // Poll until shutdown completes; no new process should ever appear.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    while !mgr.all_exited() {
        assert!(tokio::time::Instant::now() < deadline, "shutdown never completed");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    // Extra wait spanning more than the backoff would have been; still no respawn.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(mgr.all_exited(), "a respawn happened after shutdown");
    if let (Some(p), Some(fp)) = (mgr.process(0), first_pid) {
        assert_eq!(p.pid(), fp, "process should not have changed after shutdown");
    }
}
