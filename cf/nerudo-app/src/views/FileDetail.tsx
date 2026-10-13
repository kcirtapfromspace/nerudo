import type { Api, HistoryResult, TreeResult } from "../api";
import { useDetail } from "../api";
import { Badge, Box, Tabs } from "../components/classic";
import { Clock } from "../components/time";
import { subjectOf } from "../util";
import { CodeBlock } from "../components/Code";
import { Markdown } from "../components/Markdown";
import { isMarkdown } from "../readme";

export function FileDetail(props: { api: Api; path: string; stamp: string; sourceFirst?: boolean; openChange: (id: string) => void; fileLink?: (href: string) => string; openDirectory?: (path: string) => void }) {
  const q = encodeURIComponent(props.path);
  const file = useDetail<TreeResult>(props.api, `tree?path=${q}`, props.stamp);
  const hist = useDetail<HistoryResult>(props.api, `history?path=${q}`, props.stamp);
  if (file.error) return <div className="detail-pad readme-state" role="status"><h3>File unavailable</h3><p>We couldn’t read this file.</p><button className="btn" onClick={file.reload}>Try again</button><details><summary>Connection details</summary><p>{file.error}</p></details></div>;
  if (!file.data) return <div className="detail-pad mute">Loading…</div>;
  const f = file.data;
  const landings = hist.data?.landings ?? [];
  return (
    <article className="detail" tabIndex={-1}>
      <header className="d-head">
        <Badge tone="file" size={20} />
        <h1 className="d-title mono">{props.path}</h1>
        <span className="d-head-right mono mute">at {f.commit.slice(0, 7)}</span>
      </header>
      <Tabs
        key={`${props.path}:${props.sourceFirst}`}
        tabs={[
          ...(!props.sourceFirst && isMarkdown(props.path) ? [["preview", "Preview"] as [string, string]] : []),
          ["content", "Source"],
          ...(props.sourceFirst && isMarkdown(props.path) ? [["preview", "Preview"] as [string, string]] : []),
          ["history", `History (${landings.length})`],
        ]}
      >
        {(tab) =>
          tab === "history" ? (
            <Box title="Merges that changed it">
              {hist.error ? <div role="status"><p>File history is unavailable.</p><button className="btn" onClick={hist.reload}>Try again</button></div> : !hist.data ? <p className="mute" role="status">Loading history…</p> : landings.length === 0 ? (
                <p className="mute">No recorded merges changed this file.</p>
              ) : (
                <table className="grid">
                  <thead>
                    <tr>
                      <th>Merge</th>
                      <th>PR</th>
                      <th>Title</th>
                      <th>Agent</th>
                      <th>Merged</th>
                    </tr>
                  </thead>
                  <tbody>
                    {landings.map((l) => (
                      <tr key={l.seq} className="click" onClick={() => props.openChange(l.change_id)}>
                        <td className="mono">#{l.seq}</td>
                        <td className="mono">{l.change_id}</td>
                        <td>{subjectOf(l.change_id, l.subject)}</td>
                        <td>{l.author}</td>
                        <td>
                          <Clock ms={l.landed_ms} />
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              )}
            </Box>
          ) : f.kind === "dir" ? <div className="readme-state"><p>This path is a directory.</p>{props.openDirectory && <button className="btn" onClick={() => props.openDirectory?.(props.path)}>Browse this directory</button>}</div> : typeof f.text === "string" ? (
            tab === "preview" ? <Markdown text={f.text} fileLink={props.fileLink} /> : <CodeBlock code={f.text} filename={props.path} lineNumbers />
          ) : (
            <p className="mute">Binary or large file, {f.size} bytes.</p>
          )
        }
      </Tabs>
    </article>
  );
}
