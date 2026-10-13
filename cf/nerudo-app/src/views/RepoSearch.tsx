"use client";

import { useEffect, useRef, useState } from "react";
import { type Api, type RepoSearchResult } from "../api";
import { defaultSearch, highlightParts, searchRequest, type RepoSearchOptions } from "../search";

export function SearchHighlight({ text, ranges }: { text: string; ranges: [number, number][] }) {
  return <>{highlightParts(text, ranges).map((part, index) => part.match ? <mark key={index}>{part.text}</mark> : part.text)}</>;
}

export function RepoSearch({ api, stamp, search, onSearch, onClose, openFile, fileHref }: {
  api: Api; stamp: string; search: RepoSearchOptions; onSearch: (search: RepoSearchOptions) => void;
  onClose: () => void; openFile: (path: string, line?: number) => void; fileHref: (path: string, line?: number) => string;
}) {
  const [response, setResponse] = useState<{ request: string; data: RepoSearchResult } | null>(null);
  const [failure, setFailure] = useState<{ request: string; message: string } | null>(null);
  const [loading, setLoading] = useState(false);
  const [attempt, setAttempt] = useState(0);
  const results = useRef<HTMLDivElement>(null);
  const request = searchRequest(search);
  const data = response?.request === request ? response.data : null;
  const error = failure?.request === request ? failure.message : null;
  const hasQuery = search.mode === "files" || Boolean(search.query.trim()) || Boolean(search.extension || search.language || search.category || search.include);
  useEffect(() => {
    const controller = new AbortController();
    setResponse((previous) => previous?.request === request ? previous : null); setFailure(null); setLoading(hasQuery);
    if (!hasQuery) return;
    const timer = window.setTimeout(async () => {
      try {
        const result = await api.get<RepoSearchResult>(request, controller.signal);
        if (!controller.signal.aborted) setResponse({ request, data: result });
      } catch (problem) {
        if (!controller.signal.aborted) setFailure({ request, message: problem instanceof Error ? problem.message : String(problem) });
      } finally {
        if (!controller.signal.aborted) setLoading(false);
      }
    }, 75);
    return () => { window.clearTimeout(timer); controller.abort(); };
  }, [api, request, stamp, attempt, hasQuery]);

  function update(patch: Partial<RepoSearchOptions>) { onSearch({ ...search, ...patch }); }
  function focusResult() { results.current?.querySelector<HTMLAnchorElement>("a.search-hit")?.focus(); }
  const link = (path: string, line?: number) => ({
    href: fileHref(path, line),
    onClick: (event: React.MouseEvent<HTMLAnchorElement>) => {
      if (event.button !== 0 || event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return;
      event.preventDefault(); openFile(path, line);
    },
  });
  const stats = data?.stats;
  useEffect(() => {
    if (!stats?.index_pending || loading || error) return;
    const refresh = () => { if (document.visibilityState === "visible") setAttempt((value) => value + 1); };
    const timer = window.setTimeout(refresh, 1000);
    document.addEventListener("visibilitychange", refresh);
    return () => { window.clearTimeout(timer); document.removeEventListener("visibilitychange", refresh); };
  }, [data, loading, error]);
  return <section className="list repository-search" aria-label="Search repository files and code">
    <form className="repo-search-entry" role="search" onSubmit={(event) => { event.preventDefault(); setAttempt((n) => n + 1); }}>
      <label htmlFor="repo-query">Search this repository</label>
      <div><input id="repo-query" type="search" autoFocus maxLength={512} value={search.query} placeholder="Find a file or anything in the code…" onChange={(event) => update({ query: event.target.value })} onKeyDown={(event) => {
        if (event.key === "ArrowDown") { event.preventDefault(); focusResult(); }
        if (event.key === "Escape") { event.preventDefault(); event.stopPropagation(); search.query ? update({ query: "" }) : onClose(); }
      }} /><button className="btn" type="submit">Search</button></div>
    </form>
    <div className="search-status" role="status" aria-live="polite">
      <span>{loading ? "Searching…" : stats ? `${stats.matched_files} ${stats.matched_files === 1 ? "result" : "results"}${search.language || search.extension || search.category ? ` · ${search.language || search.extension || search.category}` : ""}` : ""}</span>
      <button className="link" onClick={onClose}>Browse files</button>
    </div>
    <div className="search-results" ref={results} aria-busy={loading} onKeyDown={(event) => {
      if (!["ArrowDown", "ArrowUp", "j", "k", "Home", "End", "Escape"].includes(event.key)) return;
      event.preventDefault(); event.stopPropagation();
      if (event.key === "Escape") { document.getElementById("repo-query")?.focus(); return; }
      const links = [...event.currentTarget.querySelectorAll<HTMLAnchorElement>("a.search-hit")];
      const index = links.indexOf(document.activeElement as HTMLAnchorElement);
      const next = event.key === "Home" ? 0 : event.key === "End" ? links.length - 1 : Math.max(0, Math.min(links.length - 1, index + (["ArrowDown", "j"].includes(event.key) ? 1 : -1)));
      links[next]?.focus();
    }}>
      {!hasQuery ? <div className="file-state"><p>Find files, code, and documentation in one search.</p></div> : loading && !data ? <div className="file-state"><span>Searching the repository…</span><div className="loading-lines" aria-hidden="true"><i /><i /><i /></div></div> : error ? <div className="file-state" role="alert"><b>Search unavailable</b><p>{error}</p><button className="btn" onClick={() => setAttempt((n) => n + 1)}>Try again</button></div> : data && <SearchResults data={data} link={link} />}
    </div>
    {data && <footer className="search-footnote">
      {stats?.index_pending && <p role="status">Results are updating.</p>}
      {(stats?.truncated || stats?.scan_limited) && <p className="search-limit" role="status">Showing a limited set of matches. Use a more specific search to see more.</p>}
      <span>Filenames and text · {data.commit.slice(0, 7)}</span>
      {(search.extension || search.language || search.category || search.include || search.exclude || search.regex || search.wholeWord || search.caseSensitive || search.ignored || !search.fuzzy) && <button className="link" onClick={() => onSearch(defaultSearch("all"))}>Clear filters</button>}
    </footer>}
  </section>;
}

export function SearchResults({ data, link }: { data: RepoSearchResult; link: (path: string, line?: number) => { href: string; onClick?: React.MouseEventHandler<HTMLAnchorElement> } }) {
  if (!data.files.length && data.stats.index_pending) return <div className="file-state" role="status"><b>Results are being prepared.</b></div>;
  if (!data.files.length) return <div className="file-state"><b>No matching files.</b><p>Try a shorter query, another type, or fewer path filters.</p></div>;
  return <>{data.files.map((file) => <section className="search-result-file" key={file.path}>
    <a className="search-hit search-file-path" {...link(file.path)}><span className="mono"><SearchHighlight text={file.path} ranges={file.positions.map((position) => [position, position + (file.path.codePointAt(position)! > 0xffff ? 2 : 1)])} /></span><span className="mute">{file.matches ? `${file.matches} ${file.matches === 1 ? "match" : "matches"}` : file.language || (!file.extension || file.extension === "none" ? "No extension" : `.${file.extension}`)}</span></a>
    {file.lines.slice(0, 2).map((line) => <a className="search-hit search-line" key={line.line} {...link(file.path, line.line)} aria-label={`${file.path}, line ${line.line}: ${line.text}`}><span className="search-line-number">{line.line}</span><span className="search-line-text"><SearchHighlight text={line.text} ranges={line.ranges} /></span></a>)}
    {file.lines.length > 2 && <a className="search-hit search-file-more" {...link(file.path, file.lines[2].line)}>More matches in this file</a>}
  </section>)}</>;
}
