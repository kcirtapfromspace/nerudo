import { useState } from "react";
import type { Api, CompareResult, Release, ReleaseChange, ReleaseDraft, Snapshot } from "../api";
import { useDetail } from "../api";
import { Badge, Box, Props, Tabs, Tree } from "../components/classic";
import { Markdown } from "../components/Markdown";
import { Ago, Clock } from "../components/time";
import { copy } from "../util";

/** The Releases tab's detail: the draft of the next release, or one release. A proposal in
 *  the inbox opens as release:<tag>, which is a draft until someone signs it. */
export function ReleasePane(props: { api: Api; snap: Snapshot; id: string; stamp: string; openChange: (id: string) => void }) {
  const tag = props.id.replace(/^release:/, "");
  const released = (props.snap.releases ?? []).some((r) => r.tag === tag);
  if (tag === "draft" || !released) {
    return <DraftRelease key={tag} api={props.api} snap={props.snap} version={tag === "draft" ? null : tag} stamp={props.stamp} openChange={props.openChange} />;
  }
  return <ReleaseDetail key={tag} api={props.api} tag={tag} stamp={props.stamp} openChange={props.openChange} />;
}

function ChangeTable({ changes, openChange }: { changes: ReleaseChange[]; openChange: (id: string) => void }) {
  if (changes.length === 0) return <p className="mute">No pull requests merged in this range.</p>;
  return (
    <table className="grid">
      <thead>
        <tr>
          <th>PR</th>
          <th>Title</th>
          <th>Section</th>
          <th>Agent</th>
          <th>Task</th>
          <th>Approved by</th>
        </tr>
      </thead>
      <tbody>
        {changes.map((c) => (
          <tr key={c.seq} className="click" onClick={() => openChange(c.change_id)}>
            <td className="mono">{c.pr || c.change_id}</td>
            <td>{c.subject}</td>
            <td>{c.section}</td>
            <td>{c.agent ?? ""}</td>
            <td className="mono">{c.task ?? ""}</td>
            <td>{c.approved_by.join(", ")}</td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}

function CompareTable({ api, base, head, stamp }: { api: Api; base: string; head: string; stamp: string }) {
  const { data, error } = useDetail<CompareResult>(api, `compare/${encodeURIComponent(base)}...${encodeURIComponent(head)}`, stamp);
  if (error) return <p className="err">{error}</p>;
  if (!data) return <p className="mute">Loading…</p>;
  return (
    <>
      <p>
        {base}...{head}: {data.total.files} files changed, <span className="add">+{data.total.adds}</span> <span className="del">−{data.total.dels}</span>
      </p>
      <table className="grid">
        <thead>
          <tr>
            <th>File</th>
            <th>Status</th>
            <th>Lines</th>
          </tr>
        </thead>
        <tbody>
          {data.files.map((f) => (
            <tr key={f.path}>
              <td className="mono">{f.path}</td>
              <td>{f.status}</td>
              <td className="mono">
                {f.adds === null ? <span className="mute">binary or large</span> : (
                  <>
                    <span className="add">+{f.adds}</span> <span className="del">−{f.dels}</span>
                  </>
                )}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </>
  );
}

function Stats({ r }: { r: { stats: Release["stats"]; files: Release["files"] } }) {
  const s = r.stats;
  return (
    <Tree
      items={[
        { label: "Pull requests", value: s.prs, tone: "landed" },
        { label: "Agents", value: s.agents, tone: "progress" },
        { label: "Batches", value: s.batches, tone: "queued" },
        { label: "CI runs", value: s.ci_runs, tone: "queued" },
        { label: "Tasks closed", value: s.tasks_closed.length ? s.tasks_closed.join(", ") : "none", tone: "idle" },
        ...(r.files ? [{ label: "Files changed", value: `${r.files.files} · +${r.files.adds} −${r.files.dels}`, tone: "file" }] : []),
      ]}
    />
  );
}

export function ReleaseDetail(props: { api: Api; tag: string; stamp: string; openChange: (id: string) => void }) {
  const { data, error, loading } = useDetail<Release>(props.api, `release/${encodeURIComponent(props.tag)}`, props.stamp);
  if (error) return <div className="detail-pad err">{error}</div>;
  if (!data) return <div className="detail-pad mute">{loading ? "Loading…" : ""}</div>;
  const r = data;
  return (
    <article className="detail" tabIndex={-1}>
      <header className="d-head">
        <Badge tone={r.prerelease ? "approval" : "landed"} size={20} />
        <h1 className="d-title">{r.title}</h1>
        {r.title !== r.tag && <span className="d-code">{r.tag}</span>}
        <span className="d-head-right">
          {r.latest && !r.prerelease && <span className="chip c-landed">Latest</span>}
          {r.prerelease && <span className="chip c-approval">Pre-release</span>}
          <span className="chip">signed by {r.released_by.map((s) => s.name).join(", ")}</span>
        </span>
      </header>
      <Tabs
        tabs={[
          ["notes", "Notes"],
          ["changes", `Pull requests (${r.changes.length})`],
          ["files", `Files (${r.files?.files ?? "?"})`],
          ["signature", "Signature"],
        ]}
      >
        {(tab) => {
          if (tab === "changes") {
            return (
              <Box title={`Merged in ${r.tag}`}>
                <ChangeTable changes={r.changes} openChange={props.openChange} />
              </Box>
            );
          }
          if (tab === "files") {
            return (
              <Box title={r.previous ? `Changed since ${r.previous}` : "The first release"}>
                {r.previous ? <CompareTable api={props.api} base={r.previous} head={r.tag} stamp={props.stamp} /> : <p className="mute">The first release has no earlier tag to compare with. {r.files ? `${r.files.files} files changed since the first commit on main.` : ""}</p>}
              </Box>
            );
          }
          if (tab === "signature") {
            return (
              <Box title="Who released it">
                <p>
                  The sequencer pushed the annotated tag <span className="mono">refs/tags/{r.tag}</span> after these approvers signed a release statement for this commit
                  and these notes. <span className="mono">nerudo verify</span> checks the signatures from outside, against nerudo.toml at the released commit.
                </p>
                <table className="grid">
                  <thead>
                    <tr>
                      <th>Approver</th>
                      <th>Key</th>
                      <th>Signature</th>
                    </tr>
                  </thead>
                  <tbody>
                    {r.released_by.map((s) => (
                      <tr key={s.key}>
                        <td>
                          <b>{s.name}</b>
                        </td>
                        <td className="mono">{s.key.slice(0, 16)}…</td>
                        <td className="mono">{s.signature.slice(0, 24)}…</td>
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
                <Box title="Release notes">
                  <div className="why md-box">
                    <Markdown text={r.notes} />
                  </div>
                </Box>
              </div>
              <div>
                <Box title="Release">
                  <Props
                    rows={[
                      ["Tag", <span key="t" className="mono">{r.tag}</span>],
                      ["Commit", <span key="c" className="mono">{r.commit.slice(0, 12)}</span>],
                      ["Landing", <span key="s" className="mono">#{r.seq}</span>],
                      ["Previous", <span key="p" className="mono">{r.previous ?? "none"}</span>],
                      ["Released", <span key="w"><Clock ms={r.created_ms} />, <Ago ms={r.created_ms} /> ago</span>],
                      ["Tag object", <span key="o" className="mono">{r.tag_object.slice(0, 12)}</span>],
                    ]}
                  />
                </Box>
                <Box title="In this release">
                  <Stats r={r} />
                </Box>
              </div>
            </div>
          );
        }}
      </Tabs>
    </article>
  );
}

function DraftRelease(props: { api: Api; snap: Snapshot; version: string | null; stamp: string; openChange: (id: string) => void }) {
  const path = `release/draft${props.version ? `?version=${encodeURIComponent(props.version)}` : ""}`;
  const { data, error, loading } = useDetail<ReleaseDraft>(props.api, path, props.stamp);
  const [copied, setCopied] = useState(false);
  if (error) {
    return (
      <div className="detail-pad">
        <Box title="Draft a new release">
          <p className={/^Nothing has merged/.test(error) ? "" : "err"}>{error}</p>
        </Box>
      </div>
    );
  }
  if (!data) return <div className="detail-pad mute">{loading ? "Loading…" : ""}</div>;
  const d = data;
  const command = `${d.command} --yes`;
  return (
    <article className="detail" tabIndex={-1}>
      <header className="d-head">
        <Badge tone="queued" size={20} />
        <h1 className="d-title">Draft: {d.tag}</h1>
        <span className="d-code">at landing #{d.seq}</span>
        <span className="d-head-right">
          <span className="chip">{d.previous ? `after ${d.previous}` : "first release"}</span>
        </span>
      </header>
      <Tabs
        tabs={[
          ["notes", "Preview"],
          ["changes", `Pull requests (${d.changes.length})`],
        ]}
      >
        {(tab) =>
          tab === "changes" ? (
            <Box title="What this release would ship">
              <ChangeTable changes={d.changes} openChange={props.openChange} />
            </Box>
          ) : (
            <div className="cols">
              <div>
                <Box title="Sign it in a terminal">
                  <p>
                    The next version comes from the merged PRs' subjects (feat: a minor bump, a breaking change a major one, anything else a patch). A release needs{" "}
                    {d.quorum.count} signature{d.quorum.count === 1 ? "" : "s"} from {d.quorum.eligible.join(", ") || "an approver"}. Your key stays on your machine: the app never
                    signs.
                  </p>
                  <div className="cmd">
                    <code>{command}</code>
                    <button
                      className="btn"
                      onClick={() =>
                        copy(command).then(() => {
                          setCopied(true);
                          setTimeout(() => setCopied(false), 1500);
                        })
                      }
                    >
                      {copied ? "Copied" : "Copy"}
                    </button>
                  </div>
                  <p className="mute small">
                    Leave out <span className="mono">--yes</span> to read the notes in the terminal first; add <span className="mono">--notes FILE</span> to write your own.
                  </p>
                </Box>
                <Box title="Release notes (preview)">
                  <div className="why md-box">
                    <Markdown text={d.notes} />
                  </div>
                </Box>
              </div>
              <div>
                <Box title="Draft">
                  <Props
                    rows={[
                      ["Version", <span key="v" className="mono">{d.tag}</span>],
                      ["Commit", <span key="c" className="mono">{d.commit.slice(0, 12)}</span>],
                      ["Landing", <span key="s" className="mono">#{d.seq}</span>],
                      ["Previous", <span key="p" className="mono">{d.previous ?? "none"}</span>],
                    ]}
                  />
                </Box>
                <Box title="Would ship">
                  <Stats r={d} />
                </Box>
              </div>
            </div>
          )
        }
      </Tabs>
    </article>
  );
}
