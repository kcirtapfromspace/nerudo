import type { ReactNode } from "react";
import type { ChangeState, Snapshot } from "../api";
import { withEmoji } from "../emoji";
import { STATE } from "../util";

/** A change's state as a small framed tag. */
export function StatePill({ state }: { state: ChangeState }) {
  const s = STATE[state];
  return <span className={`chip c-${s.tone}`}>{s.label}</span>;
}

export function Kbd({ children }: { children: ReactNode }) {
  return <kbd>{children}</kbd>;
}

/** Text with emoji shortcodes and highlighted @mentions of known people, teams and agents. */
export function Rich({ text, snap }: { text: string; snap: Snapshot | null }) {
  const people = new Set(snap?.people.map((p) => p.name));
  const teams = new Set(snap?.teams);
  const agents = new Set(snap?.agents);
  const parts = withEmoji(text).split(/(@[A-Za-z0-9._-]*[A-Za-z0-9_])/g);
  return (
    <>
      {parts.map((p, i) => {
        if (!p.startsWith("@")) return <span key={i}>{p}</span>;
        const name = p.slice(1);
        const kind = people.has(name) ? "person" : teams.has(name) ? "team" : agents.has(name) ? "agent" : null;
        return kind ? (
          <span key={i} className={`mention ${kind}`} title={kind}>
            {p}
          </span>
        ) : (
          <span key={i}>{p}</span>
        );
      })}
    </>
  );
}

export function Empty({ title, children }: { title: string; children?: ReactNode }) {
  return (
    <div className="empty">
      <div className="empty-title">{title}</div>
      {children && <div className="empty-body">{children}</div>}
    </div>
  );
}
