// Minimal terminal + code-editor renderer for the sticky scroll demo.
// No dependencies; builds plain DOM so it stays crisp and accessible.
import type { Scene, Tab } from "./scenes.ts";

const tok = (name?: string) => (name ? `var(--${name})` : "var(--text)");

const reduceMotion =
  typeof window !== "undefined" &&
  window.matchMedia("(prefers-reduced-motion: reduce)").matches;

interface CodeTok { t: string; k?: string }

// Lightweight JSON highlighter. ${VAR} refs and ALL-CAPS string values (the
// declared port names) share the "port" class so declaration ↔ reference link.
function tokenizePlain(s: string, out: CodeTok[]) {
  const re = /("(?:[^"\\]|\\.)*")(\s*:)?|([{}\[\],:])|(\d+)|(\s+)|([^"{}\[\],:\s]+)/g;
  let m: RegExpExecArray | null;
  while ((m = re.exec(s))) {
    if (m[1] !== undefined) {
      const inner = m[1].slice(1, -1);
      if (m[2]) {
        out.push({ t: m[1], k: "key" });
        out.push({ t: m[2], k: "punct" });
      } else if (/^[A-Z][A-Z0-9_]*$/.test(inner)) {
        out.push({ t: m[1], k: "port" });
      } else {
        out.push({ t: m[1], k: "str" });
      }
    } else if (m[3] !== undefined) out.push({ t: m[3], k: "punct" });
    else if (m[4] !== undefined) out.push({ t: m[4], k: "num" });
    else out.push({ t: m[0] });
  }
}

function highlight(line: string): CodeTok[] {
  const out: CodeTok[] = [];
  const re = /\$\{[A-Za-z_][A-Za-z0-9_]*\}/g;
  let i = 0;
  let m: RegExpExecArray | null;
  while ((m = re.exec(line))) {
    if (m.index > i) tokenizePlain(line.slice(i, m.index), out);
    out.push({ t: m[0], k: "port" });
    i = m.index + m[0].length;
  }
  if (i < line.length) tokenizePlain(line.slice(i), out);
  return out;
}

export interface Terminal {
  setScene(scene: Scene, opts?: { animate?: boolean }): void;
}

export function initTerminal(root: HTMLElement): Terminal {
  root.classList.add("term");
  root.innerHTML = `
    <div class="term-bar">
      <span class="lights"><i></i><i></i><i></i></span>
      <span class="term-title">rumor · <b data-title>session</b></span>
      <span class="spacer"></span>
      <span class="term-run" data-run><i class="dot running"></i>live</span>
    </div>
    <div class="term-tabs" data-tabs></div>
    <div class="term-body" data-body></div>
    <div class="term-foot">
      <span class="seg-mode" data-mode></span>
      <span class="seg-text" data-foot></span>
      <span class="seg-spacer"></span>
      <span class="seg-clock" data-count></span>
    </div>
  `;

  const tabsEl = root.querySelector<HTMLElement>("[data-tabs]")!;
  const bodyEl = root.querySelector<HTMLElement>("[data-body]")!;
  const footEl = root.querySelector<HTMLElement>("[data-foot]")!;
  const modeEl = root.querySelector<HTMLElement>("[data-mode]")!;
  const countEl = root.querySelector<HTMLElement>("[data-count]")!;
  const titleEl = root.querySelector<HTMLElement>("[data-title]")!;
  const runEl = root.querySelector<HTMLElement>("[data-run]")!;
  let token = 0; // cancels in-flight animations on scene change

  function renderTabs(scene: Scene) {
    tabsEl.style.display = scene.raw ? "none" : "flex";
    tabsEl.innerHTML = "";
    scene.tabs.forEach((tab: Tab, i: number) => {
      const el = document.createElement("span");
      const selected = i === scene.active;
      el.className =
        "term-tab" +
        (selected ? " selected" : "") +
        (selected && scene.focus ? " focus" : "");
      el.innerHTML = `<i class="dot ${tab.status}"></i>${tab.name}`;
      if (!reduceMotion) {
        el.style.opacity = "0";
        el.style.transform = "translateY(-4px)";
        el.style.transition = "opacity .35s ease, transform .35s ease";
        setTimeout(() => {
          el.style.opacity = "1";
          el.style.transform = "none";
        }, 60 * i);
      }
      tabsEl.appendChild(el);
    });
  }

  function renderFooter(scene: Scene) {
    modeEl.textContent = scene.id;
    footEl.textContent = scene.footer;
    const running = scene.tabs.filter((t) => t.status === "running").length;
    countEl.textContent = scene.raw ? "--raw" : `● ${running} up`;
  }

  function renderLines(scene: Scene, animate: boolean, myToken: number) {
    bodyEl.innerHTML = "";
    const add = (i: number) => {
      if (myToken !== token || i >= scene.lines.length) return;
      const line = scene.lines[i];
      const el = document.createElement("div");
      el.className = "term-line";
      el.textContent = line.t;
      el.style.color = tok(line.c);
      if (animate) {
        el.style.opacity = "0";
        el.style.transform = "translateY(3px)";
        el.style.transition = "opacity .28s ease, transform .28s ease";
      }
      bodyEl.appendChild(el);
      if (animate) requestAnimationFrame(() => {
        el.style.opacity = "1";
        el.style.transform = "none";
      });
      if (animate) setTimeout(() => add(i + 1), 150);
      else add(i + 1);
    };
    add(0);
  }

  function renderEditor(scene: Scene, animate: boolean) {
    const ed = scene.editor!;
    titleEl.textContent = ed.file;
    runEl.textContent = "json";
    tabsEl.style.display = "flex";
    tabsEl.innerHTML = `<span class="term-tab file selected"><span class="ft">{ }</span>${ed.file}</span>`;
    modeEl.textContent = "config";
    footEl.textContent = scene.footer;
    countEl.textContent = `${ed.code.length} lines`;

    bodyEl.innerHTML = "";
    const code = document.createElement("div");
    code.className = "code";
    ed.code.forEach((ln, i) => {
      const row = document.createElement("div");
      row.className = "code-line";
      const num = document.createElement("span");
      num.className = "ln";
      num.textContent = String(i + 1);
      const c = document.createElement("span");
      c.className = "code-c";
      for (const t of highlight(ln)) {
        const s = document.createElement("span");
        if (t.k) s.className = "tok-" + t.k;
        s.textContent = t.t;
        c.appendChild(s);
      }
      row.append(num, c);
      code.appendChild(row);
    });
    bodyEl.appendChild(code);
    if (animate) {
      code.style.opacity = "0";
      code.style.transition = "opacity .3s ease";
      requestAnimationFrame(() => { code.style.opacity = "1"; });
    }
  }

  function setScene(scene: Scene, opts: { animate?: boolean } = {}) {
    const animate = (opts.animate ?? true) && !reduceMotion;
    token++;
    const my = token;
    root.dataset.scene = scene.id;
    root.style.setProperty("--scene-accent", tok(scene.accent));
    root.classList.toggle("is-focus", !!scene.focus);
    root.classList.toggle("is-raw", !!scene.raw);
    root.classList.toggle("is-editor", !!scene.editor);

    if (scene.editor) {
      renderEditor(scene, animate);
      return;
    }

    titleEl.textContent = "session";
    runEl.innerHTML = `<i class="dot running"></i>live`;
    renderTabs(scene);
    renderFooter(scene);
    renderLines(scene, animate, my);
  }

  return { setScene };
}
