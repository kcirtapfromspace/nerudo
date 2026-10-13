import type { Api, TreeResult } from "../api";
import { useDetail } from "../api";
import { CodeBlock } from "../components/Code";
import { Markdown } from "../components/Markdown";
import { Tabs } from "../components/classic";
import { isMarkdown, repositoryLink, rootReadme } from "../readme";
import type { Route } from "../route";

export function RepoReadme({ api, root, route, stamp, initial }: { api: Api; root: ReturnType<typeof useDetail<TreeResult>>; route: Route; stamp: string; initial?: TreeResult | null }) {
  const path = root.data ? rootReadme(root.data) : null;
  const pinned = root.data?.at != null ? `&at=${root.data.at}` : "";
  const readme = useDetail<TreeResult>(api, path ? `tree?path=${encodeURIComponent(path)}${pinned}` : null, stamp, initial?.commit === root.data?.commit ? initial : null);
  const problem = root.error || readme.error;
  return <article className="repo-readme detail" tabIndex={-1}>
    <header className="readme-heading"><h2>{path || "README"}</h2>{readme.data?.commit && <span className="mono mute">{readme.data.commit.slice(0, 7)}</span>}</header>
    {problem ? <div className="readme-state" role="status"><h3>README unavailable</h3><p>We couldn’t load the repository’s introduction.</p><button className="btn" onClick={() => { root.reload(); readme.reload(); }}>Try again</button><details><summary>Connection details</summary><p>{problem}</p></details></div> : !root.data || (path && !readme.data) ? <div className="readme-state" role="status">Loading README…</div> : !path ? <div className="readme-state"><h3>No root README yet.</h3><p>Add a README.md to introduce the project, explain setup, and help others contribute.</p></div> : typeof readme.data?.text !== "string" ? <div className="readme-state"><h3>This README cannot be displayed.</h3><p>It is binary or exceeds the file preview limit.</p></div> : <Tabs key={path} label="README views" tabs={isMarkdown(path) ? [["preview", "Preview"], ["source", "Source"]] : [["source", "Source"]]}>{(tab) => tab === "preview" ? <Markdown text={readme.data?.text || ""} fileLink={(href) => repositoryLink(route, path, href)} /> : <CodeBlock code={readme.data?.text || ""} filename={path} lineNumbers />}</Tabs>}
  </article>;
}
