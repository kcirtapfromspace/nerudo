import type { Change, ChangeState, TaskStatus } from "./api";

export function ago(ms: number | null | undefined, now = Date.now()): string {
  if (!ms) return "";
  const s = Math.max(0, Math.round((now - ms) / 1000));
  if (s < 60) return `${s}s`;
  const m = Math.round(s / 60);
  if (m < 60) return `${m}m`;
  const h = Math.round(m / 60);
  if (h < 48) return `${h}h`;
  return `${Math.round(h / 24)}d`;
}

export function clock(ms: number | null | undefined): string {
  if (!ms) return "";
  const d = new Date(ms);
  return d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit", hour12: false });
}

/** A pull request's state, and the tone that colors it. The API says "landed" and "ejected"; people read Merged and Closed
 *  (or Needs rebase, for a conflict: see stateLabel). */
export const STATE: Record<ChangeState, { label: string; tone: string }> = {
  queued: { label: "Queued", tone: "queued" },
  awaiting_seal: { label: "Needs approval", tone: "approval" },
  awaiting_allowance: { label: "Needs an allowance", tone: "approval" },
  awaiting_pick: { label: "Waiting for pick", tone: "approval" },
  landed: { label: "Merged", tone: "landed" },
  ejected: { label: "Closed", tone: "bounced" },
};

/** Why a pull request closed without a merge: Needs rebase (a conflict), Checks failed, Not picked, or Closed. */
export function stateLabel(c: Pick<Change, "state" | "reason">): string {
  if (c.state !== "ejected") return STATE[c.state].label;
  const k = c.reason?.kind;
  return k === "conflict" ? "Needs rebase" : k === "ci_failed" ? "Checks failed" : k === "ci_crashed" ? "Checks crashed" : k === "not_picked" ? "Not picked" : "Closed";
}

/** "#9, #12": the PRs that beat a PR to its lines, or moved main under it. */
export function priorRefs(list: { number?: number | null; change_id: string }[] | undefined): string {
  return (list ?? []).map((p) => prRef(p)).join(", ");
}

/** "#12", or the change id for a PR from before numbers. */
export function prRef(c: { number?: number | null; change_id: string }): string {
  return c.number ? `#${c.number}` : c.change_id;
}

/** What stands between an open PR and main, or why it closed, in one line. */
export function mergeStatus(c: Change): string {
  const r = c.reason;
  switch (c.state) {
    case "queued":
      return c.position ? `Queued (position ${c.position} of ${c.queue_len ?? c.position})` : "Queued";
    case "awaiting_seal": {
      const missing = (r?.quorum ?? []).filter((t) => t.have.length < t.need);
      if (!missing.length) return `Blocked: needs a signature (${c.seal_reasons.join("; ")})`;
      return (
        "Blocked: needs " +
        missing
          .map((t) => {
            const more = t.need - t.have.length;
            return `${more} ${t.roles.length ? t.roles.join(" or ") : "approver"} signature${more === 1 ? "" : "s"} (${t.have.length} of ${t.need})`;
          })
          .join(" and ")
      );
    }
    case "awaiting_allowance":
      return "Blocked: needs an allowance";
    case "awaiting_pick":
      return r?.passed === false ? "Waiting for pick (its trial failed)" : "Waiting for pick";
    case "landed":
      return "Merged";
    case "ejected": {
      if (r?.kind === "conflict") {
        const by = priorRefs(r.beaten_by);
        return `Needs rebase: conflict in ${(r.paths ?? []).join(", ")}${by ? ` with ${by}` : ""}; the agent rebases`;
      }
      if (r?.kind === "ci_failed") {
        const d = r.diagnostics?.[0];
        return d ? `Checks failed: ${d.code} ${d.file}:${d.line}` : "Checks failed";
      }
      if (r?.kind === "ci_crashed") return "Checks crashed: the type checker failed";
      if (r?.kind === "not_picked") return `Not picked: ${r.picked ?? "another attempt"} shipped`;
      if (r?.kind === "closed") return `Closed by ${r.by ?? "its author"}${r.detail ? `: ${r.detail}` : ""}`;
      return `Closed${r?.kind ? `: ${r.kind.replace(/_/g, " ")}` : ""}`;
    }
  }
}

/** Labels a git host would show: the task's, and the ones the rules and paths imply. */
export function labelsFor(paths: string[], reasons: string[], taskLabels: string[] = []): string[] {
  const out = new Set(taskLabels);
  for (const r of reasons) {
    const kind = r.split(":")[0].trim();
    if (kind === "governance" || kind === "irreversible") out.add(kind);
    if (kind === "review" || kind === "risk") out.add("risk");
  }
  for (const p of paths) {
    if (p.startsWith("migrations/")) out.add("migration");
    if (/(^|\/)(deps\/|package(-lock)?\.json$|pnpm-lock\.yaml$|yarn\.lock$|Cargo\.(toml|lock)$|deps\.lock$)/.test(p)) out.add("dependencies");
    if (p.startsWith("ci/") || p.startsWith(".github/")) out.add("ci");
    if (p === "nerudo.toml" || p === "landseq.toml") out.add("governance");
  }
  return [...out];
}

export const TASK_STATUS: { id: TaskStatus; label: string }[] = [
  { id: "landing", label: "Merging" },
  { id: "in_progress", label: "In progress" },
  { id: "todo", label: "Todo" },
  { id: "backlog", label: "Backlog" },
  { id: "done", label: "Done" },
  { id: "canceled", label: "Canceled" },
];

export const PRIORITY = ["No priority", "Urgent", "High", "Medium", "Low"];

/** A commit subject without the "a01ts: " prefix the change id already shows. */
export function subjectOf(id: string, subject: string | null | undefined): string {
  const s = subject ?? "";
  return s.startsWith(`${id}: `) ? s.slice(id.length + 2) : s;
}

export function short(oid: string | null | undefined): string {
  return oid ? oid.slice(0, 7) : "—";
}

/** Two or three letters for a badge: "a01" is A1, "claude" is CL. */
export function initials(name: string): string {
  const clean = name.replace(/^@/, "");
  const m = /^([A-Za-z]+)[-_]?0*(\d{1,2})$/.exec(clean);
  if (m) return (m[1][0] + m[2]).toUpperCase();
  return clean.slice(0, 2).toUpperCase();
}

/** A stable hue for a name, so each agent and person keeps one color. Similar names get far-apart hues. */
export function hue(name: string): number {
  let h = 0x811c9dc5;
  for (const c of name) h = Math.imul(h ^ c.charCodeAt(0), 0x01000193);
  h = Math.imul(h ^ (h >>> 16), 0x85ebca6b);
  h = Math.imul(h ^ (h >>> 13), 0xc2b2ae35);
  return ((h ^ (h >>> 16)) >>> 0) % 360;
}

export function copy(text: string): Promise<void> {
  return navigator.clipboard?.writeText(text) ?? Promise.resolve();
}

/** A context file's sections by heading (plan, decisions, rejected, …), without its front matter. */
export function contextSections(text: string): { title: string; lines: string[] }[] {
  const body = text.replace(/^---\n[\s\S]*?\n---\n?/, "");
  const out: { title: string; lines: string[] }[] = [];
  let cur: { title: string; lines: string[] } | null = null;
  for (const raw of body.split("\n")) {
    const l = raw.trim();
    const h = /^#{2,6}\s+(.*)$/.exec(l);
    if (h) {
      cur = { title: h[1], lines: [] };
      out.push(cur);
    } else if (l && !l.startsWith("# ")) {
      if (!cur) {
        cur = { title: "", lines: [] };
        out.push(cur);
      }
      cur.lines.push(l);
    }
  }
  return out.filter((s) => s.lines.length > 0);
}

/** The first lines of a plan: under "Plan", then "Decisions", then other text; never the rejected ideas. */
export function planLines(text: string, n: number): string[] {
  const secs = contextSections(text);
  const is = (s: { title: string }, w: string) => s.title.toLowerCase().startsWith(w);
  const rank = (s: { title: string }) => (is(s, "plan") ? 0 : is(s, "decision") ? 1 : 2);
  return secs
    .filter((s) => !is(s, "rejected") && !is(s, "alternative"))
    .sort((a, b) => rank(a) - rank(b))
    .flatMap((s) => s.lines)
    .slice(0, n);
}

/** `decodeURIComponent`, or the text as it is when it is not percent-encoded (a bad `%`
 *  sequence throws, and a URL or a cookie is not a reason for a page to fail). */
export function decodeSafe(s: string): string {
  try {
    return decodeURIComponent(s);
  } catch {
    return s;
  }
}
