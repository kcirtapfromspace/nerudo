"use client";

import { useId, useState, type ReactNode } from "react";
import { hue, initials } from "../util";

/** A small square badge: a state's color with a glyph, or a name's initials. */
export function Badge({ tone, name, size = 16 }: { tone?: string; name?: string; size?: number }) {
  if (name) {
    return (
      <span className="badge name" style={{ ["--h" as string]: hue(name), width: size, height: size }} title={name}>
        {initials(name)}
      </span>
    );
  }
  const glyph: Record<string, string> = { queued: "→", approval: "!", landed: "✓", bounced: "✕", progress: "◐", backlog: "…", idle: "·", file: "≡", dir: "▸" };
  return (
    <span className={`badge b-${tone ?? "idle"}`} style={{ width: size, height: size }}>
      {glyph[tone ?? "idle"] ?? "•"}
    </span>
  );
}

/** A group box: a caption over an etched frame. */
export function Box({ title, children, right }: { title: ReactNode; children: ReactNode; right?: ReactNode }) {
  return (
    <fieldset className="box">
      <legend>{title}</legend>
      {right && <div className="box-right">{right}</div>}
      {children}
    </fieldset>
  );
}

/** Raised tabs. `tabs` are [id, label]; the body renders the current one. */
export function Tabs({ tabs, children, initial, onTab, label = "Detail views" }: { tabs: [string, ReactNode][]; children: (tab: string) => ReactNode; initial?: string | null; onTab?: (tab: string) => void; label?: string }) {
  const base = useId();
  const [tab, setTab] = useState(initial ?? tabs[0][0]);
  const current = tabs.some(([id]) => id === tab) ? tab : tabs[0][0];
  return (
    <div className="tabs-wrap">
      <div className="tabs" role="tablist" aria-label={label} onKeyDown={(event) => {
        if (!["ArrowLeft", "ArrowRight", "Home", "End"].includes(event.key)) return;
        event.preventDefault();
        event.stopPropagation();
        const index = tabs.findIndex(([id]) => id === current);
        const next = event.key === "Home" ? 0 : event.key === "End" ? tabs.length - 1 : (index + (event.key === "ArrowRight" ? 1 : -1) + tabs.length) % tabs.length;
        setTab(tabs[next][0]);
        onTab?.(tabs[next][0]);
        event.currentTarget.querySelectorAll<HTMLButtonElement>("[role='tab']")[next]?.focus();
      }}>
        {tabs.map(([id, label]) => (
          <button key={id} id={`${base}-${id}`} role="tab" type="button" data-tab={id} aria-selected={id === current} aria-controls={`${base}-panel`} tabIndex={id === current ? 0 : -1} className={id === current ? "on" : ""} onClick={() => (setTab(id), onTab?.(id))}>
            {label}
          </button>
        ))}
      </div>
      <div className="tab-body" id={`${base}-panel`} role="tabpanel" aria-labelledby={`${base}-${current}`} tabIndex={0}>{children(current)}</div>
    </div>
  );
}

/** A two-column property table: the label in bold, the value in a white cell. */
export function Props({ rows }: { rows: [ReactNode, ReactNode][] }) {
  return (
    <table className="props-table">
      <tbody>
        {rows.map(([k, v], i) => (
          <tr key={i}>
            <th>{k}</th>
            <td>{v}</td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}

export interface TreeItem {
  label: ReactNode;
  value?: ReactNode;
  /** A badge: a state's color, or a name's initials. */
  tone?: string;
  name?: string;
  depth?: number;
  /** done, now, todo, hold or bad: how the label reads. */
  state?: string;
  /** A bold heading line between groups of items. */
  head?: boolean;
  onClick?: () => void;
}

/** A tree of steps or items: dotted connectors, a label, and a value right-aligned in mono. */
export function Tree({ items }: { items: TreeItem[] }) {
  return (
    <ul className="tree">
      {items.map((it, i) => {
        if (it.head) {
          return (
            <li key={i} className="tree-head">
              {it.label}
            </li>
          );
        }
        const d = it.depth ?? 0;
        let j = i + 1;
        while (items[j] && !items[j].head && (items[j].depth ?? 0) > d) j++;
        const last = !(items[j] && !items[j].head && (items[j].depth ?? 0) === d);
        const cls = [d ? "child" : "", d && last ? "last" : "", it.state ?? "", it.onClick ? "click" : ""].filter(Boolean).join(" ");
        return (
          <li key={i} className={cls} style={{ paddingLeft: 2 + d * 18, ["--d" as string]: d }} onClick={it.onClick}>
            {it.name ? <Badge name={it.name} size={16} /> : it.tone ? <Badge tone={it.tone} size={16} /> : null}
            <span className="tree-label">{it.label}</span>
            {it.value !== undefined && <span className="tree-value">{it.value}</span>}
          </li>
        );
      })}
    </ul>
  );
}
