//! End-to-end smoke test: spawn a real shell under a PTY through the
//! ProcessManager, verify the vt100 parser sees the output, then shut down.
//!
//! Exercises the full pipeline without driving the TUI:
//!   ProcessManager::new -> Process::spawn -> read task -> vt100::Parser
//!     -> ProcessManager::shutdown (SIGTERM + reap).

use std::collections::HashMap;
use std::path::PathBuf;
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

use config::ProcessConfig;
use process::{Process, ProcessManager, Status};
use std::sync::Arc;

async fn wait_for_process(mgr: &ProcessManager, idx: usize) -> Arc<Process> {
    use process::Slot;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        match mgr.slot(idx) {
            Slot::Process(p) => return p,
            Slot::SpawnFailed(e) => panic!("process {idx} spawn failed: {e}"),
            Slot::Blocked(r) => panic!("process {idx} blocked: {r}"),
            Slot::Waiting => {}
        }
        if tokio::time::Instant::now() > deadline {
            panic!("process {idx} never spawned (slot: {:?})", mgr.slot(idx));
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Poll the process's vt100 screen until `pred` holds on its visible contents.
async fn wait_for_parser(proc: &Arc<Process>, pred: impl Fn(&str) -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let contents = proc.parser.lock().unwrap().screen().contents();
        if pred(&contents) {
            return;
        }
        if tokio::time::Instant::now() > deadline {
            panic!("parser never satisfied predicate; contents:\n{contents}");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Poll a file until `pred` holds on its contents (file writes lag the PTY).
async fn wait_for_file(path: &PathBuf, pred: impl Fn(&str) -> bool) -> String {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let contents = std::fs::read_to_string(path).unwrap_or_default();
        if pred(&contents) {
            return contents;
        }
        if tokio::time::Instant::now() > deadline {
            panic!("file {} never satisfied predicate; contents:\n{contents}", path.display());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn tmpdir() -> PathBuf {
    let p = std::env::temp_dir().join(format!(
        "rumor-pty-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&p).unwrap();
    p.canonicalize().unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn process_runs_under_pty_and_emits_output() {
    let dir = tmpdir();
    std::fs::write(dir.join(".env"), "GREETING=hi-from-dotenv\n").unwrap();

    let cfg = ProcessConfig {
        name: "smoke".into(),
        command: "bash".into(),
        args: vec![
            "-c".into(),
            // Print env var (proves .env loading), then a short loop, then exit.
            "echo \"hello $GREETING\"; for i in 1 2 3; do echo line $i; done"
                .into(),
        ],
        cwd: dir.clone(),
        env_files: vec![],
        global_env_files: vec![],
        env: HashMap::new(),
        dynamic_ports: HashMap::new(),
        depends_on: vec![],
        long_lived: true,
        tags: vec![],
        retry: None,
    };

    let size = PtySize { rows: 24, cols: 80, pixel_width: 0, pixel_height: 0 };
    let mgr = ProcessManager::new(vec![cfg], size, None);

    let proc = wait_for_process(&mgr, 0).await;
    let _ = tokio::time::timeout(Duration::from_secs(5), proc.wait_for_exit()).await;

    let parser = proc.parser.lock().unwrap();
    let screen = parser.screen().contents();
    drop(parser);

    assert!(
        screen.contains("hello hi-from-dotenv"),
        "expected .env value in output, got:\n{screen}"
    );
    assert!(screen.contains("line 1"), "missing line 1 in:\n{screen}");
    assert!(screen.contains("line 3"), "missing line 3 in:\n{screen}");

    // Status should be Exited (success).
    match proc.status() {
        Status::Exited(info) => assert_eq!(info.code, 0, "non-zero exit: {info}"),
        other => panic!("expected Exited, got {other:?}"),
    }

    mgr.shutdown(Duration::from_secs(2)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn clear_display_wipes_view_keeps_file_and_shows_new_output() {
    let dir = tmpdir();
    let session = dir.join("session");
    std::fs::create_dir_all(&session).unwrap();

    // A long-lived process that echoes each stdin line, so we can drive output
    // before and after the clear.
    let cfg = ProcessConfig {
        name: "echoer".into(),
        command: "bash".into(),
        args: vec![
            "-c".into(),
            "while IFS= read -r line; do echo \"got $line\"; done".into(),
        ],
        cwd: dir.clone(),
        env_files: vec![],
        global_env_files: vec![],
        env: HashMap::new(),
        dynamic_ports: HashMap::new(),
        depends_on: vec![],
        long_lived: true,
        tags: vec![],
        retry: None,
    };

    let size = PtySize { rows: 24, cols: 80, pixel_width: 0, pixel_height: 0 };
    let mgr = ProcessManager::new(vec![cfg], size, Some(session.clone()));
    let proc = wait_for_process(&mgr, 0).await;

    // Emit a line and wait for both the display and the on-disk log to show it.
    proc.write_input(b"before-clear\n");
    wait_for_parser(&proc, |s| s.contains("got before-clear")).await;
    let log = session.join("echoer.log");
    wait_for_file(&log, |s| s.contains("got before-clear")).await;

    // Clear the in-memory view.
    proc.clear_display();

    // The pre-clear output is gone from the display.
    {
        let contents = proc.parser.lock().unwrap().screen().contents();
        assert!(
            !contents.contains("before-clear"),
            "cleared display should not contain pre-clear output, got:\n{contents}"
        );
    }

    // ...but the on-disk session log still has it (display-only clear).
    let file_after = std::fs::read_to_string(&log).unwrap();
    assert!(
        file_after.contains("got before-clear"),
        "log file must retain pre-clear output after a display clear, got:\n{file_after}"
    );

    // A resize replays raw history into a fresh parser; the cleared output must
    // not come back (clear drops the raw buffer, not just the screen).
    proc.resize(PtySize { rows: 24, cols: 120, pixel_width: 0, pixel_height: 0 });
    {
        let contents = proc.parser.lock().unwrap().screen().contents();
        assert!(
            !contents.contains("before-clear"),
            "pre-clear output must not reappear after resize, got:\n{contents}"
        );
    }

    // New output after the clear is displayed.
    proc.write_input(b"after-clear\n");
    wait_for_parser(&proc, |s| s.contains("got after-clear")).await;
    {
        let contents = proc.parser.lock().unwrap().screen().contents();
        assert!(
            !contents.contains("before-clear"),
            "pre-clear output must stay gone once new output arrives, got:\n{contents}"
        );
    }

    mgr.shutdown(Duration::from_secs(2)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resize_replays_history_at_new_width() {
    // Spawn a process that emits a line wider than 40 cols. At cols=40 the
    // child writes a wrapped representation. After resize to cols=120, the
    // replay should re-render the line on a single grid row.
    let dir = tmpdir();
    let cfg = ProcessConfig {
        name: "wide".into(),
        command: "bash".into(),
        args: vec![
            "-c".into(),
            // 80-char "AAAA...A" then newline.
            "printf '%.0sA' $(seq 1 80); printf '\\n'".into(),
        ],
        cwd: dir,
        env_files: vec![],
        global_env_files: vec![],
        env: HashMap::new(),
        dynamic_ports: HashMap::new(),
        depends_on: vec![],
        long_lived: true,
        tags: vec![],
        retry: None,
    };

    let narrow = PtySize { rows: 10, cols: 40, pixel_width: 0, pixel_height: 0 };
    let mgr = ProcessManager::new(vec![cfg], narrow, None);
    let proc = wait_for_process(&mgr, 0).await;
    let _ = tokio::time::timeout(Duration::from_secs(5), proc.wait_for_exit()).await;

    // Sanity: at 40 cols, the A's are present (likely across 2 rows due to wrap).
    {
        let p = proc.parser.lock().unwrap();
        let contents = p.screen().contents();
        let a_count = contents.chars().filter(|&c| c == 'A').count();
        assert!(a_count >= 80, "expected >=80 A's at 40 cols, got {a_count}");
    }

    // Resize wider; replay should re-render the buffered bytes at cols=120.
    let wide = PtySize { rows: 10, cols: 120, pixel_width: 0, pixel_height: 0 };
    proc.resize(wide);

    // After replay at 120 cols, the 80 A's fit on a single row with no wrap.
    {
        let p = proc.parser.lock().unwrap();
        // Find the row that contains the A run and assert it has 80 contiguous A's.
        let mut found_full_row = false;
        for row_idx in 0..p.screen().size().0 {
            let mut line = String::new();
            for col in 0..p.screen().size().1 {
                if let Some(cell) = p.screen().cell(row_idx, col) {
                    line.push_str(cell.contents());
                }
            }
            if line.trim_end().chars().filter(|&c| c == 'A').count() == 80
                && !line.trim_end().chars().any(|c| c != 'A')
            {
                found_full_row = true;
                break;
            }
        }
        assert!(found_full_row, "expected one full row of 80 A's at 120 cols");
    }

    mgr.shutdown(Duration::from_secs(2)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dep_with_exit_condition_gates_dependent_spawn() {
    use config::{Dependency, ReadinessCondition};
    use process::Slot;

    let dir = tmpdir();
    // migrate: sleep 300ms then exit 0
    let migrate = ProcessConfig {
        name: "migrate".into(),
        command: "bash".into(),
        args: vec!["-c".into(), "sleep 0.3; exit 0".into()],
        cwd: dir.clone(),
        env_files: vec![],
        global_env_files: vec![],
        env: HashMap::new(),
        dynamic_ports: HashMap::new(),
        depends_on: vec![],
        long_lived: false,
        tags: vec![],
        retry: None,
    };
    // api: depends on migrate exit 0; just echo and exit
    let api = ProcessConfig {
        name: "api".into(),
        command: "bash".into(),
        args: vec!["-c".into(), "echo api-up; sleep 5".into()],
        cwd: dir,
        env_files: vec![],
        global_env_files: vec![],
        env: HashMap::new(),
        dynamic_ports: HashMap::new(),
        depends_on: vec![Dependency {
            name: "migrate".into(),
            until: ReadinessCondition::Exit(0),
        }],
        long_lived: true,
        tags: vec![],
        retry: None,
    };

    let size = PtySize { rows: 24, cols: 80, pixel_width: 0, pixel_height: 0 };
    let mgr = ProcessManager::new(vec![migrate, api], size, None);

    // While migrate is still sleeping, api should be Waiting (not yet spawned).
    tokio::time::sleep(Duration::from_millis(100)).await;
    match mgr.slot(1) {
        Slot::Waiting => {}
        other => panic!("expected api to be Waiting before migrate exits, got {other:?}"),
    }

    // Wait for migrate to finish (it sleeps 300ms then exits 0).
    let migrate_proc = wait_for_process(&mgr, 0).await;
    let _ = tokio::time::timeout(Duration::from_secs(2), migrate_proc.wait_for_exit()).await;

    // api should now spawn within a beat.
    let api_proc = wait_for_process(&mgr, 1).await;
    // Verify api actually wrote its line.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        let snap = String::from_utf8_lossy(&api_proc.raw_snapshot()).to_string();
        if snap.contains("api-up") {
            break;
        }
        if tokio::time::Instant::now() > deadline {
            panic!("api never produced output: {snap}");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    mgr.shutdown(Duration::from_secs(2)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dep_with_log_condition_unblocks_dependent() {
    use config::{Dependency, ReadinessCondition};
    use process::Slot;

    let dir = tmpdir();
    let server = ProcessConfig {
        name: "server".into(),
        command: "bash".into(),
        args: vec![
            "-c".into(),
            // Wait 200ms then print the readiness marker, then idle.
            "sleep 0.2; echo 'Listening on port 8080'; sleep 10".into(),
        ],
        cwd: dir.clone(),
        env_files: vec![],
        global_env_files: vec![],
        env: HashMap::new(),
        dynamic_ports: HashMap::new(),
        depends_on: vec![],
        long_lived: true,
        tags: vec![],
        retry: None,
    };
    let client = ProcessConfig {
        name: "client".into(),
        command: "bash".into(),
        args: vec!["-c".into(), "echo client-up; sleep 5".into()],
        cwd: dir,
        env_files: vec![],
        global_env_files: vec![],
        env: HashMap::new(),
        dynamic_ports: HashMap::new(),
        depends_on: vec![Dependency {
            name: "server".into(),
            until: ReadinessCondition::Log(r"Listening on port \d+".into()),
        }],
        long_lived: true,
        tags: vec![],
        retry: None,
    };

    let size = PtySize { rows: 24, cols: 80, pixel_width: 0, pixel_height: 0 };
    let mgr = ProcessManager::new(vec![server, client], size, None);

    // Before server emits the line, client must be Waiting.
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(matches!(mgr.slot(1), Slot::Waiting), "client should be waiting");

    // After server emits, client spawns.
    let client_proc = wait_for_process(&mgr, 1).await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        let snap = String::from_utf8_lossy(&client_proc.raw_snapshot()).to_string();
        if snap.contains("client-up") {
            break;
        }
        if tokio::time::Instant::now() > deadline {
            panic!("client never produced output: {snap}");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    mgr.shutdown(Duration::from_secs(2)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn terminate_reaps_grandchild_not_just_wrapper() {
    // Regression: a non-forwarding wrapper (sh -c, pnpm, npm) whose real
    // workload is a grandchild that ignores TERM/HUP (like nx/node's graceful
    // handlers) must be reaped as a group, not left orphaned at PPID 1.
    //
    // rumor spawns the outer `sh` (portable_pty setsid => it leads its own
    // process group, pgid == pid). Both the outer sh and the inner `sh` it
    // backgrounds trap TERM+HUP, mirroring the production case where the wrapper
    // outlives SIGTERM until the grace period expires (the `SIGTERM grace
    // expired; sending SIGKILL` path). The inner sh is a group member whose own
    // pid != pgid and which survives everything but SIGKILL. terminate escalates
    // to SIGKILL after grace: hitting only the leader pid orphans the inner sh;
    // hitting the group (negated pid) reaps it.
    let dir = tmpdir();
    let pidfile = dir.join("grandchild.pid");

    let cfg = ProcessConfig {
        name: "wrap".into(),
        command: "sh".into(),
        args: vec![
            "-c".into(),
            format!(
                "trap '' TERM HUP; \
                 sh -c 'trap \"\" TERM HUP; echo $$ > {}; while true; do sleep 1; done' & wait",
                pidfile.display()
            ),
        ],
        cwd: dir,
        env_files: vec![],
        global_env_files: vec![],
        env: HashMap::new(),
        dynamic_ports: HashMap::new(),
        depends_on: vec![],
        long_lived: true,
        tags: vec![],
        retry: None,
    };

    let size = PtySize { rows: 24, cols: 80, pixel_width: 0, pixel_height: 0 };
    let mgr = ProcessManager::new(vec![cfg], size, None);
    let proc = wait_for_process(&mgr, 0).await;

    // Wait for the grandchild to record its pid.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let grandchild: i32 = loop {
        if let Ok(s) = std::fs::read_to_string(&pidfile) {
            if let Ok(pid) = s.trim().parse::<i32>() {
                break pid;
            }
        }
        if tokio::time::Instant::now() > deadline {
            panic!("grandchild pid never recorded");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };

    let alive = |pid: i32| unsafe { libc::kill(pid, 0) == 0 };
    assert!(alive(grandchild), "grandchild should be alive before terminate");

    // Terminate the slot: SIGTERM the group, then SIGKILL the group after grace.
    proc.terminate(Duration::from_millis(500));
    let _ = tokio::time::timeout(Duration::from_secs(3), proc.wait_for_exit()).await;

    // The group SIGKILL must have reaped the trapping grandchild.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    while alive(grandchild) && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    if alive(grandchild) {
        // Don't leak a busy-idling orphan if the assertion is about to fail.
        unsafe { libc::kill(grandchild, libc::SIGKILL) };
        panic!("grandchild {grandchild} was orphaned; terminate did not reap the process group");
    }

    mgr.shutdown(Duration::from_secs(2)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn manager_kills_long_running_process_on_shutdown() {
    let dir = tmpdir();
    let cfg = ProcessConfig {
        name: "forever".into(),
        command: "bash".into(),
        args: vec!["-c".into(), "while true; do echo tick; sleep 1; done".into()],
        cwd: dir,
        env_files: vec![],
        global_env_files: vec![],
        env: HashMap::new(),
        dynamic_ports: HashMap::new(),
        depends_on: vec![],
        long_lived: true,
        tags: vec![],
        retry: None,
    };

    let size = PtySize { rows: 24, cols: 80, pixel_width: 0, pixel_height: 0 };
    let mgr = ProcessManager::new(vec![cfg], size, None);

    let proc = wait_for_process(&mgr, 0).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(proc.is_running());

    mgr.shutdown(Duration::from_secs(3)).await;
    assert!(!proc.is_running());
}
