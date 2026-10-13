"use client";

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Api, changeOfNumber, numbers, type BranchesResult, type RepoDigestResult, type Snapshot, type TreeResult, useDetail, useSnapshot } from "./api";
import { CloneBox, CloneButton } from "./components/Clone";
import { Palette } from "./components/Palette";
import { Mark } from "./components/Mark";
import { ExperienceSwitch } from "./components/Experience";
import { bare, type RepoView } from "./hub";
import { isTaskId, orgUrl, parseLocation, prNumberOf, routeUrl, VIEWS, type Route, type View } from "./route";
import { copy, short } from "./util";
import { AgentDetail } from "./views/AgentDetail";
import { ChangeDetail } from "./views/ChangeDetail";
import { FileDetail } from "./views/FileDetail";
import { ReleasePane } from "./views/Releases";
import { TaskDetail, NewTask } from "./views/TaskDetail";
import { buildItems, ListPane, type Item } from "./views/lists";
import { useMounted } from "./components/time";
import { RepoReadme } from "./views/RepoReadme";
import { RepoFiles } from "./views/RepoFiles";
import { RepoSearch } from "./views/RepoSearch";
import { RepoDigest } from "./views/RepoDigest";
import { defaultSearch, type RepoSearchOptions } from "./search";
import { repositoryLink } from "./readme";

/** `viewer`: the signed-in user, when the page opened without a link of its own. */
export function Shell(props: { initialRoute: Route; initialSnap: Snapshot | null; initialRoot?: TreeResult | null; initialReadme?: TreeResult | null; initialDigest?: RepoDigestResult | null; repo: RepoView | null; viewer?: string | null }) {
  const [route, setRoute] = useState<Route>(props.initialRoute);
  const repo = props.repo;
  const api = useMemo(() => new Api(route.trunk, route.key, route.user), [route.trunk, route.key, route.user]);
  const { snap, error, live, refresh } = useSnapshot(api, 2000, props.initialSnap);
  const [palette, setPalette] = useState(false);
  const [help, setHelp] = useState(false);
  const [creating, setCreating] = useState(false);
  const [fileFocus, setFileFocus] = useState(0);
  const dir = route.view === "repo" && route.id?.startsWith("dir:") ? route.id.slice(4) : route.view === "repo" && route.id?.startsWith("file:") ? route.id.slice(5).split("/").slice(0, -1).join("/") : "";
  const [branches, setBranches] = useState<BranchesResult | null>(null);
  const [scope, setScope] = useState("");
  const [find, setFind] = useState("");
  // The row the user picked. Activity can list one change many times, so the key, not the id, marks the row.
  const [selKey, setSelKey] = useState<string | null>(null);
  const prefix = useRef<string | null>(null);

  // Code reads the README and directories independently of sequencer state. Agent
  // branches refresh separately, since each fork is a request to Artifacts.
  const fileStamp = snap?.head || "initial";
  const root = useDetail<TreeResult>(api, route.view === "repo" ? "tree?path=" : null, fileStamp, props.initialRoot);
  const directory = useDetail<TreeResult>(api, route.view === "repo" && dir ? `tree?path=${encodeURIComponent(dir)}` : null, fileStamp);
  const files = dir ? directory : root;
  const tree = files.data;
  useEffect(() => {
    if (route.view !== "repo") return;
    const load = () => api.get<BranchesResult>("branches").then(setBranches, () => setBranches(null));
    load();
    const every = setInterval(load, 10_000);
    return () => clearInterval(every);
  }, [api, route.view, snap?.agents.length]);

  useEffect(() => {
    // Pull requests opens on the open ones, as a git host does.
    setScope(route.view === "pulls" ? "Open" : "");
    setFind("");
  }, [route.view]);

  // A PR's link carries its number: /pulls/12.
  const nums = useMemo(() => numbers(snap), [snap]);
  const go = useCallback(
    (view: View, id: string | null = null, replace = false) => {
      const next = { ...route, view, id };
      setRoute(next);
      const url = routeUrl(next, (c) => nums.get(c));
      if (replace) window.history.replaceState(null, "", url);
      else window.history.pushState(null, "", url);
    },
    [route, nums],
  );

  useEffect(() => {
    const onPop = () => {
      const { view, id, search } = parseLocation(window.location);
      const n = prNumberOf(view, id);
      setRoute((r) => ({ ...r, view, search, id: n !== null && snap ? (changeOfNumber(snap, n) ?? id) : id }));
    };
    window.addEventListener("popstate", onPop);
    return () => window.removeEventListener("popstate", onPop);
  }, [snap]);

  const searchRepo = useCallback((search: RepoSearchOptions, replace = true) => {
    const next = { ...route, view: "repo" as const, id: replace && route.view === "repo" ? route.id : null, search };
    setRoute(next);
    window.history[replace ? "replaceState" : "pushState"](null, "", routeUrl(next));
  }, [route]);
  const startSearch = (mode: RepoSearchOptions["mode"] = "all", preset: Partial<RepoSearchOptions> = {}) => {
    searchRepo({ ...defaultSearch(mode), ...preset }, false);
    requestAnimationFrame(() => document.getElementById("repo-query")?.focus());
  };
  const closeSearch = () => {
    const next = { ...route, search: undefined };
    setRoute(next);
    window.history.pushState(null, "", routeUrl(next));
  };
  const openSearchFile = (path: string, line?: number) => {
    const next = { ...route, view: "repo" as const, id: `file:${path}` };
    setRoute(next);
    setFileFocus((value) => value + 1);
    window.history.pushState(null, "", routeUrl(next) + (line ? `#L${line}` : ""));
    if (line) requestAnimationFrame(() => document.getElementById(`L${line}`)?.scrollIntoView({ block: "center" }));
  };

  const mounted = useMounted();
  const all = useMemo(() => (snap ? buildItems(route.view, snap, tree, mounted, branches) : []), [route.view, snap, tree, mounted, branches]);
  // A scope with no rows (say, Open when nothing is open) shows everything instead.
  const shownScope = scope && all.some((it) => it.group === scope) ? scope : "";
  const items = useMemo(() => {
    const words = find.toLowerCase().split(/\s+/).filter(Boolean);
    return all.filter((it) => (!shownScope || it.group === shownScope) && words.every((w) => it.text.toLowerCase().includes(w)));
  }, [all, shownScope, find]);
  const byKey = selKey ? items.findIndex((it) => it.key === selKey && it.id === route.id) : -1;
  const selected = byKey >= 0 ? byKey : route.id ? items.findIndex((it) => it.id === route.id) : -1;

  // dir is the direction of travel: a batch line in the graph is skipped, not selected.
  const select = useCallback(
    (i: number, dir = 1) => {
      while (items[i]?.head) i += dir;
      const it: Item | undefined = items[i];
      if (!it) return;
      if (it.dir) {
        go("repo", it.id ? `dir:${it.id}` : null, true);
        return;
      }
      setSelKey(it.key);
      go(route.view, it.id, true);
      requestAnimationFrame(() => document.querySelector(".row.sel")?.scrollIntoView({ block: "nearest" }));
    },
    [items, go, route.view],
  );

  const selectedChange =
    route.view === "repo" ? (route.id?.startsWith("pr:") ? route.id.slice(3) : null) : route.view !== "tasks" && route.view !== "agents" && route.id && !isTaskId(route.id) ? route.id : null;
  const parkedSelected = selectedChange && snap?.pending.find((c) => c.change_id === selectedChange && c.state === "awaiting_seal");
  const copySeal = parkedSelected
    ? () => copy(`nerudo seal --trunk ${route.trunk} --change ${parkedSelected.change_id} --rev ${parkedSelected.rev}`)
    : undefined;

  // Some shortcuts act on one tab of the detail pane. Open that tab first. A PR has
  // Conversation; a task has Overview and Discussion: the first that exists wins.
  const onTab = (tabs: string | string[], then: () => void) => {
    const tab = [tabs].flat().find((t) => document.querySelector(`.detail [data-tab="${t}"]`));
    const button = tab ? document.querySelector<HTMLButtonElement>(`.detail [data-tab="${tab}"]`) : null;
    if (button && button.getAttribute("aria-selected") !== "true") {
      button.click();
      requestAnimationFrame(then);
    } else then();
  };

  // The keyboard. Typing in a field never triggers a shortcut.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (help) return;
      if (!palette && !creating && (e.metaKey || e.ctrlKey) && (e.key.toLowerCase() === "p" || (e.shiftKey && e.key.toLowerCase() === "f"))) {
        e.preventDefault();
        startSearch("all");
        return;
      }
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "k") {
        e.preventDefault();
        setPalette((p) => !p);
        return;
      }
      const t = e.target;
      if (palette || creating || e.defaultPrevented || (t instanceof Element && t.closest("input, textarea, select, [contenteditable], .code-scroll, .md-table-scroll, .repository-search"))) return;
      if (["ArrowDown", "ArrowUp", "Home", "End", "PageDown", "PageUp", " "].includes(e.key) && t instanceof Element && t.closest(".repo-browser-scroll")) return;
      if (["Enter", " ", "ArrowDown", "ArrowUp", "Home", "End"].includes(e.key) && t instanceof Element && t.closest("button, a, summary, [role=tab]")) return;
      if (e.metaKey || e.ctrlKey || e.altKey) return;
      if (prefix.current === "g") {
        prefix.current = null;
        const v = VIEWS.find((v) => v.key === e.key);
        if (v) {
          e.preventDefault();
          go(v.id);
        }
        return;
      }
      switch (e.key) {
        case "g":
          prefix.current = "g";
          setTimeout(() => (prefix.current = null), 900);
          break;
        case "j":
        case "ArrowDown":
          e.preventDefault();
          select(Math.min(items.length - 1, selected + 1));
          break;
        case "k":
        case "ArrowUp":
          e.preventDefault();
          select(Math.max(0, selected - 1), -1);
          break;
        case "/":
          e.preventDefault();
          if (route.view === "repo") startSearch();
          else document.getElementById("find")?.focus();
          break;
        case "Enter":
          if (selected >= 0) (document.querySelector(".detail") as HTMLElement | null)?.focus();
          break;
        case "Escape":
          setHelp(false);
          if (route.id) go(route.view, null, true);
          break;
        case "c":
          if (route.view === "tasks" && snap?.me) {
            e.preventDefault();
            setCreating(true);
          } else if (document.querySelector(".detail [data-tab='conversation'], .detail [data-tab='discussion']")) {
            e.preventDefault();
            onTab(["conversation", "discussion"], () => document.getElementById("composer")?.focus());
          }
          break;
        case "s":
          onTab(["conversation", "overview"], () => document.getElementById("copy-seal")?.click());
          break;
        case "r":
          onTab(["conversation", "overview"], () => document.getElementById("review-select")?.focus());
          break;
        case "?":
          setHelp((h) => !h);
          break;
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [palette, creating, help, items, selected, select, go, route, snap]);

  const stamp = `${snap?.seq ?? 0}:${snap?.events[0]?.at_ms ?? 0}`;

  const counts: Partial<Record<View, number>> = snap
    ? {
        pulls: snap.pending.length,
        tasks: snap.tasks.filter((i) => !["done", "canceled"].includes(i.status)).length,
        inbox:
          (snap.needs_me?.reviews.length ?? 0) +
          snap.pending.filter((c) => c.state === "awaiting_seal" || c.state === "awaiting_allowance").length +
          (snap.me
            ? snap.tasks.filter((t) => !t.picked && (t.pick || "person") === "person" && snap.pending.some((c) => c.task === t.id && c.state === "awaiting_pick")).length
            : 0),
      }
    : {};
  const queued = snap?.pending.filter((c) => c.state === "queued").length ?? 0;

  const repoHome = route.view === "repo" && (!route.id || route.id.startsWith("dir:"));
  let detail: React.ReactNode = repoHome ? <div className="repo-overview"><RepoDigest api={api} stamp={fileStamp} initial={props.initialDigest} openFile={(path) => go("repo", `file:${path}`)} onSearch={(preset) => startSearch("all", preset)} /><RepoReadme api={api} root={root} route={route} stamp={fileStamp} initial={props.initialReadme} /></div> : <Idle snap={snap} view={route.view} repo={repo} />;
  if (route.view === "repo" && route.id?.startsWith("file:")) {
    const path = route.id.slice(5);
    detail = <FileDetail key={`${route.id}:${fileFocus}`} api={api} path={path} stamp={fileStamp} sourceFirst={Boolean(route.search)} fileLink={(href) => repositoryLink(route, path, href)} openDirectory={(path) => go("repo", `dir:${path}`)} openChange={(id) => go("graph", id)} />;
  } else if (snap && route.id) {
    if (route.view === "releases" || route.id.startsWith("release:")) {
      detail = <ReleasePane key={route.id} api={api} snap={snap} id={route.id} stamp={stamp} openChange={(id) => go("graph", id)} />;
    } else if (route.view === "agents") {
      detail = <AgentDetail key={route.id} api={api} snap={snap} name={route.id} stamp={stamp} openChange={(id) => go("pulls", id)} />;
    } else if (route.view === "repo") {
      // Repo rows carry a kind: file:<path>, pr:<change>, fork:<agent>, or main.
      const [kind, ...rest] = route.id.split(":");
      const what = rest.join(":");
      if (kind === "pr") detail = <ChangeDetail key={route.id} api={api} snap={snap} id={what} stamp={stamp} openTask={(id) => go("tasks", id)} openAgent={(a) => go("agents", a)} openChange={(id) => go("pulls", id)} />;
      else if (kind === "fork") detail = <AgentDetail key={route.id} api={api} snap={snap} name={what} stamp={stamp} openChange={(id) => go("pulls", id)} />;
    } else if (isTaskId(route.id)) {
      detail = <TaskDetail key={route.id} api={api} snap={snap} id={route.id} stamp={stamp} openChange={(id) => go("pulls", id)} />;
    } else {
      detail = (
        <ChangeDetail key={route.id} api={api} snap={snap} id={route.id} stamp={stamp} openTask={(id) => go("tasks", id)} openAgent={(a) => go("agents", a)} openChange={(id) => go("pulls", id)} />
      );
    }
  }

  return (
    <div className={`app${route.id && !repoHome ? " has-detail" : ""}${repo ? " named" : ""}`}>
      <header className="titlebar">
        {repo ? (
          <a className="home" href="/" title="Explore">
            <Mark />
            <span className="wordmark">NERUDO</span>
          </a>
        ) : (
          <a className="home" href={routeUrl({ ...route, view: "inbox", id: null })} onClick={(e) => (e.preventDefault(), go("inbox"))}>
            <Mark />
            <span className="wordmark">NERUDO</span>
          </a>
        )}
        {!repo && <span className="tb-trunk">{route.trunk}</span>}
        <span className="tb-dim mono">
          {snap?.branch.replace("refs/heads/", "")} @ {short(snap?.head)}
        </span>
        <span className="spacer" />
        <span className={`tb-live${live ? " on" : ""}`} title={error ?? "Live: the page reads the repo every 2 s"}>
          <i /> {live ? "live" : "offline"} · seq {snap?.seq ?? "—"}
          {queued > 0 && <> · {queued} queued</>}
        </span>
        <button className="tb-link" onClick={() => setPalette(true)}>
          Find… <kbd>⌘K</kbd>
        </button>
        <button className="tb-link" onClick={() => setHelp(true)}>
          Keys
        </button>
        <ExperienceSwitch className="shell-experience" />
        <span className="tb-me" title={snap?.me ? undefined : props.viewer ? "This repo does not list you as a person: you can read it" : "Sign in, or open your personal link, to approve, comment or write tasks"}>
          {snap?.me ? (
            props.viewer === snap.me.name ? <a href={`/${encodeURIComponent(snap.me.name)}`}>{snap.me.name}</a> : <>{snap.me.name}</>
          ) : props.viewer ? (
            <>
              <a href={`/${encodeURIComponent(props.viewer)}`}>{props.viewer}</a> · read-only here
            </>
          ) : (
            "read-only"
          )}
        </span>
      </header>
      {repo && <RepoBar repo={repo} />}

      <nav className="menubar" aria-label="Repository views">
        <span className="workspace-label">Workspace</span>
        {VIEWS.map((v) => (
          <button key={v.id} aria-current={route.view === v.id ? "page" : undefined} className={route.view === v.id ? "on" : ""} onClick={() => go(v.id)} title={`${v.label} (g ${v.key})`}>
            {v.label}
            {counts[v.id] ? <span className={`count${v.id === "inbox" ? " hot" : ""}`}>{counts[v.id]}</span> : null}
          </button>
        ))}
      </nav>

      <main className={`panes${route.view === "graph" ? " wide" : ""}${repoHome ? " repo-home" : ""}${route.view === "repo" && route.search ? ` searching${route.id?.startsWith("file:") ? " has-search-file" : ""}` : ""}`} id="main-content" tabIndex={-1}>
        {route.view === "repo" && route.search ? <RepoSearch api={api} stamp={fileStamp} search={route.search} onSearch={searchRepo} onClose={closeSearch} openFile={openSearchFile} fileHref={(path, line) => routeUrl({ ...route, view: "repo", id: `file:${path}` }) + (line ? `#L${line}` : "")} /> : route.view === "repo" ? <RepoFiles tree={tree} loading={files.loading} error={files.error} dir={dir} branches={branches} releases={snap?.releases} selected={route.id?.startsWith("file:") ? route.id.slice(5) : null} retry={files.reload} onSearch={(query) => startSearch("all", { query: query || "" })} openDirectory={(path) => go("repo", path ? `dir:${path}` : null)} openFile={(path) => go("repo", `file:${path}`)} openBranch={(id) => go("repo", id)} /> : snap ? (
          <ListPane
            view={route.view}
            all={all}
            items={items}
            selected={selected}
            onSelect={select}
            snap={snap}
            scope={shownScope}
            onScope={setScope}
            find={find}
            onFind={setFind}
            onNewTask={() => setCreating(true)}
          />
        ) : (
          <section className="list">
            <div className="list-box repo-load-state" aria-busy={!error}>
              {error ? <div className="repo-load-error" role="status"><Mark size={32} /><h2>Repository unavailable</h2><p>We couldn’t load this repository. Your current view is preserved.</p><button className="btn" onClick={refresh}>Try again</button><details><summary>Connection details</summary><p>{error}</p></details></div> : <div className="repo-loading" role="status"><span>Loading repository…</span><div className="loading-lines" aria-hidden="true"><i /><i /><i /><i /></div></div>}
            </div>
          </section>
        )}

        <section className="detail-pane" aria-label="Selected item" tabIndex={-1}>
          {route.id && !repoHome && (
            <button className="btn back" onClick={() => go(route.view, null, true)}>
              ◂ Back
            </button>
          )}
          {detail}
        </section>
      </main>

      <Palette
        open={palette}
        onClose={() => setPalette(false)}
        snap={snap}
        actions={{
          go: (v, id) => go(v, id ?? null),
          newTask: snap?.me ? () => (go("tasks"), setCreating(true)) : undefined,
          copySeal,
          searchFiles: () => startSearch("all"),
          searchContents: () => startSearch("all"),
          help: () => setHelp(true),
        }}
      />
      {creating && snap && (
        <NewTask
          api={api}
          snap={snap}
          onClose={() => setCreating(false)}
          onCreated={(id) => {
            setCreating(false);
            refresh();
            go("tasks", id);
          }}
        />
      )}
      {help && <Help onClose={() => setHelp(false)} />}
    </div>
  );
}

/** The repo's name as on a git host, whether anyone can read it, where a replay comes from, and how to clone it. */
function RepoBar({ repo }: { repo: RepoView }) {
  return (
    <div className="repobar">
      <span className="crumb">
        <a href={orgUrl(repo.org)}>{repo.org}</a>
        <span className="crumb-sep">/</span>
        <b>{repo.repo}</b>
      </span>
      <span className="chip">{repo.public ? "Public" : "Private"}</span>
      {repo.upstream && (
        <span className="upstream">
          Replayed from{" "}
          <a href={repo.upstream} target="_blank" rel="noopener noreferrer">
            {bare(repo.upstream)}
          </a>
        </span>
      )}
      <span className="spacer" />
      <CloneButton name={repo} />
    </div>
  );
}

function Idle({ snap, view, repo }: { snap: Snapshot | null; view: View; repo: RepoView | null }) {
  if (!snap) return <div className="workspace-empty" aria-hidden="true"><Mark size={64} /><p>Good work takes form.</p></div>;
  const st = snap.stats as Record<string, number>;
  const per = st.landed ? (st.ci_runs / st.landed).toFixed(2) : "—";
  return (
    <div className="idle">
      <fieldset className="box idle-box">
        <legend>{repo ? `${repo.org}/${repo.repo}` : snap.trunk}</legend>
        <table className="props-table">
          <tbody>
            <Num v={snap.seq} l="Merged" />
            <Num v={st.batches ?? 0} l="Batches" />
            <Num v={per} l="CI runs per PR" />
            <Num v={(st.ejected_conflict ?? 0) + (st.ejected_ci ?? 0)} l="Sent back to rebase (conflict or checks)" />
            <Num v={snap.pending.length} l="Open" />
            <Num v={snap.pending.filter((c) => c.state === "awaiting_seal" || c.state === "awaiting_allowance").length} l="Need approval" />
            <Num v={snap.pending.filter((c) => c.state === "awaiting_pick").length} l="Waiting for pick" />
            <Num v={snap.tasks.filter((t) => !["done", "canceled"].includes(t.status)).length} l="Open tasks" />
          </tbody>
        </table>
      </fieldset>
      {repo?.description && <p className="idle-hint">{repo.description}</p>}
      {repo && <CloneBox name={repo} />}
      <p className="idle-hint">
        {view === "tasks" ? "Pick a task, or press c to write one." : view === "graph" ? "Pick a pull request in the graph, or press ⌘K to find anything." : "Pick a row on the left, or press ⌘K to find anything."} <span className="mute">j / k moves · / finds · ? shows the keys</span>
      </p>
    </div>
  );
}

function Num({ v, l }: { v: number | string; l: string }) {
  return (
    <tr>
      <th>{l}</th>
      <td className="mono">{v}</td>
    </tr>
  );
}

function Help({ onClose }: { onClose: () => void }) {
  const dialog = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const previous = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    dialog.current?.querySelector<HTMLButtonElement>("button")?.focus();
    return () => previous?.focus();
  }, []);
  const keys: [string, string][] = [
    ["⌘K", "find anything, or run a command"],
    ["⌘/Ctrl P", "fuzzy find files across the repository"],
    ["⌘/Ctrl ⇧F", "search text within repository files"],
    ["j / k", "next / previous row"],
    ["/", "find in the list"],
    ["g then r g p t l i a", "Code, Graph, Pull requests, Tasks, Releases, Inbox, Agents"],
    ["c", "new task (in Tasks), or comment"],
    ["s", "copy the approve command"],
    ["r", "ask for a review"],
    ["esc", "close"],
  ];
  return (
    <div className="modal-back" onClick={onClose}>
      <div className="modal help" ref={dialog} role="dialog" aria-modal="true" aria-labelledby="keyboard-title" onClick={(e) => e.stopPropagation()} onKeyDown={(e) => {
        if (e.key === "Escape") { e.stopPropagation(); onClose(); }
        if (e.key === "Tab") { e.preventDefault(); dialog.current?.querySelector<HTMLButtonElement>("button")?.focus(); }
      }}>
        <div className="modal-title">
          <span id="keyboard-title">Keyboard shortcuts</span>
          <button className="modal-x" onClick={onClose} aria-label="Close">
            ✕
          </button>
        </div>
        <div className="modal-body">
          <table className="grid">
            <tbody>
              {keys.map(([k, d]) => (
                <tr key={k}>
                  <td>
                    <kbd>{k}</kbd>
                  </td>
                  <td>{d}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      </div>
    </div>
  );
}
