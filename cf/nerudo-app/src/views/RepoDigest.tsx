import type { Api, RepoDigestResult } from "../api";
import { useDetail } from "../api";

export interface RepoDigestSearchPreset { language?: string; extension?: string; category?: string }

interface RepoDigestProps {
  api: Api;
  stamp: string;
  at?: number | null;
  initial?: RepoDigestResult | null;
  openFile: (path: string) => void;
  onSearch: (preset: RepoDigestSearchPreset) => void;
}

const count = (value: number) => value.toLocaleString("en-US");
function bytes(value: number) {
  if (value < 1024) return `${count(value)} B`;
  const units = ["KiB", "MiB", "GiB", "TiB"];
  let size = value / 1024;
  let unit = 0;
  while (size >= 1024 && unit < units.length - 1) { size /= 1024; unit++; }
  return `${size.toLocaleString("en-US", { maximumFractionDigits: 1 })} ${units[unit]}`;
}
function percentage(part: number, whole: number) {
  const value = whole ? part / whole * 100 : 0;
  return value > 0 && value < .1 ? "<0.1%" : `${value.toLocaleString("en-US", { maximumFractionDigits: 1 })}%`;
}
const color = (index: number) => `digest-color-${index % 6}`;
const stackOrder = { framework: 0, language: 1, tool: 2, "package-manager": 3 };

/** A compact orientation before the repository's author-written README. */
export function RepoDigest({ api, stamp, at, initial, openFile, onSearch }: RepoDigestProps) {
  const endpoint = at != null ? `digest?at=${at}` : "digest";
  const digest = useDetail<RepoDigestResult>(api, endpoint, stamp, initial?.at === (at ?? null) ? initial : null);
  const data = digest.data;
  const languages = data?.languages.slice(0, 6) || [];
  const otherSourceBytes = data ? Math.max(0, data.source_bytes - languages.reduce((total, language) => total + language.bytes, 0)) : 0;
  const stack = [...(data?.stack || [])].filter((item) => item.paths.length).sort((a, b) => stackOrder[a.kind] - stackOrder[b.kind]).slice(0, 6);
  const files = data?.important_files.filter((file) => file.kind !== "readme").slice(0, 4) || [];
  return <section className="repo-digest" aria-label="Repository digest" aria-busy={digest.loading || (!data && !digest.error)}>
    <header className="digest-heading"><h2>Repository digest</h2>{data && data.total_files > 0 && <p className="digest-totals" title={`Commit ${data.commit}. Totals cover the full tracked tree.`}><span><b>{count(data.total_files)}</b> tracked {data.total_files === 1 ? "file" : "files"}</span><span><b>{bytes(data.source_bytes)}</b> source</span></p>}</header>
    {digest.error && <p className="digest-state" role="status" title={digest.error}>{data ? "Couldn’t refresh digest." : "Digest unavailable."} <button className="link" onClick={digest.reload}>Try again</button></p>}
    {!data && !digest.error && <div className="digest-state" role="status">Reading repository…<div className="digest-skeleton" aria-hidden="true" /></div>}
    {data && (!data.total_files ? <p className="digest-state">No files yet.</p> : <>
      {languages.length > 0 && <>
        <div className="digest-language-bar" aria-hidden="true">{languages.map((language, index) => <i key={language.name} className={color(index)} style={{ flexGrow: language.bytes }} />)}{otherSourceBytes > 0 && <i className="digest-color-other" style={{ flexGrow: otherSourceBytes }} />}</div>
        <ul className="digest-languages" aria-label="Source language composition">{languages.map((language, index) => <li key={language.name}><button type="button" onClick={() => onSearch({ language: language.name })} title={`Find ${language.name} files · ${count(language.files)} files`} aria-label={`Find ${language.name} files, ${percentage(language.bytes, data.source_bytes)} of source bytes`}><i className={`digest-swatch ${color(index)}`} aria-hidden="true" /><span className="digest-language-name">{language.name}</span><span className="digest-percentage">{percentage(language.bytes, data.source_bytes)}</span></button></li>)}{otherSourceBytes > 0 && <li><span className="digest-language-rest"><i className="digest-swatch digest-color-other" aria-hidden="true" /><span>Other</span><span className="digest-percentage">{percentage(otherSourceBytes, data.source_bytes)}</span></span></li>}</ul>
      </>}
      <p className="digest-measure" title="Source files and tests are estimated from file extensions and bytes. Common generated and vendor paths are excluded.">{languages.length ? "Source bytes · extension estimate" : "No source languages detected"}</p>
      {stack.length > 0 && <div className="digest-line"><span className="digest-label">Stack</span><ul className="digest-links" aria-label="Detected stack">{stack.map((item) => <li key={`${item.kind}:${item.name}`}><button type="button" className="digest-evidence" title={item.paths.join(", ")} onClick={() => openFile(item.paths[0])} aria-label={`Open ${item.paths[0]}, evidence for ${item.name}`}>{item.name}</button></li>)}</ul></div>}
      {files.length > 0 && <div className="digest-line"><span className="digest-label">Files</span><ul className="digest-links digest-files" aria-label="Important files">{files.map((file) => <li key={file.path}><button type="button" title={file.label} onClick={() => openFile(file.path)}>{file.path}</button></li>)}</ul></div>}
    </>)}
  </section>;
}
