"use client";

import { useEffect, useMemo, useRef, useState } from "react";
import type { OrgRow, RepoView, UserRow } from "../hub";
import { discoverRepos, discoveryOf, discoveryQuery, PAGE_SIZE, type Discovery } from "../repo-discovery";
import { appendGlobalPage, fetchGlobalSearchPages, globalSearchPlan, globalSearchRequest, localGlobalResults, type GlobalSearchResult } from "../global-search";
import { RepoRows } from "./Hub";
import { GlobalSearchResults } from "./GlobalSearchResults";

function writeLocation(state: Discovery) {
  const query = discoveryQuery(state);
  const url = query ? `/?${query}` : "/";
  if (window.location.pathname + window.location.search !== url) window.history.replaceState(null, "", url);
}

/** The homepage searches every authorized surface without exposing filter controls. */
export function RepoExplorer({ repos, orgs, users, initial, initialResult, directoryError = false }: { repos: RepoView[]; orgs?: OrgRow[]; users?: UserRow[]; initial: Discovery; initialResult?: GlobalSearchResult | null; directoryError?: boolean }) {
  const [state, setState] = useState(initial);
  const [response, setResponse] = useState<{ request: string; data: GlobalSearchResult } | null>(initialResult ? { request: globalSearchRequest(initial.q, initial.owner, initial.visibility), data: initialResult } : null);
  const [failure, setFailure] = useState<{ request: string; error: string } | null>(null);
  const [pending, setPending] = useState<string | null>(null);
  const [more, setMore] = useState<string | null>(null);
  const [attempt, setAttempt] = useState(0);
  const search = useRef<HTMLInputElement>(null);
  const resultHeading = useRef<HTMLHeadingElement>(null);
  const resultsBox = useRef<HTMLDivElement>(null);
  const controller = useRef<AbortController | null>(null);
  const generation = useRef(0);
  const continuedRequest = useRef<string | null>(null);
  const initialRead = useRef(initialResult ? globalSearchRequest(initial.q, initial.owner, initial.visibility) : null);
  const active = Boolean(state.q.trim());
  const request = globalSearchRequest(state.q, state.owner, state.visibility);
  const currentRequest = useRef(request);
  currentRequest.current = request;
  const data = response?.request === request ? response.data : null;
  const error = failure?.request === request ? failure.error : null;
  const loading = active && (pending === request || (!data && !error));
  const loadingMore = more === request;
  const plan = useMemo(() => globalSearchPlan(state.q, state.owner, state.visibility), [state.q, state.owner, state.visibility]);
  const local = useMemo(() => active ? localGlobalResults({ repos, orgs, users }, plan) : [], [active, repos, orgs, users, plan]);
  const items = data ? data.results : local;
  const directory = useMemo(() => discoverRepos(repos, { ...state, q: "" }), [repos, state]);

  const update = (patch: Partial<Discovery>) => {
    const next = { ...state, ...patch, page: patch.page ?? 1 };
    if (globalSearchRequest(next.q, next.owner, next.visibility) !== request) { generation.current++; controller.current?.abort(); }
    writeLocation(next); setState(next);
  };
  useEffect(() => {
    const id = ++generation.current;
    controller.current?.abort();
    const activeController = new AbortController();
    controller.current = activeController;
    const signal = activeController.signal;
    if (continuedRequest.current !== request) continuedRequest.current = null;
    setFailure(null); setMore(null);
    if (!active) { setResponse(null); setPending(null); return; }
    const prefetched = initialRead.current === request && attempt === 0 ? initialResult : null;
    if (prefetched && (prefetched.results.length || !prefetched.next_cursor)) { initialRead.current = null; setPending(null); return () => activeController.abort(); }
    initialRead.current = null;
    setPending(request);
    const timer = window.setTimeout(async () => {
      try {
        let result = await fetchGlobalSearchPages((cursor) => globalSearchRequest(state.q, state.owner, state.visibility, cursor), signal, prefetched?.next_cursor || undefined, () => generation.current === id);
        if (prefetched) result = appendGlobalPage(prefetched, result);
        if (!signal.aborted && generation.current === id) setResponse({ request, data: result });
      } catch (problem) {
        if (!signal.aborted && generation.current === id) setFailure({ request, error: problem instanceof Error ? problem.message : String(problem) });
      } finally {
        if (!signal.aborted && generation.current === id) setPending(null);
      }
    }, 75);
    return () => { window.clearTimeout(timer); activeController.abort(); };
  }, [request, active, attempt]);

  async function loadMore() {
    if (!data?.next_cursor || loadingMore || loading) return;
    const id = generation.current;
    const signal = controller.current?.signal;
    continuedRequest.current = request;
    setMore(request); setFailure(null);
    try {
      const next = await fetchGlobalSearchPages((cursor) => globalSearchRequest(state.q, state.owner, state.visibility, cursor), signal, data.next_cursor, () => generation.current === id);
      if (!signal?.aborted && generation.current === id) setResponse((previous) => ({ request, data: appendGlobalPage(previous?.request === request ? previous.data : null, next) }));
    } catch (problem) {
      if (!signal?.aborted && generation.current === id) setFailure({ request, error: problem instanceof Error ? problem.message : String(problem) });
    } finally {
      if (!signal?.aborted && generation.current === id) setMore(null);
    }
  }
  useEffect(() => {
    const pop = () => {
      const next = discoveryOf(new URLSearchParams(window.location.search));
      initialRead.current = null;
      if (globalSearchRequest(next.q, next.owner, next.visibility) !== currentRequest.current) { generation.current++; controller.current?.abort(); }
      setState(next);
    };
    const key = (event: KeyboardEvent) => {
      const typing = event.target instanceof Element && event.target.closest("input, textarea, select, [contenteditable]");
      if ((event.key === "/" && !typing && !event.metaKey && !event.ctrlKey && !event.altKey) || ((event.metaKey || event.ctrlKey) && ["k", "p"].includes(event.key.toLowerCase()))) { event.preventDefault(); search.current?.focus(); }
    };
    window.addEventListener("popstate", pop); window.addEventListener("keydown", key);
    return () => { window.removeEventListener("popstate", pop); window.removeEventListener("keydown", key); };
  }, []);

  useEffect(() => {
    if (!data?.stats.index_pending || data.next_cursor || continuedRequest.current === request || loading || loadingMore || error) return;
    const retry = () => { if (document.visibilityState === "visible") setAttempt((value) => value + 1); };
    const timer = window.setTimeout(retry, 1000);
    document.addEventListener("visibilitychange", retry);
    return () => { window.clearTimeout(timer); document.removeEventListener("visibilitychange", retry); };
  }, [data, request, loading, loadingMore, error]);

  const retry = () => { if (data?.next_cursor) void loadMore(); else setAttempt((value) => value + 1); };

  const partial = data && (data.stats.failed_repos > 0 || data.stats.truncated || data.stats.scan_limited || data.stats.directory_incomplete || (data.stats.partial && !data.next_cursor));
  return <section className="repo-directory global-search" id="repositories" aria-labelledby="repository-heading">
    <form className="directory-search" action="/" role="search" onSubmit={(event) => {
      event.preventDefault();
      if (active && error) retry();
      resultHeading.current?.focus();
    }}>
      <label htmlFor="global-query">Search NERUDO</label>
      <div className="directory-search-line"><input ref={search} id="global-query" name="q" type="search" placeholder="People, organizations, repositories, or code…" value={state.q} onChange={(event) => update({ q: event.target.value })} maxLength={512} autoComplete="off" onKeyDown={(event) => {
        if (event.key === "ArrowDown") { event.preventDefault(); resultsBox.current?.querySelector<HTMLAnchorElement>(".global-hit, .hub-row")?.focus(); }
        if (event.key === "Escape") { event.preventDefault(); update({ q: "", owner: "", visibility: "all" }); }
      }} /><button className="btn primary" type="submit">Search</button><kbd aria-hidden="true">/</kbd></div>
    </form>
    <div className="directory-results-heading"><h2 id="repository-heading" ref={resultHeading} tabIndex={-1}>{active ? "Search results" : "Repositories"}</h2><span role="status" aria-live="polite">{active ? items.length ? `${items.length} ${items.length === 1 ? "result" : "results"}${loading ? " · searching code…" : data?.next_cursor ? " so far" : ""}` : loading ? "Searching…" : data ? "0 results" : "" : directoryError ? "Directory unavailable" : `${directory.total} accessible ${directory.total === 1 ? "repository" : "repositories"}`}</span></div>
    <div ref={resultsBox} aria-busy={loading || loadingMore} onKeyDown={(event) => {
      if (!["ArrowDown", "ArrowUp", "Home", "End", "Escape", "j", "k"].includes(event.key)) return;
      if (!(event.target instanceof Element) || !event.target.closest("a")) return;
      event.preventDefault();
      if (event.key === "Escape") { search.current?.focus(); return; }
      const links = [...event.currentTarget.querySelectorAll<HTMLAnchorElement>(".global-hit, .hub-row")];
      const index = links.indexOf(document.activeElement as HTMLAnchorElement);
      const next = event.key === "Home" ? 0 : event.key === "End" ? links.length - 1 : Math.max(0, Math.min(links.length - 1, index + (["ArrowDown", "j"].includes(event.key) ? 1 : -1)));
      links[next]?.focus();
    }}>
      {active ? <>
        {error && <div className="global-search-error" role="alert"><p>{error}</p><button className="link" onClick={retry}>Try again</button></div>}
        {items.length > 0 && <GlobalSearchResults items={items} search={plan.search} />}
        {loading && !items.length && <p className="global-loading" role="status">Searching…</p>}
        {data && !items.length && !loading && <div className="directory-empty"><h3>{data.next_cursor || data.stats.index_pending ? "No matches yet." : partial ? "No matches in the results available." : "No matches."}</h3>{!data.stats.index_pending && <p>{data.next_cursor ? "Continue the search, or try a shorter query." : "Try a shorter name, filename, or phrase."}</p>}</div>}
      </> : directoryError ? <div className="directory-empty" role="status"><h3>Repository list unavailable.</h3><p>Search is still available. Reload to try the directory again.</p><button className="link" onClick={() => window.location.reload()}>Reload</button></div> : directory.total ? <RepoRows repos={directory.repos} empty="No accessible repositories." /> : <div className="directory-empty"><h3>No accessible repositories yet.</h3><p>Public repositories and private repositories you can access appear here.</p></div>}
    </div>
    {active && data?.next_cursor && <div className="global-search-more"><button className="btn" onClick={loadMore} disabled={loadingMore}>{loadingMore ? "Searching more…" : data.results.length ? "Show more results" : "Continue search"}</button></div>}
    {data?.stats.index_pending ? <p className="directory-access-note" role="status">More code results are being prepared.</p> : partial && <p className="directory-access-note" role="status">Some code results are unavailable or limited. Try a more specific query, or search within a repository.</p>}
    {!active && directory.pages > 1 && <nav className="directory-pagination" aria-label="Repository pages"><button className="btn" disabled={directory.page === 1} onClick={() => update({ page: directory.page - 1 })}>← Previous</button><span>Page {directory.page} of {directory.pages} · {(directory.page - 1) * PAGE_SIZE + 1}–{Math.min(directory.page * PAGE_SIZE, directory.total)} of {directory.total}</span><button className="btn" disabled={directory.page === directory.pages} onClick={() => update({ page: directory.page + 1 })}>Next →</button></nav>}
    <p className="directory-access-note">Search covers public repositories and private repositories you can access.</p>
  </section>;
}
