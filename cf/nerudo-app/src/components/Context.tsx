"use client";

import { useEffect, useState } from "react";
import type { AgentContextResult, Api, ContextResult } from "../api";
import { useDetail } from "../api";
import { STATE } from "../util";
import { Box, Props, Tree } from "./classic";
import { Markdown } from "./Markdown";
import { Ago, Clock } from "./time";

/** A pull request's Context tab: the sequencer's immutable copies of what the agent recorded,
 *  version by version, and which version the merged commit pins. */
export function ChangeContext({ api, id, rev }: { api: Api; id: string; rev: number }) {
  const [pick, setPick] = useState(rev);
  const [version, setVersion] = useState<number | null>(null);
  const path = `context/${encodeURIComponent(id)}?rev=${pick}${version ? `&v=${version}` : ""}`;
  const { data, error, loading } = useDetail<ContextResult>(api, path, path);
  if (error) return <Box title="Context"><p className="err">{error}</p></Box>;
  if (!data) return <Box title="Context"><p className="mute">{loading ? "Loading…" : ""}</p></Box>;
  const versions = data.versions ?? [];
  const shown = versions.find((v) => v.v === data.v);
  return (
    <div className="cols">
      <div>
        <Box
          title={shown ? `Context, revision ${data.rev}, v${shown.v}${shown.pinned ? " (pinned)" : ""}` : `Context, revision ${data.rev}`}
          right={shown ? <span className="mono">{shown.commit.slice(0, 7)}</span> : data.commit ? <span className="mono">{data.commit.slice(0, 7)}</span> : undefined}
        >
          {data.text != null ? (
            <Markdown text={data.text} />
          ) : (
            <p className="mute">{data.error ?? `${data.agent} recorded no context with revision ${data.rev}.`}</p>
          )}
          {data.truncated && <p className="mute small">Cut at 256 KiB.</p>}
        </Box>
      </div>
      <div>
        {versions.length > 0 && (
          <Box title={`Versions of revision ${data.rev} (${versions.length})`}>
            <Tree
              items={versions.map((v) => ({
                label: `v${v.v}${v.pinned ? " · pinned to the merged commit" : v.sent ? " · sent with this revision" : " · added later"}`,
                tone: v.pinned ? "landed" : v.sent ? "queued" : "idle",
                value: <Clock ms={v.at_ms} />,
                state: v.v === data.v ? "now" : undefined,
                onClick: () => setVersion(v.v),
              }))}
            />
          </Box>
        )}
        <Box title={`Revisions with context (${data.revisions.length})`}>
          {data.revisions.length === 0 ? (
            <p className="mute">None. An agent passes context to land; the sequencer keeps an immutable copy.</p>
          ) : (
            <Tree
              items={data.revisions.map((r) => ({
                label: `revision ${r.rev}`,
                tone: STATE[r.state].tone,
                value: r.commit.slice(0, 7),
                state: r.rev === data.rev ? "now" : undefined,
                onClick: () => {
                  setVersion(null);
                  setPick(r.rev);
                },
              }))}
            />
          )}
        </Box>
        <Box title={data.immutable ? "Immutable copy" : "Where it lives"}>
          <Props
            rows={
              shown
                ? [
                    ["Repo", <span key="r" className="mono wrap">{data.repo}</span>],
                    ["File", <span key="p" className="mono wrap">{shown.path}</span>],
                    ["Commit", <span key="c" className="mono">{shown.commit.slice(0, 12)}</span>],
                    ["Source", <span key="s" className="mono wrap">{shown.source_fork}@{shown.source_commit.slice(0, 12)}</span>],
                  ]
                : [
                    ["Agent", data.agent],
                    ["Fork", <span key="f" className="mono wrap">{data.fork}</span>],
                    ["Branch", <span key="b" className="mono">nerudo/context</span>],
                    ["File", <span key="p" className="mono wrap">{data.path ?? "—"}</span>],
                  ]
            }
          />
          <p className="mute small">
            {data.immutable
              ? "Only the sequencer writes this repo. It adds a file for every version and never changes one. The merged commit's Context: trailer pins one version, and nerudo verify checks it. The source is the agent's own branch, which the agent may rewrite."
              : "Read from the agent's fork: this host has no repo the sequencer owns."}
          </p>
        </Box>
      </div>
    </div>
  );
}

/** An agent's Context tab: its working copy (its fork's nerudo/context branch), newest
 *  commit first, and one file. The agent can rewrite it; the record is the context repo. */
export function AgentContext({ api, name }: { api: Api; name: string }) {
  const listPath = `agent/${encodeURIComponent(name)}/context`;
  const list = useDetail<AgentContextResult>(api, listPath, listPath);
  const [sel, setSel] = useState<{ commit: string; path: string } | null>(null);
  const filePath = sel ? `${listPath}?commit=${sel.commit}&path=${encodeURIComponent(sel.path)}` : null;
  const file = useDetail<AgentContextResult>(api, filePath, filePath ?? "");
  const first = list.data?.commits[0];
  useEffect(() => {
    if (!sel && first?.paths[0]) setSel({ commit: first.commit, path: first.paths[0] });
  }, [sel, first]);
  if (list.error) return <Box title="Context"><p className="err">{list.error}</p></Box>;
  const d = list.data;
  if (!d) return <Box title="Context"><p className="mute">{list.loading ? "Loading…" : ""}</p></Box>;
  const note = `Working copy: the agent's own nerudo/context branch, which it can rewrite. The sequencer's immutable copies are in ${d.record ?? "the context repo"}; each PR's Context tab shows them.`;
  if (!d.tip) {
    return (
      <Box title="Context (working copy)">
        <p className="mute">{d.error ?? `${name} has recorded no context yet. Agents pass context to land.`}</p>
        <p className="mute small">{note}</p>
      </Box>
    );
  }
  return (
    <div className="cols">
      <div>
        <Box title={sel ? `Working copy: ${sel.path}` : "Working copy"} right={sel ? <span className="mono">{sel.commit.slice(0, 7)}</span> : undefined}>
          {file.data?.file ? <Markdown text={file.data.file.text} /> : <p className="mute">{file.error ?? (file.loading ? "Loading…" : "Pick a file.")}</p>}
        </Box>
      </div>
      <div>
        <Box title={`nerudo/context on ${d.fork} (${d.commits.length})`} right={<span className="mono">{d.tip.slice(0, 7)}</span>}>
          <table className="grid">
            <thead>
              <tr>
                <th>Commit</th>
                <th>Written</th>
                <th>File</th>
              </tr>
            </thead>
            <tbody>
              {d.commits.flatMap((c) =>
                (c.paths.length ? c.paths : [""]).map((p) => (
                  <tr key={`${c.commit}:${p}`} className={`click${sel?.commit === c.commit && sel.path === p ? " on" : ""}`} onClick={() => p && setSel({ commit: c.commit, path: p })}>
                    <td className="mono">{c.commit.slice(0, 7)}</td>
                    <td>
                      <Ago ms={c.at_ms} /> ago
                    </td>
                    <td className="mono wrap">{p.replace(/^changes\//, "") || c.subject}</td>
                  </tr>
                )),
              )}
            </tbody>
          </table>
          <p className="mute small">{note}</p>
        </Box>
      </div>
    </div>
  );
}
