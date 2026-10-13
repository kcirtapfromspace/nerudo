import { common, createLowlight } from "lowlight";
import type { Root, RootContent } from "hast";

const highlighter = createLowlight(common);
const EXTENSIONS: Record<string, string> = { js: "javascript", jsx: "javascript", ts: "typescript", tsx: "typescript", mjs: "javascript", cjs: "javascript", py: "python", rb: "ruby", rs: "rust", go: "go", sh: "bash", bash: "bash", zsh: "bash", yml: "yaml", yaml: "yaml", json: "json", toml: "ini", ini: "ini", css: "css", scss: "scss", html: "xml", htm: "xml", svg: "xml", xml: "xml", md: "markdown", mdx: "markdown", sql: "sql", c: "c", h: "c", cpp: "cpp", hpp: "cpp", java: "java", kt: "kotlin", php: "php", swift: "swift", diff: "diff", patch: "diff", dockerfile: "dockerfile" };

export interface CodeToken { text: string; className: string }

export function codeLanguage(language?: string, filename?: string): string | null {
  const name = filename?.split("/").at(-1)?.toLowerCase() ?? "";
  const candidate = language?.toLowerCase().replace(/^language-/, "").split(/\s/)[0] || (name === "dockerfile" ? "dockerfile" : name === "makefile" ? "makefile" : EXTENSIONS[name.split(".").at(-1) ?? ""]);
  const normalized = EXTENSIONS[candidate ?? ""] ?? candidate;
  return normalized && highlighter.registered(normalized) ? normalized : null;
}

/** Keep multiline token context and every source character, without generating HTML. */
export function highlightedLines(code: string, language?: string, filename?: string): CodeToken[][] {
  const selected = codeLanguage(language, filename);
  let tree: Root | null = null;
  if (selected && code.length <= 65_536) {
    try { tree = highlighter.highlight(selected, code); } catch { /* Readable plain text is the fallback. */ }
  }
  const lines: CodeToken[][] = [[]];
  const append = (text: string, classes: string) => {
    const chunks = text.split("\n");
    chunks.forEach((chunk, index) => {
      if (index) lines.push([]);
      if (chunk) lines[lines.length - 1].push({ text: chunk, className: classes });
    });
  };
  const visit = (node: RootContent, classes = "") => {
    if (node.type === "text") append(node.value, classes);
    else if (node.type === "element") {
      const local = Array.isArray(node.properties.className) ? node.properties.className.join(" ") : "";
      node.children.forEach((child) => visit(child, [classes, local].filter(Boolean).join(" ")));
    }
  };
  if (tree) tree.children.forEach((node) => visit(node));
  else append(code, "");
  return lines;
}
