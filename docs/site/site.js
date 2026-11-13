// What every page shares: the dark / light switch, the marks, and a tooltip.

const cs = (() => {
  const root = document.documentElement;

  // The switch follows the system until someone picks, then remembers the pick, as rime's site does.
  const current = () =>
    root.dataset.theme ||
    (matchMedia("(prefers-color-scheme: light)").matches ? "light" : "dark");
  const buttons = document.querySelectorAll("[data-theme-set]");
  const markButtons = () =>
    buttons.forEach((b) =>
      b.setAttribute("aria-pressed", String(b.dataset.themeSet === current())),
    );
  buttons.forEach((b) => {
    b.addEventListener("click", () => {
      root.dataset.theme = b.dataset.themeSet;
      try {
        localStorage.setItem("crabshell:theme", b.dataset.themeSet);
      } catch (e) {}
      markButtons();
    });
  });
  markButtons();

  const ICON = {
    yes: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="3.4" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M5 12l4 4L19 6"/></svg>',
    part: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="3.6" stroke-linecap="round" aria-hidden="true"><path d="M6 12h12"/></svg>',
    no: "",
    fail: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="3.4" stroke-linecap="round" aria-hidden="true"><path d="m7 7 10 10M7 17 17 7"/></svg>',
    na: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="3" stroke-linecap="round" aria-hidden="true"><path d="M8 12h8"/></svg>',
  };
  const MARK_LABEL = {
    yes: "built",
    part: "partly built",
    no: "not built yet",
    fail: "tried and failed",
    na: "rust covers it",
  };
  const mark = (s) =>
    `<span class="mark ${s}" role="img" aria-label="${MARK_LABEL[s]}">${ICON[s]}</span>`;
  document.querySelectorAll("[data-mark]").forEach((el) => {
    el.outerHTML = mark(el.dataset.mark);
  });

  const esc = (s) =>
    String(s)
      .replace(/&/g, "&amp;")
      .replace(/</g, "&lt;")
      .replace(/>/g, "&gt;")
      .replace(/"/g, "&quot;");

  // A small highlighter for the code blocks, in rime's roles: keywords violet, functions blue,
  // types yellow, strings green, numbers cyan, punctuation steel, comments gray.
  const SYNTAX = {
    rust: [
      /(\/\/.*)|("(?:[^"\\]|\\.)*")|\b(use|fn|let|move|match|if|else|const|pub|mut|for|in|impl|struct|true|false|Some|None|Ok|Err)\b|\b(\d[\w.]*)\b|\b([A-Z]\w*)\b|\b([a-z_]\w*)(?=\s*(?:\(|::<|!))|([{}()[\]<>;:,.|&*=+-]+)/g,
      ["c", "s", "k", "n", "t", "f", "p"],
    ],
    toml: [
      /(#.*)|("(?:[^"\\]|\\.)*")|^(\[.*\])$|^([\w."*-]+)(?=\s*=)|\b(\d[\w.]*)\b/gm,
      ["c", "s", "k", "f", "n"],
    ],
    sh: [/^(\$ )|(#.*)/gm, ["pr", "c"]],
  };
  document.querySelectorAll("pre.code[data-lang]").forEach((pre) => {
    const rule = SYNTAX[pre.dataset.lang];
    if (!rule) return;
    const [pattern, classes] = rule;
    const text = pre.textContent;
    let html = "";
    let last = 0;
    text.replace(pattern, (match, ...rest) => {
      const offset = rest[rest.length - 2];
      const which = rest
        .slice(0, classes.length)
        .findIndex((g) => g !== undefined);
      html +=
        esc(text.slice(last, offset)) +
        `<span class="${classes[which]}">${esc(match)}</span>`;
      last = offset + match.length;
      return match;
    });
    pre.innerHTML = html + esc(text.slice(last));
  });

  const tip = document.createElement("div");
  tip.id = "tip";
  tip.setAttribute("role", "tooltip");
  tip.hidden = true;
  document.body.appendChild(tip);
  function showTip(target, text) {
    tip.textContent = text;
    tip.hidden = false;
    const r = target.getBoundingClientRect();
    const w = tip.offsetWidth;
    tip.style.left =
      Math.min(innerWidth - w - 8, Math.max(8, r.left + r.width / 2 - w / 2)) +
      "px";
    tip.style.top =
      (r.top < 60 ? r.bottom + 8 : r.top - tip.offsetHeight - 8) + "px";
  }
  const hideTip = () => {
    tip.hidden = true;
  };
  function addTip(el, text) {
    el.addEventListener("mouseenter", () => showTip(el, text));
    el.addEventListener("focus", () => showTip(el, text));
    el.addEventListener("mouseleave", hideTip);
    el.addEventListener("blur", hideTip);
  }
  addEventListener("scroll", hideTip, { passive: true });

  return { ICON, MARK_LABEL, mark, esc, showTip, hideTip, addTip };
})();
