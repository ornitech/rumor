# rumor

Multi-process TUI orchestrator — run your whole dev stack in one terminal, each process in its own
tab, with dependency gating, retry policies, and dynamic ports.

This repository is a monorepo:

| Path     | What it is                                                            |
|----------|----------------------------------------------------------------------|
| `rumor/` | The Rust crate — the `rumor` binary, its tests, docs, and examples.   |
| `web/`   | The marketing site ([Astro](https://astro.build), deployed to Cloudflare Pages). |

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/ornitech/rumor/main/install.sh | sh
```

Or with Homebrew: `brew tap ornitech/tap && brew install rumor`.
See [`rumor/README.md`](rumor/README.md) for full usage, configuration, and examples.

## Develop

- **The site, with rumor** (dogfooding): run `rumor` from the repo root. The root
  [`rumor.json`](rumor.json) installs the site's dependencies as a one-shot, then starts the Astro
  dev server on a dynamically allocated `WEB_PORT` once the install exits 0. The terminal prints the
  local URL. With a dev build instead of an installed binary: `rumor/scripts/run.sh rumor.json`.
- **The app:** `cd rumor && cargo run -- example.config.json` (see `rumor/README.md`).
- **The site, directly:** `cd web && npm install && npm run dev` (see `web/README.md`).
- **Commit hooks:** run `git config core.hooksPath .githooks` once to enable the
  [`commit-msg`](.githooks/commit-msg) hook that enforces [Conventional Commits](https://www.conventionalcommits.org)
  locally (the same types CI checks on PR titles).

## Releases

The `rumor` crate is versioned with [release-please](https://github.com/googleapis/release-please)
(config at the repo root scopes it to `rumor/`). Tagging `v*` triggers
[`.github/workflows/release.yml`](.github/workflows/release.yml), which builds macOS binaries,
publishes them to GitHub Releases, and updates the Homebrew tap.

The site has no version: it redeploys to Cloudflare Pages on every push to `main`, with preview
deployments for pull requests. See [`web/README.md`](web/README.md).

## License

MIT — see [LICENSE](LICENSE).
