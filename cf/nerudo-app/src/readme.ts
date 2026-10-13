import type { TreeResult } from "./api";
import { routeUrl, type Route } from "./route";

export const isMarkdown = (path: string) => /\.(md|markdown|mdown)$/i.test(path);

/** Root only: do not accidentally pick a directory or a nested README. */
export function rootReadme(tree: TreeResult): string | null {
  const candidates = (tree.entries ?? []).filter((entry) => !entry.dir && /^readme(?:\.(md|markdown|mdown|txt|rst))?$/i.test(entry.name));
  const rank = (name: string) => isMarkdown(name) ? 0 : /^readme$/i.test(name) ? 1 : 2;
  return candidates.sort((a, b) => rank(a.name) - rank(b.name) || a.name.localeCompare(b.name))[0]?.name ?? null;
}

/** Relative document links stay in this repository and retain the reader's access. */
export function repositoryLink(route: Route, sourcePath: string, href: string): string {
  if (!href || /[\u0000-\u0020\\]/.test(href)) return "";
  if (/^https?:\/\//i.test(href) || href.startsWith("#")) return href;
  if (/^[A-Za-z][A-Za-z\d+.-]*:/.test(href) || href.startsWith("//")) return "";
  const hashIndex = href.indexOf("#");
  const hash = hashIndex < 0 ? "" : href.slice(hashIndex);
  const raw = (hashIndex < 0 ? href : href.slice(0, hashIndex)).split("?")[0];
  const parts = raw.startsWith("/") ? [] : sourcePath.split("/").slice(0, -1);
  try {
    for (const encoded of raw.split("/")) {
      const part = decodeURIComponent(encoded);
      if (!part || part === ".") continue;
      if (part === "..") { if (!parts.length) return ""; parts.pop(); }
      else if (/[\u0000-\u001f/\\]/.test(part)) return "";
      else parts.push(part);
    }
  } catch { return ""; }
  const path = parts.join("/");
  const anchor = hash && isMarkdown(path) && !hash.startsWith("#user-content-") ? `#user-content-${hash.slice(1)}` : hash;
  return routeUrl({ ...route, view: "repo", id: path ? `${raw.endsWith("/") ? "dir" : "file"}:${path}` : null }) + anchor;
}
