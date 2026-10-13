import { decodeSafe } from "./util";
// Routes: /{org}/{repo}/{view}/{id} for a repo with a public name, as a git host has them,
// and /app/{trunk}/{view}/{id}?k=...&u=... for the others. A public repo opens with no key;
// ?k= and ?u= (a personal link) work on both forms, so a link is the whole thing: share it
// and the other person sees the same view.

import { readSearch, writeSearch, type RepoSearchOptions } from "./search";

export type View = "graph" | "pulls" | "tasks" | "releases" | "inbox" | "agents" | "repo";

export const VIEWS: { id: View; label: string; key: string }[] = [
  { id: "repo", label: "Code", key: "r" },
  { id: "graph", label: "Graph", key: "g" },
  { id: "pulls", label: "Pull requests", key: "p" },
  { id: "tasks", label: "Tasks", key: "t" },
  { id: "releases", label: "Releases", key: "l" },
  { id: "inbox", label: "Inbox", key: "i" },
  { id: "agents", label: "Agents", key: "a" },
];

// Older links used other view names. They still open the nearest view.
const ALIAS: Record<string, View> = { queue: "pulls", landed: "graph", activity: "graph", files: "repo", pull: "pulls" };

export function viewOf(part: string | undefined): View {
  if (!part) return "repo";
  if (part && VIEWS.some((v) => v.id === part)) return part as View;
  return (part && ALIAS[part]) || "graph";
}

/** A repo's public name, as on a git host. */
export interface RepoName {
  org: string;
  repo: string;
}

export interface Route {
  trunk: string;
  /** The public name, when the repo has one: links then use /{org}/{repo}. */
  name: RepoName | null;
  view: View;
  /** A change id, task id, agent, release or repo row. A PR's URL carries its number instead. */
  id: string | null;
  /** The share key or a personal key; null for a public repo opened without a link. */
  key: string | null;
  user: string | null;
  search?: RepoSearchOptions;
}

export const ORG_RE = /^[a-z0-9][a-z0-9-]{0,38}$/;
export const REPO_RE = /^[A-Za-z0-9._-]{1,100}$/;
const RESERVED = new Set(["t", "v1", "app", "api", "explore", "orgs", "settings", "login", "new", "assets", "static", "_next", "favicon.ico", "robots.txt"]);

export const isOrg = (s: string) => ORG_RE.test(s) && !RESERVED.has(s);
export const isRepo = (s: string) => REPO_RE.test(s) && s !== "." && s !== ".." && !s.endsWith(".git");

/** The view and id in a repo URL's path, after /{org}/{repo} or /app/{trunk}. */
export function partsOf(rest: string[]): { view: View; id: string | null } {
  return { view: viewOf(rest[0]), id: rest.length > 1 ? rest.slice(1).map(decodeSafe).join("/") : null };
}

/** The repo part of the URL in the address bar. */
export function parseLocation(loc: Pick<Location, "pathname" | "search">): { view: View; id: string | null; search?: RepoSearchOptions } {
  const parts = loc.pathname.split("/").filter(Boolean);
  return { ...partsOf(parts.slice(2)), search: readSearch(new URLSearchParams(loc.search)) };
}

/** The URL of a route. `numberOf` gives a PR's number, so a PR's link reads /pulls/12. */
export function routeUrl(r: Route, numberOf?: (id: string) => number | null | undefined): string {
  const q = new URLSearchParams();
  if (r.key) q.set("k", r.key);
  if (r.key && r.user) q.set("u", r.user);
  if (r.view === "repo" && r.search) writeSearch(q, r.search);
  const query = q.toString() ? `?${q.toString()}` : "";
  let id = r.id;
  if (id && r.view === "pulls") id = String(numberOf?.(id) ?? id);
  const tail = id ? "/" + id.split("/").map(encodeURIComponent).join("/") : "";
  if (!r.name) return `/app/${encodeURIComponent(r.trunk)}/${r.view}${tail}${query}`;
  const base = repoUrl(r.name);
  return r.view === "repo" && !tail ? `${base}${query}` : `${base}/${r.view}${tail}${query}`;
}

export const repoUrl = (n: RepoName) => `/${encodeURIComponent(n.org)}/${encodeURIComponent(n.repo)}`;
export const orgUrl = (org: string) => `/${encodeURIComponent(org)}`;
export const teamUrl = (org: string, team: string) => `/orgs/${encodeURIComponent(org)}/teams/${encodeURIComponent(team)}`;

/** In Pull requests, an id of digits is a PR number (#12); otherwise a change id. */
export const prNumberOf = (view: View, id: string | null) => (view === "pulls" && id && /^\d+$/.test(id) ? Number(id) : null);

export const isTaskId = (id: string) => /^[A-Z]+-\d+$/.test(id);
