use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};

/// Build the environment for a child process.
///
/// Merge order (later wins):
/// 1. The orchestrator's own env (so PATH, HOME, etc. propagate).
/// 2. Each path in `global_env_files` (root-level, shared by all processes), in
///    order. Lowest config-declared precedence.
/// 3. `<cwd>/.env` if it exists (auto-discovered, never mutates our own env).
/// 4. Each path in `env_files`, in order.
/// 5. The JSON config's `env` block (explicit overrides).
pub fn build_env(
    cwd: &Path,
    global_env_files: &[std::path::PathBuf],
    env_files: &[std::path::PathBuf],
    json_env: &HashMap<String, String>,
) -> Result<HashMap<String, String>> {
    let mut env: HashMap<String, String> = std::env::vars().collect();

    for path in global_env_files {
        load_into(path, &mut env)?;
    }

    let dotenv_path = cwd.join(".env");
    if dotenv_path.is_file() {
        load_into(&dotenv_path, &mut env)?;
    }

    for path in env_files {
        load_into(path, &mut env)?;
    }

    for (k, v) in json_env {
        env.insert(k.clone(), v.clone());
    }

    Ok(env)
}

fn load_into(path: &Path, env: &mut HashMap<String, String>) -> Result<()> {
    let iter = dotenvy::from_path_iter(path)
        .with_context(|| format!("opening {}", path.display()))?;
    for item in iter {
        let (k, v) = item.with_context(|| format!("parsing {}", path.display()))?;
        env.insert(k, v);
    }
    Ok(())
}
