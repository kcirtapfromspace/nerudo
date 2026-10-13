import { useMemo, useState } from "react";
import type { FileDiff } from "../api";
import { highlightedLines } from "../syntax";
import { CodeTokens } from "./Code";

export function Diff({ files }: { files: FileDiff[] | null }) {
  if (!files) return <div className="mute pad">The diff is not available (the fork's token may have run out).</div>;
  if (files.length === 0) return <div className="mute pad">No file changes.</div>;
  const adds = files.reduce((n, f) => n + (f.adds ?? 0), 0);
  const dels = files.reduce((n, f) => n + (f.dels ?? 0), 0);
  return (
    <div className="diff">
      <div className="diff-sum mono">
        {files.length} {files.length === 1 ? "file" : "files"} <span className="add">+{adds}</span> <span className="del">−{dels}</span>
      </div>
      {files.map((f) => (
        <DiffFile key={f.path} f={f} />
      ))}
    </div>
  );
}

function DiffFile({ f }: { f: FileDiff }) {
  const [open, setOpen] = useState(f.lines.length < 400);
  const lines = useMemo(() => highlightedLines(f.lines.map((line) => line.length === 1 ? "" : line[3]).join("\n"), undefined, f.path), [f.lines, f.path]);
  return (
    <div className="dfile">
      <button className="dfile-head mono" onClick={() => setOpen(!open)}>
        <span className="tw">{open ? "▾" : "▸"}</span>
        <span className="dpath">{f.path}</span>
        <span className={`dstatus ${f.status}`}>{f.status}</span>
        {!f.binary && (
          <span className="dcount">
            <span className="add">+{f.adds ?? 0}</span> <span className="del">−{f.dels ?? 0}</span>
          </span>
        )}
      </button>
      {open &&
        (f.binary ? (
          <div className="mute pad">Binary or large file.</div>
        ) : (
          <table className="dlines mono">
            <tbody>
              {f.lines.map((l, i) =>
                l.length === 1 ? (
                  <tr key={i} className="gap">
                    <td colSpan={3}>⋯</td>
                  </tr>
                ) : (
                  <tr key={i} className={l[0] === "+" ? "add" : l[0] === "-" ? "del" : ""}>
                    <td className="n">{l[1] ?? ""}</td>
                    <td className="n">{l[2] ?? ""}</td>
                    <td className="t">
                      <span className="op">{l[0] === " " ? " " : l[0]}</span>
                      <CodeTokens tokens={lines[i] || []} />
                    </td>
                  </tr>
                ),
              )}
            </tbody>
          </table>
        ))}
    </div>
  );
}
