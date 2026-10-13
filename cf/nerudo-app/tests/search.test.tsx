import { describe, expect, it, vi, afterEach } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { Api, type RepoSearchResult } from "../src/api";
import { routeUrl, parseLocation, type Route } from "../src/route";
import { defaultSearch, effectiveSearch, highlightParts, readSearch, searchFromRecord, searchRequest } from "../src/search";
import { SearchHighlight, SearchResults } from "../src/views/RepoSearch";

const route: Route = { trunk: "sample", name: { org: "studio", repo: "receipts" }, view: "repo", id: null, key: "share-token", user: "reviewer" };
afterEach(() => vi.unstubAllGlobals());

describe("shareable repository search", () => {
  it("round trips all filters with keys and special characters through repo and file links", () => {
    const search = { ...defaultSearch("content"), query: 'a+b & "value"', extension: "ts,tsx", language: "TypeScript", category: "tests", include: "src/**", exclude: "**/*.generated.*", ignored: true, caseSensitive: true, wholeWord: true, regex: true, fuzzy: false };
    const url = new URL(routeUrl({ ...route, search, id: "file:src/a+b.ts" }), "https://example.com");
    expect(url.searchParams.get("k")).toBe("share-token");
    expect(parseLocation(url)).toEqual({ view: "repo", id: "file:src/a+b.ts", search });
    expect(searchFromRecord(Object.fromEntries(url.searchParams))).toEqual(search);
    expect(new URLSearchParams(searchRequest(search).split("?")[1]).get("q")).toBe(search.query);
    expect(searchRequest(search)).not.toContain("share-token");
  });
  it("supports filter-only filename searches and keeps search off other views", () => {
    expect(readSearch(new URLSearchParams("mode=files&ext=rs"))).toEqual({ ...defaultSearch("files"), extension: "rs" });
    expect(readSearch(new URLSearchParams("k=secret"))).toBeUndefined();
    expect(routeUrl({ ...route, view: "tasks", search: defaultSearch() })).not.toContain("mode=");
    expect(parseLocation({ pathname: "/app/sample/repo/file%3Asrc/index.ts", search: "?mode=content&q=hello" }).search?.query).toBe("hello");
  });
  it("keeps a single field while supporting optional type, path and regex qualifiers", () => {
    expect(effectiveSearch({ ...defaultSearch(), query: 'hello type:ts,tsx path:src/** exclude:**/*.test.* lang:"TypeScript"' })).toEqual({ ...defaultSearch(), query: "hello", extension: "ts,tsx", include: "src/**", exclude: "**/*.test.*", language: "TypeScript" });
    expect(effectiveSearch({ ...defaultSearch(), query: 'regex:"function\\s+search" case:yes type:tests' })).toEqual({ ...defaultSearch(), query: "function\\s+search", regex: true, caseSensitive: true, category: "tests" });
    expect(effectiveSearch({ ...defaultSearch(), query: '"hello world"' }).query).toBe("hello world");
    expect(effectiveSearch({ ...defaultSearch(), query: 'some:unknown:value' }).query).toBe("some:unknown:value");
    expect(effectiveSearch({ ...defaultSearch(), query: '"hello type:source"' }).query).toBe("hello type:source");
    expect(new URLSearchParams(searchRequest({ ...defaultSearch(), query: "type:source", regex: true }).split("?")[1]).get("q")).toBe("type:source");
  });
  it("passes cancellation and access headers to API reads", async () => {
    const fetch = vi.fn().mockResolvedValue(new Response('{"files":[]}', { headers: { "content-type": "application/json" } }));
    vi.stubGlobal("fetch", fetch);
    const signal = new AbortController().signal;
    await new Api("sample", "token", "reviewer").get("search?q=a", signal);
    expect(fetch).toHaveBeenCalledWith("/t/sample/api/search?q=a", expect.objectContaining({ signal: expect.any(AbortSignal), headers: { "X-Nerudo-Key": "token", "X-Nerudo-User": "reviewer" } }));
  });
});

describe("search previews", () => {
  it("merges adjacent/overlapping ranges while preserving UTF-16 source text", () => {
    const text = "a😀alpha<&>";
    const parts = highlightParts(text, [[3, 6], [5, 8], [1, 3], [100, 110]]);
    expect(parts.map((part) => part.text).join("")).toBe(text);
    expect(parts.filter((part) => part.match)).toEqual([{ text: "😀alpha", match: true }]);
    const html = renderToStaticMarkup(<SearchHighlight text="<script>" ranges={[[0, 8]]} />);
    expect(html).toContain("<mark>&lt;script&gt;</mark>");
    expect(html).not.toContain("<script>");
  });
  it("groups matching lines by file with navigable exact-line links", () => {
    const data = { mode: "content", files: [{ path: "src/app.ts", positions: [], matches: 2, lines: [{ line: 7, text: "const value = '<script>';", ranges: [[6, 11]], clipped: false }] }], limits: { max_lines_per_file: 20 } } as unknown as RepoSearchResult;
    const html = renderToStaticMarkup(<SearchResults data={data} link={(path, line) => ({ href: `/repo/${path}${line ? `#L${line}` : ""}` })} />);
    expect(html).toContain('href="/repo/src/app.ts#L7"');
    expect(html).toContain("<mark>value</mark>");
    expect(html).toContain("2 matches");
    expect(html).not.toContain("<script>");
  });
  it("does not report a complete empty result while an index is being prepared", () => {
    const pending = { files: [], stats: { index_pending: true } } as unknown as RepoSearchResult;
    const html = renderToStaticMarkup(<SearchResults data={pending} link={() => ({ href: "/" })} />);
    expect(html).toContain("Results are being prepared.");
    expect(html).not.toContain("No matching files.");
    const ready = renderToStaticMarkup(<SearchResults data={{ ...pending, stats: { ...pending.stats, index_pending: false } }} link={() => ({ href: "/" })} />);
    expect(ready).toContain("No matching files.");
  });
});
