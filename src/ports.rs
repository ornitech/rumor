use std::collections::{BTreeMap, HashMap, HashSet};
use std::net::TcpListener;
use std::path::Path;

use anyhow::{anyhow, Context, Result};

use crate::template;

/// Store file holding the allocated ports, written next to the config file.
/// Per-worktree isolation falls out of each git worktree being its own
/// directory tree. Users should gitignore it.
pub const STORE_FILE: &str = ".rumor-ports.json";

/// Resolve the top-level `dynamicPorts` var names to concrete ports, stable
/// per config directory: allocations are persisted in
/// `<config_dir>/.rumor-ports.json` and reused verbatim on later runs (no
/// liveness check). Vars missing from the store are bound to a free
/// OS-assigned port before any process starts. Values are returned as decimal
/// strings, ready for env injection and `${VAR}` substitution. An empty
/// `names` list does no I/O.
pub fn resolve_dynamic_ports(
    names: &[String],
    config_dir: &Path,
) -> Result<HashMap<String, String>> {
    validate_names(names)?;
    if names.is_empty() {
        return Ok(HashMap::new());
    }

    let store = config_dir.join(STORE_FILE);
    let mut stored = read_store(&store);

    let missing: Vec<&String> = names
        .iter()
        .filter(|n| !stored.contains_key(n.as_str()))
        .collect();
    if !missing.is_empty() {
        // Hold every listener until the store is persisted so the OS can't
        // hand the same port out twice within this batch.
        let mut held = Vec::with_capacity(missing.len());
        for name in &missing {
            let listener = TcpListener::bind(("127.0.0.1", 0))
                .with_context(|| format!("dynamicPorts: allocating a free port for {name}"))?;
            let port = listener.local_addr()?.port();
            stored.insert((*name).clone(), port);
            held.push(listener);
        }
        write_store_atomic(&store, &stored)
            .with_context(|| format!("dynamicPorts: writing {}", store.display()))?;
        drop(held);
    }

    Ok(names
        .iter()
        .map(|n| (n.clone(), stored[n.as_str()].to_string()))
        .collect())
}

fn validate_names(names: &[String]) -> Result<()> {
    let mut seen = HashSet::new();
    for (i, name) in names.iter().enumerate() {
        if !template::is_strict_ident(name) {
            return Err(anyhow!(
                "dynamicPorts[{i}]: {name:?} is not a valid env var name (expected [A-Za-z_][A-Za-z0-9_]*)"
            ));
        }
        if !seen.insert(name.as_str()) {
            return Err(anyhow!("dynamicPorts: duplicate entry: {name}"));
        }
    }
    Ok(())
}

/// Read the store leniently: a missing, unreadable, or corrupt file yields an
/// empty map (the next write replaces it), and individual entries whose value
/// is not a valid port are dropped and reallocated. Unknown keys are kept so
/// multiple configs sharing a directory don't clobber each other's
/// allocations.
fn read_store(path: &Path) -> BTreeMap<String, u16> {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(_) => return BTreeMap::new(),
    };
    let map: serde_json::Map<String, serde_json::Value> = match serde_json::from_slice(&bytes) {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(
                "dynamicPorts: {} is corrupt ({e}); reallocating",
                path.display()
            );
            return BTreeMap::new();
        }
    };
    let mut out = BTreeMap::new();
    for (k, v) in map {
        match v.as_u64() {
            Some(p) if (1..=65535).contains(&p) => {
                out.insert(k, p as u16);
            }
            _ => {
                tracing::warn!(
                    "dynamicPorts: dropping invalid entry {k:?} = {v} in {}",
                    path.display()
                );
            }
        }
    }
    out
}

/// Write via a uniquely-suffixed sibling temp file + rename, so a concurrent
/// rumor instance never observes a torn file. The suffix combines the pid with
/// a process-local counter so two concurrent writers in the same process (e.g.
/// loading two configs at once) never collide on the same temp path.
fn write_store_atomic(path: &Path, map: &BTreeMap<String, u16>) -> Result<()> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let mut bytes = serde_json::to_vec_pretty(map)?;
    bytes.push(b'\n');
    let tmp = path.with_extension(format!(
        "json.tmp.{}.{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::write(&tmp, &bytes).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, path)
        .with_context(|| format!("renaming {} to {}", tmp.display(), path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn tempdir() -> PathBuf {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let p = std::env::temp_dir().join(format!(
            "rumor-ports-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn allocates_distinct_ports_and_persists() {
        let dir = tempdir();
        let resolved = resolve_dynamic_ports(&names(&["A_PORT", "B_PORT"]), &dir).unwrap();
        let a: u16 = resolved["A_PORT"].parse().unwrap();
        let b: u16 = resolved["B_PORT"].parse().unwrap();
        assert_ne!(a, 0);
        assert_ne!(b, 0);
        assert_ne!(a, b);

        let store = read_store(&dir.join(STORE_FILE));
        assert_eq!(store.get("A_PORT"), Some(&a));
        assert_eq!(store.get("B_PORT"), Some(&b));
    }

    #[test]
    fn second_resolve_reuses_stored_ports() {
        let dir = tempdir();
        let vars = names(&["A_PORT", "B_PORT"]);
        let first = resolve_dynamic_ports(&vars, &dir).unwrap();
        let second = resolve_dynamic_ports(&vars, &dir).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn stored_port_is_reused_verbatim_without_liveness_check() {
        let dir = tempdir();
        // Port 5 is privileged and unbindable, proving no liveness check runs.
        let mut seeded = BTreeMap::new();
        seeded.insert("A_PORT".to_string(), 5u16);
        write_store_atomic(&dir.join(STORE_FILE), &seeded).unwrap();

        let resolved = resolve_dynamic_ports(&names(&["A_PORT"]), &dir).unwrap();
        assert_eq!(resolved["A_PORT"], "5");
    }

    #[test]
    fn unrelated_store_keys_survive_new_allocations() {
        let dir = tempdir();
        let mut seeded = BTreeMap::new();
        seeded.insert("OTHER_CONFIGS_PORT".to_string(), 4242u16);
        write_store_atomic(&dir.join(STORE_FILE), &seeded).unwrap();

        resolve_dynamic_ports(&names(&["A_PORT"]), &dir).unwrap();

        let store = read_store(&dir.join(STORE_FILE));
        assert_eq!(store.get("OTHER_CONFIGS_PORT"), Some(&4242));
        assert!(store.contains_key("A_PORT"));
    }

    #[test]
    fn empty_list_does_no_io() {
        let dir = tempdir();
        let resolved = resolve_dynamic_ports(&[], &dir).unwrap();
        assert!(resolved.is_empty());
        assert!(!dir.join(STORE_FILE).exists());
    }

    #[test]
    fn corrupt_store_is_discarded_and_reallocated() {
        let dir = tempdir();
        std::fs::write(dir.join(STORE_FILE), "not json").unwrap();
        let resolved = resolve_dynamic_ports(&names(&["A_PORT"]), &dir).unwrap();
        let port: u16 = resolved["A_PORT"].parse().unwrap();
        assert_ne!(port, 0);
        // The store was rewritten with valid JSON.
        assert_eq!(read_store(&dir.join(STORE_FILE)).get("A_PORT"), Some(&port));
    }

    #[test]
    fn invalid_entries_are_dropped_but_valid_siblings_kept() {
        let dir = tempdir();
        std::fs::write(
            dir.join(STORE_FILE),
            r#"{"GOOD": 4242, "TOO_BIG": 99999, "ZERO": 0, "STRINGY": "8080"}"#,
        )
        .unwrap();
        let store = read_store(&dir.join(STORE_FILE));
        assert_eq!(store.get("GOOD"), Some(&4242));
        assert!(!store.contains_key("TOO_BIG"));
        assert!(!store.contains_key("ZERO"));
        assert!(!store.contains_key("STRINGY"));
    }

    #[test]
    fn rejects_invalid_var_name() {
        let dir = tempdir();
        let err = resolve_dynamic_ports(&names(&["1BAD"]), &dir)
            .unwrap_err()
            .to_string();
        assert!(err.contains("dynamicPorts[0]"), "got: {err}");
        assert!(err.contains("1BAD"), "got: {err}");
    }

    #[test]
    fn rejects_duplicate_var_name() {
        let dir = tempdir();
        let err = resolve_dynamic_ports(&names(&["A_PORT", "A_PORT"]), &dir)
            .unwrap_err()
            .to_string();
        assert!(err.contains("duplicate entry: A_PORT"), "got: {err}");
    }

    /// Regression: many threads writing the same store concurrently must never
    /// fail. With a temp name keyed only on the pid, the writers collided on a
    /// single temp path and one's rename hit ENOENT after another consumed it.
    #[test]
    fn concurrent_writes_to_same_store_never_fail() {
        let dir = tempdir();
        let store = dir.join(STORE_FILE);
        let mut handles = Vec::new();
        for i in 0..32u16 {
            let store = store.clone();
            handles.push(std::thread::spawn(move || {
                let mut map = BTreeMap::new();
                map.insert(format!("PORT_{i}"), 1000 + i);
                for _ in 0..50 {
                    write_store_atomic(&store, &map)
                        .expect("atomic write must not fail under concurrency");
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        // The surviving file is always complete, valid JSON (atomic rename).
        assert!(!read_store(&store).is_empty());
        // No temp files were leaked.
        let leaked: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp."))
            .collect();
        assert!(leaked.is_empty(), "leaked temp files: {leaked:?}");
    }

    /// Regression mirroring the test-suite trigger: multiple configs resolving
    /// the same dynamicPorts directory at once (as the fullstack example tests
    /// do) must all succeed with complete, internally-consistent maps.
    #[test]
    fn concurrent_resolve_on_shared_dir_all_succeed() {
        let dir = std::sync::Arc::new(tempdir());
        let vars = names(&["A_PORT", "B_PORT", "C_PORT", "D_PORT"]);
        let mut handles = Vec::new();
        for _ in 0..16 {
            let dir = std::sync::Arc::clone(&dir);
            let vars = vars.clone();
            handles.push(std::thread::spawn(move || {
                let m = resolve_dynamic_ports(&vars, &dir)
                    .expect("resolve must not fail under concurrency");
                assert_eq!(m.len(), 4);
                assert!(m.values().all(|v| v != "0"));
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
    }
}
