import type { RepoSearchResult } from "./api";
import { defaultSearch, effectiveSearch, replaceQueryQualifiers, writeSearch, type RepoSearchOptions } from "./search";
import { orgUrl, routeUrl } from "./route";
import type { OrgRow, RepoView, UserRow } from "./hub";

export type GlobalSearchKind = "user" | "org" | "repo" | "file";
type Ranked = { score: number; positions: number[] };
export type GlobalSearchItem =
  | (Ranked & { kind: "user"; user: string; title: string; repos: number })
  | (Ranked & { kind: "org"; org: string; title: string; description: string; repos: number })
  | (Ranked & { kind: "repo"; org: string; repo: string; trunk: string; public: boolean; description: string })
  | (RepoSearchResult["files"][number] & { kind: "file"; org: string; repo: string; trunk: string; public: boolean; commit: string });

export interface GlobalSearchResult {
  query: string;
  results: GlobalSearchItem[];
  next_cursor: string | null;
  stats: { accessible_repos: number; searched_repos: number; failed_repos: number; remaining_repos: number; matched_users: number; matched_orgs: number; matched_repos: number; matched_files: number; returned: number; partial: boolean; truncated: boolean; scan_limited: boolean; directory_incomplete: boolean; index_pending?: boolean };
  limits: { repos_per_page: number; concurrency: number; repo_results: number; timeout_ms: number };
}

export interface GlobalSearchPlan { search: RepoSearchOptions; owner: string; repo: string; kind: GlobalSearchKind | ""; visibility: string }

/** Scope is expressed in the query, keeping the main page to one search field. */
export function globalSearchPlan(query: string, owner = "", visibility = "all"): GlobalSearchPlan {
  const plan: GlobalSearchPlan = { search: defaultSearch(), owner, repo: "", kind: "", visibility };
  const plain = replaceQueryQualifiers(query, ["owner", "org", "repo", "user", "type"], (full, rawKey, quoted, rawValue) => {
    const key = rawKey.toLowerCase();
    const value = quoted ?? rawValue ?? "";
    if (key === "type") {
      const kind = ({ user: "user", users: "user", person: "user", people: "user", org: "org", orgs: "org", organization: "org", organizations: "org", repo: "repo", repos: "repo", repository: "repo", repositories: "repo", file: "file", files: "file", code: "file" } as Record<string, GlobalSearchKind>)[value.toLowerCase()];
      if (!kind) return full;
      plan.kind = kind;
    } else if (key === "repo") plan.repo = value;
    else if (key === "user") { plan.kind = "user"; plan.owner = value; }
    else plan.owner = value;
    return "";
  }).trim();
  plan.search = effectiveSearch({ ...defaultSearch(), query: plain });
  if (!plan.kind && (plan.search.extension || plan.search.language || plan.search.category || plan.search.include || plan.search.exclude || plan.search.regex || plan.search.wholeWord || plan.search.caseSensitive)) plan.kind = "file";
  return plan;
}

export function globalSearchRequest(query: string, owner = "", visibility = "all", cursor?: string) {
  const plan = globalSearchPlan(query, owner, visibility);
  const params = new URLSearchParams();
  writeSearch(params, plan.search);
  if (plan.owner) params.set("owner", plan.owner);
  if (plan.repo) params.set("repo", plan.repo);
  if (plan.kind) params.set("kind", plan.kind);
  if (visibility !== "all") params.set("visibility", visibility);
  if (cursor) params.set("cursor", cursor);
  params.set("limit", "60");
  return `/v1/hub/search?${params}`;
}

export function globalItemKey(item: GlobalSearchItem) {
  return item.kind === "user" ? `user:${item.user}` : item.kind === "org" ? `org:${item.org}` : item.kind === "repo" ? `repo:${item.org}/${item.repo}` : `file:${item.org}/${item.repo}:${item.path}`;
}

export function mergeGlobalResults(previous: GlobalSearchItem[], incoming: GlobalSearchItem[]) {
  const byKey = new Map(previous.map((item) => [globalItemKey(item), item]));
  for (const item of incoming) byKey.set(globalItemKey(item), item);
  return [...byKey.values()].sort((a, b) => b.score - a.score || globalItemKey(a).localeCompare(globalItemKey(b)));
}

/** Provisional metadata is limited to the viewer-authorized directory sent by the server. */
export function localGlobalResults(directory: { repos: RepoView[]; orgs?: OrgRow[]; users?: UserRow[] }, plan: GlobalSearchPlan): GlobalSearchItem[] {
  if (plan.kind === "file") return [];
  const owners = plan.owner.split(",").map((value) => value.trim().toLowerCase()).filter(Boolean);
  const names = plan.repo.split(",").map((value) => value.trim().toLowerCase()).filter(Boolean);
  const repos = directory.repos.filter((repo) => (!owners.length || owners.includes(repo.org.toLowerCase())) && (!names.length || names.includes(repo.repo.toLowerCase()) || names.includes(`${repo.org}/${repo.repo}`.toLowerCase())) && (plan.visibility === "all" || repo.public === (plan.visibility === "public")));
  const count = (owner: string) => repos.filter((repo) => repo.org === owner).length;
  const results: GlobalSearchItem[] = [];
  const match = (name: string, title: string, description: string): Ranked | null => {
    const rank = (text: string): Ranked | null => {
      const chars: { char: string; at: number }[] = [];
      let offset = 0;
      for (const char of text) {
        for (const folded of plan.search.caseSensitive ? char : char.toLowerCase()) chars.push({ char: folded, at: offset });
        offset += char.length;
      }
      const full = chars.map((char) => char.char);
      const nameStart = full.lastIndexOf("/") + 1;
      let score = 0;
      const positions: number[] = [];
      for (const word of plan.search.query.trim().split(/\s+/).filter(Boolean)) {
        const needle = [...(plan.search.caseSensitive ? word : word.toLowerCase())];
        let best = -Infinity;
        let bestAt = -1;
        for (let at = 0; at <= chars.length - needle.length; at++) {
          if (!needle.every((char, index) => char === full[at + index])) continue;
          const exact = (at === nameStart || at === 0) && at + needle.length === full.length;
          const value = (exact ? 10_000 : at === nameStart ? 9000 : at >= nameStart ? 8000 : 7000) - at;
          if (value > best) { best = value; bestAt = at; }
        }
        if (bestAt >= 0) { score += best; positions.push(...chars.slice(bestAt, bestAt + needle.length).map((char) => char.at)); continue; }
        if (!plan.search.fuzzy) return null;
        let hits: number[] | null = null;
        for (const start of [nameStart, 0]) {
          const candidate: number[] = [];
          for (let at = start, n = 0; at < chars.length; at++) if (full[at] === needle[n]) { candidate.push(at); n++; }
          if (candidate.length === needle.length) { hits = candidate; break; }
        }
        if (!hits) return null;
        const boundaries = hits.filter((at) => at === 0 || "/_-.".includes(full[at - 1])).length;
        score += 5000 + boundaries * 100 - (hits.at(-1)! - hits[0] + 1 - hits.length) * 10 + (hits[0] >= nameStart ? 200 : 0);
        positions.push(...hits.map((at) => chars[at].at));
      }
      return { score: score - [...text].length, positions: [...new Set(positions)].sort((a, b) => a - b) };
    };
    const nameMatch = rank(name);
    if (nameMatch) return { ...nameMatch, score: 20_000 + Math.max(1, nameMatch.score) };
    const titleMatch = rank(title);
    if (titleMatch) return { score: 15_000 + Math.floor(Math.max(1, titleMatch.score) / 4), positions: [] };
    const descriptionText = plan.search.caseSensitive ? description : description.toLowerCase();
    const terms = plan.search.query.trim().split(/\s+/).filter(Boolean).map((term) => plan.search.caseSensitive ? term : term.toLowerCase());
    return terms.length && terms.every((term) => descriptionText.includes(term)) ? { score: 10_000, positions: [] } : null;
  };
  if (!plan.kind || plan.kind === "user") for (const user of directory.users || []) {
    if ((owners.length && !owners.includes(user.user.toLowerCase())) || (names.length && !count(user.user))) continue;
    const ranked = match(user.user, user.title || "", "");
    if (ranked) results.push({ kind: "user", user: user.user, title: user.title || "", repos: count(user.user), ...ranked });
  }
  if (!plan.kind || plan.kind === "org") for (const org of directory.orgs || []) {
    if (!count(org.org)) continue;
    const ranked = match(org.org, org.title || "", org.description || "");
    if (ranked) results.push({ kind: "org", org: org.org, title: org.title || "", description: org.description || "", repos: count(org.org), ...ranked });
  }
  if (!plan.kind || plan.kind === "repo") for (const repo of repos) {
    const ranked = match(`${repo.org}/${repo.repo}`, "", repo.description || "");
    if (ranked) results.push({ kind: "repo", org: repo.org, repo: repo.repo, trunk: repo.trunk, public: repo.public, description: repo.description || "", ...ranked });
  }
  return mergeGlobalResults([], results).slice(0, 60);
}

export function appendGlobalPage(previous: GlobalSearchResult | null, next: GlobalSearchResult): GlobalSearchResult {
  if (!previous) return next;
  return { ...next, results: mergeGlobalResults(previous.results, next.results), stats: { ...next.stats, failed_repos: Math.max(previous.stats.failed_repos, next.stats.failed_repos), truncated: previous.stats.truncated || next.stats.truncated, scan_limited: previous.stats.scan_limited || next.stats.scan_limited, directory_incomplete: previous.stats.directory_incomplete || next.stats.directory_incomplete, index_pending: previous.stats.index_pending || next.stats.index_pending } };
}

/** Skip bounded empty pages, checking cancellation after every response and before following a cursor. */
export async function fetchGlobalSearchPages(request: (cursor?: string) => string, signal?: AbortSignal, cursor?: string, isCurrent: () => boolean = () => true): Promise<GlobalSearchResult> {
  let result: GlobalSearchResult | null = null;
  const check = () => { if (signal?.aborted || !isCurrent()) throw new DOMException("Search superseded", "AbortError"); };
  for (let page = 0; page < 4; page++) {
    check();
    const next = await fetchGlobalSearch(request(cursor), signal);
    check();
    result = appendGlobalPage(result, next);
    if (result.results.length || !result.next_cursor) return result;
    cursor = result.next_cursor;
  }
  return result!;
}

/** A global hit opens scoped search and preserves its source-line anchor. Access stays in the session. */
export function globalItemHref(item: GlobalSearchItem, search: RepoSearchOptions, line?: number) {
  if (item.kind === "user") return orgUrl(item.user);
  if (item.kind === "org") return orgUrl(item.org);
  const route = { trunk: item.trunk, name: { org: item.org, repo: item.repo }, view: "repo" as const, id: item.kind === "file" ? `file:${item.path}` : null, key: null, user: null };
  return routeUrl({ ...route, ...(item.kind === "file" ? { search } : {}) }) + (line ? `#L${line}` : "");
}

export async function fetchGlobalSearch(path: string, signal?: AbortSignal): Promise<GlobalSearchResult> {
  const response = await fetch(path, { credentials: "same-origin", cache: "no-store", signal });
  const unavailable = "Search is unavailable. Please try again.";
  let data: GlobalSearchResult & { error?: string };
  try { data = await response.json() as GlobalSearchResult & { error?: string }; }
  catch { throw new Error(unavailable); }
  if (!response.ok) throw new Error(response.status >= 500 ? unavailable : data.error || unavailable);
  return data;
}
