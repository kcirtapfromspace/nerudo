// The nerudo app talks to one trunk's JSON API: /t/{trunk}/api/... on the same origin.
// The share key (read) or a personal key (read and act) goes in headers. A public repo
// reads with no key.

import { useCallback, useEffect, useRef, useState } from "react";

export type ChangeState = "queued" | "awaiting_seal" | "awaiting_allowance" | "awaiting_pick" | "landed" | "ejected";

export interface Reason {
  kind?: string;
  /** awaiting_pick and not_picked: the task, how many attempts it wants, the trial's verdict, and the pick. */
  task?: string;
  attempts?: number;
  passed?: boolean | null;
  picked?: string;
  diagnostics?: { code: string; file: string | null; line: number | null; text: string }[];
  errors?: number;
  paths?: string[];
  detail?: string;
  /** closed: who closed it. */
  by?: string;
  reasons?: string[];
  units?: string[];
  quorum?: Tally[];
  /** conflict: per path, the conflicting lines with git's markers (main, then this change). */
  hunks?: Record<string, string>;
  /** conflict: the PRs that merged first and changed the same paths. */
  beaten_by?: Prior[];
  /** ci_failed: the PRs that merged since this PR's base, the likely cause first. */
  moved?: Prior[];
}

/** A PR that merged before this one and bears on it: it beat it to a path, or moved main under it. */
export interface Prior {
  change_id: string;
  rev: number;
  number?: number | null;
  agent: string;
  subject: string;
  /** Its commit on main. */
  commit: string;
  /** Its Context: trailer, `<repo>@<commit>`, when it merged with context. */
  context?: string | null;
  /** The paths or units it shares with this PR. */
  paths?: string[];
}

/** Files changed, and lines added and deleted. */
export interface Size {
  files: number;
  adds: number;
  dels: number;
}

/** A line on a PR's timeline: opened, a revision, parked, approved, merged, closed, picked, a review request, a decision. */
export interface TimelineLine {
  at_ms: number;
  kind: string;
  text: string;
  who?: string | null;
}

export interface CommitRow {
  rev: number;
  sha: string;
  base: string;
  state: ChangeState;
  subject: string;
  author: string;
  at_ms: number;
}

export interface Check {
  name: string;
  status: "pass" | "fail" | "pending" | "skip";
  detail: string;
  diagnostics?: { code: string; file: string | null; line: number | null; text: string }[] | null;
}

export interface Tally {
  kind: string;
  need: number;
  have: string[];
  roles: string[];
}

export interface Change {
  change_id: string;
  rev: number;
  state: ChangeState;
  agent: string;
  subject: string;
  footprint: string[];
  submitted_ms: number;
  landed_ms?: number | null;
  seq?: number | null;
  batch?: number | null;
  ci_runs: number;
  task?: string | null;
  seal_reasons: string[];
  reason?: Reason | null;
  sealed_by: string[];
  allocated: string[];
  comments: number;
  /** What a decision model ([decide] in nerudo.toml) answered, and what nerudo did. */
  decisions?: Decision[];
  /** The agent's context commit on its fork's nerudo/context branch. */
  context_commit?: string | null;
  /** The PR number (#12). Older records have none: show the change id. */
  number?: number | null;
  size?: Size | null;
  /** The main commit the PR's branch started from. */
  base?: string | null;
  /** The branch it was pushed as with plain git, like `feature/x`. */
  branch?: string | null;
  /** Open PRs in the merge queue: 1 is next. */
  position?: number;
  queue_len?: number;
}

export interface Decision {
  point: string;
  provider: string;
  model: string;
  calibrated: boolean;
  probabilities: Record<string, number>;
  choice: string;
  p: number;
  confidence: number;
  threshold: number;
  outcome: "merge" | "review" | "sign";
  basis: "chose" | "unsure" | "error";
  reason?: string | null;
  error?: string | null;
  at_ms: number;
}

export interface Landing {
  seq: number;
  batch: number;
  change_id: string;
  rev: number;
  commit_out: string;
  landed_ms: number;
  agent?: string | null;
  subject: string;
  sealed_by: string[];
  allocated: string[];
  ci_runs: number;
  task?: string | null;
  comments: number;
  number?: number | null;
  size?: Size | null;
  /** The main commit the PR's branch started from. */
  base?: string | null;
}

export type TaskStatus = "backlog" | "todo" | "in_progress" | "landing" | "done" | "canceled";

export interface Task {
  id: string;
  title: string;
  body: string;
  status: TaskStatus;
  priority: number;
  assignee?: string | null;
  labels: string[];
  created_by: string;
  created_ms: number;
  updated_ms: number;
  changes: string[];
  comments: number;
  /** 2 or more: competing attempts, and one is picked to ship. */
  attempts?: number;
  /** Who picks: "person" (or empty), "smallest" or "first". */
  pick?: string;
  attempting?: string[];
  picked?: string | null;
  picked_by?: string | null;
}

/** One attempt's trial: main plus this PR alone, checked, type-checked, not pushed. */
export interface Trial {
  change_id: string;
  rev: number;
  at_ms: number;
  passed: boolean | null;
  ci: Reason | null;
  files: number;
  adds: number;
  dels: number;
}

export interface Attempt {
  change_id: string;
  rev: number;
  state: ChangeState;
  agent: string;
  subject: string;
  footprint: string[];
  submitted_ms: number;
  reason?: Reason | null;
  seq?: number | null;
  trial: Trial | null;
  why: string;
  files: FileDiff[] | null;
  /** The agent's context commit, and the version of the sequencer's copy it was sent as. */
  context_commit?: string | null;
  context_version?: number | null;
}

export interface CompareResult {
  task: Task;
  policy: string;
  policies: string[];
  attempts: Attempt[];
}

export interface Note {
  kind: "mention" | "team_mention" | "review_request" | "assigned" | "release_proposal";
  change_id: string;
  from: string;
  team?: string | null;
  text: string;
  at_ms: number;
}

export interface ReviewRequest {
  change_id: string;
  reviewer: string;
  by: string;
  at_ms: number;
}

export interface ActivityEvent {
  at_ms: number;
  kind: "landed" | "ejected" | "parked" | "comment" | "review" | "task";
  text: string;
  change?: string | null;
  task?: string | null;
  who?: string | null;
}

export interface Snapshot {
  trunk: string;
  branch: string;
  head: string | null;
  seq: number;
  me: { name: string; roles: string[] } | null;
  people: { name: string; roles: string[] }[];
  teams: string[];
  agents: string[];
  stats: Record<string, number | number[]>;
  pending: Change[];
  landings: Landing[];
  ejected: Change[];
  tasks: Task[];
  needs_me: { reviews: { request: ReviewRequest; change: Change }[]; notes: Note[] } | null;
  events: ActivityEvent[];
  /** Release tags, newest first: where they sit on trunk. */
  releases?: ReleaseMark[];
}

export interface ReleaseMark {
  tag: string;
  seq: number;
  commit: string;
  title: string;
  created_ms: number;
  prerelease: boolean;
  prs: number;
  by: string[];
}

export interface ReleaseChange {
  seq: number;
  change_id: string;
  /** "#12", or the change id for PRs from before numbers. */
  pr: string;
  rev: number;
  subject: string;
  agent: string | null;
  task: string | null;
  approved_by: string[];
  commit: string;
  section: string;
}

export interface ReleaseStats {
  prs: number;
  agents: number;
  batches: number;
  ci_runs: number;
  tasks_closed: string[];
}

export interface FileStats {
  files: number;
  adds: number;
  dels: number;
}

export interface Release {
  tag: string;
  commit: string;
  seq: number;
  title: string;
  notes: string;
  previous: string | null;
  prerelease: boolean;
  created_ms: number;
  released_by: { name: string; key: string; expires_at_ms: number; signature: string }[];
  tag_object: string;
  stats: ReleaseStats;
  changes: ReleaseChange[];
  files: FileStats | null;
  latest: boolean;
}

export interface ReleaseDraft {
  tag: string;
  previous: string | null;
  commit: string;
  seq: number;
  title: string;
  notes: string;
  notes_digest: string;
  rules_digest: string;
  quorum: { roles: string[]; count: number; eligible: string[] };
  stats: ReleaseStats;
  changes: ReleaseChange[];
  files: FileStats;
  command: string;
}

export interface ReleaseList {
  releases: Release[];
  proposals: { at_ms: number; by: string; version: string | null; title: string | null; notes: string | null }[];
}

export interface CompareResult {
  base: string;
  head: string;
  base_commit: string;
  head_commit: string;
  files: { path: string; status: "added" | "deleted" | "changed"; adds: number | null; dels: number | null }[];
  total: FileStats;
}

export interface Comment {
  id: string;
  change_id: string;
  author: string;
  agent: boolean;
  body: string;
  at_ms: number;
}

/** emoji -> who reacted */
export type Reactions = Record<string, string[]>;

export interface Thread {
  comments: { comment: Comment; reactions: Reactions }[];
  reactions: Reactions;
}

/** [op, old line, new line, text], or ["…"] for a skipped stretch. */
export type DiffLine = [string, number | null, number | null, string] | ["…"];

export interface FileDiff {
  path: string;
  status: "added" | "deleted" | "changed";
  binary: boolean;
  adds?: number;
  dels?: number;
  lines: DiffLine[];
}

export interface ChangeDetail {
  change: Change;
  revisions: { rev: number; state: ChangeState; kind: string | null; context?: boolean }[];
  why: string;
  files: FileDiff[] | null;
  landing: {
    seq: number;
    batch: number;
    commit_out: string;
    commit_in: string;
    landed_ms: number;
    author: string;
    seal_reasons: string[];
    sealed_by: string[];
    change_commit: string | null;
    break_glass: string | null;
  } | null;
  quorum: Tally[] | null;
  reviews: { request: ReviewRequest; status: "sealed" | "requested" | "closed" }[];
  task: { id: string; title: string; status: TaskStatus } | null;
  thread: Thread;
  seal_command: string;
  /** A git host's PR page (trunk/prinfo.rs). Absent from an older Worker. */
  timeline?: TimelineLine[];
  commits?: CommitRow[];
  checks?: Check[];
  size?: Size | null;
  /** "<fork>:land/<change>-<rev>" and "main". */
  head_ref?: string;
  base_ref?: string;
}

export interface TaskDetail {
  task: Task;
  changes: Change[];
  thread: Thread;
}

export interface RefRow {
  name: string;
  oid: string;
  change: { change_id: string; rev: number; state: ChangeState | null } | null;
  stale: boolean;
}

export interface BranchesResult {
  branch: string;
  head: string | null;
  seq: number;
  forks: { agent: string; fork: string; refs: { ok: RefRow[] } | { error: string } }[];
}

export interface AgentDetail {
  agent: string;
  fork: string;
  remote: string;
  refs: { ok: RefRow[] } | { error: string };
  changes: Change[];
  notes: Note[];
}

export interface TreeResult {
  kind: "dir" | "file";
  path: string;
  entries?: { name: string; dir: boolean }[];
  text?: string | null;
  size?: number;
  commit: string;
  at: number | null;
}

export interface HistoryResult {
  path: string | null;
  landings: { seq: number; batch: number; change_id: string; rev: number; landed_ms: number; subject: string; author: string }[];
  more: boolean;
  next_before: number;
}

export interface RepoSearchResult {
  mode: "all" | "files" | "content";
  query: string;
  commit: string;
  at: number | null;
  files: { path: string; extension: string; language: string | null; category: string; size: number; score: number; positions: number[]; matches: number; lines: { line: number; text: string; ranges: [number, number][]; clipped: boolean }[] }[];
  stats: { total_files: number; filtered_files: number; scanned_files: number; scanned_bytes: number; matched_files: number; matched_lines: number; returned_files: number; returned_lines: number; skipped_filtered: number; skipped_binary: number; skipped_large: number; skipped_gitlinks: number; skipped_unavailable: number; truncated: boolean; scan_limited: boolean; index_pending?: boolean; index_stale?: boolean; index_state?: string; index_head?: string | null; current_head?: string; partial?: boolean };
  limits: { max_file_bytes: number; max_scan_bytes: number; max_files: number; max_results: number; max_lines_per_file: number };
}

export interface RepoDigestResult {
  commit: string; at: number | null; method: "extension-based";
  total_files: number; total_bytes: number; source_files: number; source_bytes: number;
  excluded_files: number; skipped_files: number; truncated: boolean;
  categories: { name: string; files: number; bytes: number }[];
  languages: { name: string; bytes: number; files: number; extensions: string[] }[];
  file_types: { extension: string; files: number; bytes: number }[];
  directories: { path: string; files: number; bytes: number }[];
  stack: { name: string; kind: "language" | "framework" | "tool" | "package-manager"; paths: string[] }[];
  important_files: { path: string; label: string; kind: "readme" | "manifest" | "license" | "configuration" | "entrypoint" }[];
  readme: { path: string; text: string | null; truncated: boolean } | null;
}

export class ApiError extends Error {
  constructor(public status: number, message: string) {
    super(message);
  }
}

export class Api {
  constructor(public trunk: string, private key: string | null, public user: string | null) {}

  private headers(): Record<string, string> {
    return keyHeaders(this.key, this.user);
  }

  private url(path: string): string {
    return `/t/${encodeURIComponent(this.trunk)}/api/${path}`;
  }

  async get<T>(path: string, signal?: AbortSignal): Promise<T> {
    const controller = new AbortController();
    const abort = () => controller.abort(signal?.reason);
    if (signal?.aborted) abort();
    signal?.addEventListener("abort", abort, { once: true });
    const timer = setTimeout(() => controller.abort(new Error("The repository took too long to respond. Try again.")), 15_000);
    try {
      const r = await fetch(this.url(path), { headers: this.headers(), cache: "no-store", signal: controller.signal });
      return await this.read<T>(r);
    } finally {
      clearTimeout(timer);
      signal?.removeEventListener("abort", abort);
    }
  }

  async post<T>(path: string, body: unknown): Promise<T> {
    const r = await fetch(this.url(path), {
      method: "POST",
      headers: { ...this.headers(), "Content-Type": "application/json" },
      body: JSON.stringify(body),
    });
    return this.read<T>(r);
  }

  private async read<T>(r: Response): Promise<T> {
    const text = await r.text();
    let data: unknown = null;
    try {
      data = text ? JSON.parse(text) : null;
    } catch {
      data = { error: text };
    }
    if (!r.ok) {
      const msg = (data as { error?: string } | null)?.error ?? `HTTP ${r.status}`;
      throw new ApiError(r.status, msg);
    }
    return data as T;
  }
}

/** The key headers: none for a public repo opened without a link. */
export function keyHeaders(key: string | null, user: string | null): Record<string, string> {
  const h: Record<string, string> = {};
  if (key) h["X-Nerudo-Key"] = key;
  if (key && user) h["X-Nerudo-User"] = user;
  return h;
}

/** The change id of PR #n, if the snapshot lists it. */
export function changeOfNumber(snap: Snapshot, n: number): string | null {
  const all: { number?: number | null; change_id: string }[] = [...snap.pending, ...snap.landings, ...snap.ejected];
  return all.find((c) => c.number === n)?.change_id ?? null;
}

/** PR numbers by change id, for links that read /pulls/12. */
export function numbers(snap: Snapshot | null): Map<string, number> {
  const m = new Map<string, number>();
  if (snap) for (const c of [...snap.ejected, ...snap.landings, ...snap.pending]) if (c.number) m.set(c.change_id, c.number);
  return m;
}

/** The shell's snapshot, polled every `ms` while the page is visible. */
export function useSnapshot(api: Api | null, ms = 2000, initial: Snapshot | null = null) {
  const [snap, setSnap] = useState<Snapshot | null>(initial);
  const [error, setError] = useState<string | null>(null);
  const [live, setLive] = useState(initial !== null);
  const busy = useRef(false);

  const refresh = useCallback(async () => {
    if (!api || busy.current) return;
    busy.current = true;
    try {
      const s = await api.get<Snapshot>("state");
      setSnap(s);
      setError(null);
      setLive(true);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
      setLive(false);
    } finally {
      busy.current = false;
    }
  }, [api]);

  useEffect(() => {
    if (!api) return;
    refresh();
    const t = window.setInterval(() => {
      if (document.visibilityState === "visible") refresh();
    }, ms);
    return () => window.clearInterval(t);
  }, [api, ms, refresh]);

  return { snap, error, live, refresh };
}

/** Load `path` when it or `stamp` changes (a stamp from the snapshot keeps details live). */
export function useDetail<T>(api: Api | null, path: string | null, stamp: string, initial?: T | null) {
  const [data, setData] = useState<T | null>(initial ?? null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const last = useRef<string | null>(initial ? path : null);
  const request = useRef(0);

  const load = useCallback(async () => {
    if (!api || !path) return;
    const current = ++request.current;
    if (last.current !== path) {
      setLoading(true);
      setData(null);
      setError(null);
    }
    last.current = path;
    try {
      const d = await api.get<T>(path);
      if (request.current === current && last.current === path) {
        setData(d);
        setError(null);
      }
    } catch (e) {
      if (request.current === current && last.current === path) setError(e instanceof Error ? e.message : String(e));
    } finally {
      if (request.current === current) setLoading(false);
    }
  }, [api, path]);

  useEffect(() => {
    load();
    return () => { request.current++; };
  }, [load, stamp]);

  return { data, error, loading, reload: load };
}

/** GET context/{change}[?rev=]: what the agent recorded with one revision. */
export interface ContextResult {
  change_id: string;
  rev: number;
  agent: string;
  fork: string;
  branch: string;
  commit: string | null;
  path?: string;
  text: string | null;
  truncated?: boolean;
  size?: number;
  error?: string;
  files?: string[];
  revisions: { rev: number; commit: string; state: ChangeState }[];
  /** The sequencer's immutable copies of this revision's context, oldest first. */
  repo?: string | null;
  versions?: { v: number; commit: string; path: string; source_fork: string; source_commit: string; at_ms: number; pinned: boolean; sent: boolean }[];
  /** The version shown. */
  v?: number;
  immutable?: boolean;
}

/** GET agent/{name}/context[?commit=&path=]: an agent's context history, newest first. */
export interface AgentContextResult {
  agent: string;
  fork: string;
  branch: string;
  tip: string | null;
  error?: string;
  /** This is the agent's own branch: it can rewrite it. `record` is the context repo. */
  working_copy?: boolean;
  record?: string | null;
  commits: { commit: string; subject: string; at_ms: number; paths: string[] }[];
  file?: { commit: string; path: string; text: string; truncated: boolean; size: number };
}
