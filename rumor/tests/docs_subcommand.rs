//! Integration test for the `docs` subcommand. Spawns the built binary (no
//! config required) and checks the bundled docs are emitted with the right exit
//! codes.

use std::process::Command;

fn rumor() -> Command {
    Command::new(env!("CARGO_BIN_EXE_rumor"))
}

#[test]
fn docs_agent_prints_guide_to_stdout() {
    for flag in ["--agent", "-a"] {
        let out = rumor()
            .args(["docs", flag])
            .output()
            .expect("spawn rumor");
        assert!(out.status.success(), "`docs {flag}` should exit 0");
        let stdout = String::from_utf8(out.stdout).unwrap();
        assert!(
            stdout.contains("# rumor — AI agent guide"),
            "`docs {flag}` should print the agent guide, got:\n{stdout}"
        );
        // A few schema anchors so the embedded doc can't silently go empty/stale.
        assert!(stdout.contains("dynamicPorts"));
        assert!(stdout.contains("dependsOn"));
    }
}

#[test]
fn bare_docs_prints_index_to_stdout() {
    let out = rumor().arg("docs").output().expect("spawn rumor");
    assert!(out.status.success(), "bare `docs` should exit 0");
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(stdout.contains("documentation topics"));
    assert!(stdout.contains("rumor docs --agent"));
}

#[test]
fn help_flag_points_agents_to_docs() {
    for flag in ["--help", "-h"] {
        let out = rumor().arg(flag).output().expect("spawn rumor");
        assert!(out.status.success(), "`{flag}` should exit 0");
        let stdout = String::from_utf8(out.stdout).unwrap();
        assert!(stdout.contains("usage: rumor"), "`{flag}` should print usage");
        assert!(
            stdout.contains("rumor docs --agent"),
            "`{flag}` should point agents to `rumor docs --agent`, got:\n{stdout}"
        );
    }
}

#[test]
fn bad_args_print_usage_to_stderr_exit_2() {
    let out = rumor().arg("--bogus").output().expect("spawn rumor");
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(stderr.contains("usage: rumor"));
    assert!(stderr.contains("rumor docs --agent"));
}

#[test]
fn unknown_docs_flag_exits_2() {
    let out = rumor()
        .args(["docs", "--bogus"])
        .output()
        .expect("spawn rumor");
    assert_eq!(out.status.code(), Some(2), "unknown docs flag should exit 2");
    // Index goes to stderr in the error case.
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(stderr.contains("documentation topics"));
}
