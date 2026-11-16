// Writes search.js, the index the site's search reads, and checks every
// link between the pages. Run it after editing a page:
//
//   node docs/site/build-search.mjs
//
// It exits with 1 and names each link that leads nowhere.
import { readFileSync, readdirSync, writeFileSync } from "node:fs";
import { dirname, join, posix } from "node:path";
import { fileURLToPath } from "node:url";

const root = dirname(fileURLToPath(import.meta.url));
const pages = readdirSync(root, { recursive: true })
  .filter((file) => file.endsWith(".html"))
  .map((file) => file.split("\\").join("/"))
  .sort();

const plain = (html) =>
  html
    .replace(/<[^>]+>/g, " ")
    .replace(/&lt;/g, "<")
    .replace(/&gt;/g, ">")
    .replace(/&#124;/g, "|")
    .replace(/&quot;/g, '"')
    .replace(/&amp;/g, "&")
    .replace(/\s+/g, " ")
    .trim();
// Each word once, so a section costs little and any word in it is found.
const words = (html) =>
  [
    ...new Set(
      plain(html)
        .toLowerCase()
        .match(/[a-z0-9_]{3,}/g) ?? [],
    ),
  ].join(" ");

const index = [];
const ids = new Map();
const links = [];
for (const page of pages) {
  const html = readFileSync(join(root, page), "utf8");
  ids.set(
    page,
    new Set([...html.matchAll(/\sid="([^"]+)"/g)].map((m) => m[1])),
  );
  for (const [, href] of html.matchAll(/\shref="([^"]+)"/g)) {
    links.push([page, href]);
  }

  const body = html.slice(html.indexOf("<main"), html.indexOf("</main>"));
  const title = plain(body.match(/<h1[^>]*>([\s\S]*?)<\/h1>/)?.[1] ?? page);
  const heads = [
    ...body.matchAll(/<h([23]) id="([^"]+)"[^>]*>([\s\S]*?)<\/h\1>/g),
  ];
  const first = heads[0]?.index ?? body.length;
  const sections = [["", title, words(body.slice(0, first)), ""]];
  heads.forEach((head, at) => {
    const end = heads[at + 1]?.index ?? body.length;
    const text = body.slice(head.index + head[0].length, end);
    // What a section's tables define, in their first column, ranks it higher.
    const defined = [...text.matchAll(/<tr>\s*<td>([\s\S]*?)<\/td>/g)]
      .map((row) => row[1])
      .join(" ");
    sections.push([head[2], plain(head[3]), words(text), words(defined)]);
  });
  index.push({ p: page, t: title, s: sections });
}

let broken = 0;
for (const [page, href] of links) {
  if (/^(https?:|mailto:)/.test(href)) continue;
  const [path, id] = href.split("#");
  const target = path ? posix.normalize(posix.join(dirname(page), path)) : page;
  if (/\.html$/.test(target) || !path) {
    if (!ids.has(target)) {
      console.error(`${page}: ${href} names a page that doesn't exist`);
      broken++;
    } else if (id && !ids.get(target).has(id)) {
      console.error(`${page}: ${href} names an id that ${target} doesn't have`);
      broken++;
    }
  }
}

// The bar and the crab draw the shell from a copy of its path in site.js.
// Hold that copy to the file.
const mark = readFileSync(join(root, "logo.svg"), "utf8").match(
  /class="shell" d="([^"]+)"/,
)?.[1];
if (!mark || !readFileSync(join(root, "site.js"), "utf8").includes(mark)) {
  console.error("site.js: MARK is no longer the shell's path in logo.svg");
  broken++;
}

// One string in the shape prettier prints, so formatting the site leaves the file alone.
const json = JSON.stringify(index).replace(/\\/g, "\\\\").replace(/'/g, "\\'");
writeFileSync(
  join(root, "search.js"),
  `// Made by build-search.mjs. Don't edit it by hand.\nwindow.CRABSHELL_SEARCH = JSON.parse(\n  '${json}',\n);\n`,
);
console.log(
  `search.js: ${index.length} pages, ${index.reduce((n, page) => n + page.s.length, 0)} sections`,
);
process.exit(broken ? 1 : 0);
