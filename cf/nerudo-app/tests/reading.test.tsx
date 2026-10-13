import { describe, expect, it } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { Markdown } from "../src/components/Markdown";
import { CodeBlock } from "../src/components/Code";
import { diagramImage, diagramTitle } from "../src/components/Mermaid";
import { codeLanguage, highlightedLines } from "../src/syntax";
import { repositoryLink, rootReadme } from "../src/readme";
import { partsOf, routeUrl, type Route } from "../src/route";

const route: Route = { trunk: "sample", name: { org: "studio", repo: "receipts" }, view: "repo", id: null, key: "share-token", user: "reviewer" };

describe("README discovery and navigation", () => {
  it("opens bare repo links on Code, while explicit graph and old aliases still work", () => {
    expect(partsOf([])).toEqual({ view: "repo", id: null });
    expect(partsOf(["graph"]).view).toBe("graph");
    expect(partsOf(["activity"]).view).toBe("graph");
    expect(routeUrl({ ...route, key: null, user: null })).toBe("/studio/receipts");
    expect(routeUrl({ ...route, view: "graph", key: null, user: null })).toBe("/studio/receipts/graph");
  });
  it("finds a case-insensitive root Markdown README before a text fallback", () => {
    const tree = { kind: "dir" as const, path: "", commit: "abc", at: null, entries: [{ name: "README.txt", dir: false }, { name: "docs/README.md", dir: false }, { name: "readme.md", dir: false }, { name: "README.markdown", dir: true }] };
    expect(rootReadme(tree)).toBe("readme.md");
    expect(rootReadme({ ...tree, entries: [{ name: "src", dir: true }] })).toBeNull();
  });
  it("resolves document links within the repo and retains its access parameters", () => {
    expect(repositoryLink(route, "docs/README.md", "../src/index.ts#L2")).toBe("/studio/receipts/repo/file%3Asrc/index.ts?k=share-token&u=reviewer#L2");
    expect(repositoryLink(route, "README.md", "src/")).toContain("/repo/dir%3Asrc?");
    expect(repositoryLink(route, "README.md", "hello%20world.md")).toContain("hello%20world.md");
    expect(repositoryLink(route, "docs/README.md", "/src/index.ts")).toContain("/repo/file%3Asrc/index.ts?");
    expect(repositoryLink(route, "docs/README.md", "/")).toBe("/studio/receipts?k=share-token&u=reviewer");
    for (const bad of ["../outside.md", "javascript:alert(1)", "//evil.example/", "src%2f..%2fkey", "data:text/html,x", "..%5csecret"]) expect(repositoryLink(route, "README.md", bad)).toBe("");
    expect(repositoryLink(route, "README.md", "https://example.com/docs")).toBe("https://example.com/docs");
  });
});

describe("code reading", () => {
  it("uses file extensions and language aliases without guessing unknown formats", () => {
    expect(codeLanguage(undefined, "src/app.tsx")).toBe("typescript");
    expect(codeLanguage("sh")).toBe("bash");
    expect(codeLanguage(undefined, "nerudo.toml")).toBe("ini");
    expect(codeLanguage("unknown-language")).toBeNull();
  });
  it("preserves source text including multiline comments, tabs, and a trailing newline", () => {
    const source = '/* first\nsecond */\n\tconst value = "<script>";\n';
    const lines = highlightedLines(source, "typescript");
    expect(lines.map((line) => line.map((token) => token.text).join("")).join("\n")).toBe(source);
    expect(lines[1].some((token) => token.className.includes("hljs-comment"))).toBe(true);
    const html = renderToStaticMarkup(<CodeBlock code={source} language="typescript" lineNumbers />);
    expect(html).toContain("hljs-keyword");
    expect(html).not.toContain("<script>");
    expect(html).toContain("&lt;script&gt;");
  });
  it("falls back to readable text for unsupported languages and large code", () => {
    const source = "x".repeat(70_000);
    expect(highlightedLines(source, "typescript")[0][0].className).toBe("");
    expect(highlightedLines("unrecognized <tag>", "unknown-language")[0][0].text).toBe("unrecognized <tag>");
  });
});

describe("natural document rendering", () => {
  it("renders tables, tasks, fences, and stable heading anchors", () => {
    const html = renderToStaticMarkup(<Markdown text={'# Setup\n\n[Jump](#setup)\n\n| Name | Value |\n| --- | --- |\n| runtime | edge |\n\n- [x] Install\n\n```ts\nconst ready = true;\n```\n\n# Setup'} />);
    expect(html).toContain('<h1 id="user-content-setup"');
    expect(html).toContain('id="user-content-setup-1"');
    expect(html).toContain('href="#user-content-setup"');
    expect(html).toContain("<table>");
    expect(html).toContain('type="checkbox"');
    expect(html).toContain("hljs-keyword");
  });
  it("renders useful HTML while stripping scripts, event handlers, and unsafe URLs", () => {
    const html = renderToStaticMarkup(<Markdown text={'<strong onclick="alert(1)">Useful</strong>\n\n<script>alert(1)</script>\n\n<a href="javascript:alert(1)">bad</a>\n\n<img src="https://example.com/logo.png" onerror="alert(1)" alt="Logo">'} />);
    expect(html).toContain("<strong>Useful</strong>");
    expect(html).not.toMatch(/<script|onclick=|onerror=|javascript:/i);
    expect(html).toContain('referrerPolicy="no-referrer"');
  });
  it("keeps footnote references, back links, and already-prefixed anchors connected after sanitizing", () => {
    const html = renderToStaticMarkup(<Markdown text={'# Setup\n\n[Jump](#user-content-setup)\n\n<a name="custom"></a>\n\n[Custom](#custom)\n\nA useful note[^1].\n\n[^1]: More context.'} />);
    const ids = new Set([...html.matchAll(/\bid="([^"]+)"/g)].map((match) => match[1]));
    for (const match of html.matchAll(/href="#([^"]+)"/g)) expect(ids.has(match[1])).toBe(true);
    expect(html).toContain('aria-label="Back to reference 1"');
  });
  it("keeps front matter and maps relative links through the repository reader", () => {
    const html = renderToStaticMarkup(<Markdown text={'---\ntitle: Receipt guide\n---\n\n[Source](src/index.ts)'} fileLink={(href) => repositoryLink(route, "README.md", href)} />);
    expect(html).toContain("Receipt guide");
    expect(html).toContain("/repo/file%3Asrc/index.ts?k=share-token&amp;u=reviewer");
  });
});

describe("diagrams", () => {
  it("shows the source of a mermaid block until the browser draws it", () => {
    const html = renderToStaticMarkup(<Markdown text={'```mermaid\nflowchart LR\n  A --> B\n```\n\n```ts\nconst ready = true;\n```'} />);
    expect(html).toContain("md-diagram-pending");
    expect(html).toContain("A --&gt; B");
    expect(html.match(/class="code-block"/g)).toHaveLength(2);
  });
  it("names a diagram by its title, else by its kind", () => {
    expect(diagramTitle('%%{init: {}}%%\nxychart-beta\n  title "Landings each minute"\n  bar [1, 2]')).toBe("Landings each minute");
    expect(diagramTitle("flowchart LR\n  accTitle: The path of one change\n  A --> B")).toBe("The path of one change");
    expect(diagramTitle("---\ntitle: Queue\n---\nflowchart LR\n  A --> B")).toBe("Queue");
    expect(diagramTitle("%%{init: {}}%%\nflowchart LR\n  A --> B")).toBe("Diagram (flowchart)");
    expect(diagramTitle("")).toBe("Diagram");
  });
  it("draws the SVG as an image, at the width that mermaid gave it", () => {
    const svg = '<svg id="m" width="100%" style="max-width: 640.5px;" viewBox="0 0 640.5 300"><script>alert(1)</script><text>a&b</text></svg>';
    const image = diagramImage(svg);
    expect(image.width).toBe(641);
    expect(image.src.startsWith("data:image/svg+xml;charset=utf-8,")).toBe(true);
    expect(decodeURIComponent(image.src.slice(image.src.indexOf(",") + 1))).toBe(svg);
    expect(diagramImage('<svg viewBox="-8 -8 300 200"></svg>').width).toBe(300);
    expect(diagramImage("<svg></svg>").width).toBeNull();
  });
});
