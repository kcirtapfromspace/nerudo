import type { RepoView } from "./hub";

export interface Discovery { q: string; owner: string; visibility: "all" | "public" | "private"; sort: "recent" | "name"; page: number }
export const PAGE_SIZE = 20;
export function discoveryOf(params: URLSearchParams): Discovery {
  const visibility = params.get("visibility");
  return { q: (params.get("q") || "").trim().slice(0, 512), owner: params.get("owner") || "", visibility: visibility === "public" || visibility === "private" ? visibility : "all", sort: params.get("sort") === "name" ? "name" : "recent", page: Math.max(1, Math.min(100_000, Number.parseInt(params.get("page") || "1", 10) || 1)) };
}
export function discoveryQuery(state: Discovery): string {
  const params = new URLSearchParams();
  if (state.q.trim()) params.set("q", state.q.trim());
  if (state.owner) params.set("owner", state.owner);
  if (state.visibility !== "all") params.set("visibility", state.visibility);
  if (state.sort !== "recent") params.set("sort", state.sort);
  if (state.page > 1) params.set("page", String(state.page));
  return params.toString();
}
export function discoverRepos(repos: RepoView[], state: Discovery) {
  const words = state.q.toLowerCase().trim().split(/\s+/).filter(Boolean);
  const matches = repos.filter((repo) => (!state.owner || repo.org === state.owner) && (state.visibility === "all" || repo.public === (state.visibility === "public")) && words.every((word) => `${repo.org}/${repo.repo} ${repo.description ?? ""}`.toLowerCase().includes(word)));
  const activity = (repo: RepoView) => repo.summary?.updated_ms ?? repo.created_ms;
  matches.sort((a, b) => state.sort === "recent" ? activity(b) - activity(a) || `${a.org}/${a.repo}`.localeCompare(`${b.org}/${b.repo}`) : `${a.org}/${a.repo}`.localeCompare(`${b.org}/${b.repo}`, undefined, { numeric: true, sensitivity: "base" }));
  const pages = Math.max(1, Math.ceil(matches.length / PAGE_SIZE));
  const page = Math.min(state.page, pages);
  return { total: matches.length, page, pages, repos: matches.slice((page - 1) * PAGE_SIZE, page * PAGE_SIZE) };
}
