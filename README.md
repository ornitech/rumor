# rumor

Multi-process TUI orchestrator. Run a JSON-configured set of long-lived processes side by side in one terminal, each in its own PTY tab with full ANSI rendering and interactive input.

## Install

```bash
brew tap ornitech/tap
brew install rumor
```

## Usage

```bash
rumor <config.json>
```

Set `"longLived": false` on one-shot processes (migrations, seed scripts) so a
clean `exit 0` is shown as success (green) rather than a crash (red). Defaults
to `true`.

See [`example.config.json`](example.config.json) for the schema. A minimal config:

```json
{
  "processes": [
    { "name": "counter", "command": "bash", "args": ["-c", "i=0; while true; do echo $i; i=$((i+1)); sleep 1; done"] },
    { "name": "repl",    "command": "python3", "args": ["-q"], "env": { "PS1": "py> " } }
  ]
}
```

Relative paths in `cwd` and `envFiles` are resolved against the **config file's directory**, not the orchestrator's working directory. So `"envFiles": ["./.env", "./api/.env.local"]` looks for those files next to the config file, regardless of where you invoke `rumor` from.

## Environment variable references

String fields in the config may reference environment variables with `${NAME}`.
Use `$${NAME}` to emit a literal `${NAME}`.

Substitution applies to:

- `command`
- each entry of `args`
- `dependsOn[].until.port` and `dependsOn[].until.exit` (write them as JSON
  strings, e.g. `"port": "${API_PORT}"`; literal numbers also still work)
- `dependsOn[].until.log` (the regex string)

The lookup uses, in order (later wins): the orchestrator's own env, then
`<cwd>/.env`, then each `envFiles` entry, then the process's `env` block. So
env files referenced from the config are loaded *before* substitution.

A `${...}` whose contents aren't a strict identifier (e.g. `${RATE:-1}`) is
passed through verbatim, so shell-side interpolation in `args` keeps working.
A referenced variable that isn't set substitutes to an empty string and emits
a warning to `~/Library/Logs/rumor/rumor.log`.

Example: a single `.env` file drives both the spawned process and rumor's
readiness check.

```json
{
  "processes": [
    { "name": "db", "command": "postgres", "cwd": "./db", "envFiles": ["../.env"] },
    {
      "name": "api",
      "command": "./bin/api",
      "cwd": "./api",
      "envFiles": ["../.env"],
      "dependsOn": [
        { "name": "db", "until": { "port": "${DB_PORT}" } }
      ]
    }
  ]
}
```

## Examples

### [`examples/fullstack/`](examples/fullstack/) — four-service stack

A realistic four-service topology that exercises rumor's more interesting features:

- **`db`** (postgres in docker) and **`redis`** (also docker, wrapped in `bash -c` so env vars expand into args) start in parallel.
- **`api`** (python stdlib HTTP server) waits for both via port-based readiness checks (`dependsOn` + `until.port`).
- **`frontend`** (python static server) waits for `api`.

It also demonstrates the three-layer env merge: a central `examples/fullstack/.env`, per-service `<svc>/.env.local`, and a JSON `env` block on one service that overrides both files. Every `.env.local` overrides something visible (db's password, redis's log level, api's log level, frontend's title), so each layer's effect is observable end-to-end.

Run:

```bash
rumor examples/fullstack/fullstack.config.json
# or, from a clone:
cargo run -- examples/fullstack/fullstack.config.json
```

Requires `docker`, `python3`, and free ports `5432` / `6379` / `3000` / `8080`. Open <http://localhost:8080> for the frontend; see the example's [README](examples/fullstack/README.md) for the env-precedence table and verification steps.

## Keys

Two modes: **Nav** (default) and **Focus** (keystrokes go to the selected child).

| Key | Action |
| --- | --- |
| `Left` / `Right` | Switch tab |
| `Up` / `Down` / `PgUp` / `PgDn` / `Home` / `End` | Scroll the selected tab's scrollback |
| `Enter` | Enter Focus mode (input forwarded to the child PTY) |
| `Esc` | Leave Focus mode |
| `r` | Restart the selected process |
| `k` | Kill the selected process |
| `Ctrl+R` | Restart all |
| `Ctrl+K` | Kill all |
| `w` | Toggle line-wrap on the selected tab |
| `q` / `Ctrl+C` | Quit |

## Logs

Written to `~/Library/Logs/rumor/rumor.log` on macOS (`~/.local/share/rumor/rumor.log` on Linux). Set `RUMOR_LOG=debug` for verbose tracing.

## License

[MIT](LICENSE)
