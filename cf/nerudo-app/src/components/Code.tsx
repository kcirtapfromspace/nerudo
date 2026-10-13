"use client";

import { useEffect, useMemo, useState } from "react";
import { codeLanguage, highlightedLines, type CodeToken } from "../syntax";

export function CodeTokens({ tokens }: { tokens: CodeToken[] }) {
  return <>{tokens.map((token, index) => token.className ? <span key={index} className={token.className}>{token.text}</span> : token.text)}</>;
}

export function CodeBlock({ code, language, filename, lineNumbers = false }: { code: string; language?: string; filename?: string; lineNumbers?: boolean }) {
  const [feedback, setFeedback] = useState("");
  const [anchorLine, setAnchorLine] = useState<number | null>(null);
  const lines = useMemo(() => highlightedLines(code, language, filename), [code, language, filename]);
  const selected = codeLanguage(language, filename);
  useEffect(() => {
    setFeedback("");
    const followAnchor = () => {
      const match = lineNumbers ? /^#L(\d+)$/.exec(window.location.hash) : null;
      setAnchorLine(match ? Number(match[1]) : null);
      if (match) document.getElementById(`L${match[1]}`)?.scrollIntoView({ block: "center" });
    };
    followAnchor();
    window.addEventListener("hashchange", followAnchor);
    window.addEventListener("popstate", followAnchor);
    return () => { window.removeEventListener("hashchange", followAnchor); window.removeEventListener("popstate", followAnchor); };
  }, [code, lineNumbers]);
  async function copyCode() {
    try { await navigator.clipboard.writeText(code); setFeedback("Copied"); }
    catch { setFeedback("Select the code to copy it"); }
  }
  return <div className="code-block">
    <div className="code-caption"><span>{filename || selected || "Plain text"}</span><span className="code-feedback" role="status">{feedback}</span><button type="button" onClick={copyCode} aria-label={`Copy ${filename || "code"}`}>Copy</button></div>
    <pre className="code-scroll" tabIndex={0} aria-label={filename ? `Source of ${filename}` : "Code example"}><code>{lines.map((tokens, index) => <span className={`code-line${lineNumbers ? " numbered" : ""}${anchorLine === index + 1 ? " search-target" : ""}`} id={lineNumbers ? `L${index + 1}` : undefined} key={index}>{lineNumbers && <span className="code-number" aria-hidden="true">{index + 1}</span>}<span className="code-text"><CodeTokens tokens={tokens} />{!tokens.length && "\u200b"}</span></span>)}</code></pre>
  </div>;
}
