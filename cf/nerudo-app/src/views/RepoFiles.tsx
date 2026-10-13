import { useEffect, useRef, useState } from "react";
import type { BranchesResult, ReleaseMark, TreeResult } from "../api";
import { Badge } from "../components/classic";
import { STATE } from "../util";

export function RepoFiles({ tree, loading, error, dir, branches, releases, selected, retry, openDirectory, openFile, openBranch, onSearch }: { tree: TreeResult | null; loading: boolean; error: string | null; dir: string; branches: BranchesResult | null; releases?: ReleaseMark[]; selected?: string | null; retry: () => void; openDirectory: (path: string) => void; openFile: (path: string) => void; openBranch: (id: string) => void; onSearch: (query?: string) => void }) {
  const [find, setFind] = useState("");
  const scroller = useRef<HTMLDivElement>(null);
  const rows = useRef<HTMLDivElement>(null);
  useEffect(() => { scroller.current?.scrollTo({ top: 0 }); }, [dir]);
  const entries = tree?.entries || [];
  const parts = dir.split("/").filter(Boolean);
  const base = dir ? `${dir}/` : "";
  return <section className="list repository-files" aria-label="Repository files">
    <div className="repo-files-heading"><h2>Files</h2><span className="mono mute">{branches?.branch.replace("refs/heads/", "") || "Trunk"}</span></div>
    <form className="repo-search-entry" role="search" onSubmit={(event) => { event.preventDefault(); onSearch(find); }}><label htmlFor="find">Search this repository</label><div><input id="find" type="search" aria-label="Search this repository" placeholder="Find a file or anything in the code…" value={find} onChange={(event) => setFind(event.target.value)} /><button className="btn" type="submit">Search</button></div></form>
    <nav className="file-breadcrumbs" aria-label="Directory"><button onClick={() => { setFind(""); openDirectory(""); }}>Root</button>{parts.map((part, index) => <span key={index}><span aria-hidden="true"> / </span><button onClick={() => { setFind(""); openDirectory(parts.slice(0, index + 1).join("/")); }}>{part}</button></span>)}</nav>
    <div className="repo-browser-scroll" ref={scroller} role="region" aria-label="Files, branches, and tags" tabIndex={0}>
    <div className="list-box file-rows" ref={rows} aria-busy={loading} onKeyDown={(event) => {
      if (!["ArrowDown", "ArrowUp", "Home", "End"].includes(event.key)) return;
      const buttons = [...event.currentTarget.querySelectorAll<HTMLButtonElement>(".file-row")];
      if (!buttons.length) return;
      event.preventDefault(); event.stopPropagation();
      const index = buttons.indexOf(document.activeElement as HTMLButtonElement);
      const next = event.key === "Home" ? 0 : event.key === "End" ? buttons.length - 1 : Math.max(0, Math.min(buttons.length - 1, index + (event.key === "ArrowDown" ? 1 : -1)));
      buttons[next].focus();
    }}>
      {error ? <div className="file-state" role="status"><b>Files unavailable</b><p>We couldn’t read this directory.</p><button className="btn" onClick={retry}>Try again</button></div> : !tree ? <div className="file-state" role="status">Loading files…</div> : <>
        {dir && <button className="file-row parent" onClick={() => { setFind(""); openDirectory(parts.slice(0, -1).join("/")); }}><Badge tone="dir" /><span>..</span><span className="mute">Parent</span></button>}
        {entries.map((entry) => <button className={`file-row${selected === base + entry.name ? " selected" : ""}`} aria-current={selected === base + entry.name ? "page" : undefined} key={entry.name} onClick={() => { if (entry.dir) { setFind(""); openDirectory(base + entry.name); } else openFile(base + entry.name); }}><Badge tone={entry.dir ? "dir" : "file"} /><span>{entry.name}{entry.dir && "/"}</span>{entry.dir && <span aria-hidden="true">→</span>}</button>)}
        {!entries.length && <div className="file-state">This directory is empty.</div>}
      </>}
    </div>
    {branches && <details className="repo-branches"><summary>Agent branches ({branches.forks.length})</summary>{branches.forks.map((fork) => <div className="repo-fork" key={fork.agent}><button className="link" onClick={() => openBranch(`fork:${fork.agent}`)}>{fork.agent}</button>{"ok" in fork.refs ? fork.refs.ok.map((ref) => <div key={ref.name}><button className="link mono" onClick={() => openBranch(ref.change ? `pr:${ref.change.change_id}` : `fork:${fork.agent}`)}>{ref.name.replace("refs/heads/", "")}</button>{ref.change?.state && <span className={`t-${STATE[ref.change.state].tone}`}> · {STATE[ref.change.state].label}</span>}{ref.stale && <span className="mute"> · stale</span>}</div>) : <p className="mute">Branch details unavailable.</p>}</div>)}</details>}
    {!!releases?.length && <details className="repo-branches"><summary>Tags ({releases.length})</summary>{releases.map((release) => <div className="repo-fork" key={release.tag}><button className="link mono" onClick={() => openBranch(`release:${release.tag}`)}>{release.tag}</button>{release.prerelease && <span className="mute"> · pre-release</span>}</div>)}</details>}
    </div>
    <p className="file-count mute">{entries.length} {entries.length === 1 ? "entry" : "entries"}</p>
  </section>;
}
