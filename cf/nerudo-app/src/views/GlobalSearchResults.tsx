import { globalItemHref, globalItemKey, type GlobalSearchItem } from "../global-search";
import type { RepoSearchOptions } from "../search";
import { SearchHighlight } from "./RepoSearch";

const highlighted = (text: string, positions: number[]) => <SearchHighlight text={text} ranges={positions.map((at) => [at, at + (text.codePointAt(at)! > 0xffff ? 2 : 1)])} />;

/** Entity and code hits share one plain list, with context on each result. */
export function GlobalSearchResults({ items, search }: { items: GlobalSearchItem[]; search: RepoSearchOptions }) {
  return <div className="global-results" aria-label="Global search results">{items.map((item) => {
    const href = globalItemHref(item, search);
    if (item.kind === "file") return <article className="global-result" key={globalItemKey(item)}>
      <div className="global-result-context"><a href={globalItemHref({ kind: "repo", org: item.org, repo: item.repo, trunk: item.trunk, public: item.public, description: "", score: 0, positions: [] }, search)}>{item.org}/{item.repo}</a><span>{item.public ? "File" : "Private file"}</span></div>
      <a href={href} className="global-result-title global-hit mono">{highlighted(item.path, item.positions)}</a>
      {item.lines.slice(0, 2).map((line) => <a className="global-result-line global-hit" href={globalItemHref(item, search, line.line)} key={line.line} aria-label={`${item.org}/${item.repo}, ${item.path}, line ${line.line}: ${line.text}`}><span className="global-line-number">{line.line}</span><span><SearchHighlight text={line.text} ranges={line.ranges} /></span></a>)}
      {item.lines.length > 2 && <a href={globalItemHref(item, search, item.lines[2].line)} className="global-result-more global-hit">More matches in this file</a>}
    </article>;
    const name = item.kind === "user" ? item.user : item.kind === "org" ? item.org : `${item.org}/${item.repo}`;
    const label = item.kind === "user" ? "Person" : item.kind === "org" ? "Organization" : item.public ? "Repository" : "Private repository";
    const description = item.kind === "user" ? item.title : item.kind === "org" ? [item.title, item.description].filter(Boolean).join(" · ") : item.description;
    return <article className="global-result" key={globalItemKey(item)}><div className="global-result-context"><span>{label}</span></div><a className="global-result-title global-hit" href={href}>{highlighted(name, item.positions)}</a>{description && <p>{description}</p>}</article>;
  })}</div>;
}
