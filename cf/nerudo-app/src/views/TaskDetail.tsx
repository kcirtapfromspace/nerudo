import { useEffect, useState } from "react";
import type { Api, Task, TaskDetail as Data, TaskStatus, Snapshot } from "../api";
import { useDetail } from "../api";
import { Rich } from "../components/bits";
import { Badge, Box, Props, Tabs, Tree } from "../components/classic";
import { Thread } from "../components/Thread";
import { TASK_STATUS, PRIORITY, STATE, stateLabel, subjectOf } from "../util";
import { Ago } from "../components/time";
import { Compare } from "./Compare";

const PICKS: [string, string][] = [
  ["person", "A person picks"],
  ["smallest", "Smallest passing attempt"],
  ["first", "First passing attempt"],
];

const TONE: Record<string, string> = { landing: "queued", done: "landed", canceled: "bounced", in_progress: "progress", backlog: "backlog", todo: "idle" };

export function TaskDetail(props: { api: Api; snap: Snapshot; id: string; stamp: string; openChange: (id: string) => void }) {
  const { data, error, loading, reload } = useDetail<Data>(props.api, `task/${encodeURIComponent(props.id)}`, props.stamp);
  const [edit, setEdit] = useState<"title" | "body" | null>(null);
  const [draft, setDraft] = useState("");
  const [err, setErr] = useState<string | null>(null);
  useEffect(() => setEdit(null), [props.id]);
  if (error) return <div className="detail-pad err">{error}</div>;
  if (!data) return <div className="detail-pad mute">{loading ? "Loading…" : ""}</div>;
  const i = data.task;
  const me = props.snap.me?.name ?? null;
  const save = async (patch: Partial<Task> & Record<string, unknown>) => {
    try {
      await props.api.post(`task/${encodeURIComponent(i.id)}`, patch);
      setErr(null);
      setEdit(null);
      reload();
    } catch (e) {
      setErr(e instanceof Error ? e.message : String(e));
    }
  };
  const assignees = [
    ...props.snap.agents.map((a) => ({ v: a, l: `${a} (agent)` })),
    ...props.snap.people.map((p) => ({ v: p.name, l: p.name })),
    ...props.snap.teams.map((t) => ({ v: "@" + t, l: "@" + t + " (team)" })),
  ];
  const status = TASK_STATUS.find((s) => s.id === i.status)?.label ?? i.status;
  return (
    <article className="detail" tabIndex={-1}>
      <header className="d-head">
        <Badge tone={TONE[i.status] ?? "idle"} size={20} />
        {edit === "title" ? (
          <input
            className="title-edit"
            autoFocus
            value={draft}
            onChange={(e) => setDraft(e.target.value)}
            onBlur={() => setEdit(null)}
            onKeyDown={(e) => {
              e.stopPropagation();
              if (e.key === "Enter") save({ title: draft });
              if (e.key === "Escape") setEdit(null);
            }}
          />
        ) : (
          <h1 className={`d-title${me ? " edit" : ""}`} onClick={() => me && (setDraft(i.title), setEdit("title"))} title={me ? "Click to edit" : undefined}>
            {i.title}
          </h1>
        )}
        <span className="d-code">{i.id}</span>
        <span className="d-head-right">
          <span className={`chip c-${TONE[i.status] ?? "idle"}`}>{status}</span>
        </span>
      </header>

      <Tabs
        tabs={
          (i.attempts ?? 1) >= 2
            ? // While nothing is picked, the comparison is what a person came for.
              i.picked
              ? [["overview", "Overview"], ["compare", `Compare (${data.changes.length})`], ["discussion", `Discussion (${data.thread.comments.length})`]]
              : [["compare", `Compare (${data.changes.length})`], ["overview", "Overview"], ["discussion", `Discussion (${data.thread.comments.length})`]]
            : [
                ["overview", "Overview"],
                ["discussion", `Discussion (${data.thread.comments.length})`],
              ]
        }
      >
        {(tab) =>
          tab === "compare" ? (
            <Compare api={props.api} snap={props.snap} id={i.id} stamp={props.stamp} openChange={props.openChange} onPicked={reload} />
          ) : tab === "discussion" ? (
            <Thread api={props.api} snap={props.snap} thread={i.id} data={data.thread} onDone={reload} />
          ) : (
            <div className="cols">
              <div>
                <Box title="Task">
                  <Props
                    rows={[
                      [
                        "Status",
                        <select key="s" disabled={!me} value={i.status} onChange={(e) => save({ status: e.target.value as TaskStatus })}>
                          {TASK_STATUS.map((s) => (
                            <option key={s.id} value={s.id}>
                              {s.label}
                            </option>
                          ))}
                        </select>,
                      ],
                      [
                        "Priority",
                        <select key="p" disabled={!me} value={i.priority} onChange={(e) => save({ priority: Number(e.target.value) })}>
                          {PRIORITY.map((p, n) => (
                            <option key={n} value={n}>
                              {p}
                            </option>
                          ))}
                        </select>,
                      ],
                      [
                        "Assignee",
                        <select key="a" disabled={!me} value={i.assignee ?? ""} onChange={(e) => save({ assignee: e.target.value })}>
                          <option value="">Nobody</option>
                          {assignees.map((a) => (
                            <option key={a.v} value={a.v}>
                              {a.l}
                            </option>
                          ))}
                        </select>,
                      ],
                      [
                        "Labels",
                        <input
                          key={i.labels.join(",")}
                          disabled={!me}
                          defaultValue={i.labels.join(", ")}
                          placeholder="api, billing"
                          onKeyDown={(e) => {
                            e.stopPropagation();
                            if (e.key === "Enter") (e.target as HTMLInputElement).blur();
                          }}
                          onBlur={(e) => e.target.value !== i.labels.join(", ") && save({ labels: e.target.value as unknown as string[] })}
                        />,
                      ],
                      [
                        "Attempts",
                        <select key="t" disabled={!me || !!i.picked} value={i.attempts ?? 1} onChange={(e) => save({ attempts: Number(e.target.value) })}>
                          {[1, 2, 3, 4, 5].map((n) => (
                            <option key={n} value={n}>
                              {n === 1 ? "One PR" : `${n} competing attempts`}
                            </option>
                          ))}
                        </select>,
                      ],
                      ...((i.attempts ?? 1) >= 2
                        ? ([
                            [
                              "Who picks",
                              <select key="k" disabled={!me || !!i.picked} value={i.pick || "person"} onChange={(e) => save({ pick: e.target.value })}>
                                {PICKS.map(([v, l]) => (
                                  <option key={v} value={v}>
                                    {l}
                                  </option>
                                ))}
                              </select>,
                            ],
                            ["Taken by", (i.attempting ?? []).join(", ") || <span key="n" className="mute">nobody yet</span>],
                          ] as [string, React.ReactNode][])
                        : []),
                      ["Written by", <span key="w">{i.created_by}, <Ago ms={i.created_ms} /> ago</span>],
                      ["Updated", <span key="u"><Ago ms={i.updated_ms} /> ago</span>],
                    ]}
                  />
                  {err && <p className="err">{err}</p>}
                </Box>

                <Box title="What should change, and why">
                  {edit === "body" ? (
                    <>
                      <textarea autoFocus rows={8} value={draft} onChange={(e) => setDraft(e.target.value)} onKeyDown={(e) => e.stopPropagation()} />
                      <div className="row-end">
                        <button className="btn" onClick={() => setEdit(null)}>
                          Cancel
                        </button>
                        <button className="btn primary" onClick={() => save({ body: draft })}>
                          Save
                        </button>
                      </div>
                    </>
                  ) : (
                    <div className={`why${me ? " edit" : ""}`} onClick={() => me && (setDraft(i.body), setEdit("body"))} title={me ? "Click to edit" : undefined}>
                      {i.body ? <Rich text={i.body} snap={props.snap} /> : <span className="mute">No description. {me ? "Click to write one." : ""}</span>}
                    </div>
                  )}
                </Box>
              </div>
              <div>
                <Box title={`Pull requests (${data.changes.length})`}>
                  {data.changes.length === 0 ? (
                    <p className="mute">
                      No PR yet. An agent claims the task, then opens a PR with <span className="mono">task: {i.id}</span>. The merge closes the task.
                    </p>
                  ) : (
                    <Tree
                      items={data.changes.map((c) => ({
                        label: (
                          <>
                            <span className="mono">{c.change_id}</span> {subjectOf(c.change_id, c.subject)}
                          </>
                        ),
                        tone: STATE[c.state].tone,
                        value: stateLabel(c),
                        onClick: () => props.openChange(c.change_id),
                      }))}
                    />
                  )}
                </Box>
                {i.labels.length > 0 && (
                  <Box title="Labels">
                    {i.labels.map((l) => (
                      <span key={l} className="chip">
                        {l}
                      </span>
                    ))}
                  </Box>
                )}
              </div>
            </div>
          )
        }
      </Tabs>
    </article>
  );
}

export function NewTask(props: { api: Api; snap: Snapshot; onClose: () => void; onCreated: (id: string) => void }) {
  const [title, setTitle] = useState("");
  const [body, setBody] = useState("");
  const [priority, setPriority] = useState(0);
  const [assignee, setAssignee] = useState("");
  const [labels, setLabels] = useState("");
  const [attempts, setAttempts] = useState(1);
  const [pick, setPick] = useState("person");
  const [err, setErr] = useState<string | null>(null);
  const create = async () => {
    try {
      const i = await props.api.post<Task>("tasks", { title, body, priority, assignee, labels, attempts, pick });
      props.onCreated(i.id);
    } catch (e) {
      setErr(e instanceof Error ? e.message : String(e));
    }
  };
  return (
    <div className="modal-back" onClick={props.onClose}>
      <div
        className="modal"
        role="dialog"
        aria-label="New task"
        onClick={(e) => e.stopPropagation()}
        onKeyDown={(e) => {
          e.stopPropagation();
          if (e.key === "Escape") props.onClose();
          if (e.key === "Enter" && (e.metaKey || e.ctrlKey) && title.trim()) create();
        }}
      >
        <div className="modal-title">
          <span>New task</span>
          <button className="modal-x" onClick={props.onClose} aria-label="Close">
            ✕
          </button>
        </div>
        <div className="modal-body">
          <input autoFocus placeholder="What should change?" value={title} onChange={(e) => setTitle(e.target.value)} />
          <textarea rows={6} placeholder="Why, and what done looks like. Agents read this when they claim it." value={body} onChange={(e) => setBody(e.target.value)} />
          <Props
            rows={[
              [
                "Priority",
                <select key="p" value={priority} onChange={(e) => setPriority(Number(e.target.value))}>
                  {PRIORITY.map((p, n) => (
                    <option key={n} value={n}>
                      {p}
                    </option>
                  ))}
                </select>,
              ],
              [
                "Assignee",
                <select key="a" value={assignee} onChange={(e) => setAssignee(e.target.value)}>
                  <option value="">Nobody yet</option>
                  {props.snap.agents.map((a) => (
                    <option key={a} value={a}>
                      {a} (agent)
                    </option>
                  ))}
                  {props.snap.people.map((p) => (
                    <option key={p.name} value={p.name}>
                      {p.name}
                    </option>
                  ))}
                </select>,
              ],
              ["Labels", <input key="l" value={labels} placeholder="api, billing" onChange={(e) => setLabels(e.target.value)} />],
              [
                "Attempts",
                <select key="t" value={attempts} onChange={(e) => setAttempts(Number(e.target.value))}>
                  {[1, 2, 3, 4, 5].map((n) => (
                    <option key={n} value={n}>
                      {n === 1 ? "One PR" : `${n} competing attempts, compared side by side`}
                    </option>
                  ))}
                </select>,
              ],
              ...(attempts >= 2
                ? ([
                    [
                      "Who picks",
                      <select key="k" value={pick} onChange={(e) => setPick(e.target.value)}>
                        {PICKS.map(([v, l]) => (
                          <option key={v} value={v}>
                            {l}
                          </option>
                        ))}
                      </select>,
                    ],
                  ] as [string, React.ReactNode][])
                : []),
            ]}
          />
          {err && <div className="err">{err}</div>}
          <div className="row-end">
            <button className="btn" onClick={props.onClose}>
              Cancel
            </button>
            <button className="btn primary" disabled={!title.trim()} onClick={create}>
              Write task <kbd>⌘↵</kbd>
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}
