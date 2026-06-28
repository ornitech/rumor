# rumor — AI agent guide

`rumor` runs a JSON-configured set of processes side by side, each in its own
PTY. This document is the machine-readable reference for driving `rumor` and for
authoring its config files. Print it with `rumor docs --agent`.

## Running rumor for non-interactive use

The default UI is an interactive TUI. As an agent, use raw mode instead:

```bash
rumor --raw                          # run ./rumor.json, stream combined output
rumor config.json --raw              # explicit config path
rumor --raw --only api,worker        # run everything, print only these
rumor --raw --color                  # keep ANSI escapes (default: stripped)
rumor -t backend --raw               # run only tagged processes (+ their deps)
```

- With no config argument, `rumor` loads `./rumor.json` from the current directory.
- `--raw` skips the TUI and streams every process's output to a single stdout,
  one line at a time prefixed with `[name]`. ANSI escape codes are stripped by
  default (they waste tokens and break naive line parsers); pass `--color` to keep them.
- `--only NAME,...` filters which processes' output is **printed**; every process
  still **runs** (including dependencies). `--only` and `--color` require `--raw`.
- `-t`/`--tags TAG,...` selects the run-set: processes carrying **any** of the
  given tags, plus their transitive `dependsOn` targets. A positional config path
  must come before the first `-t`.
- `rumor` runs until it receives Ctrl+C / SIGTERM, then SIGTERMs every child and
  exits. If every selected process is run-to-completion (`longLived: false`),
  `rumor` exits on its own once they have all finished.

Exit code `2` with a usage message on stderr means the CLI args were invalid.

## Config file

JSON. Top level is `{ "processes": [ ... ] }` plus optional `envFiles` and
`dynamicPorts`. Paths in the config resolve against the **config file's
directory**, not the current working directory.

### Top level

| Field | Type | Default | Meaning |
| --- | --- | --- | --- |
| `processes` | process[] | required | Processes to run. Must be non-empty. |
| `envFiles` | string[] | `[]` | `.env` files loaded for **every** process, in order. Lowest precedence in the env merge. Use for a shared monorepo root `.env`. |
| `dynamicPorts` | string[] | `[]` | Env var names `rumor` resolves to free ports (see Dynamic ports). Highest env precedence; usable in `${VAR}`. |

### Process object

| Field | Type | Default | Meaning |
| --- | --- | --- | --- |
| `name` | string | required | Unique within the config; the tab label. |
| `command` | string | required | Executable to spawn. Supports `${VAR}`. |
| `args` | string[] | `[]` | Arguments; each entry supports `${VAR}`. |
| `cwd` | string | required | Working directory. Relative to the config file's directory. Must exist. |
| `env` | object | `{}` | Inline env vars. Highest precedence below `dynamicPorts`. |
| `envFiles` | string[] | `[]` | Extra `.env` files loaded after `<cwd>/.env`; later files override earlier ones. |
| `dependsOn` | dependency[] | `[]` | Readiness gates that must pass before this process starts. |
| `longLived` | bool | `true` | If `false`, a clean `exit 0` is success (green) instead of a crash (red). Use for migrations / one-shot setup. |
| `tags` | string[] | `[]` | Labels for `-t`/`--tags` selection. Empty/whitespace entries ignored. |
| `retry` | object | none | Auto-restart policy on failure. Omit to disable. |

### Retry object (`retry`)

`rumor` restarts the process after a failed exit, up to `maxRetries` times, with
a backoff delay between attempts. A `longLived` process that exits **at all** is
a failure; a one-shot (`longLived: false`) fails only on a non-zero exit code or
a signal. A user-initiated stop never triggers a retry; the budget resets on a
manual restart. After exhaustion the tab shows `exited (...) (retries exhausted)`.

| Field | Type | Default | Meaning |
| --- | --- | --- | --- |
| `maxRetries` | u32 | required | Max auto-restart attempts. Must be `>= 1`. |
| `strategy` | string | `"fixed"` | `"fixed"` (`delayMs`), `"linear"` (`delayMs * attempt`), or `"exponential"` (`delayMs * 2^(attempt-1)`). |
| `delayMs` | u64 | required | Base delay in milliseconds. Must be `>= 1`. |
| `maxDelayMs` | u64 | uncapped | Optional cap on the computed delay. Must be `>= delayMs` when set. |

### Dependency object (`dependsOn[]`)

| Field | Type | Meaning |
| --- | --- | --- |
| `name` | string | Another process in this config. Cycles and self-references are rejected at load. |
| `until` | object | Exactly one readiness condition (below). |

Readiness conditions (`until`):

| Key | Value type | Ready when |
| --- | --- | --- |
| `port` | u16 or `"${VAR}"` | TCP connect to `127.0.0.1:port` or `[::1]:port` succeeds. |
| `log` | string (regex) | The regex matches the dep's accumulated stdout/stderr. Supports `${VAR}`. |
| `exit` | i32 or `"${VAR}"` | The dep process exits with this code (typically `0` for one-shot setup). |

## `${VAR}` substitution

Applies to `command`, each `args` entry, and `dependsOn[].until` values
(`port`/`exit` written as JSON strings, and the `log` regex).

- `${NAME}` (NAME = `[A-Za-z_][A-Za-z0-9_]*`) → the env value.
- `$$` → a literal `$`.
- A `${...}` whose contents aren't a strict identifier (e.g. `${RATE:-1}`) is
  passed through verbatim, so shell-side interpolation in `args` still works.
- An unset variable substitutes to an empty string and logs a warning.

### Env merge precedence (lowest → highest)

1. The orchestrator's own environment (PATH, HOME, ...).
2. Top-level (global) `envFiles`.
3. `<cwd>/.env` (auto-discovered per process).
4. Per-process `envFiles`, in order.
5. The process's `env` block.
6. Top-level `dynamicPorts` allocations (always highest).

Env files are loaded before substitution, so `${VAR}` sees the merged result.

## Dynamic ports (git worktrees)

Declare port env vars as `dynamicPorts` to give each git worktree its own ports
without clashes. On first run each var is bound to a free OS-assigned port and
saved to `.rumor-ports.json` next to the config file; later runs reuse it. A
different worktree (different directory) gets its own `.rumor-ports.json` and its
own ports. Delete the file to reallocate; add it to `.gitignore`. Allocations are
injected with the highest precedence, so a hardcoded `.env`/`env` value cannot
shadow them.

## Status meanings

- Running — the process is up.
- Starting / waiting for dependencies — spawning, or a `dependsOn` gate is pending.
- Blocked — an unmet dependency.
- Exited — non-zero exit, or a `longLived` process that exited at all (a crash);
  also a hard spawn failure (command not found).
- Killed — signal-killed deliberately (not an error).

## Logs

- Session logs (ANSI stripped, grep-friendly), one dir per run:
  `~/Library/Logs/rumor/sessions/<config>-<YYYYMMDD-HHMMSS>/<process>.log` (macOS)
  or `~/.local/share/rumor/sessions/...` (Linux). Set `RUMOR_NO_SESSION_LOGS=1` to disable.
- Main log: `~/Library/Logs/rumor/rumor.log` (macOS) / `~/.local/share/rumor/rumor.log`
  (Linux). Set `RUMOR_LOG=debug` to trace dependency readiness checks and `${VAR}` warnings.

## Complete example

A four-service stack: two docker services start in parallel, the API waits on
both via port readiness, the frontend waits on the API. All ports are dynamic;
every service has a retry policy. Adapt the shape to your own stack.

```json
{
  "envFiles": ["./.env"],
  "dynamicPorts": ["POSTGRES_PORT", "REDIS_PORT", "API_PORT", "FRONTEND_PORT"],
  "processes": [
    {
      "name": "db",
      "command": "docker",
      "args": [
        "run", "--rm", "--name", "myapp-db",
        "-e", "POSTGRES_USER", "-e", "POSTGRES_PASSWORD", "-e", "POSTGRES_DB",
        "-p", "${POSTGRES_PORT}:5432", "postgres:16-alpine"
      ],
      "cwd": "./db",
      "envFiles": ["./db/.env.local"],
      "retry": { "maxRetries": 5, "strategy": "exponential", "delayMs": 1000, "maxDelayMs": 30000 }
    },
    {
      "name": "redis",
      "command": "docker",
      "args": ["run", "--rm", "--name", "myapp-redis", "-p", "${REDIS_PORT}:6379", "redis:7-alpine"],
      "cwd": "./redis",
      "retry": { "maxRetries": 3, "delayMs": 2000 }
    },
    {
      "name": "migrate",
      "command": "./bin/migrate",
      "cwd": "./api",
      "longLived": false,
      "dependsOn": [{ "name": "db", "until": { "port": "${POSTGRES_PORT}" } }]
    },
    {
      "name": "api",
      "command": "python3",
      "args": ["server.py"],
      "cwd": "./api",
      "tags": ["backend"],
      "dependsOn": [
        { "name": "migrate", "until": { "exit": 0 } },
        { "name": "redis",   "until": { "port": "${REDIS_PORT}" } }
      ],
      "retry": { "maxRetries": 5, "strategy": "linear", "delayMs": 1000, "maxDelayMs": 10000 }
    },
    {
      "name": "frontend",
      "command": "npm",
      "args": ["run", "dev", "--", "--port", "${FRONTEND_PORT}"],
      "cwd": "./frontend",
      "env": { "LOG_LEVEL": "warn" },
      "tags": ["frontend"],
      "dependsOn": [{ "name": "api", "until": { "log": "listening on" } }]
    }
  ]
}
```
