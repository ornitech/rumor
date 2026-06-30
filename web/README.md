# getrumor.dev — marketing site

The landing page for [rumor](../rumor), built with [Astro](https://astro.build) and deployed to
Cloudflare Pages. Static output, dark-only, styled after [pi.dev](https://pi.dev). The signature
piece is a sticky terminal panel that animates rumor's TUI scene-by-scene as you scroll.

## Develop

```sh
npm install
npm run dev      # http://localhost:4321
npm run build    # → dist/
npm run preview  # serve the production build
```

## Structure

- `src/pages/index.astro` — the page; assembles the sections.
- `src/components/terminal/` — the scroll demo. `scenes.ts` is the scripted content,
  `terminal.ts` renders/animates it, `TerminalDemo.astro` wires scenes to scroll via an
  `IntersectionObserver` over each `[data-scene]` feature row.
- `src/components/` — `Hero`, `FeatureRow`, `FeatureGrid`, `InstallTabs`, `Nav`, `Footer`,
  `Logo`, and `StatusBar` (the signature fixed bottom tmux-style bar with the live clock).
- `src/consts.ts` — **set `SITE_URL` to the real domain** (canonical, OG, sitemap). Optional
  `CF_ANALYTICS_TOKEN` enables the Cloudflare Web Analytics beacon.
- `public/_redirects` — maps `/install.sh` to the canonical script in the repo root.

## Identity

Deliberately its own look (not a clone of the site it was inspired by): a greenish-charcoal
"terminal cockpit", **Space Grotesk** display + **JetBrains Mono** workhorse (self-hosted variable
fonts in `public/fonts/`, no third-party requests), and the **status spectrum as the brand**.

The spectrum in `src/styles/global.css` mirrors the real TUI
(`rumor/src/status_color.rs` + `src/ui.rs`): running=green, pending=orange (xterm 208),
waiting=yellow, blocked=magenta, crashed=red, killed=gray, info=cyan, focus=yellow. Each feature
section owns one spectrum color, echoed in its spine node, the terminal status line, and the bottom
status bar as you scroll past it.

## Open Graph image

`/og` is a 1200×630 card. It is intentionally **version-less** — social platforms cache
OG images for days, so a live version there would be misleading, and `public/og.png` only
needs regenerating when the card's *design* changes (not on releases).

Regenerate it locally with headless Chrome (no extra dependency, no CI involvement):

```sh
npm run build && npm run preview &           # serve the built /og on :4321
"/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" \
  --headless=new --hide-scrollbars --force-device-scale-factor=1 \
  --window-size=1200,630 --screenshot="public/og.png" http://localhost:4321/og
kill %1                                       # stop preview
```

## Deploy (Cloudflare, via GitHub Actions)

The site deploys to Cloudflare as an **assets-only Worker** (static files, no server code).
[`wrangler.jsonc`](wrangler.jsonc) points the deploy at `dist/`, and
[`.github/workflows/deploy-web.yml`](../.github/workflows/deploy-web.yml) runs it automatically on
every push to `main` **that touches `web/`** — a Rust-only commit to `rumor/` never triggers a site
deploy. Deploys are owned in the repo, not the dashboard, so there is no "hit Deploy" step.

One-time setup (so the workflow can authenticate):

1. Cloudflare dashboard → My Profile → API Tokens → create one from the **"Edit Cloudflare Workers"**
   template (scope it to this account).
2. Grab your **Account ID** (Workers & Pages overview, right sidebar).
3. Add both as GitHub repo secrets: `CLOUDFLARE_API_TOKEN` and `CLOUDFLARE_ACCOUNT_ID`.

The first run creates the `getrumor` Worker and deploys it. Then attach the domain once: the Worker →
Settings → Domains & Routes → add `getrumor.dev` (Cloudflare wires the DNS automatically since the
zone is already on it).

Do **not** also connect the repo via Cloudflare's dashboard "Workers Builds" Git integration — this
Action is the single deploy path; running both would double-deploy. There is no version or tag for
the site (decoupled from the `rumor` binary's release-please flow). `dist/_redirects` (e.g.
`/install.sh` → the canonical installer) is honored by Workers static assets.
