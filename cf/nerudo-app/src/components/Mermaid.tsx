/// <reference types="vite/client" />
"use client";

import { useEffect, useId, useState } from "react";
import type { Mermaid } from "mermaid";
import { CodeBlock } from "./Code";

// System fonts only: the diagram is drawn as an image, and an image cannot use the page's web fonts.
const FONT = 'Tahoma, Verdana, "DejaVu Sans", "Segoe UI", sans-serif';

// Diagrams are drawn only in the browser. The server build leaves Mermaid out of the Worker.
const importMermaid = import.meta.env.SSR ? null : () => import("mermaid");

let loading: Promise<Mermaid> | null = null;
/** Mermaid is large, so only a page with a diagram loads it, and only once. */
function mermaid(): Promise<Mermaid> {
  if (!importMermaid) return Promise.reject(new Error("diagrams are drawn in the browser"));
  loading ??= importMermaid().then(({ default: m }) => {
    m.initialize({ startOnLoad: false, securityLevel: "strict", suppressErrorRendering: true, htmlLabels: false, theme: "neutral", fontFamily: FONT, themeVariables: { fontFamily: FONT, xyChart: { backgroundColor: "transparent" } } });
    return m;
  }).catch((error) => { loading = null; throw error; });
  return loading;
}

/** The name of a diagram for its alt text: its title or accTitle line, else its kind. */
export function diagramTitle(code: string): string {
  const named = /^\s*(?:accTitle\s*:\s*(.+)|title\s*:\s*(.+)|title\s+"([^"]+)"|title\s+(.+))$/m.exec(code);
  const title = named && (named[1] || named[2] || named[3] || named[4])?.trim();
  if (title) return title;
  const body = code.replace(/^\s*---\n[\s\S]*?\n---\n/, "");
  const kind = body.split("\n").map((line) => line.trim()).find((line) => line && !line.startsWith("%%"))?.split(/\s/)[0];
  return kind ? `Diagram (${kind})` : "Diagram";
}

/** A drawn diagram: the SVG as an image source, and its width in pixels. */
type Drawing = { src: string; width: number | null };

/** The SVG as an image source. An <img> runs no script and loads nothing, whatever the diagram contains. */
export function diagramImage(svg: string): Drawing {
  const style = /<svg[^>]*\sstyle="[^"]*max-width:\s*([\d.]+)px/.exec(svg);
  const box = /<svg[^>]*\sviewBox="[\d.-]+\s+[\d.-]+\s+([\d.]+)\s+[\d.]+"/.exec(svg);
  const width = Number(style?.[1] ?? box?.[1]);
  return { src: `data:image/svg+xml;charset=utf-8,${encodeURIComponent(svg)}`, width: Number.isFinite(width) && width > 0 ? Math.ceil(width) : null };
}

// The diagrams drawn in this tab, by their source. A block that mounts again shows its image at
// once, not its source until Mermaid draws it again.
const DRAWN_MAX = 64;
const drawn = new Map<string, Drawing>();
export function rememberDiagram(code: string, image: Drawing) {
  drawn.delete(code);
  drawn.set(code, image);
  if (drawn.size > DRAWN_MAX) drawn.delete(drawn.keys().next().value!);
}

/** A ```mermaid block of a document, drawn as a diagram. Until it is drawn, and if it cannot be drawn, the source shows. */
export function MermaidBlock({ code }: { code: string }) {
  const id = `mermaid-${useId().replace(/[^A-Za-z0-9_-]/g, "")}`;
  const [image, setImage] = useState<Drawing | null>(() => drawn.get(code) ?? null);
  const [failed, setFailed] = useState(false);
  useEffect(() => {
    const known = drawn.get(code);
    setImage(known ?? null);
    setFailed(false);
    if (known) return;
    let live = true;
    mermaid()
      .then((m) => m.render(id, code))
      .then(({ svg }) => {
        const made = diagramImage(svg);
        rememberDiagram(code, made);
        if (live) setImage(made);
      })
      .catch(() => { if (live) setFailed(true); });
    return () => { live = false; };
  }, [code, id]);
  if (!image) return <div className="md-diagram-pending" data-failed={failed || undefined}>
    {failed && <p className="md-diagram-note" role="status">This diagram cannot be drawn. Its source follows.</p>}
    <CodeBlock code={code} language="mermaid" />
  </div>;
  const title = diagramTitle(code);
  return <figure className="md-diagram">
    <img src={image.src} alt={title} style={image.width ? { width: image.width } : undefined} />
    <details><summary>Diagram source</summary><CodeBlock code={code} language="mermaid" /></details>
  </figure>;
}
