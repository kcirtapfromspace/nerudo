import { useMemo, useRef, useState } from "react";
import type { Api, Reactions, Snapshot, Thread as ThreadData } from "../api";
import { PICKER, QUICK, SHORTCODES } from "../emoji";
import { Rich } from "./bits";
import { Badge } from "./classic";
import { Clock } from "./time";

/** Reactions on a change, an task, or one comment. */
export function ReactionBar(props: { api: Api; thread: string; comment?: string; reactions: Reactions; me: string | null; onDone: () => void }) {
  const [picker, setPicker] = useState(false);
  const shown = Object.entries(props.reactions).filter(([, who]) => who.length > 0);
  const react = async (emoji: string) => {
    setPicker(false);
    await props.api.post("react", { thread: props.thread, comment: props.comment ?? "", emoji });
    props.onDone();
  };
  return (
    <div className="rx">
      {shown.map(([e, who]) => (
        <button key={e} className={`rx-chip${props.me && who.includes(props.me) ? " on" : ""}`} title={who.join(", ")} disabled={!props.me} onClick={() => react(e)}>
          {e} <span>{who.length}</span>
        </button>
      ))}
      {props.me && (
        <span className="rx-add">
          <button className="rx-chip ghost" onClick={() => setPicker(!picker)} title="React (e)">
            ☺ +
          </button>
          {picker && (
            <div className="rx-picker" onMouseLeave={() => setPicker(false)}>
              {PICKER.map((e) => (
                <button key={e} onClick={() => react(e)}>
                  {e}
                </button>
              ))}
            </div>
          )}
        </span>
      )}
    </div>
  );
}

export function Thread(props: { api: Api; snap: Snapshot | null; thread: string; data: ThreadData; onDone: () => void }) {
  const me = props.snap?.me?.name ?? null;
  return (
    <div className="thread">
      <ReactionBar api={props.api} thread={props.thread} reactions={props.data.reactions} me={me} onDone={props.onDone} />
      {props.data.comments.map(({ comment: c, reactions }) => (
        <div className="comment" key={c.id}>
          <Badge name={c.author} size={22} />
          <div className="comment-main">
            <div className="comment-head">
              <b>{c.author}</b>
              {c.agent && <span className="tag">agent</span>}
              <span className="mute"><Clock ms={c.at_ms} /></span>
            </div>
            <div className="comment-body">
              <Rich text={c.body} snap={props.snap} />
            </div>
            <ReactionBar api={props.api} thread={props.thread} comment={c.id} reactions={reactions} me={me} onDone={props.onDone} />
          </div>
        </div>
      ))}
      {me ? (
        <Composer api={props.api} snap={props.snap} thread={props.thread} onDone={props.onDone} />
      ) : (
        <div className="mute small">Open your personal link to comment, react or ask for a review.</div>
      )}
    </div>
  );
}

/** A comment box that completes @people, @teams, agents and :shortcodes:. */
export function Composer(props: { api: Api; snap: Snapshot | null; thread: string; onDone: () => void }) {
  const [text, setText] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [pick, setPick] = useState(0);
  const ref = useRef<HTMLTextAreaElement>(null);

  const names = useMemo(() => {
    const s = props.snap;
    if (!s) return [] as { label: string; hint: string }[];
    return [
      ...s.people.map((p) => ({ label: p.name, hint: p.roles.join(", ") || "person" })),
      ...s.teams.map((t) => ({ label: t, hint: "team" })),
      ...s.agents.map((a) => ({ label: a, hint: "agent" })),
    ];
  }, [props.snap]);

  // The token being typed at the caret: @name or :shortcode.
  const caret = ref.current?.selectionStart ?? text.length;
  const before = text.slice(0, caret);
  const at = /(^|\s)@([A-Za-z0-9._-]*)$/.exec(before);
  const colon = /(^|\s):([a-z0-9_+-]{2,})$/.exec(before);
  const options = at
    ? names.filter((n) => n.label.toLowerCase().startsWith(at[2].toLowerCase())).slice(0, 6).map((n) => ({ insert: `@${n.label} `, label: `@${n.label}`, hint: n.hint }))
    : colon
      ? SHORTCODES.filter(([n]) => n.startsWith(colon[2])).slice(0, 6).map(([n, e]) => ({ insert: `${e} `, label: `${e} :${n}:`, hint: "" }))
      : [];

  const complete = (insert: string) => {
    const m = at ?? colon;
    if (!m) return;
    const start = caret - m[2].length - 1;
    const next = text.slice(0, start) + insert + text.slice(caret);
    setText(next);
    setPick(0);
    requestAnimationFrame(() => {
      const pos = start + insert.length;
      ref.current?.setSelectionRange(pos, pos);
      ref.current?.focus();
    });
  };

  const send = async () => {
    if (!text.trim() || busy) return;
    setBusy(true);
    setError(null);
    try {
      await props.api.post("comment", { thread: props.thread, body: text });
      setText("");
      props.onDone();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="composer">
      <textarea
        ref={ref}
        id="composer"
        value={text}
        rows={3}
        placeholder="Comment. @person, @team or an agent notifies them; :rocket: works; ⌘↵ sends."
        onChange={(e) => setText(e.target.value)}
        onKeyDown={(e) => {
          if (options.length && (e.key === "Tab" || e.key === "Enter")) {
            e.preventDefault();
            complete(options[pick % options.length].insert);
          } else if (options.length && (e.key === "ArrowDown" || e.key === "ArrowUp")) {
            e.preventDefault();
            setPick((p) => (e.key === "ArrowDown" ? p + 1 : p + options.length - 1) % options.length);
          } else if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) {
            e.preventDefault();
            send();
          } else if (e.key === "Escape") {
            (e.target as HTMLTextAreaElement).blur();
          }
          e.stopPropagation();
        }}
      />
      {options.length > 0 && (
        <div className="complete">
          {options.map((o, i) => (
            <button key={o.label} className={i === pick % options.length ? "on" : ""} onMouseDown={(e) => (e.preventDefault(), complete(o.insert))}>
              <span>{o.label}</span>
              <span className="mute">{o.hint}</span>
            </button>
          ))}
        </div>
      )}
      <div className="composer-foot">
        <span className="quick">
          {QUICK.slice(0, 4).map((e) => (
            <button key={e} onClick={() => setText((t) => t + e)} title="Add to the comment">
              {e}
            </button>
          ))}
        </span>
        {error && <span className="err">{error}</span>}
        <button className="btn primary" disabled={busy || !text.trim()} onClick={send}>
          Comment <kbd>⌘↵</kbd>
        </button>
      </div>
    </div>
  );
}
