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
#[path = "../src/template.rs"]
mod template;
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
        depends_on: vec![],
        long_lived: true,
        tags: vec![],
    };

    let size = PtySize { rows: 24, cols: 80, pixel_width: 0, pixel_height: 0 };
    let mgr = ProcessManager::new(vec![cfg], size);

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
        depends_on: vec![],
        long_lived: true,
        tags: vec![],
    };

    let narrow = PtySize { rows: 10, cols: 40, pixel_width: 0, pixel_height: 0 };
    let mgr = ProcessManager::new(vec![cfg], narrow);
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
        depends_on: vec![],
        long_lived: false,
        tags: vec![],
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
        depends_on: vec![Dependency {
            name: "migrate".into(),
            until: ReadinessCondition::Exit(0),
        }],
        long_lived: true,
        tags: vec![],
    };

    let size = PtySize { rows: 24, cols: 80, pixel_width: 0, pixel_height: 0 };
    let mgr = ProcessManager::new(vec![migrate, api], size);

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
        depends_on: vec![],
        long_lived: true,
        tags: vec![],
    };
    let client = ProcessConfig {
        name: "client".into(),
        command: "bash".into(),
        args: vec!["-c".into(), "echo client-up; sleep 5".into()],
        cwd: dir,
        env_files: vec![],
        global_env_files: vec![],
        env: HashMap::new(),
        depends_on: vec![Dependency {
            name: "server".into(),
            until: ReadinessCondition::Log(r"Listening on port \d+".into()),
        }],
        long_lived: true,
        tags: vec![],
    };

    let size = PtySize { rows: 24, cols: 80, pixel_width: 0, pixel_height: 0 };
    let mgr = ProcessManager::new(vec![server, client], size);

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
        depends_on: vec![],
        long_lived: true,
        tags: vec![],
    };

    let size = PtySize { rows: 24, cols: 80, pixel_width: 0, pixel_height: 0 };
    let mgr = ProcessManager::new(vec![cfg], size);

    let proc = wait_for_process(&mgr, 0).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(proc.is_running());

    mgr.shutdown(Duration::from_secs(3)).await;
    assert!(!proc.is_running());
}
