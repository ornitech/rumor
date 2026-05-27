use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;

use crate::env as envmod;
use crate::template;

#[derive(Debug, Clone)]
pub enum ReadinessCondition {
    /// TCP connect to 127.0.0.1:port succeeds.
    Port(u16),
    /// A regex match against the dep's accumulated stdout/stderr bytes.
    Log(String),
    /// The dep process exits with this code (typical: 0 for one-shot setup).
    Exit(i32),
}

#[derive(Debug, Clone)]
pub struct Dependency {
    pub name: String,
    pub until: ReadinessCondition,
}

#[derive(Debug, Clone)]
pub struct ProcessConfig {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    /// Additional env files to load after `<cwd>/.env`. Loaded in order; later
    /// files override earlier ones. Already canonicalized at load time.
    pub env_files: Vec<PathBuf>,
    pub env: HashMap<String, String>,
    pub depends_on: Vec<Dependency>,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub processes: Vec<ProcessConfig>,
}

#[derive(Debug)]
pub struct LoadedConfig {
    pub config: Config,
    pub config_dir: PathBuf,
}

// --- Raw (pre-substitution) deserialization shapes ---------------------------

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum NumOrTemplate<N> {
    Num(N),
    Template(String),
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
enum RawReadinessCondition {
    Port(NumOrTemplate<u16>),
    Log(String),
    Exit(NumOrTemplate<i32>),
}

#[derive(Debug, Deserialize)]
struct RawDependency {
    name: String,
    until: RawReadinessCondition,
}

#[derive(Debug, Deserialize)]
struct RawProcessConfig {
    name: String,
    command: String,
    #[serde(default)]
    args: Vec<String>,
    cwd: PathBuf,
    #[serde(default, rename = "envFiles")]
    env_files: Vec<PathBuf>,
    #[serde(default)]
    env: HashMap<String, String>,
    #[serde(default, rename = "dependsOn")]
    depends_on: Vec<RawDependency>,
}

#[derive(Debug, Deserialize)]
struct RawConfig {
    processes: Vec<RawProcessConfig>,
}

// --- Loader ------------------------------------------------------------------

impl Config {
    pub fn load(path: &Path) -> Result<LoadedConfig> {
        let bytes = std::fs::read(path)
            .with_context(|| format!("reading config file {}", path.display()))?;
        let raw: RawConfig = serde_json::from_slice(&bytes)
            .with_context(|| format!("parsing config file {}", path.display()))?;

        let config_dir = path
            .canonicalize()
            .with_context(|| format!("canonicalizing {}", path.display()))?
            .parent()
            .ok_or_else(|| anyhow!("config path has no parent directory"))?
            .to_path_buf();

        if raw.processes.is_empty() {
            return Err(anyhow!("config has no processes"));
        }

        let mut seen = HashSet::new();
        let mut processes: Vec<ProcessConfig> = Vec::with_capacity(raw.processes.len());

        for raw_proc in raw.processes {
            let RawProcessConfig {
                name,
                command,
                args,
                mut cwd,
                mut env_files,
                env,
                depends_on,
            } = raw_proc;

            if name.trim().is_empty() {
                return Err(anyhow!("process name cannot be empty"));
            }
            if !seen.insert(name.clone()) {
                return Err(anyhow!("duplicate process name: {}", name));
            }
            if command.trim().is_empty() {
                return Err(anyhow!("process {}: command cannot be empty", name));
            }

            if cwd.is_relative() {
                cwd = config_dir.join(&cwd);
            }
            let canonical_cwd = cwd.canonicalize().with_context(|| {
                format!(
                    "process {}: cwd does not exist or is not accessible: {}",
                    name,
                    cwd.display()
                )
            })?;
            if !canonical_cwd.is_dir() {
                return Err(anyhow!(
                    "process {}: cwd is not a directory: {}",
                    name,
                    canonical_cwd.display()
                ));
            }
            cwd = canonical_cwd;

            for ef in &mut env_files {
                if ef.is_relative() {
                    *ef = config_dir.join(&ef);
                }
                let canonical = ef.canonicalize().with_context(|| {
                    format!(
                        "process {}: envFiles entry not found: {}",
                        name,
                        ef.display()
                    )
                })?;
                if !canonical.is_file() {
                    return Err(anyhow!(
                        "process {}: envFiles entry is not a file: {}",
                        name,
                        canonical.display()
                    ));
                }
                *ef = canonical;
            }

            // Build the env used both to spawn the child and to substitute
            // ${VAR} references in this process's templated fields.
            let subst_env = envmod::build_env(&cwd, &env_files, &env).with_context(|| {
                format!("process {}: building env for template substitution", name)
            })?;

            // Substitute templated string fields.
            let command = template::substitute(
                &command,
                &subst_env,
                &format!("process '{name}' command"),
            )?;
            let mut subst_args = Vec::with_capacity(args.len());
            for (idx, a) in args.into_iter().enumerate() {
                subst_args.push(template::substitute(
                    &a,
                    &subst_env,
                    &format!("process '{name}' args[{idx}]"),
                )?);
            }
            let args = subst_args;

            // Resolve dependsOn.
            let mut deps: Vec<Dependency> = Vec::with_capacity(depends_on.len());
            for raw_dep in depends_on {
                let RawDependency {
                    name: dep_name,
                    until,
                } = raw_dep;
                let until = match until {
                    RawReadinessCondition::Port(NumOrTemplate::Num(n)) => {
                        ReadinessCondition::Port(n)
                    }
                    RawReadinessCondition::Port(NumOrTemplate::Template(t)) => {
                        let ctx = format!(
                            "process '{name}' dependsOn[{dep_name}].until.port"
                        );
                        let s = template::substitute(&t, &subst_env, &ctx)?;
                        let port: u16 = s.parse().map_err(|_| {
                            anyhow!(
                                "{ctx}: value {:?} is not a valid u16 (range 0-65535)",
                                s
                            )
                        })?;
                        ReadinessCondition::Port(port)
                    }
                    RawReadinessCondition::Exit(NumOrTemplate::Num(n)) => {
                        ReadinessCondition::Exit(n)
                    }
                    RawReadinessCondition::Exit(NumOrTemplate::Template(t)) => {
                        let ctx = format!(
                            "process '{name}' dependsOn[{dep_name}].until.exit"
                        );
                        let s = template::substitute(&t, &subst_env, &ctx)?;
                        let code: i32 = s.parse().map_err(|_| {
                            anyhow!("{ctx}: value {:?} is not a valid i32", s)
                        })?;
                        ReadinessCondition::Exit(code)
                    }
                    RawReadinessCondition::Log(rx) => {
                        let ctx = format!(
                            "process '{name}' dependsOn[{dep_name}].until.log"
                        );
                        let resolved = template::substitute(&rx, &subst_env, &ctx)?;
                        regex::Regex::new(&resolved).with_context(|| {
                            format!(
                                "process {}: dependsOn[{}].until.log is not a valid regex",
                                name, dep_name
                            )
                        })?;
                        ReadinessCondition::Log(resolved)
                    }
                };
                deps.push(Dependency {
                    name: dep_name,
                    until,
                });
            }

            processes.push(ProcessConfig {
                name,
                command,
                args,
                cwd,
                env_files,
                env,
                depends_on: deps,
            });
        }

        validate_dep_graph(&processes)?;

        Ok(LoadedConfig {
            config: Config { processes },
            config_dir,
        })
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
