import type { ReactNode } from "react";
import type { BranchesResult, Change, Landing, Task, Snapshot, TreeResult } from "../api";
import { Empty } from "../components/bits";
import { Badge } from "../components/classic";
import type { View } from "../route";
import { TASK_STATUS, PRIORITY, STATE, hue, mergeStatus, prRef, stateLabel, subjectOf } from "../util";
import { Ago, Clock } from "../components/time";

/** A PR's branch where it crosses one row: its lane (main is lane 0), the agent's color,
 *  and dashed while the PR is open. */
export interface Strand {
  lane: number;
  hue: number | null;
  dashed?: boolean;
}

/** One row's slice of the graph. Rows stack, so the slices join into one tree: main runs down
 *  lane 0, and each PR's branch runs in a lane of its own, from the main commit it started
 *  from up to the batch that merged it. */
export interface GraphCell {
  /** The node on this row. */
  node: "batch" | "merged" | "closed" | "open" | "wait" | "root";
  /** The node's own branch, for a PR: up to the batch that merged it, down to where it started. */
  own?: Strand & { up: boolean; down: boolean };
  /** Main through lane 0: none, down from this row, full, up into this row. */
  main: "none" | "down" | "full" | "up";
  /** Branches that pass this row. */
  pass: Strand[];
  /** Branches that meet main at this row's node from below: a PR it merged, or one that
   *  started here and closed. */
  merges: Strand[];
  /** Branches that start from main at this row's node: they go up. */
  starts: Strand[];
  /** Lanes in the whole graph, for its width. */
  lanes: number;
}

export interface Item {
  key: string;
  id: string;
  group: string;
  /** The badge: a name's initials when `who` is set, else a state's color. */
  tone: string;
  who?: string | null;
  /** In the Graph view, the graph replaces the badge. */
  graph?: GraphCell;
  /** A batch line in the graph: it labels the rows under it and is not selectable. */
  head?: boolean;
  /** A tooltip for the row. */
  hint?: string;
  title: ReactNode;
  /** A short id after the title, in mono. */
  code?: ReactNode;
  /** A second line, for a reason or a mention. */
  note?: ReactNode;
  tag?: ReactNode;
  when?: ReactNode;
  /** The words that "find" matches. */
  text: string;
  /** A directory in the Files view: selecting it opens the directory. */
  dir?: boolean;
  flash?: boolean;
}

function changeItem(c: Change, group: string): Item {
  return {
    key: `${group}:${c.change_id}:${c.rev}`,
    id: c.change_id,
    group,
    tone: STATE[c.state].tone,
    who: c.agent,
    title: subjectOf(c.change_id, c.subject) || <span className="mute">rev {c.rev}</span>,
    code: prRef(c),
    tag: c.size ? <SizeTag size={c.size} /> : (c.task ?? (c.comments > 0 ? `💬${c.comments}` : null)),
    when: <Ago ms={c.landed_ms ?? c.submitted_ms} />,
    text: [prRef(c), c.change_id, c.subject, c.agent, c.task ?? "", stateLabel(c), ...c.footprint].join(" "),
  };
}

/** "+12 −3", in the add and delete colors. */
export function SizeTag({ size }: { size: { adds: number; dels: number; files?: number } }) {
  return (
    <span className="size mono">
      <span className="add">+{size.adds}</span> <span className="del">−{size.dels}</span>
    </span>
  );
}

function reasonLine(c: Change): string | undefined {
  const r = c.reason;
  if (c.state === "awaiting_seal") return c.seal_reasons.join("; ");
  if (c.state === "awaiting_allowance") return "needs an allowance";
  if (c.state === "awaiting_pick") {
    const verdict = r?.passed === true ? "its trial passed" : r?.passed === false ? "its trial failed the checks" : "no check verdict";
    return `an attempt for ${r?.task ?? "its task"}: ${verdict}`;
  }
  if (!r) return undefined;
  if (r.kind === "not_picked") return `not picked: ${r.picked ?? "another attempt"} shipped for ${r.task ?? "its task"}`;
  if (r.kind === "ci_failed" && r.diagnostics?.[0]) {
    const d = r.diagnostics[0];
    const after = r.moved?.length ? ` (main moved: ${r.moved.map((p) => prRef(p)).join(", ")})` : "";
    return `${d.code} ${d.file}:${d.line} ${d.text}${after}`;
  }
  if (r.kind === "conflict") {
    const by = (r.beaten_by ?? []).map((p) => prRef(p)).join(", ");
    return `needs rebase: conflict in ${(r.paths ?? []).join(", ")}${by ? ` with ${by}` : ""}`;
  }
  if (r.kind === "ci_crashed") return "the type checker crashed";
  if (r.kind === "closed") return `closed by ${r.by ?? "its author"}${r.detail ? `: ${r.detail}` : ""}`;
  return r.kind?.replace(/_/g, " ");
}

function agentTag(agent: string | null | undefined): ReactNode {
  if (!agent) return null;
  return (
    <>
      <Badge name={agent} size={14} /> {agent}
    </>
  );
}

const TASK_TONE: Record<string, string> = { landing: "queued", done: "landed", canceled: "bounced", in_progress: "progress", backlog: "backlog", todo: "idle" };

function taskItem(i: Task, group: string, snap: Snapshot): Item {
  const pending = snap.pending.filter((c) => c.task === i.id);
  return {
    key: `${group}:${i.id}`,
    id: i.id,
    group,
    tone: TASK_TONE[i.status] ?? "idle",
    who: i.assignee || null,
    title: i.title,
    code: i.id,
    note:
      (i.attempts ?? 1) >= 2
        ? i.picked
          ? `${i.attempts} attempts; ${i.picked} shipped`
          : `${i.attempts} attempts, ${pending.filter((c) => c.state === "awaiting_pick").length} waiting for a pick`
        : pending.some((c) => c.state === "awaiting_seal")
          ? "a PR needs approval"
          : undefined,
    tag: i.priority ? <span className={`prio-tag p${i.priority}`}>{PRIORITY[i.priority]}</span> : null,
    when: <Ago ms={i.updated_ms} />,
    text: [i.id, i.title, i.assignee ?? "", ...i.labels, i.status.replace("_", " ")].join(" "),
  };
}

/** A PR's branch, before it has a lane: from row `top` (the batch that merged it, or the PR's
 *  own row) down to the row of the main commit it started from. */
interface Leg {
  row: number;
  top: number;
  base?: string | null;
  strand: Strand;
}

/** Past this many lanes, a branch shares the lane that frees first. */
const MAX_LANES = 8;

/** The Graph: open PRs at the top, then each batch as a node on main with the PRs it merged
 *  under it. Each PR's branch starts at the main commit it was based on, so PRs that were
 *  open at the same time run side by side. PRs that closed without a merge end in a cross,
 *  under the batch that merged just before they closed. */
function graphItems(snap: Snapshot, mounted: boolean): Item[] {
  const out: Item[] = [];
  const legs: Leg[] = [];
  const cell = (node: GraphCell["node"]): GraphCell => ({ node, main: "none", pass: [], merges: [], starts: [], lanes: 1 });
  const leg = (top: number, base: string | null | undefined, agent: string | null | undefined, dashed: boolean) =>
    legs.push({ row: out.length, top, base, strand: { lane: 0, hue: agent ? hue(agent) : null, dashed } });
  // Where each main commit is: its seq, for "behind main".
  const seqOf = new Map(snap.landings.map((l) => [l.commit_out, l.seq]));
  const first = snap.landings[snap.landings.length - 1];
  if (first?.seq === 1 && first.base) seqOf.set(first.base, 0);
  const started = (base: string | null | undefined) => {
    if (!base) return "";
    const n = seqOf.has(base) ? snap.seq - seqOf.get(base)! : null;
    return `branched from main at ${base.slice(0, 7)}${n === null ? "" : n === 0 ? ", the tip of main" : `, ${n} merge${n === 1 ? "" : "s"} ago`}`;
  };

  for (const c of snap.pending) {
    leg(out.length, c.base, c.agent, true);
    out.push({
      ...changeItem(c, "Open"),
      graph: cell(c.state === "queued" ? "open" : "wait"),
      tag: agentTag(c.agent),
      note: c.state === "queued" ? (c.branch ? `${c.branch} · ${started(c.base)}` : started(c.base)) || undefined : reasonLine(c),
      hint: started(c.base),
      when: <span className={`t-${STATE[c.state].tone}`}>{stateLabel(c)}</span>,
    });
  }

  // Batches, newest first, as they come in the snapshot.
  const batches: { batch: number; rows: Landing[]; at: number }[] = [];
  for (const l of snap.landings) {
    const b = batches[batches.length - 1];
    if (b && b.batch === l.batch) {
      b.rows.push(l);
      b.at = Math.max(b.at, l.landed_ms);
    } else batches.push({ batch: l.batch, rows: [l], at: l.landed_ms });
  }
  const closedAt = new Map<string, number>();
  for (const e of snap.events) if (e.kind === "ejected" && e.change && !closedAt.has(e.change)) closedAt.set(e.change, e.at_ms);
  const closed = snap.ejected.map((c) => ({ c, at: closedAt.get(c.change_id) ?? c.submitted_ms })).sort((a, b) => b.at - a.at);
  const placed = new Set<Change>();
  const closedRow = (c: Change) => {
    leg(out.length, c.base, c.agent, true);
    out.push({ ...closedItem(c), hint: started(c.base) });
  };
  // The row of each main commit: its batch's line.
  const rowOf = new Map<string, number>();

  const newest = snap.landings[0]?.seq;
  for (const b of batches) {
    const head = out.length;
    for (const l of b.rows) rowOf.set(l.commit_out, head);
    const ci = Math.max(...b.rows.map((l) => l.ci_runs));
    // A release cut at a landing in this batch: a tag on the batch line.
    const tags = (snap.releases ?? []).filter((r) => b.rows.some((l) => l.seq === r.seq));
    out.push({
      key: `batch:${b.batch}`,
      id: `batch:${b.batch}`,
      group: "main",
      tone: "landed",
      head: true,
      graph: cell("batch"),
      title: (
        <>
          <b>Merged batch {b.batch}</b>
          {tags.map((r) => (
            <span key={r.tag} className="tag-mark" title={`Release ${r.tag} at landing ${r.seq}`}>
              {r.tag}
            </span>
          ))}
        </>
      ),
      code: b.rows[0].commit_out.slice(0, 7),
      tag: `${b.rows.length} PR${b.rows.length === 1 ? "" : "s"} · ${ci} CI run${ci === 1 ? "" : "s"}`,
      when: <Clock ms={b.at} />,
      text: `batch ${b.batch} ${b.rows.map((l) => `${l.change_id} ${l.subject} ${l.agent ?? ""}`).join(" ")}`,
    });
    for (const l of b.rows) {
      leg(head, l.base, l.agent, false);
      out.push({
        key: `merged:${l.seq}`,
        id: l.change_id,
        group: "main",
        tone: "landed",
        graph: cell("merged"),
        title: subjectOf(l.change_id, l.subject) || <span className="mute">seq {l.seq}</span>,
        code: prRef(l),
        note: l.sealed_by.length ? `approved by ${l.sealed_by.join(", ")}` : undefined,
        hint: [started(l.base), `merged in batch ${b.batch}`].filter(Boolean).join("; "),
        tag: agentTag(l.agent),
        when: <span className="mono">{l.commit_out.slice(0, 7)}</span>,
        text: [prRef(l), l.change_id, l.subject, l.agent ?? "", l.task ?? "", l.commit_out, "merged"].join(" "),
        flash: mounted && l.seq === newest && Date.now() - l.landed_ms < 6000,
      });
    }
    // PRs that closed after this batch merged (most often, because of it).
    for (const x of closed) {
      if (placed.has(x.c) || x.at < b.at) continue;
      placed.add(x.c);
      closedRow(x.c);
    }
  }
  for (const x of closed) if (!placed.has(x.c)) closedRow(x.c);

  // With the whole history in view, main starts at the first merge's base.
  let root: number | null = null;
  if (first?.seq === 1 && first.base) {
    root = out.length;
    rowOf.set(first.base, root);
    out.push({
      key: "root",
      id: "root",
      group: "main",
      tone: "landed",
      head: true,
      graph: cell("root"),
      title: <span className="mute">main starts here</span>,
      code: first.base.slice(0, 7),
      text: `main ${first.base}`,
    });
  }

  // Lanes: each branch takes the lowest lane that is free over all its rows. A batch row can
  // hold one branch that starts there (going up) and one that merges there (from below).
  const spans = legs
    .map((g) => {
      // A base older than the rows in view runs off the bottom.
      const bottom = !g.base ? g.row : rowOf.get(g.base) ?? root ?? out.length;
      // A PR that closed under the batch it was based on hangs from that batch.
      return { ...g, bottom, top: Math.min(g.top, bottom), end: Math.max(bottom, g.row) };
    })
    .sort((a, b) => a.top - b.top);
  const busy: number[] = [];
  for (const g of spans) {
    let lane = busy.findIndex((last) => last <= g.top);
    if (lane < 0) lane = busy.length < MAX_LANES ? busy.push(0) - 1 : busy.indexOf(Math.min(...busy));
    busy[lane] = g.end;
    const s = { ...g.strand, lane: lane + 1 };
    for (let r = g.top; r <= Math.min(g.end, out.length - 1); r++) {
      const c = out[r].graph!;
      if (r === g.row) c.own = { ...s, up: g.top < g.row, down: g.bottom > g.row };
      else if (r === g.top) c.merges.push(s);
      else if (r === g.bottom) c.starts.push(s);
      else c.pass.push(s);
    }
  }

  // Main: from the newest batch down to where it starts, or off the bottom.
  const top = out.findIndex((it) => it.graph?.node === "batch");
  if (top >= 0) {
    const end = root ?? out.length;
    for (let r = top; r < out.length; r++) out[r].graph!.main = r === top ? "down" : r < end ? "full" : r === end ? "up" : "none";
  }
  for (const it of out) it.graph!.lanes = busy.length + 1;
  return out;
}

function closedItem(c: Change): Item {
  return {
    ...changeItem(c, "main"),
    key: `closed:${c.change_id}:${c.rev}`,
    graph: { node: "closed", main: "none", pass: [], merges: [], starts: [], lanes: 1 },
    code: `${prRef(c)} r${c.rev}`,
    note: reasonLine(c),
    tag: agentTag(c.agent),
    when: <span className="t-bounced">{stateLabel(c)}</span>,
  };
}

/** Main, then each agent's fork and the branches in it, with what became of each branch's PR. */
function branchItems(snap: Snapshot, branches: BranchesResult | null): Item[] {
  const out: Item[] = [];
  const main = snap.branch.replace("refs/heads/", "");
  out.push({
    key: "branch:main",
    id: "main",
    group: "Branches",
    tone: "landed",
    title: <b>{main}</b>,
    code: snap.head?.slice(0, 7),
    tag: `${snap.seq} merged`,
    note: `${snap.agents.length} agent fork${snap.agents.length === 1 ? "" : "s"} · ${snap.pending.length} open PR${snap.pending.length === 1 ? "" : "s"}`,
    text: `${main} main`,
  });
  for (const r of snap.releases ?? []) {
    out.push({
      key: `tag:${r.tag}`,
      id: `release:${r.tag}`,
      group: "Tags",
      tone: r.prerelease ? "approval" : "landed",
      title: <b>{r.tag}</b>,
      code: r.commit.slice(0, 7),
      tag: r.prerelease ? "pre-release" : `${r.prs} PR${r.prs === 1 ? "" : "s"}`,
      when: <Ago ms={r.created_ms} />,
      text: `${r.tag} ${r.title} tag release`,
    });
  }
  if (!branches) return out;
  for (const f of branches.forks) {
    const group = `Fork: ${f.agent}`;
    if (!("ok" in f.refs)) {
      out.push({ key: `fork:${f.agent}`, id: `fork:${f.agent}`, group, tone: "bounced", who: f.agent, title: f.fork, note: f.refs.error, text: `${f.agent} ${f.fork}` });
      continue;
    }
    for (const r of f.refs.ok) {
      const name = r.name.replace("refs/heads/", "");
      const c = r.change;
      const tone = c?.state ? STATE[c.state].tone : "idle";
      out.push({
        key: `ref:${f.agent}:${r.name}`,
        id: c ? `pr:${c.change_id}` : `fork:${f.agent}`,
        group,
        tone,
        who: c ? null : f.agent,
        title: name,
        code: r.oid.slice(0, 7),
        tag: r.stale ? <span className="mute">stale</span> : null,
        when: c?.state ? <span className={`t-${tone}`}>{STATE[c.state].label}</span> : <span className="mute">{name === "main" ? "fork base" : ""}</span>,
        text: [f.agent, f.fork, name, c?.change_id ?? "", r.stale ? "stale" : ""].join(" "),
      });
    }
  }
  return out;
}

export function buildItems(view: View, snap: Snapshot, files: TreeResult | null, mounted = false, branches: BranchesResult | null = null): Item[] {
  const out: Item[] = [];
  if (view === "graph") return graphItems(snap, mounted);
  if (view === "inbox") {
    for (const r of snap.needs_me?.reviews ?? []) {
      const who = r.request.reviewer.startsWith("@") ? r.request.reviewer : "you";
      out.push({ ...changeItem(r.change, "Review requests for you"), note: `${r.request.by} asked ${who}`, text: `${r.change.change_id} ${r.change.subject} ${r.request.by}` });
    }
    // Tasks whose competing attempts wait for a person to pick one.
    if (snap.me) {
      for (const t of snap.tasks.filter((t) => (t.attempts ?? 1) >= 2 && !t.picked && (t.pick || "person") === "person")) {
        const waiting = snap.pending.filter((c) => c.task === t.id && c.state === "awaiting_pick");
        if (waiting.length === 0) continue;
        out.push({
          key: `Pick:${t.id}`,
          id: t.id,
          group: "Waiting for your pick",
          tone: "approval",
          title: t.title,
          code: t.id,
          note: `${waiting.length} of ${t.attempts} attempts in: ${waiting.map((c) => c.change_id).join(", ")}`,
          when: <Ago ms={Math.max(...waiting.map((c) => c.submitted_ms))} />,
          text: [t.id, t.title, ...waiting.map((c) => c.change_id), "pick"].join(" "),
        });
      }
    }
    for (const c of snap.pending.filter((c) => c.state !== "queued" && c.state !== "awaiting_pick")) {
      out.push({ ...changeItem(c, "Needs approval"), note: reasonLine(c) });
    }
    for (const n of snap.needs_me?.notes ?? []) {
      if (n.kind === "review_request") continue;
      const proposal = n.kind === "release_proposal";
      // A proposal is done once its tag is released.
      if (proposal && (snap.releases ?? []).some((r) => `release:${r.tag}` === n.change_id)) continue;
      const what = proposal ? "asks for a release" : n.kind === "assigned" ? "assigned you" : n.kind === "team_mention" ? `mentioned @${n.team}` : "mentioned you";
      out.push({
        key: `Mentions:${n.change_id}:${n.at_ms}`,
        id: n.change_id,
        group: proposal ? "Release proposals" : "Mentions",
        tone: "idle",
        who: n.from,
        title: (
          <>
            <b>{n.from}</b> {what}
          </>
        ),
        code: n.change_id,
        note: n.text,
        when: <Ago ms={n.at_ms} />,
        text: `${n.change_id} ${n.from} ${n.text}`,
      });
    }
  } else if (view === "tasks") {
    for (const s of TASK_STATUS) {
      let list = snap.tasks.filter((i) => i.status === s.id);
      if (s.id === "done" || s.id === "canceled") list = list.slice(0, 12);
      list.sort((a, b) => (a.priority || 5) - (b.priority || 5) || b.updated_ms - a.updated_ms);
      for (const i of list) out.push(taskItem(i, s.label, snap));
    }
  } else if (view === "pulls") {
    // GitHub's three: Open (in queue order, then blocked, then waiting for a pick), Merged, Closed.
    const open = [...snap.pending].sort((a, b) => (a.position ?? 1e9) - (b.position ?? 1e9));
    for (const c of open) out.push({ ...changeItem(c, "Open"), note: mergeStatus(c) });
    for (const l of snap.landings) {
      out.push({
        key: `Merged:${l.seq}`,
        id: l.change_id,
        group: "Merged",
        tone: "landed",
        who: l.agent,
        title: subjectOf(l.change_id, l.subject) || <span className="mute">seq {l.seq}</span>,
        code: prRef(l),
        tag: l.size ? <SizeTag size={l.size} /> : null,
        note: `merged as ${l.commit_out.slice(0, 7)} in batch ${l.batch}${l.sealed_by.length ? `, approved by ${l.sealed_by.join(", ")}` : ""}`,
        when: <Ago ms={l.landed_ms} />,
        text: [prRef(l), l.change_id, l.subject, l.agent ?? "", l.task ?? "", "merged"].join(" "),
      });
    }
    // A PR closed for a conflict is not done: its agent rebases it. It gets a group of its own.
    const closed = snap.ejected.slice(0, 30);
    for (const g of ["Needs rebase", "Closed"]) {
      for (const c of closed.filter((x) => (x.reason?.kind === "conflict" ? "Needs rebase" : "Closed") === g)) {
        out.push({ ...changeItem(c, g), key: `${g}:${c.change_id}:${c.rev}`, code: `${prRef(c)} r${c.rev}`, note: mergeStatus(c) });
      }
    }
  } else if (view === "agents") {
    for (const a of snap.agents) {
      const mine = snap.pending.filter((c) => c.agent === a);
      const last = snap.landings.find((l) => l.agent === a);
      const now = mine[0];
      out.push({
        key: `Agents:${a}`,
        id: a,
        group: now ? "Working" : "Idle",
        tone: now ? STATE[now.state].tone : "idle",
        who: a,
        title: a,
        note: now ? `${stateLabel(now)}: ${now.change_id} ${subjectOf(now.change_id, now.subject)}` : last ? `last merged ${last.change_id}` : "no PRs yet",
        tag: `${snap.landings.filter((l) => l.agent === a).length} merged`,
        when: <Ago ms={now?.submitted_ms ?? last?.landed_ms} />,
        text: [a, now?.change_id ?? "", now?.subject ?? ""].join(" "),
      });
    }
    out.sort((x, y) => (x.group === y.group ? 0 : x.group === "Working" ? -1 : 1));
  } else if (view === "releases") {
    const rels = snap.releases ?? [];
    const since = snap.seq - (rels[0]?.seq ?? 0);
    out.push({
      key: "draft",
      id: "draft",
      group: "Draft",
      tone: since > 0 ? "queued" : "idle",
      title: <b>Draft the next release</b>,
      note: since > 0 ? `${since} PR${since === 1 ? "" : "s"} merged since ${rels[0]?.tag ?? "the start"}` : `nothing merged since ${rels[0]?.tag}`,
      text: "draft next release new",
    });
    rels.forEach((r, i) =>
      out.push({
        key: `rel:${r.tag}`,
        id: r.tag,
        group: "Releases",
        tone: r.prerelease ? "approval" : "landed",
        title: (
          <>
            <b>{r.tag}</b>
            {r.title !== r.tag && <> {r.title}</>}
          </>
        ),
        code: r.commit.slice(0, 7),
        tag: i === 0 && !r.prerelease ? <span className="chip c-landed">Latest</span> : r.prerelease ? <span className="chip c-approval">Pre-release</span> : null,
        when: <Ago ms={r.created_ms} />,
        note: `${r.prs} PR${r.prs === 1 ? "" : "s"} · signed by ${r.by.join(", ") || "?"}`,
        text: `${r.tag} ${r.title} ${r.by.join(" ")}`,
      }),
    );
  } else if (view === "repo") {
    out.push(...branchItems(snap, branches));
    if (files?.kind !== "dir") return out;
    const base = files.path ? `${files.path}/` : "";
    const group = `Files on ${snap.branch.replace("refs/heads/", "")}: /${files.path}`;
    if (files.path) {
      const up = files.path.split("/").slice(0, -1).join("/");
      out.push({ key: "..", id: up, group, tone: "dir", title: "..", text: "..", dir: true });
    }
    for (const e of files.entries ?? []) {
      out.push({
        key: `${base}${e.name}`,
        id: e.dir ? `${base}${e.name}` : `file:${base}${e.name}`,
        group,
        tone: e.dir ? "dir" : "file",
        title: e.name + (e.dir ? "/" : ""),
        text: e.name,
        dir: e.dir,
      });
    }
  }
  return out;
}

const NOUN: Record<View, [string, string]> = {
  graph: ["row", "rows"],
  pulls: ["pull request", "pull requests"],
  tasks: ["task", "tasks"],
  releases: ["release", "releases"],
  inbox: ["item", "items"],
  agents: ["agent", "agents"],
  repo: ["item", "items"],
};

export function ListPane(props: {
  view: View;
  all: Item[];
  items: Item[];
  selected: number;
  onSelect: (i: number) => void;
  snap: Snapshot;
  scope: string;
  onScope: (s: string) => void;
  find: string;
  onFind: (s: string) => void;
  onNewTask: () => void;
}) {
  const scopes = new Map<string, number>();
  for (const it of props.all) if (!it.head) scopes.set(it.group, (scopes.get(it.group) ?? 0) + 1);
  const groups: { label: string; items: [Item, number][] }[] = [];
  props.items.forEach((it, i) => {
    const g = groups[groups.length - 1];
    if (g && g.label === it.group) g.items.push([it, i]);
    else groups.push({ label: it.group, items: [[it, i]] });
  });
  const graph = props.view === "graph";
  const showHeads = !graph && !props.scope && groups.length > 1;
  const [one, many] = NOUN[props.view];
  // The Releases tab's draft row is an action, not a release.
  const counted = (it: Item) => !it.head && !(props.view === "releases" && it.id === "draft");
  const count = props.items.filter(counted).length;
  const total = props.all.filter(counted).length;
  const st = props.snap.stats as Record<string, number>;
  const closed = (st.ejected_conflict ?? 0) + (st.ejected_check ?? 0) + (st.ejected_ci ?? 0) + (st.ejected_other ?? 0) + (st.not_picked ?? 0);
  const summary = graph || (props.view === "pulls" && !props.find)
    ? `${props.snap.pending.length} open · ${props.snap.seq} merged · ${closed || props.snap.ejected.length} closed`
    : `${count} ${count === 1 ? one : many}${count !== total ? ` of ${total}` : ""}`;
  return (
    <section className={`list${graph ? " wide" : ""}`}>
      <div className="list-tools">
        {!graph && (
          <select className="scope" value={props.scope} onChange={(e) => props.onScope(e.target.value)} aria-label="Show">
            <option value="">All {many} ({total})</option>
            {[...scopes].map(([label, n]) => (
              <option key={label} value={label}>
                {label} ({n})
              </option>
            ))}
          </select>
        )}
        <input
          id="find"
          type="search"
          aria-label={`Find ${many}`}
          className="find"
          placeholder={graph ? "find a PR, an agent or a commit" : "find"}
          value={props.find}
          onChange={(e) => props.onFind(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Escape") {
              props.onFind("");
              (e.target as HTMLInputElement).blur();
            }
            if (e.key === "Enter" || e.key === "ArrowDown") {
              e.preventDefault();
              (e.target as HTMLInputElement).blur();
              props.onSelect(0);
            }
          }}
        />
      </div>
      <div className={`list-box${graph ? " graph" : ""}`} role="listbox" aria-label={many} tabIndex={0} aria-activedescendant={props.selected >= 0 && props.items[props.selected] ? `row-${encodeURIComponent(props.items[props.selected].key)}` : undefined} style={graph ? { ["--gw" as string]: `${graphWidth(props.all[0]?.graph?.lanes ?? 1)}px` } : undefined}>
        {props.items.length === 0 && (props.all.length > 0 ? <Empty title="Nothing matches">Clear the find box, or pick another scope.</Empty> : <EmptyFor view={props.view} snap={props.snap} />)}
        {groups.map((g) => (
          <div key={g.label} className="group">
            {showHeads && (
              <div className="group-head">
                <span>{g.label}</span>
                <span className="group-count">{g.items.length}</span>
              </div>
            )}
            {g.items.map(([it, i]) => (
              <Row key={it.key} it={it} selected={i === props.selected} onClick={() => !it.head && props.onSelect(i)} />
            ))}
          </div>
        ))}
      </div>
      <div className="statusbar">
        <span className="cell grow">{summary}</span>
        {props.view === "tasks" && props.snap.me && (
          <button className="cell btn-cell" onClick={props.onNewTask} title="New task (c)">
            New task…
          </button>
        )}
      </div>
    </section>
  );
}

/** The graph's geometry, in px: lane 0's x, the space between lanes, and the node's y. */
const X0 = 8;
const LANE = 13;
const Y = 13;
const lx = (lane: number) => X0 + lane * LANE;

/** The graph column's width for this many lanes. */
export function graphWidth(lanes: number): number {
  return lx(lanes - 1) + 10;
}

function Graph({ g }: { g: GraphCell }) {
  const pen = (s: Strand) => ({ stroke: s.hue === null ? "var(--title)" : `hsl(${s.hue} 48% 40%)`, strokeDasharray: s.dashed ? "3 2" : undefined });
  const x = g.own ? lx(g.own.lane) : X0;
  return (
    <span className="gc" aria-hidden>
      <svg>
        {g.pass.map((s, i) => (
          <line key={`p${i}`} {...pen(s)} x1={lx(s.lane)} x2={lx(s.lane)} y1="0" y2="100%" />
        ))}
        {g.main !== "none" && <line className="m" x1={X0} x2={X0} y1={g.main === "down" ? Y : 0} y2={g.main === "up" ? Y : "100%"} />}
        {g.merges.map((s, i) => (
          <g key={`m${i}`} {...pen(s)}>
            <line x1={lx(s.lane)} x2={lx(s.lane)} y1={Y + 6} y2="100%" />
            <path d={`M${lx(s.lane)} ${Y + 6} Q${lx(s.lane)} ${Y} ${lx(s.lane) - 6} ${Y} H${X0}`} />
          </g>
        ))}
        {g.starts.map((s, i) => (
          <g key={`s${i}`} {...pen(s)}>
            <line x1={lx(s.lane)} x2={lx(s.lane)} y1="0" y2={Y - 6} />
            <path d={`M${X0} ${Y} H${lx(s.lane) - 6} Q${lx(s.lane)} ${Y} ${lx(s.lane)} ${Y - 6}`} />
          </g>
        ))}
        {g.own && (
          <g {...pen(g.own)}>
            {g.own.up && <line x1={x} x2={x} y1="0" y2={Y} />}
            {g.own.down && <line x1={x} x2={x} y1={Y} y2="100%" />}
          </g>
        )}
        {g.node === "batch" && <circle className="n batch" cx={X0} cy={Y} r={5.5} />}
        {g.node === "root" && <circle className="n root" cx={X0} cy={Y} r={4} />}
        {g.node === "merged" && <circle className="n merged" cx={x} cy={Y} r={4} style={{ fill: pen(g.own!).stroke }} />}
        {g.node === "open" && <circle className="n open" cx={x} cy={Y} r={4.5} style={{ stroke: pen(g.own!).stroke }} />}
        {g.node === "wait" && <circle className="n wait" cx={x} cy={Y} r={4.5} />}
        {g.node === "closed" && (
          <g className="n closed">
            <rect x={x - 5} y={Y - 5} width={10} height={10} />
            <path d={`M${x - 2.5} ${Y - 2.5} l5 5 M${x + 2.5} ${Y - 2.5} l-5 5`} />
          </g>
        )}
      </svg>
    </span>
  );
}

function Row({ it, selected, onClick }: { it: Item; selected: boolean; onClick: () => void }) {
  const cls = ["row", selected && "sel", it.flash && "flash", it.note && "two", it.graph && "g", it.head && "head"].filter(Boolean).join(" ");
  return (
    <div id={`row-${encodeURIComponent(it.key)}`} className={cls} onClick={onClick} title={it.hint} role={it.head ? "presentation" : "option"} aria-selected={it.head ? undefined : selected}>
      {it.graph ? <Graph g={it.graph} /> : it.who ? <Badge name={it.who} size={18} /> : <Badge tone={it.tone} size={18} />}
      <span className="row-main">
        <span className="row-title">{it.title}</span>
        {it.code && <span className="row-code mono">{it.code}</span>}
      </span>
      {it.tag ? <span className="row-tag">{it.tag}</span> : <span />}
      <span className="row-when">{it.when}</span>
      {it.note && <span className="row-note">{it.note}</span>}
    </div>
  );
}

function EmptyFor({ view, snap }: { view: View; snap: Snapshot }) {
  if (view === "releases") return <Empty title="No releases yet">The sequencer drafts one from what merged; an approver signs it with nerudo release.</Empty>;
  if (view === "graph") return <Empty title="Nothing merged yet">Agents open pull requests through nerudo mcp. The graph shows them waiting, merged and closed.</Empty>;
  if (view === "inbox") return <Empty title="All clear">Nothing needs you. PRs that need your approval, attempts that wait for your pick, review requests and mentions show up here.</Empty>;
  if (view === "tasks") return <Empty title="No tasks yet">{snap.me ? "Press c to write what should change, and why." : "People write tasks with their personal link."}</Empty>;
  if (view === "pulls") return <Empty title="No pull requests">Agents open PRs through nerudo mcp. A PR waits here until it merges.</Empty>;
  if (view === "agents") return <Empty title="No agents yet">An agent shows up when it gets its fork.</Empty>;
  return <Empty title="Loading the repo" />;
}
