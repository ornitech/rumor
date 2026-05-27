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

See [`example.config.json`](example.config.json) for the schema. A minimal config:

```json
{
  "processes": [
    { "name": "counter", "command": "bash", "args": ["-c", "i=0; while true; do echo $i; i=$((i+1)); sleep 1; done"] },
    { "name": "repl",    "command": "python3", "args": ["-q"], "env": { "PS1": "py> " } }
  ]
}
```

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
