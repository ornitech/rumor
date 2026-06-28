# Fullstack example

A four-service example that shows off:

- **Dependency ordering** via port-based readiness (`dependsOn` / `until.port`)
- **Docker-managed services** treated as ordinary long-lived processes
- **Three layers of environment variables** with explicit precedence
- **`${VAR}` substitution in config strings** so a single env var drives both a docker `-p` mapping and the matching `dependsOn.until.port` check
- **`dynamicPorts`** so every checkout (e.g. each git worktree) gets its own ports instead of fighting over hardcoded ones

## Topology

```
db (postgres in docker)   redis (in docker)
        \                    /
         \                  /
           api (python http)
                  |
            frontend (python http)
```

`api` waits for `db:${POSTGRES_PORT}` and `redis:${REDIS_PORT}` before starting.
`frontend` waits for `api:${API_PORT}` before starting. All four port numbers are declared as `dynamicPorts` in the config: rumor allocates free ports on first run, saves them to `.rumor-ports.json` next to the config, and reuses them on every later run. One allocated value drives the docker `-p` mapping, the server's listen port, and rumor's readiness check.

## Prereqs

- `docker` (running)
- `python3` (stdlib only, no pip installs)

No fixed free ports required: the four ports are `dynamicPorts`, allocated by rumor on first run.

## Run

From the repo root:

```sh
cargo run -- examples/fullstack/fullstack.config.json
```

Or, if rumor is installed on your PATH:

```sh
rumor examples/fullstack/fullstack.config.json
```

The allocated ports are in `examples/fullstack/.rumor-ports.json` (also visible in each tab's startup output and the details screen, `d`):

```sh
cat examples/fullstack/.rumor-ports.json
```

Open the frontend at `http://localhost:<FRONTEND_PORT>` and the api at `http://localhost:<API_PORT>`. Delete `.rumor-ports.json` to force fresh ports; a second clone or git worktree of this repo gets its own allocation automatically, so both stacks can run at the same time.

## What proves the env layering works

Layers, loaded in this order (later wins), per `src/env.rs`:

1. orchestrator's own environment
2. the top-level (global) `envFiles` in `fullstack.config.json` — the central `./.env`, declared once and shared by every service (lowest config precedence)
3. each service's own `envFiles` (here: `./<service>/.env.local`)
4. the JSON `env` block on the service
5. the top-level `dynamicPorts` allocations (always highest)

Relative paths in either `envFiles` resolve against the **config file's directory**, not the service's `cwd`.

| Variable            | Central `.env`              | `<svc>/.env.local`                       | JSON `env`               | Final value (which service) |
| ------------------- | --------------------------- | ---------------------------------------- | ------------------------ | --------------------------- |
| `LOG_LEVEL`         | `info`                      | `debug` (api)                            | `warn` (frontend)        | api: `debug`, frontend: `warn`, db/redis: `info` |
| `FRONTEND_TITLE`    | `Rumor Fullstack Example`   | `Frontend (local override)` (frontend)   | -                        | frontend: `Frontend (local override)` |
| `POSTGRES_PASSWORD` | `changeme`                  | `db-local-secret` (db)                   | -                        | db: `db-local-secret` (forwarded into the container via `-e POSTGRES_PASSWORD`) |
| `REDIS_LOG_LEVEL`   | `notice`                    | `debug` (redis)                          | -                        | redis: `debug` (rumor substitutes `${REDIS_LOG_LEVEL}` into the redis service's args, so docker runs `redis-server --loglevel debug`) |
| `POSTGRES_PORT`     | - (`dynamicPorts`)          | -                                        | -                        | allocated by rumor, persisted in `.rumor-ports.json`; drives both the db `-p ${POSTGRES_PORT}:5432` mapping and api's `until.port`. Same pattern for `REDIS_PORT`, `API_PORT`, `FRONTEND_PORT`. Always wins the merge, so an `.env` value could not shadow it |

Each `.env.local` overrides something real, so every service exercises the merge.

To verify all the layers in one run (substitute the ports from `.rumor-ports.json`):

- `curl http://localhost:<API_PORT>/` -> JSON includes `"LOG_LEVEL": "debug"` (api `.env.local` beat central `.env`).
- Open `http://localhost:<FRONTEND_PORT>` -> page title says "Frontend (local override)" (frontend `.env.local` beat central `.env`) AND the page reports `LOG_LEVEL=warn` (JSON `env` block beat both files).
- `docker exec rumor-example-db env | grep POSTGRES_PASSWORD` -> `POSTGRES_PASSWORD=db-local-secret` (db `.env.local` beat central `.env`, forwarded through docker).
- The `redis` tab's stdout shows DEBUG-level boot messages (redis `.env.local` set `REDIS_LOG_LEVEL=debug`, which rumor substituted into the args list so docker ran `redis-server --loglevel debug`).
- Re-run rumor: the ports stay the same (read back from `.rumor-ports.json`). Delete that file and re-run: all four services come up on fresh ports, and every `-p` mapping, listen port, and `until.port` check follows along — nothing else to edit.

## Why bare `-e VAR` on the docker services

The docker `-e VAR` flag (no `=value`) forwards `VAR` from the current process's environment into the container. Because rumor merges `envFiles` into the child env before spawning, the docker process inherits `POSTGRES_USER`, `POSTGRES_PASSWORD`, etc. from the central `.env`, and docker hands them off to the container. This keeps secrets in env files instead of inlined in the orchestrator config.

## Cleanup

Containers run with `--rm`, so Ctrl-C is enough in the normal case. If rumor was force-killed mid-startup, clean up by name:

```sh
docker rm -f rumor-example-db rumor-example-redis
```
