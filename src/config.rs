use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReadinessCondition {
    /// TCP connect to 127.0.0.1:port succeeds.
    Port(u16),
    /// A regex match against the dep's accumulated stdout/stderr bytes.
    Log(String),
    /// The dep process exits with this code (typical: 0 for one-shot setup).
    Exit(i32),
}

#[derive(Debug, Clone, Deserialize)]
pub struct Dependency {
    pub name: String,
    pub until: ReadinessCondition,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProcessConfig {
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    pub cwd: PathBuf,
    /// Additional env files to load after `<cwd>/.env`. Relative paths are
    /// resolved against the config file's directory. Loaded in order; later
    /// files override earlier ones.
    #[serde(default, rename = "envFiles")]
    pub env_files: Vec<PathBuf>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    #[serde(default, rename = "dependsOn")]
    pub depends_on: Vec<Dependency>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub processes: Vec<ProcessConfig>,
}

#[derive(Debug)]
pub struct LoadedConfig {
    pub config: Config,
    pub config_dir: PathBuf,
}

impl Config {
    pub fn load(path: &Path) -> Result<LoadedConfig> {
        let bytes = std::fs::read(path)
            .with_context(|| format!("reading config file {}", path.display()))?;
        let mut config: Config = serde_json::from_slice(&bytes)
            .with_context(|| format!("parsing config file {}", path.display()))?;

        let config_dir = path
            .canonicalize()
            .with_context(|| format!("canonicalizing {}", path.display()))?
            .parent()
            .ok_or_else(|| anyhow!("config path has no parent directory"))?
            .to_path_buf();

        if config.processes.is_empty() {
            return Err(anyhow!("config has no processes"));
        }

        let mut seen = HashSet::new();
        for proc in &mut config.processes {
            if proc.name.trim().is_empty() {
                return Err(anyhow!("process name cannot be empty"));
            }
            if !seen.insert(proc.name.clone()) {
                return Err(anyhow!("duplicate process name: {}", proc.name));
            }
            if proc.command.trim().is_empty() {
                return Err(anyhow!("process {}: command cannot be empty", proc.name));
            }

            if proc.cwd.is_relative() {
                proc.cwd = config_dir.join(&proc.cwd);
            }
            let canonical = proc.cwd.canonicalize().with_context(|| {
                format!(
                    "process {}: cwd does not exist or is not accessible: {}",
                    proc.name,
                    proc.cwd.display()
                )
            })?;
            if !canonical.is_dir() {
                return Err(anyhow!(
                    "process {}: cwd is not a directory: {}",
                    proc.name,
                    canonical.display()
                ));
            }
            proc.cwd = canonical;

            for dep in &proc.depends_on {
                if let ReadinessCondition::Log(rx) = &dep.until {
                    regex::Regex::new(rx).with_context(|| {
                        format!(
                            "process {}: dependsOn[{}].until.log is not a valid regex",
                            proc.name, dep.name
                        )
                    })?;
                }
            }

            for ef in &mut proc.env_files {
                if ef.is_relative() {
                    *ef = config_dir.join(&ef);
                }
                let canonical = ef.canonicalize().with_context(|| {
                    format!(
                        "process {}: envFiles entry not found: {}",
                        proc.name,
                        ef.display()
                    )
                })?;
                if !canonical.is_file() {
                    return Err(anyhow!(
                        "process {}: envFiles entry is not a file: {}",
                        proc.name,
                        canonical.display()
                    ));
                }
                *ef = canonical;
            }
        }

        validate_dep_graph(&config.processes)?;

        Ok(LoadedConfig { config, config_dir })
    }
}

/// Validate that all `dependsOn` references resolve to a process in the config
/// and that the dependency graph has no cycles. Cycle detection is a DFS with
/// a recursion stack (white/gray/black coloring).
fn validate_dep_graph(processes: &[ProcessConfig]) -> Result<()> {
    let name_to_idx: HashMap<&str, usize> = processes
        .iter()
        .enumerate()
        .map(|(i, p)| (p.name.as_str(), i))
        .collect();

    let mut adj: Vec<Vec<usize>> = vec![vec![]; processes.len()];
    for (i, p) in processes.iter().enumerate() {
        for dep in &p.depends_on {
            let target = name_to_idx.get(dep.name.as_str()).copied().ok_or_else(|| {
                anyhow!(
                    "process {}: dependsOn references unknown process '{}'",
                    p.name,
                    dep.name
                )
            })?;
            if target == i {
                return Err(anyhow!(
                    "process {}: cannot depend on itself",
                    p.name
                ));
            }
            adj[i].push(target);
        }
    }

    // Iterative DFS with coloring: 0 white, 1 gray (on stack), 2 black (done).
    let mut color = vec![0u8; processes.len()];
    let mut stack: Vec<(usize, usize)> = Vec::new(); // (node, next-neighbor-index)
    for start in 0..processes.len() {
        if color[start] != 0 {
            continue;
        }
        color[start] = 1;
        stack.push((start, 0));
        while let Some(&mut (node, ref mut next)) = stack.last_mut() {
            if *next < adj[node].len() {
                let nb = adj[node][*next];
                *next += 1;
                match color[nb] {
                    0 => {
                        color[nb] = 1;
                        stack.push((nb, 0));
                    }
                    1 => {
                        let cycle_names: Vec<&str> = stack
                            .iter()
                            .map(|(n, _)| processes[*n].name.as_str())
                            .collect();
                        return Err(anyhow!(
                            "dependsOn cycle detected: {} -> {}",
                            cycle_names.join(" -> "),
                            processes[nb].name
                        ));
                    }
                    _ => {}
                }
            } else {
                color[node] = 2;
                stack.pop();
            }
        }
    }
    Ok(())
}
