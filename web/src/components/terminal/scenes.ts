// Scripted scenes for the sticky terminal. Each scene mirrors one feature and a
// real rumor TUI state. Status tokens map to the CSS palette in global.css
// (running/pending/waiting/blocked/crashed/killed) which mirrors the actual TUI
// colors in rumor/src/status_color.rs + src/ui.rs.

export type Status =
  | "running" | "pending" | "waiting" | "blocked" | "crashed" | "killed";

export interface Tab { name: string; status: Status; }
export interface Line { t: string; c?: string; } // c = palette token for the line

// An editor scene swaps the terminal body for a code view of a real config file.
export interface Editor { file: string; code: string[]; }

export interface Scene {
  id: string;
  accent: string;        // palette token used as the section/terminal accent
  tabs: Tab[];
  active: number;        // selected tab index
  focus?: boolean;       // focus-mode (selected tab + mode badge turn yellow)
  raw?: boolean;         // raw mode: one combined stream, no tab bar
  editor?: Editor;       // render as a code editor instead of a terminal
  footer: string;
  lines: Line[];
}

export const scenes: Scene[] = [
  {
    id: "tabs",
    accent: "running",
    tabs: [
      { name: "db", status: "running" },
      { name: "redis", status: "running" },
      { name: "api", status: "running" },
      { name: "web", status: "running" },
    ],
    active: 2,
    footer: "4 running  ·  r restart   k kill   ← → switch tab",
    lines: [
      { t: "[api] starting uvicorn on http://127.0.0.1:8000" },
      { t: "[api] INFO  Application startup complete", c: "running" },
      { t: "[api] GET /health 200  1.2ms" },
      { t: "[api] GET /api/orders 200  8.4ms" },
    ],
  },
  {
    id: "deps",
    accent: "pending",
    tabs: [
      { name: "db", status: "running" },
      { name: "migrate", status: "running" },
      { name: "api", status: "pending" },
      { name: "web", status: "blocked" },
    ],
    active: 2,
    footer: "api ● waiting for dependencies",
    lines: [
      { t: "[api] waiting for db   ·   tcp 127.0.0.1:5432", c: "waiting" },
      { t: "[api] waiting for migrate   ·   exit 0", c: "waiting" },
      { t: "[migrate] migrations applied (12) — exited 0", c: "running" },
      { t: "[db] database system is ready to accept connections", c: "running" },
      { t: "[api] dependencies ready — starting", c: "running" },
    ],
  },
  {
    id: "retry",
    accent: "crashed",
    tabs: [
      { name: "db", status: "running" },
      { name: "api", status: "running" },
      { name: "worker", status: "pending" },
    ],
    active: 2,
    footer: "worker ● retry 2/5  ·  exponential backoff",
    lines: [
      { t: "[worker] connecting to redis://localhost:6379" },
      { t: "[worker] panicked: connection reset by peer", c: "crashed" },
      { t: "[worker] exited (1); retry 2/5 in 400ms", c: "pending" },
      { t: "[worker] restarted", c: "running" },
      { t: "[worker] INFO  consuming jobs", c: "running" },
    ],
  },
  {
    id: "focus",
    accent: "focus",
    tabs: [
      { name: "db", status: "running" },
      { name: "api", status: "running" },
      { name: "repl", status: "running" },
    ],
    active: 2,
    focus: true,
    footer: "FOCUS — keystrokes go to repl   ·   Esc to leave",
    lines: [
      { t: "[repl] >>> orders.where(status='open').count()" },
      { t: "[repl] 1429" },
      { t: "[repl] >>> retry_failed()" },
      { t: "[repl] requeued 6 jobs", c: "running" },
      { t: "[repl] >>> ▌", c: "focus" },
    ],
  },
  {
    id: "ports",
    accent: "info",
    tabs: [],
    active: 0,
    footer: "dynamicPorts → free ports allocated per worktree, injected as ${VAR}",
    editor: {
      file: "rumor.json",
      code: [
        "{",
        '  "dynamicPorts": ["API_PORT", "WEB_PORT"],',
        '  "processes": [',
        "    {",
        '      "name": "api",',
        '      "command": "uvicorn",',
        '      "args": ["app:app", "--port", "${API_PORT}"]',
        "    },",
        "    {",
        '      "name": "web",',
        '      "command": "vite",',
        '      "args": ["--port", "${WEB_PORT}"],',
        '      "dependsOn": [',
        '        { "name": "api", "until": { "port": "${API_PORT}" } }',
        "      ]",
        "    }",
        "  ]",
        "}",
      ],
    },
    lines: [],
  },
  {
    id: "logs",
    accent: "waiting",
    tabs: [
      { name: "db", status: "running" },
      { name: "api", status: "running" },
      { name: "web", status: "running" },
    ],
    active: 1,
    footer: "/ search   ·   n / N matches   ·   y copy log path",
    lines: [
      { t: "/ timeout                              3 matches", c: "waiting" },
      { t: "[api] WARN  upstream timeout (1/3) — retrying", c: "waiting" },
      { t: "[api] WARN  upstream timeout (2/3) — retrying", c: "waiting" },
      { t: "[api] INFO  upstream recovered" },
      { t: "~/Library/Logs/rumor/sessions/myapp-20260627-143000/api.log", c: "info" },
    ],
  },
  {
    id: "raw",
    accent: "running",
    tabs: [],
    active: 0,
    raw: true,
    footer: "--raw   ·   one stream, [name]-prefixed, ANSI-stripped",
    lines: [
      { t: "$ rumor app.json --raw --only api,web", c: "info" },
      { t: "[api] INFO  Application startup complete", c: "running" },
      { t: "[web] listening on http://localhost:54419", c: "running" },
      { t: "[api] GET /api/orders 200  8.4ms" },
      { t: "[web] GET / 200  0.6ms" },
      { t: "[api] GET /api/orders/42 200  3.1ms" },
    ],
  },
];
