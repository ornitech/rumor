# Agent guidelines

## Always keep the app runnable as `rumor-<branch>`

Every workspace must expose a launcher command named `rumor-<branch>`, where
`<branch>` is the current git branch (`git rev-parse --abbrev-ref HEAD`). This lets
multiple parallel workspaces each be launched without clobbering one another.

- Command name: `rumor-<branch>` (e.g. branch `terminal-resize-on-restart` →
  `rumor-terminal-resize-on-restart`).
- Install it as an executable script in `~/.local/bin` (already on `PATH`).
- The launcher rebuilds this workspace's source (`cargo build --release`) and execs
  the resulting binary, so the command always reflects the branch's current code.
  Build quietly and surface output only on failure, then `exec` the binary directly
  (do not use `cargo run`, which leaks cached warnings onto the TUI screen).

Template:

```sh
#!/bin/sh
MANIFEST="<absolute path>/Cargo.toml"
BIN="<absolute path>/target/release/rumor"
if ! out="$(cargo build --release --quiet --manifest-path "$MANIFEST" 2>&1)"; then
  printf '%s\n' "$out" >&2
  exit 1
fi
exec "$BIN" "$@"
```

Usage: `rumor-<branch> <config.json>` (e.g.
`rumor-<branch> examples/fullstack/fullstack.config.json`).

When starting work on a new branch, create the matching `rumor-<branch>` launcher
if it does not already exist.
