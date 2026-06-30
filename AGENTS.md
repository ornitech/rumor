# Agent guidelines

This is a monorepo with two halves that must not drift apart:

| Path     | What it is                                                        |
|----------|-------------------------------------------------------------------|
| `rumor/` | The Rust crate: the `rumor` binary, its tests, docs, and examples.|
| `web/`   | The marketing site (Astro, getrumor.dev, deployed to Cloudflare). |

`rumor/AGENTS.md` and `web/AGENTS.md` hold the rules specific to each half.

## Keep the website in sync with the app

The website mirrors the app from two sources of truth. Both are consumed at web
build time by `web/scripts/sync-version.mjs`, so you edit the source, never a copy.

| What the site shows | Single source of truth        | How it reaches the site                     |
|---------------------|-------------------------------|---------------------------------------------|
| Version badge       | `rumor/Cargo.toml` `version`  | read at build into `web` `VERSION`          |
| `/docs` page        | `rumor/docs/AGENTS_GUIDE.md`  | copied in and rendered at build             |

What this means when you work on the app:

- **Never hardcode the version anywhere in `web/`.** release-please bumps
  `rumor/Cargo.toml` on each release, and `deploy-web.yml` redeploys the site on
  that commit. The badge updates itself.
- **When you change app behavior** (a command, flag, keybinding, config field,
  status meaning), update `rumor/docs/AGENTS_GUIDE.md` in the same change. That
  file is the reference the binary prints via `rumor docs --agent` AND the source
  of the site's `/docs` page, so one edit keeps the binary, the docs, and the
  website aligned.
- **Then review the editorial marketing copy** for the same fact:
  `web/src/pages/index.astro` (feature rows) and
  `web/src/components/terminal/scenes.ts` (demo scenes) restate keybindings and
  flags by hand. These are not auto-generated, so a renamed flag or changed key
  must be updated there too.
