import { describe, expect, it, vi, afterEach } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { fetchGlobalSearch, fetchGlobalSearchPages, globalItemHref, globalSearchPlan, globalSearchRequest, localGlobalResults, mergeGlobalResults, type GlobalSearchItem, type GlobalSearchResult } from "../src/global-search";
import type { RepoView } from "../src/hub";
import { defaultSearch } from "../src/search";
import { GlobalSearchResults } from "../src/views/GlobalSearchResults";
import { RepoExplorer } from "../src/views/RepoExplorer";

const file = (org: string, path = "src/app.ts"): GlobalSearchItem => ({ kind: "file", org, repo: "web", trunk: `${org}-web`, public: org === "public", commit: "abc123", path, extension: "ts", language: "TypeScript", category: "source", size: 200, score: 20, positions: [4, 5, 6], matches: 1, lines: [{ line: 9, text: 'return "<script>";', ranges: [[8, 14]], clipped: false }] });
const items: GlobalSearchItem[] = [
  { kind: "user", user: "river", title: "Route maintainer", repos: 2, score: 30, positions: [] },
  { kind: "org", org: "route-labs", title: "Route Labs", description: "Edge tools", repos: 2, score: 30, positions: [] },
  { kind: "repo", org: "route-labs", repo: "router", trunk: "routes", public: true, description: "HTTP routes", score: 30, positions: [11, 12] },
  file("public"), file("studio"),
];
const emptyResult: GlobalSearchResult = { query: "", results: [], next_cursor: null, stats: { accessible_repos: 0, searched_repos: 0, failed_repos: 0, remaining_repos: 0, matched_users: 0, matched_orgs: 0, matched_repos: 0, matched_files: 0, returned: 0, partial: false, truncated: false, scan_limited: false, directory_incomplete: false }, limits: { repos_per_page: 8, concurrency: 4, repo_results: 12, timeout_ms: 8000 } };
const repos: RepoView[] = [{ org: "route-labs", repo: "router", trunk: "routes", public: true, description: "HTTP routes", created_ms: 1, upstream: null, summary: null }, { org: "studio", repo: "private-web", trunk: "private", public: false, description: "Internal routes", created_ms: 1, upstream: null, summary: null }];
afterEach(() => vi.unstubAllGlobals());

describe("global query planning", () => {
  it("uses one query for entities and code, with optional hidden scope qualifiers", () => {
    const plan = globalSearchPlan('search owner:studio repo:web type:ts path:src/** lang:"TypeScript"');
    expect(plan.owner).toBe("studio");
    expect(plan.repo).toBe("web");
    expect(plan.kind).toBe("file");
    expect(plan.search).toEqual({ ...defaultSearch(), query: "search", extension: "ts", include: "src/**", language: "TypeScript" });
    expect(globalSearchPlan("river type:people").kind).toBe("user");
    expect(globalSearchPlan("user:river")).toMatchObject({ owner: "river", kind: "user", search: { query: "" } });
    expect(globalSearchPlan("type:org").kind).toBe("org");
    expect(globalSearchPlan("route").kind).toBe("");
  });
  it("encodes queries and continuation cursors without access credentials", () => {
    const url = new URL(globalSearchRequest('a+b & "value"', "studio", "private", "cursor+/="), "https://example.com");
    expect(url.pathname).toBe("/v1/hub/search");
    expect(url.searchParams.get("q")).toBe('a+b & "value"');
    expect(url.searchParams.get("cursor")).toBe("cursor+/=");
    expect(url.searchParams.get("visibility")).toBe("private");
    expect(url.searchParams.get("mode")).toBe("all");
    expect(url.searchParams.has("k")).toBe(false);
    expect(url.searchParams.has("u")).toBe(false);
  });
  it("keeps qualifier-like text in quoted phrases and regex scoped links literal", () => {
    const phrase = globalSearchPlan('"hello owner:studio type:source"');
    expect(phrase.owner).toBe("");
    expect(phrase.kind).toBe("");
    expect(phrase.search.query).toBe("hello owner:studio type:source");
    const regex = globalSearchPlan('regex:"owner:needle"');
    expect(regex.owner).toBe("");
    expect(regex.search.query).toBe("owner:needle");
    expect(regex.search.regex).toBe(true);
  });
  it("opens global code hits with a scoped query and the matching source anchor", () => {
    const search = globalSearchPlan("handler owner:studio lang:TypeScript").search;
    const url = new URL(globalItemHref(file("studio", "src/a+b.ts"), search, 9), "https://example.com");
    expect(url.pathname).toBe("/studio/web/repo/file%3Asrc/a%2Bb.ts");
    expect(url.hash).toBe("#L9");
    expect(url.searchParams.get("q")).toBe("handler");
    expect(url.searchParams.get("language")).toBe("TypeScript");
    expect(url.searchParams.has("owner")).toBe(false);
    expect(globalItemHref(items[0], search)).toBe("/river");
  });
});

describe("global results", () => {
  it("renders authorized metadata immediately in the same list before code results arrive", () => {
    const html = renderToStaticMarkup(<RepoExplorer repos={repos} orgs={[{ org: "route-labs", title: "Route Labs", description: "Edge tools", repos: 1 }]} users={[{ user: "river", title: "Route maintainer", repos: 0 }]} initial={{ q: "route", owner: "", visibility: "all", sort: "recent", page: 1 }} />);
    expect(html).toContain("Route maintainer");
    expect(html).toContain("route-labs/router");
    expect(html).toContain("Organization");
    expect(html).toContain("searching code…");
    expect(html.match(/type="search"/g)).toHaveLength(1);
    expect(html).not.toContain("<select");
  });
  it("applies query scopes to provisional metadata and leaves explicit code queries to indexed search", () => {
    const directory = { repos, orgs: [{ org: "route-labs", title: "Route Labs", description: "Edge tools", repos: 1 }], users: [{ user: "river", title: "Route maintainer", repos: 0 }] };
    expect(localGlobalResults(directory, globalSearchPlan("route type:people")).map((item) => item.kind)).toEqual(["user"]);
    expect(localGlobalResults(directory, globalSearchPlan("route", "studio", "private"))).toMatchObject([{ kind: "repo", org: "studio" }]);
    expect(localGlobalResults(directory, globalSearchPlan("route", "studio", "public"))).toEqual([]);
    expect(localGlobalResults(directory, globalSearchPlan("handler lang:Rust"))).toEqual([]);
    expect(localGlobalResults(directory, globalSearchPlan("route repo:router"))).toHaveLength(2);
  });
  it("highlights fuzzy and Unicode metadata matches in original UTF-16 positions", () => {
    const directory = { repos: [{ ...repos[0], repo: "İ😀Router" }] };
    const matches = localGlobalResults(directory, globalSearchPlan("😀rtr"));
    expect(matches).toHaveLength(1);
    expect(matches[0].positions).toEqual([12, 14, 17, 19]);
    expect(localGlobalResults(directory, globalSearchPlan("😀rtr exact:true"))).toEqual([]);
  });
  it("keeps same-path files from different repos distinct and replaces duplicate continuation hits", () => {
    const changed = { ...file("studio"), matches: 3 };
    const merged = mergeGlobalResults([file("public"), file("studio")], [changed]);
    expect(merged).toHaveLength(2);
    expect(merged[1]).toMatchObject({ org: "studio", matches: 3 });
    expect(items).toHaveLength(5);
  });
  it("moves later exact filename hits ahead of earlier content-only results", () => {
    const content = { ...file("public"), score: 0 };
    const exact = { ...file("studio"), score: 10000 };
    expect(mergeGlobalResults([content], [exact])[0]).toMatchObject({ org: "studio" });
  });
  it("renders all result kinds, repo context, safe snippets and exact-line links", () => {
    const html = renderToStaticMarkup(<GlobalSearchResults items={items} search={{ ...defaultSearch(), query: "app" }} />);
    for (const label of ["Person", "Organization", "Repository", "Private file"]) expect(html).toContain(label);
    expect(html).toContain("public/web");
    expect(html).toContain("studio/web");
    expect(html).toContain("#L9");
    expect(html).not.toContain("<script>");
    expect(html).toContain("&lt;script");
  });
  it("keeps homepage search to one field without filter widgets", () => {
    const result = { ...emptyResult, query: "route", results: items, stats: { ...emptyResult.stats, returned: 5 } };
    const html = renderToStaticMarkup(<RepoExplorer repos={[]} initial={{ q: "route", owner: "", visibility: "all", sort: "recent", page: 1 }} initialResult={result} />);
    expect(html.match(/type="search"/g)).toHaveLength(1);
    expect(html).not.toContain("<select");
    expect(html).toContain("Search NERUDO");
    expect(html).toContain("Route maintainer");
    expect(html).not.toContain("Show more results");
  });
  it("does not claim a complete empty search while more repositories remain", () => {
    const result = { ...emptyResult, query: "late", next_cursor: "next", stats: { ...emptyResult.stats, partial: true } };
    const html = renderToStaticMarkup(<RepoExplorer repos={[]} initial={{ q: "late", owner: "", visibility: "all", sort: "recent", page: 1 }} initialResult={result} />);
    expect(html).toContain("No matches yet.");
    expect(html).toContain("Continue search");
    expect(html).not.toContain("No matches.</h3>");
  });
});

describe("global requests", () => {
  it("continues bounded empty pages with the original query and preserves partial coverage", async () => {
    const first = { ...emptyResult, next_cursor: "page-2", stats: { ...emptyResult.stats, failed_repos: 1, index_pending: true } };
    const next = { ...emptyResult, results: [file("public")], next_cursor: "page-3" };
    const fetch = vi.fn().mockResolvedValueOnce(new Response(JSON.stringify(first))).mockResolvedValueOnce(new Response(JSON.stringify(next)));
    vi.stubGlobal("fetch", fetch);
    const result = await fetchGlobalSearchPages((cursor) => globalSearchRequest("route", "studio", "private", cursor));
    expect(result.results).toHaveLength(1);
    expect(result.next_cursor).toBe("page-3");
    expect(result.stats.failed_repos).toBe(1);
    expect(result.stats.index_pending).toBe(true);
    const url = new URL(fetch.mock.calls[1][0], "https://example.com");
    expect(url.searchParams.get("cursor")).toBe("page-2");
    expect(url.searchParams.get("q")).toBe("route");
    expect(url.searchParams.get("owner")).toBe("studio");
    expect(url.searchParams.get("visibility")).toBe("private");
  });
  it("discards a delayed response after the query changes even if the fetch ignores cancellation", async () => {
    let resolve!: (response: Response) => void;
    const fetch = vi.fn().mockImplementation(() => new Promise<Response>((done) => { resolve = done; }));
    vi.stubGlobal("fetch", fetch);
    let current = true;
    const signal = new AbortController().signal;
    const request = fetchGlobalSearchPages((cursor) => globalSearchRequest("old", "", "all", cursor), signal, undefined, () => current);
    current = false;
    resolve(new Response(JSON.stringify({ ...emptyResult, results: [file("public")], next_cursor: "old-next" })));
    await expect(request).rejects.toMatchObject({ name: "AbortError" });
    expect(fetch).toHaveBeenCalledTimes(1);
  });
  it("stops continuation after cancellation and bounds empty-page work", async () => {
    const controller = new AbortController();
    const fetch = vi.fn().mockImplementation(async () => { controller.abort(); return new Response(JSON.stringify({ ...emptyResult, next_cursor: "next" })); });
    vi.stubGlobal("fetch", fetch);
    await expect(fetchGlobalSearchPages((cursor) => globalSearchRequest("route", "", "all", cursor), controller.signal)).rejects.toMatchObject({ name: "AbortError" });
    expect(fetch).toHaveBeenCalledTimes(1);
    fetch.mockImplementation(async () => new Response(JSON.stringify({ ...emptyResult, next_cursor: "next" })));
    const result = await fetchGlobalSearchPages((cursor) => globalSearchRequest("route", "", "all", cursor));
    expect(result.next_cursor).toBe("next");
    expect(fetch).toHaveBeenCalledTimes(5);
  });
  it("uses session cookies, cancellation and no shared browser cache", async () => {
    const fetch = vi.fn().mockResolvedValue(new Response('{"results":[]}'));
    vi.stubGlobal("fetch", fetch);
    const signal = new AbortController().signal;
    await fetchGlobalSearch("/v1/hub/search?q=route", signal);
    expect(fetch).toHaveBeenCalledWith("/v1/hub/search?q=route", { signal, credentials: "same-origin", cache: "no-store" });
  });
  it("reports invalid-pattern errors without replacing them with an empty result", async () => {
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(new Response('{"error":"Invalid search pattern"}', { status: 400 })));
    await expect(fetchGlobalSearch("/v1/hub/search?q=%5B&regex=1")).rejects.toThrow("Invalid search pattern");
  });
  it("keeps transient server details out of public search errors", async () => {
    vi.stubGlobal("fetch", vi.fn().mockResolvedValueOnce(new Response('{"error":"PRIVATE_REPO_INTERNAL_DETAILS"}', { status: 500 })).mockResolvedValueOnce(new Response("PRIVATE_REPO_INTERNAL_DETAILS", { status: 503 })));
    await expect(fetchGlobalSearch("/v1/hub/search?q=route")).rejects.toThrow("Search is unavailable. Please try again.");
    await expect(fetchGlobalSearch("/v1/hub/search?q=route")).rejects.toThrow("Search is unavailable. Please try again.");
  });
});
