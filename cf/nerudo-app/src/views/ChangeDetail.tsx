import { useState, type ReactNode } from "react";
import type { Api, Check, ChangeDetail as Data, CommitRow, ContextResult, Prior, Snapshot, TimelineLine } from "../api";
import { useDetail } from "../api";
import { Rich } from "../components/bits";
import { Badge, Box, Props, Tabs, Tree, type TreeItem } from "../components/classic";
import { ChangeContext } from "../components/Context";
import { Diff } from "../components/Diff";
import { Composer, ReactionBar } from "../components/Thread";
import { ago, copy, labelsFor, mergeStatus, planLines, prRef, short, STATE, stateLabel, subjectOf } from "../util";
import { Ago, Clock, useMounted } from "../components/time";
import { DecisionBox } from "./Decision";

/** A pull request, laid out the way a git host shows one: the header and what it wants to
 *  merge, then Conversation, Commits, Checks, Files changed and Context, with a sidebar. */
export function ChangeDetail(props: {
  api: Api;
  snap: Snapshot;
  id: string;
  stamp: string;
  openTask: (id: string) => void;
  openAgent: (a: string) => void;
  openChange?: (id: string) => void;
}) {
  const [rev, setRev] = useState<number | null>(null);
  const path = `change/${encodeURIComponent(props.id)}${rev ? `?rev=${rev}` : ""}`;
  const { data, error, loading, reload } = useDetail<Data>(props.api, path, props.stamp);
  const mounted = useMounted();
  if (error) return <div className="detail-pad err">{error}</div>;
  if (!data) return <div className="detail-pad mute">{loading ? "Loading…" : ""}</div>;
  const c = data.change;
  const me = props.snap.me?.name ?? null;
  const checks = data.checks ?? [];
  const commits = data.commits ?? [];
  const failed = checks.filter((k) => k.status === "fail").length;
  return (
    <article className="detail" tabIndex={-1}>
      <header className="d-head">
        <Badge tone={STATE[c.state].tone} size={20} />
        <h1 className="d-title">
          <span className="pr-num">{prRef(c)}</span> {subjectOf(c.change_id, c.subject) || `rev ${c.rev}`}
        </h1>
        <span className="d-head-right">
          <span className={`chip c-${STATE[c.state].tone}`}>{stateLabel(c)}</span>
          {data.revisions.length > 1 && (
            <select value={c.rev} onChange={(e) => setRev(Number(e.target.value))} aria-label="Revision">
              {data.revisions.map((r) => (
                <option key={r.rev} value={r.rev}>
                  rev {r.rev} · {stateLabel({ state: r.state, reason: r.kind ? { kind: r.kind } : null })}
                </option>
              ))}
            </select>
          )}
        </span>
      </header>
      <WantsToMerge data={data} openAgent={props.openAgent} />

      <Tabs
        tabs={[
          ["conversation", `Conversation (${data.thread.comments.length})`],
          ["commits", `Commits (${commits.length || data.revisions.length})`],
          ["checks", failed ? `Checks (${failed} failed)` : `Checks (${checks.length})`],
          ["files", `Files changed (${data.files?.length ?? data.size?.files ?? 0})`],
          ["context", `Context (${data.revisions.filter((r) => r.context).length})`],
        ]}
      >
        {(tab) => {
          if (tab === "commits") return <Commits data={data} onPick={setRev} />;
          if (tab === "checks") return <Checks checks={checks} />;
          if (tab === "files") return <Diff files={data.files} />;
          if (tab === "context") return <ChangeContext key={c.rev} api={props.api} id={c.change_id} rev={c.rev} />;
          return (
            <div className="cols">
              <div>
                <Conversation api={props.api} snap={props.snap} data={data} me={me} onDone={reload} openChange={props.openChange} />
              </div>
              <div>
                <Reviewers api={props.api} snap={props.snap} data={data} me={me} onDone={reload} />
                <Box title="Assignee">
                  <button className="link" onClick={() => props.openAgent(c.agent)}>
                    <Badge name={c.agent} size={14} /> {c.agent}
                  </button>
                </Box>
                <Labels data={data} snap={props.snap} />
                <Box title="Development">
                  {data.task ? (
                    <button className="chip" onClick={() => props.openTask(data.task!.id)}>
                      <span className="mono">{data.task.id}</span> {data.task.title}
                    </button>
                  ) : (
                    <span className="mute">No task linked. An agent links one with land's task field.</span>
                  )}
                </Box>
                {data.landing && <LandingBox data={data} />}
                <DecisionBox decisions={c.decisions} />
                <Box title="Path">
                  <Tree items={lifecycle(data, mounted)} />
                </Box>
              </div>
            </div>
          );
        }}
      </Tabs>
    </article>
  );
}

/** "a01 wants to merge 1 commit into main from fork:land/a01c3-1 · +12 −3 in 4 files". */
function WantsToMerge({ data, openAgent }: { data: Data; openAgent: (a: string) => void }) {
  const c = data.change;
  const size = data.size ?? c.size;
  const base = data.base_ref ?? "main";
  const head = data.head_ref ?? `land/${c.change_id}-${c.rev}`;
  const who = (
    <button className="link strong" onClick={() => openAgent(c.agent)}>
      {c.agent}
    </button>
  );
  let what: ReactNode;
  if (c.state === "landed") {
    what = (
      <>
        {who} merged 1 commit into <code>{base}</code> from <code>{head}</code> as <code>{short(data.landing?.commit_out)}</code> in batch #{c.batch}
      </>
    );
  } else if (c.state === "ejected") {
    what = (
      <>
        {who} wanted to merge 1 commit into <code>{base}</code> from <code>{head}</code>. {mergeStatus(c)}.
      </>
    );
  } else {
    what = (
      <>
        {who} wants to merge 1 commit into <code>{base}</code> from <code>{head}</code>
      </>
    );
  }
  return (
    <div className="pr-sub">
      <span>{what}</span>
      {size && (
        <span className="pr-size mono">
          <span className="add">+{size.adds}</span> <span className="del">−{size.dels}</span> in {size.files} file{size.files === 1 ? "" : "s"}
        </span>
      )}
    </div>
  );
}

/** The description, then the timeline with the comments in it, then what stands between
 *  the PR and main, then the comment box. */
function Conversation(props: { api: Api; snap: Snapshot; data: Data; me: string | null; onDone: () => void; openChange?: (id: string) => void }) {
  const { data } = props;
  const c = data.change;
  type Entry = { at: number; line?: TimelineLine; comment?: Data["thread"]["comments"][number] };
  const entries: Entry[] = [
    ...(data.timeline ?? []).map((line) => ({ at: line.at_ms, line })),
    ...data.thread.comments.map((comment) => ({ at: comment.comment.at_ms, comment })),
  ].sort((a, b) => a.at - b.at);
  return (
    <div className="conversation">
      <div className="comment">
        <Badge name={c.agent} size={22} />
        <div className="comment-main">
          <div className="comment-head">
            <b>{c.agent}</b>
            <span className="tag">agent</span>
            <span>opened this pull request</span>
            <span className="mute">
              <Ago ms={c.submitted_ms} /> ago
            </span>
          </div>
          <div className="comment-body">{data.why ? <Rich text={data.why} snap={props.snap} /> : <span className="mute">No description: the agent's commit has no body.</span>}</div>
          <div className="rx-row">
            <ReactionBar api={props.api} thread={c.change_id} reactions={data.thread.reactions} me={props.me} onDone={props.onDone} />
          </div>
        </div>
      </div>

      <ol className="tl">
        {entries.map((e, i) =>
          e.line ? (
            <li key={`l${i}`} className={`tl-line k-${e.line.kind}`}>
              <span className="tl-dot" aria-hidden>
                {GLYPH[e.line.kind] ?? "•"}
              </span>
              <span className="tl-text">{e.line.text}</span>
              <span className="tl-when mute">
                <Clock ms={e.line.at_ms} />
              </span>
            </li>
          ) : (
            <li key={`c${e.comment!.comment.id}`} className="tl-comment">
              <div className="comment">
                <Badge name={e.comment!.comment.author} size={22} />
                <div className="comment-main">
                  <div className="comment-head">
                    <b>{e.comment!.comment.author}</b>
                    {e.comment!.comment.agent && <span className="tag">agent</span>}
                    <span>commented</span>
                    <span className="mute">
                      <Clock ms={e.comment!.comment.at_ms} />
                    </span>
                  </div>
                  <div className="comment-body">
                    <Rich text={e.comment!.comment.body} snap={props.snap} />
                  </div>
                  <div className="rx-row">
                    <ReactionBar api={props.api} thread={c.change_id} comment={e.comment!.comment.id} reactions={e.comment!.reactions} me={props.me} onDone={props.onDone} />
                  </div>
                </div>
              </div>
            </li>
          ),
        )}
      </ol>

      <MergeBox data={data} api={props.api} openChange={props.openChange} />

      {props.me ? (
        <Composer api={props.api} snap={props.snap} thread={c.change_id} onDone={props.onDone} />
      ) : (
        <p className="mute small">Open your personal link to comment, react or ask for a review.</p>
      )}
    </div>
  );
}

const GLYPH: Record<string, string> = {
  opened: "→",
  parked: "!",
  approved: "✓",
  merged: "✓",
  ejected: "✕",
  review: "@",
  decision: "◐",
  task: "◆",
};

/** What stands between this PR and main, like a git host's merge box. */
function MergeBox({ data, api, openChange }: { data: Data; api: Api; openChange?: (id: string) => void }) {
  const c = data.change;
  if (c.state === "awaiting_seal" || c.state === "awaiting_allowance") return <Approval data={data} />;
  if (c.state === "ejected" && c.reason) return <Bounced data={data} api={api} openChange={openChange} />;
  if (c.state === "awaiting_pick") {
    return (
      <Box title="Waiting for pick">
        <p>
          One of {c.reason?.attempts ?? "several"} competing attempts for {c.reason?.task}. Its trial on main{" "}
          {c.reason?.passed === true ? "passed the checks" : c.reason?.passed === false ? "failed the checks" : "has no check verdict"}. It merges only if it is
          picked: open the task's Compare tab.
        </p>
      </Box>
    );
  }
  if (c.state === "landed") {
    return (
      <Box title="Merged">
        <p>
          Merged into main as <code>{short(data.landing?.commit_out)}</code> in batch #{c.batch}, with {c.ci_runs} CI run{c.ci_runs === 1 ? "" : "s"} for the batch.
          {c.sealed_by.length > 0 && <> Approved by {c.sealed_by.join(", ")}.</>}
        </p>
      </Box>
    );
  }
  return (
    <Box title="Merge queue">
      <p>
        {mergeStatus(c)}. Nobody presses merge: the sequencer merges it in the next batch whose paths don't overlap, after one CI run for the whole batch.
      </p>
    </Box>
  );
}

function Commits({ data, onPick }: { data: Data; onPick: (rev: number) => void }) {
  const rows: CommitRow[] =
    data.commits ??
    data.revisions.map((r) => ({ rev: r.rev, sha: "", base: "", state: r.state, subject: "", author: data.change.agent, at_ms: data.change.submitted_ms }));
  return (
    <Box title={`Commits on ${data.head_ref?.split(":")[0] ?? "the agent's fork"}`}>
      <p className="mute small">Each revision is one commit. A closed revision was rebased and sent again. Pick a row to read that revision.</p>
      <table className="grid">
        <thead>
          <tr>
            <th>Revision</th>
            <th>Commit</th>
            <th>Message</th>
            <th>Author</th>
            <th>Date</th>
            <th>State</th>
          </tr>
        </thead>
        <tbody>
          {rows.map((r) => (
            <tr key={r.rev} className={`click${r.rev === data.change.rev ? " on" : ""}`} onClick={() => onPick(r.rev)}>
              <td className="mono">rev {r.rev}</td>
              <td className="mono">{short(r.sha)}</td>
              <td>{subjectOf(data.change.change_id, r.subject)}</td>
              <td>{r.author}</td>
              <td>
                <Clock ms={r.at_ms} />
              </td>
              <td>{stateLabel({ state: r.state, reason: { kind: data.revisions.find((x) => x.rev === r.rev)?.kind ?? undefined } })}</td>
            </tr>
          ))}
          {data.landing && (
            <tr>
              <td className="mono">main</td>
              <td className="mono">{short(data.landing.commit_out)}</td>
              <td>the commit on main (the sequencer adds trailers)</td>
              <td>{data.landing.author.split(" <")[0]}</td>
              <td>
                <Clock ms={data.landing.landed_ms} />
              </td>
              <td>Merged</td>
            </tr>
          )}
        </tbody>
      </table>
    </Box>
  );
}

const CHECK_GLYPH: Record<Check["status"], string> = { pass: "✓", fail: "✕", pending: "•", skip: "–" };

function Checks({ checks }: { checks: Check[] }) {
  if (!checks.length) return <p className="mute pad">The Worker sends no checks for this PR yet.</p>;
  const n = (s: Check["status"]) => checks.filter((k) => k.status === s).length;
  const summary = [n("fail") && `${n("fail")} failed`, n("pending") && `${n("pending")} pending`, n("pass") && `${n("pass")} passed`, n("skip") && `${n("skip")} skipped`].filter(Boolean).join(", ");
  return (
    <Box title={`Checks: ${summary}`}>
      <ul className="checks">
        {checks.map((k) => (
          <li key={k.name} className={`check ck-${k.status}`}>
            <span className="ck-glyph" aria-label={k.status}>
              {CHECK_GLYPH[k.status]}
            </span>
            <b>{k.name}</b>
            <span className="ck-detail">{k.detail}</span>
            {k.diagnostics && k.diagnostics.length > 0 && (
              <div className="ck-diags">
                {k.diagnostics.map((d, i) => (
                  <div key={i} className="diag mono">
                    <b>{d.code}</b> {d.file}:{d.line} <span>{d.text}</span>
                  </div>
                ))}
              </div>
            )}
          </li>
        ))}
      </ul>
    </Box>
  );
}

function Labels({ data, snap }: { data: Data; snap: Snapshot }) {
  const c = data.change;
  const task = c.task ? snap.tasks.find((t) => t.id === c.task) : undefined;
  const labels = labelsFor(
    (data.files ?? []).map((f) => f.path),
    [...c.seal_reasons, ...(data.landing?.seal_reasons ?? [])],
    task?.labels ?? [],
  );
  return (
    <Box title="Labels">
      {labels.length ? (
        labels.map((l) => (
          <span key={l} className={`chip lbl lbl-${l}`}>
            {l}
          </span>
        ))
      ) : (
        <span className="mute">None yet</span>
      )}
    </Box>
  );
}

/** Reviewers and their state: requested, approved (signed), or commented. */
function Reviewers(props: { api: Api; snap: Snapshot; data: Data; me: string | null; onDone: () => void }) {
  const [who, setWho] = useState("");
  const [error, setError] = useState<string | null>(null);
  const c = props.data.change;
  const state = new Map<string, "requested" | "approved" | "commented">();
  for (const r of props.data.reviews) state.set(r.request.reviewer, r.status === "sealed" ? "approved" : "requested");
  for (const s of c.sealed_by) state.set(s, "approved");
  for (const { comment } of props.data.thread.comments) {
    if (!comment.agent && comment.author !== c.agent && !state.has(comment.author)) state.set(comment.author, "commented");
  }
  const options = [
    ...props.snap.people.map((p) => ({ v: p.name, l: p.name })),
    ...props.snap.teams.map((t) => ({ v: "@" + t, l: "@" + t + " (team)" })),
    ...props.snap.agents.map((a) => ({ v: a, l: a + " (agent)" })),
  ];
  const ask = async () => {
    if (!who) return;
    try {
      await props.api.post("review", { change: c.change_id, reviewer: who });
      setWho("");
      setError(null);
      props.onDone();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  };
  return (
    <Box title="Reviewers">
      {state.size === 0 && <p className="mute">No reviews yet.</p>}
      {[...state].map(([name, s]) => (
        <div key={name} className="side-row">
          <Badge name={name} size={14} />
          <span>{name}</span>
          <span className={`chip c-${s === "approved" ? "landed" : s === "requested" ? "approval" : "idle"}`}>{s}</span>
        </div>
      ))}
      {props.me && (
        <div className="ask">
          <select id="review-select" value={who} onChange={(e) => setWho(e.target.value)}>
            <option value="">Ask for a review…</option>
            {options.map((o) => (
              <option key={o.v} value={o.v}>
                {o.l}
              </option>
            ))}
          </select>
          <button className="btn" disabled={!who} onClick={ask}>
            Request <kbd>r</kbd>
          </button>
          {error && <span className="err">{error}</span>}
        </div>
      )}
    </Box>
  );
}

/** The life of a change, from sent to landed. */
function lifecycle(data: Data, mounted: boolean): TreeItem[] {
  const c = data.change;
  const out: TreeItem[] = [{ label: "Opened", tone: "landed", state: "done", value: mounted ? ago(c.submitted_ms) + " ago" : "" }];
  const gated = c.footprint.length > 0 || c.state !== "queued";
  out.push({ label: "Paths checked", tone: gated ? "landed" : "queued", state: gated ? "done" : "now", value: gated ? `${c.footprint.length} path${c.footprint.length === 1 ? "" : "s"}` : "waiting" });
  for (const f of c.footprint.slice(0, 6)) out.push({ label: f, depth: 1, state: "done" });
  if (c.state === "awaiting_seal" || c.state === "awaiting_allowance") {
    out.push({ label: "Needs approval", tone: "approval", state: "hold", value: c.state === "awaiting_seal" ? "a signature" : "an allowance" });
    for (const t of data.quorum ?? []) out.push({ label: t.kind, depth: 1, value: `${t.have.length} of ${t.need}` });
    out.push({ label: "Batch", tone: "idle", state: "todo" }, { label: "CI", tone: "idle", state: "todo" }, { label: "Merged", tone: "idle", state: "todo" });
  } else if (c.state === "awaiting_pick") {
    const passed = c.reason?.passed;
    out.push({ label: "Trial", tone: passed === false ? "bounced" : "landed", state: passed === false ? "bad" : "done", value: passed === true ? "passed" : passed === false ? "failed" : "no verdict" });
    out.push({ label: "Waiting for pick", tone: "approval", state: "hold", value: c.reason?.task ?? "" });
    out.push({ label: "Batch", tone: "idle", state: "todo" }, { label: "CI", tone: "idle", state: "todo" }, { label: "Merged", tone: "idle", state: "todo" });
  } else if (c.state === "ejected" && c.reason?.kind === "not_picked") {
    out.push({ label: "Trial", tone: "landed", state: "done" });
    out.push({ label: "Not picked", tone: "bounced", state: "bad", value: `${c.reason.picked ?? "another"} shipped` });
  } else if (c.state === "ejected") {
    const ci = c.reason?.kind?.startsWith("ci");
    out.push({ label: "Batch", tone: "landed", state: "done" });
    out.push({ label: "CI", tone: ci ? "bounced" : "landed", state: ci ? "bad" : "done", value: ci ? (c.reason?.diagnostics?.[0]?.code ?? "failed") : "passed" });
    out.push({ label: stateLabel(c), tone: "bounced", state: "bad", value: "rebase on main" });
  } else if (c.state === "landed") {
    out.push({ label: "Batch", tone: "landed", state: "done", value: `#${c.batch}` });
    out.push({ label: "CI", tone: "landed", state: "done", value: `${c.ci_runs} run${c.ci_runs === 1 ? "" : "s"}` });
    if (c.sealed_by.length) out.push({ label: "Approved", tone: "landed", state: "done", value: c.sealed_by.join(", ") });
    out.push({ label: "Merged", tone: "landed", state: "done", value: `#${c.seq}` });
  } else {
    out.push({ label: "Batch", tone: gated ? "queued" : "idle", state: gated ? "now" : "todo" }, { label: "CI", tone: "idle", state: "todo" }, { label: "Merged", tone: "idle", state: "todo" });
  }
  return out;
}

function Bounced({ data, api, openChange }: { data: Data; api: Api; openChange?: (id: string) => void }) {
  const c = data.change;
  const r = c.reason!;
  return (
    <Box title={stateLabel(c)}>
      {r.kind === "not_picked" ? (
        <p>
          A competing attempt for {r.task}. <span className="mono">{r.picked}</span> was picked to ship, so this one closed. Nothing to fix; its branch is stale.
        </p>
      ) : r.kind === "ci_failed" ? (
        <>
          <p>The type check failed on main with this PR. It can pass on its own base: a PR that merged first can break it.</p>
          {r.diagnostics?.map((d, i) => (
            <div key={i} className="diag mono">
              <b>{d.code}</b> {d.file}:{d.line} <span>{d.text}</span>
            </div>
          ))}
          {r.moved && r.moved.length > 0 && <Priors title="Main moved under it: merged since its base" list={r.moved} api={api} openChange={openChange} />}
        </>
      ) : r.kind === "conflict" ? (
        <>
          <p>
            It edits lines that changed on main: <span className="mono">{(r.paths ?? []).join(", ")}</span>. The agent rebases it on main and opens it again
            {r.beaten_by?.length ? ", and keeps the decision of the PR that merged first unless its task says otherwise" : ""}.
          </p>
          {r.beaten_by && r.beaten_by.length > 0 && <Priors title="Beaten by" list={r.beaten_by} api={api} openChange={openChange} />}
          {Object.entries(r.hunks ?? {}).map(([path, text]) => (
            <div key={path} className="hunk">
              <div className="mono small">Conflict in {path} (main first, then this PR)</div>
              <pre className="mono why">{text}</pre>
            </div>
          ))}
        </>
      ) : r.kind === "ci_crashed" ? (
        <p>The type checker crashed. The agent can open the PR again.</p>
      ) : (
        <pre className="mono why">{JSON.stringify(r, null, 2)}</pre>
      )}
    </Box>
  );
}

/** The PRs that beat this one to its lines, or moved main under it: each links to its PR,
 *  with the first lines of the plan its merged commit pins. */
function Priors({ title, list, api, openChange }: { title: string; list: Prior[]; api: Api; openChange?: (id: string) => void }) {
  return (
    <div className="priors">
      <div className="small">
        <b>{title}</b>
      </div>
      <ul className="checks">
        {list.map((p) => (
          <li key={p.commit} className="check">
            <span className="ck-glyph">◆</span>
            {openChange ? (
              <button className="link strong" onClick={() => openChange(p.change_id)}>
                {prRef(p)}
              </button>
            ) : (
              <b>{prRef(p)}</b>
            )}
            <span className="ck-detail">
              <Badge name={p.agent} size={14} /> {p.agent} · {subjectOf(p.change_id, p.subject)} · <span className="mono">{short(p.commit)}</span>
              {p.paths?.length ? <span className="mute"> · {p.paths.join(", ")}</span> : null}
            </span>
            {p.context ? <PriorPlan api={api} p={p} /> : <div className="mute small">It merged without context.</div>}
          </li>
        ))}
      </ul>
    </div>
  );
}

function PriorPlan({ api, p }: { api: Api; p: Prior }) {
  const path = `context/${encodeURIComponent(p.change_id)}${p.rev ? `?rev=${p.rev}` : ""}`;
  const { data, error } = useDetail<ContextResult>(api, path, path);
  if (error) return <div className="mute small">Its plan can't be read: {error}</div>;
  if (!data) return <div className="mute small">Reading its plan…</div>;
  const lines = data.text ? planLines(data.text, 4) : [];
  if (!lines.length) return <div className="mute small">Its context has no plan.</div>;
  return (
    <blockquote className="plan small">
      {lines.map((l, i) => (
        <div key={i}>{l}</div>
      ))}
    </blockquote>
  );
}

function LandingBox({ data }: { data: Data }) {
  const l = data.landing!;
  const c = data.change;
  const rows: [string, ReactNode][] = [
    ["Seq on main", <span key="s" className="mono">{l.seq}</span>],
    ["Batch", <span key="b">#{l.batch} · {c.ci_runs} CI run{c.ci_runs === 1 ? "" : "s"}</span>],
    ["Merged at", <Clock key="c" ms={l.landed_ms} />],
    ["On main", <span key="o" className="mono">{short(l.commit_out)}</span>],
    ["Agent's commit", <span key="i" className="mono">{short(l.commit_in)}</span>],
  ];
  if (c.allocated.length) rows.push(["Numbered", <span key="n" className="mono">{c.allocated.join(", ")}</span>]);
  for (const r of l.seal_reasons) rows.push(["Needed approval", <span key={r} className="small wrap">{r}</span>]);
  // A Sealed-By trailer is "name key time signature": show the name, keep the rest on hover.
  for (const s of l.sealed_by) rows.push(["Approved by", <span key={s} title={s}>{s.split(" ")[0]} <span className="mute small">(signed)</span></span>]);
  if (l.break_glass) rows.push(["Break-glass", <span key="bg" className="small wrap">{l.break_glass}</span>]);
  return (
    <Box title="Merge">
      <Props rows={rows} />
    </Box>
  );
}

function Approval({ data }: { data: Data }) {
  const [copied, setCopied] = useState(false);
  const c = data.change;
  return (
    <Box title="Blocked: needs approval">
      <p>Agents can't merge this alone. Read the files changed, then approve with your key in a terminal.</p>
      <table className="grid">
        <thead>
          <tr>
            <th>Rule</th>
            <th>Who</th>
            <th>Have</th>
          </tr>
        </thead>
        <tbody>
          {data.quorum?.map((t) => (
            <tr key={t.kind}>
              <td className="mono">{t.kind}</td>
              <td>{t.roles.length ? t.roles.map((r) => "@" + r).join(" or ") : "approvers"}</td>
              <td>
                <span className="meter">
                  {Array.from({ length: t.need }).map((_, i) => (
                    <i key={i} className={i < t.have.length ? "on" : ""} />
                  ))}
                </span>{" "}
                {t.have.length} of {t.need} {t.have.length > 0 && <span className="mute">({t.have.join(", ")})</span>}
              </td>
            </tr>
          ))}
          {!data.quorum?.length &&
            c.seal_reasons.map((r) => (
              <tr key={r}>
                <td colSpan={3}>{r}</td>
              </tr>
            ))}
        </tbody>
      </table>
      <div className="cmd">
        <code>{data.seal_command}</code>
        <button
          className="btn"
          id="copy-seal"
          onClick={() => {
            copy(data.seal_command).then(() => {
              setCopied(true);
              setTimeout(() => setCopied(false), 1500);
            });
          }}
        >
          {copied ? "Copied" : "Copy"} <kbd>s</kbd>
        </button>
      </div>
    </Box>
  );
}
