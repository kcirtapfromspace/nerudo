"use client";

import { Command } from "cmdk";
import type { Snapshot } from "../api";
import { VIEWS, type View } from "../route";
import { prRef, STATE, stateLabel, subjectOf } from "../util";

export interface PaletteActions {
  go: (view: View, id?: string | null) => void;
  newTask?: () => void;
  copySeal?: () => void;
  help: () => void;
  searchFiles?: () => void;
  searchContents?: () => void;
}

export function Palette(props: { open: boolean; onClose: () => void; snap: Snapshot | null; actions: PaletteActions }) {
  const s = props.snap;
  const run = (f: () => void) => {
    props.onClose();
    f();
  };
  return (
    <Command.Dialog open={props.open} onOpenChange={(o) => !o && props.onClose()} label="Command palette" className="palette" overlayClassName="palette-back">
      <div className="modal-title">
        <span>Find</span>
        <button className="modal-x" onClick={props.onClose} aria-label="Close">
          ✕
        </button>
      </div>
      <Command.Input autoFocus placeholder="A pull request, a task, an agent… or a command" />
      <Command.List>
        <Command.Empty>Nothing matches.</Command.Empty>
        <Command.Group heading="Go to">
          {VIEWS.map((v) => (
            <Command.Item key={v.id} value={`go ${v.label}`} onSelect={() => run(() => props.actions.go(v.id))}>
              {v.label}
              <span className="pk">
                g {v.key}
              </span>
            </Command.Item>
          ))}
        </Command.Group>
        <Command.Group heading="Actions">
          {props.actions.searchFiles && <Command.Item value="find files filename path fuzzy search" onSelect={() => run(props.actions.searchFiles!)}>Search repository<span className="pk">⌘P</span></Command.Item>}
          {props.actions.newTask && (
            <Command.Item value="new task write" onSelect={() => run(props.actions.newTask!)}>
              New task<span className="pk">c</span>
            </Command.Item>
          )}
          {props.actions.copySeal && (
            <Command.Item value="copy approve command seal" onSelect={() => run(props.actions.copySeal!)}>
              Copy the approve command<span className="pk">s</span>
            </Command.Item>
          )}
          <Command.Item value="keyboard shortcuts help" onSelect={() => run(props.actions.help)}>
            Keyboard shortcuts<span className="pk">?</span>
          </Command.Item>
        </Command.Group>
        {s && s.pending.length > 0 && (
          <Command.Group heading="Open pull requests">
            {s.pending.map((c) => (
              <Command.Item key={`p${c.change_id}`} value={`${prRef(c)} ${c.change_id} ${c.subject} ${c.agent}`} onSelect={() => run(() => props.actions.go("pulls", c.change_id))}>
                <span className="callsign">{prRef(c)}</span> {subjectOf(c.change_id, c.subject)} <span className={`pk t-${STATE[c.state].tone}`}>{stateLabel(c)}</span>
              </Command.Item>
            ))}
          </Command.Group>
        )}
        {s && s.tasks.length > 0 && (
          <Command.Group heading="Tasks">
            {s.tasks.map((i) => (
              <Command.Item key={i.id} value={`${i.id} ${i.title} ${i.labels.join(" ")}`} onSelect={() => run(() => props.actions.go("tasks", i.id))}>
                <span className="callsign">{i.id}</span> {i.title} <span className="pk">{i.status.replace("_", " ")}</span>
              </Command.Item>
            ))}
          </Command.Group>
        )}
        {s && s.agents.length > 0 && (
          <Command.Group heading="Agents">
            {s.agents.map((a) => (
              <Command.Item key={a} value={`agent ${a}`} onSelect={() => run(() => props.actions.go("agents", a))}>
                {a}
              </Command.Item>
            ))}
          </Command.Group>
        )}
        {s && s.landings.length > 0 && (
          <Command.Group heading="Merged">
            {s.landings.slice(0, 40).map((l) => (
              <Command.Item key={`l${l.seq}`} value={`${prRef(l)} ${l.change_id} ${l.subject} seq ${l.seq}`} onSelect={() => run(() => props.actions.go("graph", l.change_id))}>
                <span className="callsign">{prRef(l)}</span> {subjectOf(l.change_id, l.subject)} <span className="pk">{l.commit_out.slice(0, 7)}</span>
              </Command.Item>
            ))}
          </Command.Group>
        )}
      </Command.List>
    </Command.Dialog>
  );
}
