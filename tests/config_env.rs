use std::collections::HashMap;
use std::path::PathBuf;

#[path = "../src/config.rs"]
mod config;
#[path = "../src/env.rs"]
mod env;
#[path = "../src/template.rs"]
mod template;
#[path = "../src/ports.rs"]
mod ports;

use config::{BackoffStrategy, Config, ReadinessCondition};

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn loads_example_config() {
    let path = workspace_root().join("example.config.json");
    let loaded = Config::load(&path).expect("example.config.json should parse");
    let names: Vec<&str> = loaded.config.processes.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["counter", "ticker", "repl", "migration"]);

    // cwd should be canonicalized (absolute).
    for proc in &loaded.config.processes {
        assert!(proc.cwd.is_absolute(), "{} cwd not absolute", proc.name);
        assert!(proc.cwd.is_dir(), "{} cwd not dir", proc.name);
    }

    // migration is the only short-lived process; rest default to long-lived.
    let by_name: HashMap<&str, bool> = loaded
        .config
        .processes
        .iter()
        .map(|p| (p.name.as_str(), p.long_lived))
        .collect();
    assert_eq!(by_name["counter"], true);
    assert_eq!(by_name["ticker"], true);
    assert_eq!(by_name["repl"], true);
    assert_eq!(by_name["migration"], false);
}

#[test]
fn fullstack_build_env_succeeds_for_every_service() {
    let path = workspace_root().join("examples/fullstack/fullstack.config.json");
    let loaded = Config::load(&path).expect("fullstack config parses");
    for proc in &loaded.config.processes {
        let result = env::build_env(
            &proc.cwd,
            &proc.global_env_files,
            &proc.env_files,
            &proc.env,
            &proc.dynamic_ports,
        );
        if let Err(e) = &result {
            panic!("build_env failed for {}: {e:#}", proc.name);
        }
    }
}

#[test]
fn loads_fullstack_example() {
    let path = workspace_root().join("examples/fullstack/fullstack.config.json");
    let loaded = Config::load(&path).expect("fullstack.config.json should parse");
    let names: Vec<&str> = loaded.config.processes.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["db", "redis", "api", "frontend"]);

    // api depends on db and redis; frontend depends on api.
    let api = loaded.config.processes.iter().find(|p| p.name == "api").unwrap();
    let api_deps: Vec<&str> = api.depends_on.iter().map(|d| d.name.as_str()).collect();
    assert_eq!(api_deps, ["db", "redis"]);
    let frontend = loaded.config.processes.iter().find(|p| p.name == "frontend").unwrap();
    let fe_deps: Vec<&str> = frontend.depends_on.iter().map(|d| d.name.as_str()).collect();
    assert_eq!(fe_deps, ["api"]);

    // The shared env now lives in the root envFiles; each service keeps its own
    // .env.local. All paths must resolve to existing files (Config::load
    // canonicalizes & checks is_file).
    for proc in &loaded.config.processes {
        assert_eq!(proc.global_env_files.len(), 1, "{} global_env_files", proc.name);
        assert_eq!(proc.env_files.len(), 1, "{} env_files", proc.name);
        for ef in proc.global_env_files.iter().chain(proc.env_files.iter()) {
            assert!(ef.is_file(), "{} env file missing: {}", proc.name, ef.display());
        }
    }

    // The frontend service overrides LOG_LEVEL via the JSON env block — that's
    // the "one service also overrides a config-level env var" demonstration.
    assert_eq!(frontend.env.get("LOG_LEVEL").map(|s| s.as_str()), Some("warn"));

    // Every service demonstrates a retry policy; the example exercises all three
    // strategies plus the `fixed` default (redis omits `strategy`).
    let db = loaded.config.processes.iter().find(|p| p.name == "db").unwrap();
    let redis = loaded.config.processes.iter().find(|p| p.name == "redis").unwrap();
    let db_retry = db.retry.as_ref().expect("db has retry");
    assert_eq!(db_retry.strategy, BackoffStrategy::Exponential);
    assert_eq!(db_retry.max_retries, 5);
    assert_eq!(db_retry.max_delay, Some(std::time::Duration::from_millis(30000)));
    let redis_retry = redis.retry.as_ref().expect("redis has retry");
    assert_eq!(redis_retry.strategy, BackoffStrategy::Fixed); // default
    assert_eq!(redis_retry.max_delay, None);
    let api = loaded.config.processes.iter().find(|p| p.name == "api").unwrap();
    assert_eq!(api.retry.as_ref().unwrap().strategy, BackoffStrategy::Linear);
    assert_eq!(frontend.retry.as_ref().unwrap().strategy, BackoffStrategy::Exponential);
}

#[test]
fn rejects_duplicate_names() {
    let dir = tempdir();
    let cfg = dir.join("dup.json");
    let body = format!(
        r#"{{
            "processes": [
                {{ "name": "a", "command": "echo", "cwd": "{0}" }},
                {{ "name": "a", "command": "echo", "cwd": "{0}" }}
            ]
        }}"#,
        dir.display()
    );
    std::fs::write(&cfg, body).unwrap();
    let err = Config::load(&cfg).unwrap_err().to_string();
    assert!(err.contains("duplicate"), "got: {err}");
}

#[test]
fn rejects_missing_cwd() {
    let dir = tempdir();
    let cfg = dir.join("missing.json");
    let missing = dir.join("nope");
    let body = format!(
        r#"{{
            "processes": [
                {{ "name": "a", "command": "echo", "cwd": "{}" }}
            ]
        }}"#,
        missing.display()
    );
    std::fs::write(&cfg, body).unwrap();
    assert!(Config::load(&cfg).is_err());
}

#[test]
fn dotenv_is_loaded_from_cwd() {
    let dir = tempdir();
    std::fs::write(dir.join(".env"), "FROM_DOTENV=42\nBOTH=dotenv\n").unwrap();
    let mut overrides = HashMap::new();
    overrides.insert("BOTH".to_string(), "explicit".to_string());

    let merged = env::build_env(&dir, &[], &[], &overrides, &HashMap::new()).unwrap();
    assert_eq!(merged.get("FROM_DOTENV").map(String::as_str), Some("42"));
    // JSON env block beats .env.
    assert_eq!(merged.get("BOTH").map(String::as_str), Some("explicit"));
    // PATH from orchestrator env should propagate.
    if std::env::var("PATH").is_ok() {
        assert!(merged.contains_key("PATH"));
    }
}

#[test]
fn no_dotenv_is_fine() {
    let dir = tempdir();
    let merged = env::build_env(&dir, &[], &[], &HashMap::new(), &HashMap::new()).unwrap();
    assert!(!merged.contains_key("FROM_DOTENV"));
}

#[test]
fn env_files_load_in_order_and_override_dotenv() {
    let dir = tempdir();
    std::fs::write(dir.join(".env"), "L=dotenv\nA=dotenv\n").unwrap();
    let f1 = dir.join("first.env");
    let f2 = dir.join("second.env");
    std::fs::write(&f1, "A=first\nB=first\n").unwrap();
    std::fs::write(&f2, "B=second\nC=second\n").unwrap();

    let merged = env::build_env(&dir, &[], &[f1, f2], &HashMap::new(), &HashMap::new()).unwrap();
    // .env-only key survives.
    assert_eq!(merged.get("L").map(String::as_str), Some("dotenv"));
    // first.env overrides .env.
    assert_eq!(merged.get("A").map(String::as_str), Some("first"));
    // second.env overrides first.env.
    assert_eq!(merged.get("B").map(String::as_str), Some("second"));
    // second.env-only key.
    assert_eq!(merged.get("C").map(String::as_str), Some("second"));
}

#[test]
fn json_env_beats_env_files() {
    let dir = tempdir();
    let f = dir.join("x.env");
    std::fs::write(&f, "K=from-file\n").unwrap();
    let mut explicit = HashMap::new();
    explicit.insert("K".to_string(), "from-json".to_string());

    let merged = env::build_env(&dir, &[], &[f], &explicit, &HashMap::new()).unwrap();
    assert_eq!(merged.get("K").map(String::as_str), Some("from-json"));
}

#[test]
fn config_resolves_relative_env_files_against_config_dir() {
    let dir = tempdir();
    let cwd = dir.join("proc");
    std::fs::create_dir_all(&cwd).unwrap();
    std::fs::write(dir.join("shared.env"), "SHARED=yes\n").unwrap();

    let cfg_path = dir.join("conf.json");
    let body = format!(
        r#"{{
            "processes": [
                {{
                    "name": "a",
                    "command": "echo",
                    "cwd": "{}",
                    "envFiles": ["shared.env"]
                }}
            ]
        }}"#,
        cwd.display()
    );
    std::fs::write(&cfg_path, body).unwrap();
    let loaded = Config::load(&cfg_path).expect("config should load");
    let ef = &loaded.config.processes[0].env_files;
    assert_eq!(ef.len(), 1);
    assert!(ef[0].is_absolute(), "envFiles entry should be canonicalized");
    assert!(ef[0].ends_with("shared.env"));
}

#[test]
fn config_rejects_unknown_dep_name() {
    let dir = tempdir();
    let cfg = dir.join("c.json");
    let body = format!(
        r#"{{"processes": [
            {{ "name": "a", "command": "echo", "cwd": "{0}",
               "dependsOn": [{{"name": "ghost", "until": {{"exit": 0}}}}] }}
        ]}}"#,
        dir.display()
    );
    std::fs::write(&cfg, body).unwrap();
    let err = Config::load(&cfg).unwrap_err().to_string();
    assert!(err.contains("unknown process 'ghost'"), "got: {err}");
}

#[test]
fn config_rejects_self_dependency() {
    let dir = tempdir();
    let cfg = dir.join("c.json");
    let body = format!(
        r#"{{"processes": [
            {{ "name": "a", "command": "echo", "cwd": "{0}",
               "dependsOn": [{{"name": "a", "until": {{"exit": 0}}}}] }}
        ]}}"#,
        dir.display()
    );
    std::fs::write(&cfg, body).unwrap();
    let err = Config::load(&cfg).unwrap_err().to_string();
    assert!(err.contains("cannot depend on itself"), "got: {err}");
}

#[test]
fn config_rejects_cycles() {
    let dir = tempdir();
    let cfg = dir.join("c.json");
    let body = format!(
        r#"{{"processes": [
            {{ "name": "a", "command": "echo", "cwd": "{0}",
               "dependsOn": [{{"name": "b", "until": {{"exit": 0}}}}] }},
            {{ "name": "b", "command": "echo", "cwd": "{0}",
               "dependsOn": [{{"name": "c", "until": {{"exit": 0}}}}] }},
            {{ "name": "c", "command": "echo", "cwd": "{0}",
               "dependsOn": [{{"name": "a", "until": {{"exit": 0}}}}] }}
        ]}}"#,
        dir.display()
    );
    std::fs::write(&cfg, body).unwrap();
    let err = Config::load(&cfg).unwrap_err().to_string();
    assert!(err.contains("cycle"), "got: {err}");
}

#[test]
fn config_rejects_bad_log_regex() {
    let dir = tempdir();
    let cfg = dir.join("c.json");
    let body = format!(
        r#"{{"processes": [
            {{ "name": "a", "command": "echo", "cwd": "{0}" }},
            {{ "name": "b", "command": "echo", "cwd": "{0}",
               "dependsOn": [{{"name": "a", "until": {{"log": "[invalid"}}}}] }}
        ]}}"#,
        dir.display()
    );
    std::fs::write(&cfg, body).unwrap();
    let err = Config::load(&cfg).unwrap_err().to_string();
    assert!(err.contains("valid regex") || err.contains("not a valid regex"), "got: {err}");
}

#[test]
fn config_accepts_valid_dep_graph() {
    let dir = tempdir();
    let cfg = dir.join("c.json");
    let body = format!(
        r#"{{"processes": [
            {{ "name": "db", "command": "echo", "cwd": "{0}" }},
            {{ "name": "migrate", "command": "echo", "cwd": "{0}",
               "dependsOn": [{{"name": "db", "until": {{"port": 5432}}}}] }},
            {{ "name": "api", "command": "echo", "cwd": "{0}",
               "dependsOn": [
                 {{"name": "db", "until": {{"port": 5432}}}},
                 {{"name": "migrate", "until": {{"exit": 0}}}}
               ] }}
        ]}}"#,
        dir.display()
    );
    std::fs::write(&cfg, body).unwrap();
    Config::load(&cfg).expect("valid graph should load");
}

#[test]
fn config_rejects_missing_env_file() {
    let dir = tempdir();
    let cfg_path = dir.join("conf.json");
    let body = format!(
        r#"{{
            "processes": [
                {{
                    "name": "a",
                    "command": "echo",
                    "cwd": "{}",
                    "envFiles": ["nope.env"]
                }}
            ]
        }}"#,
        dir.display()
    );
    std::fs::write(&cfg_path, body).unwrap();
    let err = Config::load(&cfg_path).unwrap_err().to_string();
    assert!(err.contains("envFiles"), "got: {err}");
}

// ---------------------------------------------------------------------------
// root-level (global) envFiles
// ---------------------------------------------------------------------------

#[test]
fn global_env_files_are_lowest_config_precedence() {
    let dir = tempdir();
    // Each later source overrides the global file's value for K.
    let g = dir.join("global.env");
    std::fs::write(&g, "K=global\nGLOBAL_ONLY=yes\n").unwrap();
    std::fs::write(dir.join(".env"), "K=dotenv\n").unwrap();
    let f = dir.join("proc.env");
    std::fs::write(&f, "K=procfile\n").unwrap();
    let globals = vec![g];

    // global < dotenv: dotenv wins.
    let m = env::build_env(&dir, &globals, &[], &HashMap::new(), &HashMap::new()).unwrap();
    assert_eq!(m.get("K").map(String::as_str), Some("dotenv"));
    assert_eq!(m.get("GLOBAL_ONLY").map(String::as_str), Some("yes"));

    // global < per-process envFiles.
    let no_dotenv = tempdir();
    let m = env::build_env(&no_dotenv, &globals, &[f], &HashMap::new(), &HashMap::new()).unwrap();
    assert_eq!(m.get("K").map(String::as_str), Some("procfile"));

    // global < json env block.
    let mut json = HashMap::new();
    json.insert("K".to_string(), "json".to_string());
    let m = env::build_env(&no_dotenv, &globals, &[], &json, &HashMap::new()).unwrap();
    assert_eq!(m.get("K").map(String::as_str), Some("json"));
}

#[test]
fn global_env_files_load_in_order() {
    let dir = tempdir();
    let g1 = dir.join("g1.env");
    let g2 = dir.join("g2.env");
    std::fs::write(&g1, "A=one\nB=one\n").unwrap();
    std::fs::write(&g2, "B=two\n").unwrap();
    let m = env::build_env(&dir, &[g1, g2], &[], &HashMap::new(), &HashMap::new()).unwrap();
    assert_eq!(m.get("A").map(String::as_str), Some("one"));
    assert_eq!(m.get("B").map(String::as_str), Some("two"));
}

#[test]
fn root_env_files_apply_to_every_process() {
    let dir = tempdir();
    let cwd_a = dir.join("a");
    let cwd_b = dir.join("b");
    std::fs::create_dir_all(&cwd_a).unwrap();
    std::fs::create_dir_all(&cwd_b).unwrap();
    std::fs::write(dir.join(".env.shared"), "SHARED=global\n").unwrap();

    let cfg_path = dir.join("conf.json");
    let body = format!(
        r#"{{
            "envFiles": [".env.shared"],
            "processes": [
                {{ "name": "a", "command": "echo", "cwd": "{}" }},
                {{ "name": "b", "command": "echo", "cwd": "{}" }}
            ]
        }}"#,
        cwd_a.display(),
        cwd_b.display()
    );
    std::fs::write(&cfg_path, body).unwrap();
    let loaded = Config::load(&cfg_path).expect("config should load");

    for proc in &loaded.config.processes {
        assert_eq!(proc.global_env_files.len(), 1, "{} global_env_files", proc.name);
        let merged = env::build_env(
            &proc.cwd,
            &proc.global_env_files,
            &proc.env_files,
            &proc.env,
            &proc.dynamic_ports,
        )
        .unwrap();
        assert_eq!(
            merged.get("SHARED").map(String::as_str),
            Some("global"),
            "{} should see the global env",
            proc.name
        );
    }
}

#[test]
fn config_resolves_relative_root_env_files_against_config_dir() {
    let dir = tempdir();
    let cwd = dir.join("proc");
    std::fs::create_dir_all(&cwd).unwrap();
    std::fs::write(dir.join("root.env"), "ROOT=yes\n").unwrap();

    let cfg_path = dir.join("conf.json");
    let body = format!(
        r#"{{
            "envFiles": ["root.env"],
            "processes": [
                {{ "name": "a", "command": "echo", "cwd": "{}" }}
            ]
        }}"#,
        cwd.display()
    );
    std::fs::write(&cfg_path, body).unwrap();
    let loaded = Config::load(&cfg_path).expect("config should load");
    let g = &loaded.config.processes[0].global_env_files;
    assert_eq!(g.len(), 1);
    assert!(g[0].is_absolute(), "root envFiles entry should be canonicalized");
    assert!(g[0].ends_with("root.env"));
}

#[test]
fn config_rejects_missing_root_env_file() {
    let dir = tempdir();
    let cfg_path = dir.join("conf.json");
    let body = format!(
        r#"{{
            "envFiles": ["nope.env"],
            "processes": [
                {{ "name": "a", "command": "echo", "cwd": "{}" }}
            ]
        }}"#,
        dir.display()
    );
    std::fs::write(&cfg_path, body).unwrap();
    let err = Config::load(&cfg_path).unwrap_err().to_string();
    assert!(err.contains("root"), "got: {err}");
    assert!(err.contains("envFiles"), "got: {err}");
}

#[test]
fn root_env_file_value_resolves_template() {
    let dir = tempdir();
    let cwd = dir.join("proc");
    std::fs::create_dir_all(&cwd).unwrap();
    std::fs::write(dir.join(".env"), "API_PORT=8080\n").unwrap();

    let cfg_path = dir.join("conf.json");
    let body = format!(
        r#"{{
            "envFiles": [".env"],
            "processes": [
                {{ "name": "a", "command": "echo", "cwd": "{}", "args": ["${{API_PORT}}"] }}
            ]
        }}"#,
        cwd.display()
    );
    std::fs::write(&cfg_path, body).unwrap();
    let loaded = Config::load(&cfg_path).expect("config should load");
    assert_eq!(loaded.config.processes[0].args, vec!["8080".to_string()]);
}

// ---------------------------------------------------------------------------
// longLived parsing
// ---------------------------------------------------------------------------

#[test]
fn long_lived_defaults_to_true() {
    let dir = tempdir();
    let cfg = dir.join("c.json");
    let body = format!(
        r#"{{"processes": [
            {{ "name": "a", "command": "echo", "cwd": "{0}" }}
        ]}}"#,
        dir.display()
    );
    std::fs::write(&cfg, body).unwrap();
    let loaded = Config::load(&cfg).expect("should load");
    assert_eq!(loaded.config.processes[0].long_lived, true);
}

#[test]
fn long_lived_false_parses() {
    let dir = tempdir();
    let cfg = dir.join("c.json");
    let body = format!(
        r#"{{"processes": [
            {{ "name": "a", "command": "echo", "cwd": "{0}", "longLived": false }}
        ]}}"#,
        dir.display()
    );
    std::fs::write(&cfg, body).unwrap();
    let loaded = Config::load(&cfg).expect("should load");
    assert_eq!(loaded.config.processes[0].long_lived, false);
}

#[test]
fn long_lived_true_parses() {
    let dir = tempdir();
    let cfg = dir.join("c.json");
    let body = format!(
        r#"{{"processes": [
            {{ "name": "a", "command": "echo", "cwd": "{0}", "longLived": true }}
        ]}}"#,
        dir.display()
    );
    std::fs::write(&cfg, body).unwrap();
    let loaded = Config::load(&cfg).expect("should load");
    assert_eq!(loaded.config.processes[0].long_lived, true);
}

// ---------------------------------------------------------------------------
// ${VAR} substitution in config fields
// ---------------------------------------------------------------------------

fn port_of(loaded: &config::LoadedConfig, proc_idx: usize, dep_idx: usize) -> u16 {
    match &loaded.config.processes[proc_idx].depends_on[dep_idx].until {
        ReadinessCondition::Port(p) => *p,
        other => panic!("expected Port, got {other:?}"),
    }
}

#[test]
fn port_template_resolves_from_env_block() {
    let dir = tempdir();
    let cfg = dir.join("c.json");
    let body = format!(
        r#"{{"processes": [
            {{ "name": "db", "command": "echo", "cwd": "{0}" }},
            {{ "name": "api", "command": "echo", "cwd": "{0}",
               "env": {{ "API_PORT": "5432" }},
               "dependsOn": [{{"name": "db", "until": {{"port": "${{API_PORT}}"}}}}] }}
        ]}}"#,
        dir.display()
    );
    std::fs::write(&cfg, body).unwrap();
    let loaded = Config::load(&cfg).expect("should load");
    assert_eq!(port_of(&loaded, 1, 0), 5432);
}

#[test]
fn port_template_resolves_from_env_file() {
    let dir = tempdir();
    std::fs::write(dir.join("ports.env"), "DB_PORT=6543\n").unwrap();
    let cfg = dir.join("c.json");
    let body = format!(
        r#"{{"processes": [
            {{ "name": "db", "command": "echo", "cwd": "{0}" }},
            {{ "name": "api", "command": "echo", "cwd": "{0}",
               "envFiles": ["ports.env"],
               "dependsOn": [{{"name": "db", "until": {{"port": "${{DB_PORT}}"}}}}] }}
        ]}}"#,
        dir.display()
    );
    std::fs::write(&cfg, body).unwrap();
    let loaded = Config::load(&cfg).expect("should load");
    assert_eq!(port_of(&loaded, 1, 0), 6543);
}

#[test]
fn port_template_resolves_from_orchestrator_env() {
    // Unique var name so this test does not race with anything else.
    let var = "RUMOR_TEST_ORCH_PORT_8FB2";
    // SAFETY: process-wide env mutation. The var name is unique to this test.
    unsafe { std::env::set_var(var, "7777"); }
    let dir = tempdir();
    let cfg = dir.join("c.json");
    let body = format!(
        r#"{{"processes": [
            {{ "name": "db", "command": "echo", "cwd": "{0}" }},
            {{ "name": "api", "command": "echo", "cwd": "{0}",
               "dependsOn": [{{"name": "db", "until": {{"port": "${{{var}}}"}}}}] }}
        ]}}"#,
        dir.display()
    );
    std::fs::write(&cfg, body).unwrap();
    let loaded = Config::load(&cfg).expect("should load");
    assert_eq!(port_of(&loaded, 1, 0), 7777);
    // SAFETY: cleanup, same justification.
    unsafe { std::env::remove_var(var); }
}

#[test]
fn port_template_unknown_var_substitutes_empty_then_fails_parse() {
    let dir = tempdir();
    let cfg = dir.join("c.json");
    let body = format!(
        r#"{{"processes": [
            {{ "name": "db", "command": "echo", "cwd": "{0}" }},
            {{ "name": "api", "command": "echo", "cwd": "{0}",
               "dependsOn": [{{"name": "db", "until": {{"port": "${{NOPE_THIS_IS_UNSET_1234}}"}}}}] }}
        ]}}"#,
        dir.display()
    );
    std::fs::write(&cfg, body).unwrap();
    let err = Config::load(&cfg).unwrap_err().to_string();
    assert!(err.contains("not a valid u16"), "got: {err}");
    assert!(err.contains("until.port"), "got: {err}");
}

#[test]
fn port_template_non_numeric_value_errors() {
    let dir = tempdir();
    let cfg = dir.join("c.json");
    let body = format!(
        r#"{{"processes": [
            {{ "name": "db", "command": "echo", "cwd": "{0}" }},
            {{ "name": "api", "command": "echo", "cwd": "{0}",
               "env": {{ "P": "hello" }},
               "dependsOn": [{{"name": "db", "until": {{"port": "${{P}}"}}}}] }}
        ]}}"#,
        dir.display()
    );
    std::fs::write(&cfg, body).unwrap();
    let err = Config::load(&cfg).unwrap_err().to_string();
    assert!(err.contains("not a valid u16"), "got: {err}");
}

#[test]
fn port_literal_number_still_works() {
    let dir = tempdir();
    let cfg = dir.join("c.json");
    let body = format!(
        r#"{{"processes": [
            {{ "name": "db", "command": "echo", "cwd": "{0}" }},
            {{ "name": "api", "command": "echo", "cwd": "{0}",
               "dependsOn": [{{"name": "db", "until": {{"port": 5432}}}}] }}
        ]}}"#,
        dir.display()
    );
    std::fs::write(&cfg, body).unwrap();
    let loaded = Config::load(&cfg).expect("should load");
    assert_eq!(port_of(&loaded, 1, 0), 5432);
}

#[test]
fn log_template_resolves_then_regex_compiles() {
    let dir = tempdir();
    let cfg = dir.join("c.json");
    let body = format!(
        r#"{{"processes": [
            {{ "name": "db", "command": "echo", "cwd": "{0}" }},
            {{ "name": "api", "command": "echo", "cwd": "{0}",
               "env": {{ "MARKER": "ready" }},
               "dependsOn": [{{"name": "db", "until": {{"log": "^${{MARKER}}$"}}}}] }}
        ]}}"#,
        dir.display()
    );
    std::fs::write(&cfg, body).unwrap();
    let loaded = Config::load(&cfg).expect("should load");
    match &loaded.config.processes[1].depends_on[0].until {
        ReadinessCondition::Log(rx) => assert_eq!(rx, "^ready$"),
        other => panic!("expected Log, got {other:?}"),
    }
}

#[test]
fn escaped_dollar_is_literal_in_args() {
    let dir = tempdir();
    let cfg = dir.join("c.json");
    let body = format!(
        r#"{{"processes": [
            {{ "name": "a", "command": "echo", "cwd": "{0}",
               "args": ["$${{RATE}}"] }}
        ]}}"#,
        dir.display()
    );
    std::fs::write(&cfg, body).unwrap();
    let loaded = Config::load(&cfg).expect("should load");
    assert_eq!(loaded.config.processes[0].args, vec!["${RATE}".to_string()]);
}

#[test]
fn command_and_args_substituted_from_env_block() {
    let dir = tempdir();
    let cfg = dir.join("c.json");
    let body = format!(
        r#"{{"processes": [
            {{ "name": "a", "command": "${{RUNNER}}", "cwd": "{0}",
               "args": ["--flag=${{FLAG}}"],
               "env": {{ "RUNNER": "bash", "FLAG": "on" }} }}
        ]}}"#,
        dir.display()
    );
    std::fs::write(&cfg, body).unwrap();
    let loaded = Config::load(&cfg).expect("should load");
    assert_eq!(loaded.config.processes[0].command, "bash");
    assert_eq!(
        loaded.config.processes[0].args,
        vec!["--flag=on".to_string()]
    );
}

#[test]
fn exit_template_resolves() {
    let dir = tempdir();
    let cfg = dir.join("c.json");
    let body = format!(
        r#"{{"processes": [
            {{ "name": "a", "command": "echo", "cwd": "{0}" }},
            {{ "name": "b", "command": "echo", "cwd": "{0}",
               "env": {{ "EXP": "0" }},
               "dependsOn": [{{"name": "a", "until": {{"exit": "${{EXP}}"}}}}] }}
        ]}}"#,
        dir.display()
    );
    std::fs::write(&cfg, body).unwrap();
    let loaded = Config::load(&cfg).expect("should load");
    match &loaded.config.processes[1].depends_on[0].until {
        ReadinessCondition::Exit(code) => assert_eq!(*code, 0),
        other => panic!("expected Exit, got {other:?}"),
    }
}

#[test]
fn unterminated_template_errors() {
    let dir = tempdir();
    let cfg = dir.join("c.json");
    let body = format!(
        r#"{{"processes": [
            {{ "name": "a", "command": "echo", "cwd": "{0}",
               "args": ["${{BAD"] }}
        ]}}"#,
        dir.display()
    );
    std::fs::write(&cfg, body).unwrap();
    let err = Config::load(&cfg).unwrap_err().to_string();
    assert!(err.contains("unterminated"), "got: {err}");
}

// ---------------------------------------------------------------------------
// dynamicPorts
// ---------------------------------------------------------------------------

#[test]
fn dynamic_port_flows_into_args_deps_and_env_with_highest_precedence() {
    let dir = tempdir();
    // Competing values that the dynamic allocation must beat.
    std::fs::write(dir.join(".env"), "API_PORT=9999\n").unwrap();
    let cfg = dir.join("c.json");
    let body = format!(
        r#"{{
            "dynamicPorts": ["API_PORT"],
            "processes": [
                {{ "name": "db", "command": "echo", "cwd": "{0}" }},
                {{ "name": "api", "command": "echo", "cwd": "{0}",
                   "args": ["${{API_PORT}}"],
                   "env": {{ "API_PORT": "1111" }},
                   "dependsOn": [{{"name": "db", "until": {{"port": "${{API_PORT}}"}}}}] }}
            ]
        }}"#,
        dir.display()
    );
    std::fs::write(&cfg, body).unwrap();
    let loaded = Config::load(&cfg).expect("should load");

    // The allocated port was persisted next to the config.
    let store: HashMap<String, u16> = serde_json::from_slice(
        &std::fs::read(dir.join(".rumor-ports.json")).expect("store file written"),
    )
    .expect("store is valid json");
    let port = store["API_PORT"];
    let port_str = port.to_string();

    // Dynamic beats both the .env file and the JSON env block.
    assert_ne!(port_str, "1111");
    assert_ne!(port_str, "9999");

    // ${API_PORT} substitution in args and until.port saw the allocated value.
    let api = &loaded.config.processes[1];
    assert_eq!(api.args, vec![port_str.clone()]);
    assert_eq!(port_of(&loaded, 1, 0), port);
    assert_eq!(loaded.dynamic_ports.get("API_PORT"), Some(&port_str));

    // Spawn-time env (same build_env call as Process::spawn) sees it too.
    let merged = env::build_env(
        &api.cwd,
        &api.global_env_files,
        &api.env_files,
        &api.env,
        &api.dynamic_ports,
    )
    .unwrap();
    assert_eq!(merged.get("API_PORT"), Some(&port_str));
}

#[test]
fn dynamic_ports_are_stable_across_loads() {
    let dir = tempdir();
    let cfg = dir.join("c.json");
    let body = format!(
        r#"{{
            "dynamicPorts": ["A_PORT", "B_PORT"],
            "processes": [
                {{ "name": "a", "command": "echo", "cwd": "{0}", "args": ["${{A_PORT}}", "${{B_PORT}}"] }}
            ]
        }}"#,
        dir.display()
    );
    std::fs::write(&cfg, body).unwrap();
    let first = Config::load(&cfg).expect("should load");
    let second = Config::load(&cfg).expect("should load again");
    assert_eq!(first.dynamic_ports, second.dynamic_ports);
    assert_eq!(
        first.config.processes[0].args,
        second.config.processes[0].args
    );
}

#[test]
fn config_rejects_bad_dynamic_port_name() {
    let dir = tempdir();
    let cfg = dir.join("c.json");
    let body = format!(
        r#"{{
            "dynamicPorts": ["1BAD"],
            "processes": [
                {{ "name": "a", "command": "echo", "cwd": "{0}" }}
            ]
        }}"#,
        dir.display()
    );
    std::fs::write(&cfg, body).unwrap();
    let err = format!("{:#}", Config::load(&cfg).unwrap_err());
    assert!(err.contains("dynamicPorts"), "got: {err}");
    assert!(err.contains("1BAD"), "got: {err}");
}

#[test]
fn config_rejects_duplicate_dynamic_port() {
    let dir = tempdir();
    let cfg = dir.join("c.json");
    let body = format!(
        r#"{{
            "dynamicPorts": ["API_PORT", "API_PORT"],
            "processes": [
                {{ "name": "a", "command": "echo", "cwd": "{0}" }}
            ]
        }}"#,
        dir.display()
    );
    std::fs::write(&cfg, body).unwrap();
    let err = format!("{:#}", Config::load(&cfg).unwrap_err());
    assert!(err.contains("duplicate entry: API_PORT"), "got: {err}");
}

#[test]
fn no_dynamic_ports_leaves_no_store_file() {
    let dir = tempdir();
    let cfg = dir.join("c.json");
    let body = format!(
        r#"{{"processes": [
            {{ "name": "a", "command": "echo", "cwd": "{0}" }}
        ]}}"#,
        dir.display()
    );
    std::fs::write(&cfg, body).unwrap();
    Config::load(&cfg).expect("should load");
    assert!(!dir.join(".rumor-ports.json").exists());
}

// --- retry config ----------------------------------------------------------

fn load_one_process(retry_json: &str) -> Result<config::Config, String> {
    let dir = tempdir();
    let cfg = dir.join("retry.json");
    let retry_field = if retry_json.is_empty() {
        String::new()
    } else {
        format!(", \"retry\": {retry_json}")
    };
    let body = format!(
        r#"{{"processes": [
            {{ "name": "a", "command": "echo", "cwd": "{0}"{1} }}
        ]}}"#,
        dir.display(),
        retry_field
    );
    std::fs::write(&cfg, body).unwrap();
    Config::load(&cfg)
        .map(|l| l.config)
        .map_err(|e| format!("{e:#}"))
}

#[test]
fn retry_absent_is_none() {
    let config = load_one_process("").expect("should load");
    assert!(config.processes[0].retry.is_none());
}

#[test]
fn retry_minimal_defaults_to_fixed() {
    let config = load_one_process(r#"{ "maxRetries": 3, "delayMs": 500 }"#).expect("should load");
    let retry = config.processes[0].retry.as_ref().expect("retry set");
    assert_eq!(retry.max_retries, 3);
    assert_eq!(retry.strategy, BackoffStrategy::Fixed);
    assert_eq!(retry.delay, std::time::Duration::from_millis(500));
    assert_eq!(retry.max_delay, None);
}

#[test]
fn retry_all_fields_parse() {
    let config = load_one_process(
        r#"{ "maxRetries": 5, "strategy": "exponential", "delayMs": 500, "maxDelayMs": 30000 }"#,
    )
    .expect("should load");
    let retry = config.processes[0].retry.as_ref().expect("retry set");
    assert_eq!(retry.max_retries, 5);
    assert_eq!(retry.strategy, BackoffStrategy::Exponential);
    assert_eq!(retry.delay, std::time::Duration::from_millis(500));
    assert_eq!(retry.max_delay, Some(std::time::Duration::from_millis(30000)));

    let config = load_one_process(r#"{ "maxRetries": 2, "strategy": "linear", "delayMs": 100 }"#)
        .expect("should load");
    assert_eq!(
        config.processes[0].retry.as_ref().unwrap().strategy,
        BackoffStrategy::Linear
    );
}

#[test]
fn retry_zero_max_retries_rejected() {
    let err = load_one_process(r#"{ "maxRetries": 0, "delayMs": 100 }"#).unwrap_err();
    assert!(err.contains("maxRetries must be >= 1"), "got: {err}");
}

#[test]
fn retry_zero_delay_rejected() {
    let err = load_one_process(r#"{ "maxRetries": 3, "delayMs": 0 }"#).unwrap_err();
    assert!(err.contains("delayMs must be >= 1"), "got: {err}");
}

#[test]
fn retry_max_delay_below_delay_rejected() {
    let err =
        load_one_process(r#"{ "maxRetries": 3, "delayMs": 1000, "maxDelayMs": 500 }"#).unwrap_err();
    assert!(err.contains("must be >= delayMs"), "got: {err}");
}

#[test]
fn retry_unknown_field_rejected() {
    let err = load_one_process(r#"{ "maxRetries": 3, "delayMs": 100, "maxRetry": 5 }"#).unwrap_err();
    assert!(err.contains("unknown field") || err.contains("maxRetry"), "got: {err}");
}

#[test]
fn retry_bad_strategy_rejected() {
    let err =
        load_one_process(r#"{ "maxRetries": 3, "strategy": "fibonacci", "delayMs": 100 }"#).unwrap_err();
    assert!(err.contains("fibonacci") || err.contains("unknown variant"), "got: {err}");
}

// ---------------------------------------------------------------------------

fn tempdir() -> PathBuf {
    // Counter disambiguates parallel tests that land on the same timestamp
    // (the system clock is coarser than nanos), which made dirs collide.
    static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let base = std::env::temp_dir().join(format!(
        "rumor-test-{}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&base).unwrap();
    base.canonicalize().unwrap()
}
