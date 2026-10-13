import { useState } from "react";
import type { AgentDetail as Data, Api, Snapshot } from "../api";
import { useDetail } from "../api";
import { StatePill } from "../components/bits";
import { Badge, Box, Props, Tabs } from "../components/classic";
import { AgentContext } from "../components/Context";
import { Ago } from "../components/time";
import { subjectOf } from "../util";

export function AgentDetail(props: { api: Api; snap: Snapshot; name: string; stamp: string; openChange: (id: string) => void }) {
  const { data, error, loading, reload } = useDetail<Data>(props.api, `agent/${encodeURIComponent(props.name)}`, props.stamp);
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState<string | null>(null);
  if (error) return <div className="detail-pad err">{error}</div>;
  if (!data) return <div className="detail-pad mute">{loading ? "Loading…" : ""}</div>;
  const me = props.snap.me?.name ?? null;
  const rows = "ok" in data.refs ? data.refs.ok : null;
  const stale = rows?.filter((r) => r.stale).length ?? 0;
  const landed = props.snap.landings.filter((l) => l.agent === data.agent).length;
  const bounced = props.snap.ejected.filter((c) => c.agent === data.agent).length;
  const tasks = props.snap.tasks.filter((t) => t.assignee === data.agent && !["done", "canceled"].includes(t.status));
  const cleanup = async () => {
    setBusy(true);
    try {
      const r = await props.api.post<{ deleted: number }>("cleanup", { agent: data.agent });
      setNote(`Deleted ${r.deleted} stale ref${r.deleted === 1 ? "" : "s"}.`);
      reload();
    } catch (e) {
      setNote(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };
  return (
    <article className="detail" tabIndex={-1}>
      <header className="d-head">
        <Badge name={data.agent} size={20} />
        <h1 className="d-title">{data.agent}</h1>
        <span className="d-code">agent</span>
      </header>
      <Tabs
        tabs={[
          ["overview", "Overview"],
          ["refs", `Branches (${rows?.length ?? 0})`],
          ["changes", `Pull requests (${data.changes.length})`],
          ["context", "Context"],
        ]}
      >
        {(tab) => {
          if (tab === "refs") {
            return (
              <Box title="Working branches in the fork">
                {rows ? (
                  <table className="grid">
                    <thead>
                      <tr>
                        <th>Branch</th>
                        <th>Commit</th>
                        <th>Pull request</th>
                        <th>State</th>
                      </tr>
                    </thead>
                    <tbody>
                      {rows.map((r) => (
                        <tr key={r.name} className={r.stale ? "stale" : ""}>
                          <td className="mono">{r.name.replace("refs/heads/", "")}</td>
                          <td className="mono">{r.oid.slice(0, 7)}</td>
                          <td>
                            {r.change ? (
                              <button className="link" onClick={() => props.openChange(r.change!.change_id)}>
                                {r.change.change_id} rev {r.change.rev}
                              </button>
                            ) : r.name.endsWith("/main") || r.name === "HEAD" ? (
                              <span className="mute">where the fork started</span>
                            ) : null}
                          </td>
                          <td>
                            {r.change?.state && <StatePill state={r.change.state} />} {r.stale && <span className="chip">stale</span>}
                          </td>
                        </tr>
                      ))}
                    </tbody>
                  </table>
                ) : (
                  <p className="err">{"error" in data.refs ? data.refs.error : ""}</p>
                )}
                <div className="ask">
                  <button className="btn" disabled={!me || busy || stale === 0} onClick={cleanup} title={me ? "" : "A personal link can delete them"}>
                    Delete {stale} stale branch{stale === 1 ? "" : "es"}
                  </button>
                  <span className="mute small">A branch is stale when its PR merged, or a later revision replaced it.</span>
                </div>
                {note && <p className="mute">{note}</p>}
              </Box>
            );
          }
          if (tab === "context") return <AgentContext api={props.api} name={data.agent} />;
          if (tab === "changes") {
            return (
              <Box title="Pull requests">
                <table className="grid">
                  <thead>
                    <tr>
                      <th>PR</th>
                      <th>Title</th>
                      <th>State</th>
                      <th>Sent</th>
                    </tr>
                  </thead>
                  <tbody>
                    {data.changes.map((c) => (
                      <tr key={`${c.change_id}:${c.rev}`} className="click" onClick={() => props.openChange(c.change_id)}>
                        <td className="mono">
                          {c.change_id} <span className="mute">r{c.rev}</span>
                        </td>
                        <td>{subjectOf(c.change_id, c.subject)}</td>
                        <td>
                          <StatePill state={c.state} />
                        </td>
                        <td>
                          <Ago ms={c.submitted_ms} />
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </Box>
            );
          }
          return (
            <div className="cols">
              <div>
                <Box title="Agent">
                  <Props
                    rows={[
                      ["Fork", <span key="f" className="mono wrap">{data.fork}</span>],
                      ["Merged", landed],
                      ["Closed", bounced],
                      ["Stale branches", stale],
                      [
                        "Open tasks",
                        tasks.length ? (
                          tasks.map((t) => (
                            <span key={t.id} className="chip">
                              <span className="mono">{t.id}</span> {t.title}
                            </span>
                          ))
                        ) : (
                          <span className="mute">none</span>
                        ),
                      ],
                    ]}
                  />
                </Box>
                {data.notes.length > 0 && (
                  <Box title={`Who called this agent (${data.notes.length})`}>
                    <table className="grid">
                      <thead>
                        <tr>
                          <th>From</th>
                          <th>On</th>
                          <th>Text</th>
                        </tr>
                      </thead>
                      <tbody>
                        {data.notes.map((n, i) => (
                          <tr key={i} className="click" onClick={() => props.openChange(n.change_id)}>
                            <td>
                              <b>{n.from}</b> <span className="mute">{n.kind.replace("_", " ")}</span>
                            </td>
                            <td className="mono">{n.change_id}</td>
                            <td>{n.text}</td>
                          </tr>
                        ))}
                      </tbody>
                    </table>
                  </Box>
                )}
              </div>
              <div />
            </div>
          );
        }}
      </Tabs>
    </article>
  );
}
