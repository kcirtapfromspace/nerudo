"use client";

import { useState } from "react";
import type { Api, Attempt, CompareResult, ContextResult, FileDiff, Snapshot } from "../api";
import { useDetail } from "../api";
import { Rich } from "../components/bits";
import { Badge, Box, Props } from "../components/classic";
import { Diff } from "../components/Diff";
import { Ago } from "../components/time";
import { contextSections, STATE, stateLabel, subjectOf } from "../util";

const POLICY: Record<string, string> = {
  person: "a person, here",
  smallest: "the smallest policy: the passing attempt that changes the fewest lines, once every attempt is in",
  first: "the first policy: the first passing attempt, once every attempt is in",
};

/** A task's competing attempts side by side: each one's trial, why, plan and files, and a pick. */
export function Compare(props: { api: Api; snap: Snapshot; id: string; stamp: string; openChange: (id: string) => void; onPicked: () => void }) {
  const { data, error, loading, reload } = useDetail<CompareResult>(props.api, `task/${encodeURIComponent(props.id)}/compare`, props.stamp);
  const [busy, setBusy] = useState<string | null>(null);
  const [err, setErr] = useState<string | null>(null);
  if (error) return <p className="err">{error}</p>;
  if (!data) return <p className="mute">{loading ? "Loading the attempts…" : ""}</p>;
  const t = data.task;
  const me = props.snap.me?.name ?? null;
  const attempts = data.attempts;
  const pick = async (cid: string) => {
    setBusy(cid);
    try {
      await props.api.post(`task/${encodeURIComponent(t.id)}/pick`, { change: cid });
      setErr(null);
      reload();
      props.onPicked();
    } catch (e) {
      setErr(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(null);
    }
  };
  const cols = { gridTemplateColumns: `repeat(${Math.max(attempts.length, 1)}, minmax(280px, 1fr))` };
  // Paths changed by more attempts first: those are where the attempts really differ.
  const counts = new Map<string, number>();
  for (const a of attempts) for (const f of a.files ?? []) counts.set(f.path, (counts.get(f.path) ?? 0) + 1);
  const paths = [...counts.keys()].sort((x, y) => (counts.get(y) ?? 0) - (counts.get(x) ?? 0) || x.localeCompare(y));

  return (
    <div className="compare">
      <Box title={`${attempts.length} of ${t.attempts ?? attempts.length} attempts`}>
        <p>
          {t.picked ? (
            <>
              <b>{t.picked}</b> shipped, picked by {t.picked_by ?? "?"}. The other attempts closed as not picked.
            </>
          ) : (
            <>Each attempt got a trial: applied to main as main is now, checked and type-checked, but not pushed. Nothing merges until one is picked; the picked attempt merges with its own CI run, and the others close.</>
          )}
        </p>
        <p className="mute small">Who picks: {POLICY[data.policy] ?? data.policy}. Attempts taken by {(t.attempting ?? []).join(", ") || "nobody yet"}.</p>
        {err && <p className="err">{err}</p>}
      </Box>

      {attempts.length === 0 ? (
        <p className="mute">No attempt yet. Agents claim the task, and each lands its attempt with task: {t.id}.</p>
      ) : (
        <>
          <div className="cmp-cols" style={cols}>
            {attempts.map((a) => (
              <Candidate key={a.change_id} api={props.api} a={a} picked={t.picked ?? null} me={me} busy={busy} onPick={pick} openChange={props.openChange} snap={props.snap} />
            ))}
          </div>
          <Box title="Files, side by side">
            {paths.map((p) => (
              <div key={p} className="cmp-file">
                <div className="cmp-path mono">
                  {p} <span className="mute small">changed by {counts.get(p)} of {attempts.length}</span>
                </div>
                <div className="cmp-cols" style={cols}>
                  {attempts.map((a) => {
                    const f: FileDiff | undefined = a.files?.find((x) => x.path === p);
                    return (
                      <div key={a.change_id} className="cmp-cell">
                        {f ? <Diff files={[f]} /> : <span className="mute small">{a.files ? "not changed" : "diff not available"}</span>}
                      </div>
                    );
                  })}
                </div>
              </div>
            ))}
          </Box>
        </>
      )}
    </div>
  );
}

function Candidate(props: {
  api: Api;
  a: Attempt;
  picked: string | null;
  me: string | null;
  busy: string | null;
  snap: Snapshot;
  onPick: (cid: string) => void;
  openChange: (id: string) => void;
}) {
  const { a, picked } = props;
  const trial = a.trial;
  const won = picked === a.change_id;
  const failed = trial?.passed === false;
  const waiting = a.state === "awaiting_pick";
  const verdict =
    trial == null ? (
      <span className="mute">no trial yet</span>
    ) : trial.passed === true ? (
      <span className="chip c-landed">Passed</span>
    ) : trial.passed === false ? (
      <span className="chip c-bounced">Failed</span>
    ) : (
      <span className="chip">No verdict</span>
    );
  const diags = failed ? (trial?.ci?.diagnostics ?? []) : [];
  const title = !props.me
    ? "Open your personal link to pick"
    : picked
      ? "An attempt is already picked"
      : failed
        ? "Its checks failed"
        : !waiting
          ? "It is not waiting for a pick"
          : "Merge this attempt; close the others";
  return (
    <fieldset className={`box cmp-col${won ? " won" : ""}${picked && !won ? " lost" : ""}`}>
      <legend>
        <Badge name={a.agent} size={14} /> {a.agent} · <span className="mono">{a.change_id}</span>
      </legend>
      <div className="cmp-subject">{subjectOf(a.change_id, a.subject) || <span className="mute">rev {a.rev}</span>}</div>
      <Props
        rows={[
          ["State", <span key="s" className={`chip c-${STATE[a.state].tone}`}>{won && a.state !== "landed" ? "Picked" : stateLabel(a)}</span>],
          ["Checks", verdict],
          ["Files", trial ? trial.files : "—"],
          [
            "Lines",
            trial ? (
              <span key="l" className="mono">
                <span className="add">+{trial.adds}</span> <span className="del">−{trial.dels}</span>
              </span>
            ) : (
              "—"
            ),
          ],
          [
            "Paths",
            a.footprint.length ? (
              a.footprint.map((f) => (
                <span key={f} className="chip">
                  {f}
                </span>
              ))
            ) : (
              <span className="mute">—</span>
            ),
          ],
          ["Opened", <span key="o"><Ago ms={a.submitted_ms} /> ago, rev {a.rev}</span>],
        ]}
      />
      {diags.slice(0, 3).map((d, i) => (
        <div key={i} className="diag mono">
          <b>{d.code}</b> {d.file}:{d.line} <span>{d.text}</span>
        </div>
      ))}
      <div className="why cmp-why">{a.why ? <Rich text={a.why} snap={props.snap} /> : <span className="mute">No why in the commit.</span>}</div>
      <AttemptPlan api={props.api} a={a} />
      <div className="row-end">
        <button className="link" onClick={() => props.openChange(a.change_id)}>
          Open the PR
        </button>
        <button className="btn primary" disabled={!props.me || !!picked || failed || !waiting || props.busy !== null} title={title} onClick={() => props.onPick(a.change_id)}>
          {props.busy === a.change_id ? "Shipping…" : won ? "Shipped" : "Ship this one"}
        </button>
      </div>
    </fieldset>
  );
}

/** An attempt's plan, decisions and rejected alternatives: the version of its context that the
 *  sequencer copied when the attempt was sent (GET context/{change}?rev=&v=). */
function AttemptPlan({ api, a }: { api: Api; a: Attempt }) {
  const path = a.context_commit ? `context/${encodeURIComponent(a.change_id)}?rev=${a.rev}${a.context_version ? `&v=${a.context_version}` : ""}` : null;
  const { data, error } = useDetail<ContextResult>(api, path, path ?? "");
  if (!path) return <p className="mute small cmp-plan">No plan: the agent sent no context with this attempt.</p>;
  if (error) return <p className="mute small cmp-plan">Its plan can't be read: {error}</p>;
  if (!data) return <p className="mute small cmp-plan">Reading its plan…</p>;
  const sections = data.text ? contextSections(data.text) : [];
  if (!sections.length) return <p className="mute small cmp-plan">{data.error ?? "Its context is empty."}</p>;
  return (
    <div className="cmp-plan">
      {sections.map((s, i) => (
        <div key={i}>
          {s.title && (
            <div className="small">
              <b>{s.title}</b>
            </div>
          )}
          <blockquote className="plan small">
            {s.lines.map((l, j) => (
              <div key={j}>{l}</div>
            ))}
          </blockquote>
        </div>
      ))}
      <div className="mute small">
        Context v{data.v ?? "?"} of revision {data.rev}
        {data.immutable ? ", the sequencer's copy" : ", from the agent's fork"}.
      </div>
    </div>
  );
}
