// Builds what every page shares: the bar, the docs menu, the table of
// contents, code colors and search. Pages work without it; they just lose
// the navigation. The crab that lives in the shell is at the end.
(() => {
  const root = new URL(".", document.currentScript.src);
  const REPO = "https://github.com/w0x7y/crabshell";
  const CRATE = "https://crates.io/crates/crabshell";
  // The shell's path in logo.svg, so the page can draw it in its own color.
  // Each "M x y h w" is one row of pixels. build-search.mjs checks it against
  // the file.
  const MARK =
    "M15 0h8v1h-8zM12 1h14v1h-14zM10 2h18v1h-18zM8 3h22v1h-22zM7 4h24v1h-24zM6 5h26v1h-26zM5 6h28v1h-28zM4 7h30v1h-30zM3 8h15v1h-15zM25 8h10v1h-10zM3 9h12v1h-12zM27 9h8v1h-8zM2 10h12v1h-12zM28 10h8v1h-8zM2 11h11v1h-11zM18 11h6v1h-6zM29 11h7v1h-7zM1 12h11v1h-11zM16 12h10v1h-10zM30 12h7v1h-7zM1 13h11v1h-11zM15 13h12v1h-12zM31 13h6v1h-6zM1 14h11v1h-11zM15 14h13v1h-13zM32 14h5v1h-5zM0 15h11v1h-11zM14 15h15v1h-15zM32 15h5v1h-5zM0 16h11v1h-11zM14 16h15v1h-15zM33 16h5v1h-5zM0 17h11v1h-11zM14 17h16v1h-16zM33 17h5v1h-5zM0 18h11v1h-11zM14 18h16v1h-16zM33 18h5v1h-5zM1 19h11v1h-11zM15 19h3v1h-3zM21 19h9v1h-9zM33 19h5v1h-5zM1 20h11v1h-11zM17 20h1v1h-1zM21 20h9v1h-9zM33 20h5v1h-5zM1 21h12v1h-12zM21 21h9v1h-9zM33 21h5v1h-5zM2 22h12v1h-12zM20 22h10v1h-10zM33 22h4v1h-4zM2 23h14v1h-14zM18 23h12v1h-12zM33 23h4v1h-4zM2 24h28v1h-28zM33 24h4v1h-4zM3 25h27v1h-27zM33 25h4v1h-4zM3 26h26v1h-26zM32 26h4v1h-4zM4 27h25v1h-25zM32 27h4v1h-4zM5 28h23v1h-23zM31 28h4v1h-4zM6 29h21v1h-21zM31 29h4v1h-4zM7 30h19v1h-19zM30 30h4v1h-4zM9 31h16v1h-16zM29 31h4v1h-4zM11 32h12v1h-12zM28 32h4v1h-4zM14 33h6v1h-6zM27 33h4v1h-4zM25 34h5v1h-5zM23 35h5v1h-5zM20 36h6v1h-6zM16 37h6v1h-6z";

  // The crab that lives in the shell, on the same grid. Each part is rows of
  // pixels, as [x, y, width, height]. logo.svg is this drawing with the crab
  // out, and the bar's logo and the crab itself are built from it below.
  const ART = {
    body: [
      [-7, 26, 11],
      [-10, 27, 17],
      [-11, 28, 20],
      [-11, 29, 21],
      [-11, 30, 22],
      [-11, 31, 23],
      [-10, 32, 24],
      [-8, 33, 24],
    ],
    door: [
      [5, 33, 5],
      [4, 34, 7, 3],
      [5, 37, 5],
    ],
    doorLine: [[4, 35, 4]],
    big: [
      [-19, 24, 5],
      [-20, 25, 7],
      [-18, 26, 5],
      [-20, 27, 7],
      [-19, 28, 5],
      [-13, 26, 3, 2],
    ],
    small: [
      [-14, 31, 3],
      [-13, 32, 2],
      [-14, 33, 3],
    ],
    stalks: [
      [-10, 21, 2, 6],
      [-3, 24, 2, 2],
    ],
    stalksUp: [
      [-10, 18, 2, 9],
      [-3, 21, 2, 5],
    ],
    bulbs: [
      [-11, 15, 4],
      [-12, 16, 6, 4],
      [-11, 20, 4],
      [-4, 18, 4],
      [-5, 19, 6, 4],
      [-4, 23, 4],
    ],
    whites: [
      [-11, 16, 4, 4],
      [-4, 19, 4, 4],
    ],
    pupils: [
      [-10, 17, 2, 2],
      [-3, 20, 2, 2],
    ],
    shut: [
      [-11, 18, 4],
      [-4, 21, 4],
    ],
    bang: [
      [-17, 2, 2, 6],
      [-17, 10, 2, 2],
    ],
    legsA: [
      [-7, 34, 2],
      [-9, 35, 2],
      [-10, 36, 2, 2],
      [-2, 34, 2],
      [-3, 35, 2],
      [-4, 36, 2, 2],
      [3, 34, 2],
      [4, 35, 2],
      [5, 36, 2, 2],
      [9, 34, 2],
      [10, 35, 2],
      [11, 36, 2, 2],
    ],
    legsB: [
      [-7, 34, 2],
      [-8, 35, 2, 3],
      [-2, 34, 2, 2],
      [-1, 36, 2, 2],
      [3, 34, 2, 4],
      [9, 34, 2, 2],
      [8, 36, 2, 2],
    ],
  };
  const rows = (rects) =>
    rects.map(([x, y, w, h = 1]) => `M${x} ${y}h${w}v${h}h-${w}z`).join("");
  const part = (name, fill, rects) =>
    `<path class="part p-${name} c-${fill}" d="${rows(rects)}"/>`;
  // Every part is in the picture, and the stylesheet shows the ones a pose has.
  const crabArt = (pose) => `
    <svg class="crab-art" viewBox="-21 -1 60 39" shape-rendering="crispEdges" aria-hidden="true" data-pose="${pose}" data-legs="a">
      <g class="lift">
        ${part("body", "crab", ART.body)}
        <path class="c-shell" d="${MARK}"/>
        ${part("door", "crab", ART.door)}${part("door", "cut", ART.doorLine)}
        ${part("big", "crab", ART.big)}${part("small", "crab", ART.small)}
        ${part("stalks", "crab", ART.stalks)}${part("stalks-up", "crab", ART.stalksUp)}
        <g class="part p-eyes">
          <path class="c-crab" d="${rows(ART.bulbs)}"/>
          <g class="open">
            <path class="c-white" d="${rows(ART.whites)}"/>
            <path class="c-pupil" d="${rows(ART.pupils)}"/>
          </g>
          <path class="c-cut shut" d="${rows(ART.shut)}"/>
        </g>
        ${part("bang", "ink", ART.bang)}
      </g>
      <g class="part p-legs c-crab">
        <path class="legs-a" d="${rows(ART.legsA)}"/>
        <path class="legs-b" d="${rows(ART.legsB)}"/>
      </g>
    </svg>`;

  const SITE = [
    ["index.html", "Home"],
    ["docs/index.html", "Docs"],
  ];
  const DOCS = [
    [
      "Start",
      [
        ["docs/index.html", "Overview"],
        ["docs/install.html", "Install and run"],
        ["docs/first-bar.html", "Your first bar"],
        ["docs/examples.html", "Examples"],
      ],
    ],
    [
      "The toolkit",
      [
        ["docs/reactivity.html", "Signals and effects"],
        ["docs/windows.html", "Windows"],
        ["docs/widgets.html", "Widgets"],
        ["docs/layout.html", "Layout"],
        ["docs/styles.html", "Styles"],
        ["docs/input.html", "Pointer and keyboard"],
        ["docs/motion.html", "Timers and motion"],
        ["docs/drawing.html", "Drawing and images"],
      ],
    ],
    [
      "The desktop",
      [
        ["docs/services.html", "Desktop services"],
        ["docs/compositors.html", "Compositors and windows"],
        ["docs/system.html", "Programs, files and IPC"],
        ["docs/lock.html", "Lock screen and logins"],
      ],
    ],
    [
      "Running it",
      [
        ["docs/cli.html", "The crabshell command"],
        ["docs/troubleshooting.html", "When something breaks"],
        ["docs/architecture.html", "How it works"],
      ],
    ],
  ];

  const url = (path) => new URL(path, root).href;
  const here = location.pathname.replace(/\/$/, "/index.html");
  const isHere = (path) => new URL(path, root).pathname === here;
  const inDocs = DOCS.some(([, pages]) => pages.some(([path]) => isHere(path)));
  const esc = (text) =>
    text.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
  const el = (html) => {
    const t = document.createElement("template");
    t.innerHTML = html.trim();
    return t.content.firstElementChild;
  };
  const link = ([path, title]) =>
    `<a href="${url(path)}"${isHere(path) ? ' aria-current="page"' : ""}>${title}</a>`;

  // The bar.
  let bar = document.querySelector(".bar");
  if (!bar) document.body.prepend((bar = el('<header class="bar"></header>')));
  const workspaces = SITE.map(([path, title]) => {
    const current = isHere(path) || (inDocs && path === "docs/index.html");
    return `<a class="ws" href="${url(path)}"${current ? ' aria-current="page"' : ""}>${title}</a>`;
  }).join("");
  bar.innerHTML = `
    <a class="brand" href="${url("index.html")}">${crabArt("out")}crabshell</a>
    <nav class="workspaces" aria-label="Site">${workspaces}</nav>
    <button class="menu-button" type="button" aria-expanded="false" aria-controls="menu">Menu</button>
    <span class="spacer"></span>
    <button class="search-button" type="button">Search <kbd class="hint">/</kbd></button>
    <a class="extra" href="${REPO}">GitHub</a>
    <a class="extra" href="${CRATE}">crates.io</a>`;
  document.body.prepend(
    el('<a class="skip" href="#content">Skip to the content</a>'),
  );
  const main = document.querySelector("main");
  if (main && !main.id) main.id = "content";

  // The theme. A choice is kept in localStorage. Until there is one, the
  // page follows the system, which the stylesheet does without this script.
  // The switch is a small panel on the screen's right edge.
  const system = matchMedia("(prefers-color-scheme: dark)");
  const themeNow = () =>
    document.documentElement.dataset.theme ||
    (system.matches ? "dark" : "light");
  const themeSwitch = el(
    `<button class="theme-switch" type="button" role="switch" aria-label="Dark theme">
      <svg class="sun" viewBox="0 0 9 9" shape-rendering="crispEdges" aria-hidden="true"><path d="M4 0h1v1h-1zM1 1h1v1h-1zM7 1h1v1h-1zM3 3h3v3h-3zM0 4h1v1h-1zM8 4h1v1h-1zM1 7h1v1h-1zM7 7h1v1h-1zM4 8h1v1h-1z"/></svg>
      <svg class="moon" viewBox="0 0 9 9" shape-rendering="crispEdges" aria-hidden="true"><path d="M3 0h2v1h-2zM2 1h2v1h-2zM1 2h2v1h-2zM0 3h3v1h-3zM0 4h3v1h-3zM0 5h4v1h-4zM1 6h4v1h-4zM2 7h5v1h-5zM3 8h3v1h-3z"/></svg>
    </button>`,
  );
  const nameTheme = () => {
    const dark = themeNow() === "dark";
    themeSwitch.setAttribute("aria-checked", String(dark));
    themeSwitch.title = `Switch to the ${dark ? "light" : "dark"} theme`;
  };
  themeSwitch.addEventListener("click", () => {
    const next = themeNow() === "dark" ? "light" : "dark";
    document.documentElement.dataset.theme = next;
    try {
      localStorage.setItem("crabshell:theme", next);
    } catch {
      // Without storage the choice lasts for this page.
    }
    nameTheme();
  });
  // With no choice made, the switch follows the system as the page does.
  system.addEventListener("change", nameTheme);
  nameTheme();
  bar.after(themeSwitch);

  // The menu: the site's pages, and the docs tree inside the docs.
  const groups = inDocs
    ? DOCS.map(
        ([name, pages]) =>
          `<h2>${name}</h2><ul>${pages.map((p) => `<li>${link(p)}</li>`).join("")}</ul>`,
      ).join("")
    : "";
  const menu = el(
    `<nav class="menu" id="menu" aria-label="${inDocs ? "Docs" : "Site"}">
      <ul class="site-links">${SITE.map((p) => `<li>${link(p)}</li>`).join("")}<li><a href="${REPO}">GitHub</a></li><li><a href="${CRATE}">crates.io</a></li></ul>
      ${groups}
    </nav>`,
  );
  (inDocs && main ? main : document.body).prepend(menu);
  const menuButton = bar.querySelector(".menu-button");
  const setMenu = (open) => {
    document.body.classList.toggle("menu-open", open);
    menuButton.setAttribute("aria-expanded", String(open));
  };
  menuButton.addEventListener("click", () =>
    setMenu(!document.body.classList.contains("menu-open")),
  );
  menu.addEventListener("click", (event) => {
    if (event.target.closest("a")) setMenu(false);
  });

  // Docs pages: anchors, the table of contents, previous and next.
  const article = document.querySelector("article");
  if (article) {
    const heads = [...article.querySelectorAll("h2[id], h3[id]")];
    for (const head of heads) {
      head.append(
        el(
          `<a class="anchor" href="#${head.id}" aria-label="Link to this section">#</a>`,
        ),
      );
    }
    for (const table of article.querySelectorAll("table")) {
      const box = el('<div class="scroll" tabindex="0"></div>');
      table.replaceWith(box);
      box.append(table);
    }
    const tocHeads = heads.filter((head) => head.tagName === "H2");
    if (inDocs && tocHeads.length > 1) {
      const toc = el(
        `<aside class="toc" aria-label="On this page"><h2>On this page</h2><ul>${tocHeads
          .map(
            (head) =>
              `<li><a href="#${head.id}">${esc(head.firstChild.textContent)}</a></li>`,
          )
          .join("")}</ul></aside>`,
      );
      main.append(toc);
      const links = new Map(
        [...toc.querySelectorAll("a")].map((a) => [a.hash.slice(1), a]),
      );
      // The section being read is the last one whose heading has passed
      // the top of the page. At the very bottom it is the last section,
      // whose heading may never get that far up.
      const line =
        parseFloat(
          getComputedStyle(document.documentElement).scrollPaddingTop,
        ) + 8;
      const spy = () => {
        const page = document.documentElement;
        const end =
          scrollY > 0 && innerHeight + scrollY >= page.scrollHeight - 2;
        const passed = tocHeads.filter(
          (head) => head.getBoundingClientRect().top <= line,
        );
        const now = end ? tocHeads.at(-1) : (passed.at(-1) ?? tocHeads[0]);
        for (const [id, a] of links) a.classList.toggle("here", id === now.id);
      };
      addEventListener("scroll", spy, { passive: true });
      addEventListener("resize", spy);
      spy();
    }
    if (inDocs) {
      const flat = DOCS.flatMap(([, pages]) => pages);
      const at = flat.findIndex(([path]) => isHere(path));
      const side = (page, cls, label) =>
        page
          ? `<a class="${cls}" href="${url(page[0])}"><span>${label}</span>${page[1]}</a>`
          : "";
      article.append(
        el(
          `<nav class="pager" aria-label="Previous and next page">${side(flat[at - 1], "prev", "Previous")}${side(flat[at + 1], "next", "Next")}</nav>`,
        ),
      );
    }
  }

  document.body.append(
    el(
      `<footer class="foot"><div class="wrap">
        <p class="grow">Crabshell is licensed MIT or Apache-2.0, your choice.</p>
        <p><a href="${REPO}">Source on GitHub</a></p>
        <p><a href="${CRATE}">crates.io</a></p>
      </div></footer>`,
    ),
  );

  // Code colors, by the roles of the rime theme (w0x7y.dev/rime/docs/syntax).
  // Each rule is a pattern and the theme's color and style for it, and the
  // first rule that matches at a place wins.
  const RUST = [
    [/\/\/.*$/, "comment i"],
    [/"(?:\\.|[^"\\])*"|'(?:\\.|[^'\\])'/, "green"],
    [/#!?\[[^\]\n]*\]/, "cyan"],
    [/'[a-z_]+\b/, "pink i"],
    [/\b(?:if|else|match)\b/, "red b"],
    [/\breturn\b/, "red"],
    [/\b(?:for|while|loop|in|break|continue)\b/, "yellow"],
    [
      /\b(?:as|async|await|const|crate|dyn|enum|fn|impl|let|mod|move|mut|pub|ref|static|struct|super|trait|type|unsafe|use|where)\b/,
      "violet",
    ],
    [/\b(?:true|false)\b/, "pink"],
    [
      /\b(?:self|Self|Some|None|Ok|Err|bool|char|str|u8|u16|u32|u64|i8|i16|i32|i64|f32|f64|usize|isize)\b/,
      "cyan",
    ],
    [/\b[a-z_][a-z0-9_]*!(?!=)/, "cyan"],
    [
      /\b0x[0-9a-fA-F_]+\b|\b\d[\d_]*(?:\.\d+)?(?:_?[iuf](?:8|16|32|64|size))?\b/,
      "pink",
    ],
    [/\b[A-Z][A-Z0-9_]+\b/, "pink"],
    [/\b[A-Z][A-Za-z0-9]*\b/, "yellow"],
    [/\b[a-z_][a-z0-9_]*(?=::)/, "yellow"],
    [/\b[a-z_][a-z0-9_]*(?=\()/, "blue"],
    [/->|=>|&&|\.\.=?|[=+\-*/!&?%^]+/, "muted"],
    [/[()[\]{}<>,;:.|]/, "steel"],
  ];
  const SHELL = [
    [/(?:^|(?<=\s))#.*$/, "comment i"],
    [/"(?:\\.|[^"\\])*"|'[^']*'/, "green"],
    [/^\$(?= )/, "steel"],
    [/(?<=^\$ (?:[A-Z_]+=\S+ )*)[\w./~-]+/, "blue"],
    [/&&|\|\||[|>]/, "muted"],
  ];
  const TOML = [
    [/#.*$/, "comment i"],
    [/(?<=^\[+)[^\]\n]+(?=\]+\s*$)/, "red"],
    [/"(?:\\.|[^"\\])*"/, "green"],
    [/^\s*[\w.-]+(?=\s*=)/, "red"],
    [/\b(?:true|false)\b|\b\d[\d_.]*\b/, "pink"],
    [/=/, "muted"],
    [/[[\]{},.]/, "steel"],
  ];
  // A block with no language, such as a compositor's config: only comments.
  const PLAIN = [[/^\s*#.*$/, "comment i"]];
  const paint = (code, rules) => {
    const pattern = new RegExp(
      rules.map(([rule]) => `(${rule.source})`).join("|"),
      "gm",
    );
    let out = "";
    let last = 0;
    for (const match of code.matchAll(pattern)) {
      const [, look] = rules[match.slice(1).findIndex((part) => part != null)];
      const classes = look.split(" ").map((name) => `r-${name}`);
      // Inside a string, an escape takes its own color.
      const text =
        look === "green"
          ? esc(match[0]).replace(/\\./g, '<span class="r-cyan">$&</span>')
          : esc(match[0]);
      out += esc(code.slice(last, match.index));
      out += `<span class="${classes.join(" ")}">${text}</span>`;
      last = match.index + match[0].length;
    }
    return out + esc(code.slice(last));
  };
  for (const code of document.querySelectorAll("pre > code")) {
    const rules = { rs: RUST, sh: SHELL, toml: TOML }[code.className] ?? PLAIN;
    code.innerHTML = paint(code.textContent, rules);
  }
  if (navigator.clipboard) {
    for (const pre of document.querySelectorAll("pre:not(.term)")) {
      const button = el('<button class="copy" type="button">Copy</button>');
      button.addEventListener("click", async () => {
        const text = pre.querySelector("code")?.textContent ?? "";
        await navigator.clipboard.writeText(text.replace(/^\$ /gm, ""));
        button.textContent = "Copied";
        setTimeout(() => (button.textContent = "Copy"), 1500);
      });
      pre.append(button);
    }
  }

  // Search. The index is search.js, made by build-search.mjs.
  const dialog = el(
    `<dialog class="search" aria-label="Search the docs">
      <input type="search" placeholder="Search the docs" aria-label="Search the docs" autocomplete="off" spellcheck="false">
      <ol role="listbox" aria-label="Results"></ol>
    </dialog>`,
  );
  document.body.append(dialog);
  const field = dialog.querySelector("input");
  const results = dialog.querySelector("ol");
  let entries = null;
  let picked = 0;
  const loadIndex = () =>
    new Promise((done) => {
      if (entries) return done();
      const script = document.createElement("script");
      script.src = url("search.js");
      script.onload = script.onerror = () => {
        entries = (window.CRABSHELL_SEARCH ?? []).flatMap((page) =>
          page.s.map(([id, heading, words, defined]) => ({
            href: url(page.p) + (id ? `#${id}` : ""),
            page: page.t,
            heading,
            hay: `${heading} ${page.t} ${words}`.toLowerCase(),
            head: heading.toLowerCase(),
            defined,
            key: id.replace(/[^a-z0-9]/g, ""),
          })),
        );
        done();
      };
      document.head.append(script);
    });
  const pick = (index) => {
    const items = [...results.children];
    picked = Math.max(0, Math.min(index, items.length - 1));
    items.forEach((item, at) =>
      item.setAttribute("aria-selected", String(at === picked)),
    );
    items[picked]?.scrollIntoView({ block: "nearest" });
  };
  const find = () => {
    const words = field.value.toLowerCase().split(/\s+/).filter(Boolean);
    const found = entries
      .filter((entry) => words.every((word) => entry.hay.includes(word)))
      .map((entry) => ({
        entry,
        // A section named for the word comes first, then a heading that
        // has it, then a table that defines it.
        score: words.reduce(
          (sum, word) =>
            sum +
            (entry.key.includes(word.replace(/[^a-z0-9]/g, "")) ? 5 : 0) +
            (entry.head.includes(word) ? 4 : 0) +
            (entry.defined.includes(word) ? 2 : 0),
          0,
        ),
      }))
      .sort((a, b) => b.score - a.score)
      .slice(0, 30);
    results.innerHTML = found
      .map(
        ({ entry }) =>
          `<li role="option"><a href="${entry.href}">${esc(entry.heading)}<span>${esc(entry.page)}</span></a></li>`,
      )
      .join("");
    results.nextElementSibling?.remove();
    if (!found.length) {
      results.after(
        el(
          `<p class="empty">${words.length ? "Nothing matches. Try a method name, such as per_output." : "The index didn't load. Run node build-search.mjs in docs/site."}</p>`,
        ),
      );
    }
    pick(0);
  };
  const openSearch = async () => {
    await loadIndex();
    field.value = "";
    find();
    dialog.showModal();
  };
  bar.querySelector(".search-button").addEventListener("click", openSearch);
  field.addEventListener("input", find);
  field.addEventListener("keydown", (event) => {
    if (event.key === "ArrowDown") pick(picked + 1);
    else if (event.key === "ArrowUp") pick(picked - 1);
    else if (event.key === "Enter")
      results.children[picked]?.querySelector("a").click();
    else return;
    event.preventDefault();
  });
  results.addEventListener("click", (event) => {
    if (event.target.closest("a")) dialog.close();
  });
  dialog.addEventListener("click", (event) => {
    if (event.target === dialog) dialog.close();
  });
  document.addEventListener("keydown", (event) => {
    const typing = /^(INPUT|TEXTAREA|SELECT)$/.test(event.target.tagName);
    const slash = event.key === "/" && !typing;
    const k = event.key === "k" && (event.ctrlKey || event.metaKey);
    if ((slash || k) && !dialog.open) {
      event.preventDefault();
      openSearch();
    } else if (event.key === "Escape") {
      setMenu(false);
    }
  });

  // The crab. It lives in its shell, large on the home page and small in the
  // docs menu, and it acts out two of Crabshell's rules. It is out
  // while there is input and goes in three seconds after the last of it, and
  // the page stops with it. Poke it too often and its click handler panics:
  // a notification shows the log line, and the handler stays disabled until
  // the notification is dismissed. Everything else carries on meanwhile.
  const host =
    document.querySelector("#crab") ??
    (inDocs ? menu.appendChild(el('<div class="crab"></div>')) : null);
  if (!host) return;
  const home = host.id === "crab";
  const where = `PanelWindow[DP-1]/${home ? "Hero" : "Sidebar"}/Crab`;
  host.innerHTML = `
    <div class="crab-run"><button class="crab-poke" type="button" aria-label="Poke the hermit crab">${crabArt("in")}</button></div>
    <div class="crab-notes" role="status"></div>`;
  const run = host.querySelector(".crab-run");
  const poke = host.querySelector(".crab-poke");
  const art = host.querySelector(".crab-art");
  const notes = host.querySelector(".crab-notes");
  const calm = matchMedia("(prefers-reduced-motion: reduce)");
  const IDLE = 3000;

  let state = "asleep"; // or "awake", or "dozing" while the crab goes in
  let steps = []; // the poses still to show, as [pose, milliseconds]
  let stepEnds = 0;
  let after = null;
  let looping = false;
  let at = 0; // cells walked from the right end of the ground, 0 or less
  let goal = 0;
  let strideAt = 0;
  let lastInput = 0;
  let nap = 0;
  let blink = 0;
  let pokes = 0;
  let poked = 0;
  let handler = true;

  // One loop shows the poses and takes the steps, and it asks for a frame
  // only while one of the two is left to do.
  const frame = (now) => {
    looping = false;
    if (now >= stepEnds) {
      if (steps.length) {
        const [name, ms] = steps.shift();
        art.dataset.pose = name;
        stepEnds = now + ms;
      } else if (after) {
        const then = after;
        after = null;
        then();
      }
    }
    const walking = state === "awake" && at !== goal;
    if (walking && now >= strideAt && /out|blink/.test(art.dataset.pose)) {
      at += Math.sign(goal - at);
      strideAt = now + 60;
      // The legs swap on each cell, and stand still on the last one.
      art.dataset.legs = at !== goal && art.dataset.legs === "a" ? "b" : "a";
      run.style.setProperty("--at", at);
    }
    if (steps.length || after || (state === "awake" && at !== goal)) kick();
  };
  const kick = () => {
    if (looping) return;
    looping = true;
    requestAnimationFrame(frame);
  };
  // With reduced motion the in-between poses are left out: each change is a cut.
  const play = (list, then = null) => {
    steps = calm.matches
      ? list.filter(([, ms], i) => ms >= 150 || i === list.length - 1)
      : list.slice();
    after = then;
    stepEnds = 0;
    kick();
  };

  const settle = () => {
    // Whatever still runs is finished, or paused if it has no end.
    for (const animation of document.getAnimations()) {
      if (animation.playState !== "running") continue;
      if (Number.isFinite(animation.effect?.getComputedTiming().endTime)) {
        animation.finish();
      } else {
        animation.pause();
      }
    }
    state = "asleep";
  };
  const blinkSoon = () => {
    clearTimeout(blink);
    if (calm.matches) return;
    blink = setTimeout(
      () => {
        if (state !== "awake") return;
        if (!steps.length && art.dataset.pose === "out") {
          play([
            ["blink", 110],
            ["out", 0],
          ]);
        }
        blinkSoon();
      },
      1200 + Math.random() * 1600,
    );
  };
  const OUT = ["in", "peek1", "peek2", "out"];
  const wake = () => {
    if (state === "awake") return;
    state = "awake";
    // From wherever it got to on its way in.
    play(
      OUT.slice(OUT.indexOf(art.dataset.pose) + 1).map((name) => [name, 80]),
    );
    blinkSoon();
  };
  const sleep = () => {
    state = "dozing";
    clearTimeout(blink);
    art.dataset.legs = "a";
    play(
      [
        ["peek2", 90],
        ["peek1", 90],
        ["in", 0],
      ],
      settle,
    );
  };
  const doze = () => {
    const left = IDLE - (performance.now() - lastInput);
    nap = left > 0 ? setTimeout(doze, left) : 0;
    if (!nap) sleep();
  };
  // The eyes turn to the pointer, and the crab walks to it.
  const follow = ({ clientX, clientY }) => {
    const box = art.getBoundingClientRect();
    const cell = box.width / 60;
    const dx = clientX - (box.left + 15 * cell);
    const dy = clientY - (box.top + 20 * cell);
    const far = Math.hypot(dx, dy) || 1;
    art.style.setProperty("--lx", Math.round(dx / far));
    art.style.setProperty("--ly", Math.round(dy / far));
    if (calm.matches || Math.abs(dx) < 3 * cell) return;
    const room = Math.floor((run.clientWidth - box.width) / cell);
    goal = Math.max(-room, Math.min(0, at + Math.round(dx / cell)));
    if (goal !== at) kick();
  };
  const input = (event) => {
    lastInput = performance.now();
    if (!nap) nap = setTimeout(doze, IDLE);
    wake();
    if (event.clientX !== undefined) follow(event);
  };
  for (const type of [
    "pointermove",
    "pointerdown",
    "keydown",
    "wheel",
    "scroll",
    "touchstart",
  ]) {
    addEventListener(type, input, { passive: true, capture: true });
  }
  addEventListener("resize", () => {
    at = goal = 0;
    run.style.removeProperty("--at");
  });

  // Dismissing the notification is what a fix and a reload are to a shell.
  const dismiss = () => {
    const note = notes.firstElementChild;
    if (!note) return;
    const focused = note === document.activeElement;
    note.remove();
    handler = true;
    pokes = 0;
    poke.setAttribute("aria-label", "Poke the hermit crab");
    if (focused) poke.focus();
  };
  addEventListener("keydown", (event) => {
    if (event.key === "Escape") dismiss();
  });
  poke.addEventListener("click", (event) => {
    input(event);
    if (!handler) return;
    const now = performance.now();
    pokes = now - poked < 700 ? pokes + 1 : 1;
    poked = now;
    if (pokes < 6) {
      play([
        ["flinch", 160],
        ["out", 0],
      ]);
      return;
    }
    // The handler panicked. As in a real shell, it is logged with its place
    // in the tree and not called again, and the rest keeps running. Here the
    // log line is a notification, and dismissing it brings the handler back.
    handler = false;
    poke.setAttribute(
      "aria-label",
      "The hermit crab. Its click handler panicked and is disabled.",
    );
    const note = el(
      `<button class="note" type="button">
        <span class="note-head"><strong>crabshell</strong>${new Date().toTimeString().slice(0, 5)}</span>
        <span class="note-body"><span class="r-red r-b">ERROR</span> handler <span class="note-path">${where}.on_click</span> panicked and is disabled: too many pokes</span>
        <span class="note-hint">Dismiss this to poke the crab again</span>
      </button>`,
    );
    note.addEventListener("click", dismiss);
    notes.replaceChildren(note);
    play([
      ["panic-a", 70],
      ["panic-b", 70],
      ["panic-a", 70],
      ["panic-b", 70],
      ["panic-a", 70],
      ["panic-b", 70],
      ["panic", 450],
      ["out", 0],
    ]);
  });

  // The home page's crab comes out as the page loads. The small one in the
  // docs starts asleep.
  if (home) input({});
})();
