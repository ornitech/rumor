//! End-to-end session log capture: spawn real processes through the
//! ProcessManager with a session dir and assert the on-disk log files are
//! ANSI-stripped, named safely, and append across restarts.

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
#[path = "../src/process.rs"]
mod process;

use config::ProcessConfig;
use process::ProcessManager;

fn tmpdir() -> PathBuf {
    let p = std::env::temp_dir().join(format!(
        "rumor-logfile-e2e-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&p).unwrap();
    p.canonicalize().unwrap()
}

fn cfg(name: &str, script: &str, cwd: PathBuf) -> ProcessConfig {
    ProcessConfig {
        name: name.into(),
        command: "bash".into(),
        args: vec!["-c".into(), script.into()],
        cwd,
        env_files: vec![],
        global_env_files: vec![],
        env: HashMap::new(),
        depends_on: vec![],
        long_lived: true,
        tags: vec![],
    }
}

/// Poll until `pred` holds for the file's contents (which may lag the PTY).
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn output_is_captured_ansi_stripped() {
    let dir = tmpdir();
    let session = dir.join("session");
    std::fs::create_dir_all(&session).unwrap();

    let c = cfg(
        "colors",
        r"printf '\033[31mred\033[0m line\n'; sleep 5",
        dir.clone(),
    );
    let size = PtySize { rows: 24, cols: 80, pixel_width: 0, pixel_height: 0 };
    let mgr = ProcessManager::new(vec![c], size, Some(session.clone()));

    let log = session.join("colors.log");
    let contents = wait_for_file(&log, |s| s.contains("red line")).await;
    assert!(
        !contents.contains('\x1b'),
        "log should be ANSI-stripped, got: {contents:?}"
    );

    mgr.shutdown(Duration::from_secs(2)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn restart_appends_with_separator() {
    let dir = tmpdir();
    let session = dir.join("session");
    std::fs::create_dir_all(&session).unwrap();

    let c = cfg("oneshot", "echo marker-line; sleep 5", dir.clone());
    let size = PtySize { rows: 24, cols: 80, pixel_width: 0, pixel_height: 0 };
    let mgr = ProcessManager::new(vec![c], size, Some(session.clone()));

    let log = session.join("oneshot.log");
    wait_for_file(&log, |s| s.contains("marker-line")).await;

    mgr.restart(0);
    let contents = wait_for_file(&log, |s| s.matches("marker-line").count() >= 2).await;
    assert_eq!(
        contents.matches("----- restarted at").count(),
        1,
        "expected one restart separator, got:\n{contents}"
    );

    mgr.shutdown(Duration::from_secs(2)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn process_names_are_sanitized_for_filenames() {
    let dir = tmpdir();
    let session = dir.join("session");
    std::fs::create_dir_all(&session).unwrap();

    let c = cfg("web server/dev", "echo named; sleep 5", dir.clone());
    let size = PtySize { rows: 24, cols: 80, pixel_width: 0, pixel_height: 0 };
    let mgr = ProcessManager::new(vec![c], size, Some(session.clone()));

    let log = session.join("web_server_dev.log");
    wait_for_file(&log, |s| s.contains("named")).await;

    mgr.shutdown(Duration::from_secs(2)).await;
}
