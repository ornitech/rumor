# Fullstack example

A four-service example that shows off:

- **Dependency ordering** via port-based readiness (`dependsOn` / `until.port`)
- **Docker-managed services** treated as ordinary long-lived processes
- **Three layers of environment variables** with explicit precedence

## Topology

```
db (postgres in docker)   redis (in docker)
        \                    /
         \                  /
           api (python http)
                  |
            frontend (python http)
```

`api` waits for `db:5432` and `redis:6379` before starting.
`frontend` waits for `api:3000` before starting.

## Prereqs

- `docker` (running)
- `python3` (stdlib only, no pip installs)
- Free ports: `5432`, `6379`, `3000`, `8080`

## Run

From the repo root:

```sh
cargo run -- examples/fullstack/fullstack.config.json
```

Or, if rumor is installed on your PATH:

```sh
rumor examples/fullstack/fullstack.config.json
```

Open the frontend at <http://localhost:8080> and the api at <http://localhost:3000>.

## What proves the env layering works

There are three layers, loaded in this order (later wins), per `src/env.rs`:

1. orchestrator's own environment
2. `envFiles` paths in `fullstack.config.json` (here: central `./.env`, then `./<service>/.env.local`) — relative paths resolve against the **config file's directory**, not the service's `cwd`
3. the JSON `env` block on the service

| Variable            | Central `.env`              | `<svc>/.env.local`                       | JSON `env`               | Final value (which service) |
| ------------------- | --------------------------- | ---------------------------------------- | ------------------------ | --------------------------- |
| `LOG_LEVEL`         | `info`                      | `debug` (api)                            | `warn` (frontend)        | api: `debug`, frontend: `warn`, db/redis: `info` |
| `FRONTEND_TITLE`    | `Rumor Fullstack Example`   | `Frontend (local override)` (frontend)   | -                        | frontend: `Frontend (local override)` |
| `POSTGRES_PASSWORD` | `changeme`                  | `db-local-secret` (db)                   | -                        | db: `db-local-secret` (forwarded into the container via `-e POSTGRES_PASSWORD`) |
| `REDIS_LOG_LEVEL`   | `notice`                    | `debug` (redis)                          | -                        | redis: `debug` (expanded into `redis-server --loglevel` by the bash wrapper) |

Each `.env.local` overrides something real, so every service exercises the merge.

To verify all three layers in one run:

- `curl http://localhost:3000/` -> JSON includes `"LOG_LEVEL": "debug"` (api `.env.local` beat central `.env`).
- Open <http://localhost:8080> -> page title says "Frontend (local override)" (frontend `.env.local` beat central `.env`) AND the page reports `LOG_LEVEL=warn` (JSON `env` block beat both files).
- `docker exec rumor-example-db env | grep POSTGRES_PASSWORD` -> `POSTGRES_PASSWORD=db-local-secret` (db `.env.local` beat central `.env`, forwarded through docker).
- The `redis` tab's stdout shows DEBUG-level boot messages (redis `.env.local` set `REDIS_LOG_LEVEL=debug`, which the bash wrapper expanded into `redis-server --loglevel debug`).

## Why bare `-e VAR` on the docker services

The docker `-e VAR` flag (no `=value`) forwards `VAR` from the current process's environment into the container. Because rumor merges `envFiles` into the child env before spawning, the docker process inherits `POSTGRES_USER`, `POSTGRES_PASSWORD`, etc. from the central `.env`, and docker hands them off to the container. This keeps secrets in env files instead of inlined in the orchestrator config.

## Cleanup

Containers run with `--rm`, so Ctrl-C is enough in the normal case. If rumor was force-killed mid-startup, clean up by name:

```sh
docker rm -f rumor-example-db rumor-example-redis
```
