import { createContext, createElement, useContext, useMemo } from "react";
import ReactMarkdown, { defaultUrlTransform, type Components, type Options } from "react-markdown";
import remarkGfm from "remark-gfm";
import rehypeRaw from "rehype-raw";
import rehypeSanitize from "rehype-sanitize";
import type { Element, Root, RootContent } from "hast";
import { Props } from "./classic";
import { CodeBlock } from "./Code";
import { MermaidBlock } from "./Mermaid";

const textOf = (nodes: RootContent[]): string => nodes.map((node) => node.type === "text" ? node.value : node.type === "element" ? textOf(node.children) : "").join("");
const slugOf = (text: string) => text.toLowerCase().trim().replace(/[^\p{L}\p{N}\s_-]/gu, "").replace(/\s+/g, "-");

function documentHeadings() {
  return (tree: Root) => {
    const used = new Map<string, number>();
    const visit = (node: RootContent) => {
      if (node.type !== "element") return;
      if (/^h[1-6]$/.test(node.tagName) && !node.properties.id) {
        const slug = slugOf(textOf(node.children)) || "section";
        const count = used.get(slug) ?? 0; used.set(slug, count + 1);
        node.properties.id = slug + (count ? `-${count}` : "");
      }
      node.children.forEach(visit);
    };
    tree.children.forEach(visit);
  };
}

function documentAnchors() {
  return (tree: Root) => {
    const elements: Element[] = [];
    const visit = (node: RootContent) => {
      if (node.type !== "element") return;
      if (node.tagName === "a" && !node.properties.id && node.properties.name) node.properties.id = node.properties.name;
      elements.push(node); node.children.forEach(visit);
    };
    tree.children.forEach(visit);
    const ids = new Set(elements.map((node) => String(node.properties.id || "")));
    const resolve = (target: string) => !ids.has(target) && ids.has(`user-content-${target}`) ? `user-content-${target}` : target;
    for (const node of elements) {
      const href = node.properties.href;
      if (typeof href === "string" && href.startsWith("#")) node.properties.href = `#${resolve(href.slice(1))}`;
      for (const property of ["ariaDescribedBy", "ariaLabelledBy"]) {
        const references = node.properties[property];
        if (references) node.properties[property] = (Array.isArray(references) ? references.map(String) : String(references).split(/\s+/)).map(resolve).join(" ");
      }
    }
  };
}

type FileLink = (href: string) => string;
/** How the document on screen links to a repo file. A context keeps the components below the same in each render. */
const FileLinkContext = createContext<FileLink | undefined>(undefined);

// One set of components for all documents. React sees a component made again in a render as a
// new element type, and mounts again all that it draws: a diagram goes back to its source and a
// table loses its scroll position each time the page polls.
const components: Components = {
  pre: ({ node }) => {
    const code = node?.children.find((child): child is Element => child.type === "element" && child.tagName === "code");
    const classes = code?.properties.className;
    const language = Array.isArray(classes) ? String(classes.find((value) => String(value).startsWith("language-")) ?? "").replace(/^language-/, "") : undefined;
    const text = code ? textOf(code.children).replace(/\n$/, "") : "";
    return language === "mermaid" ? <MermaidBlock code={text} /> : <CodeBlock code={text} language={language} />;
  },
  a: function Link({ href = "", children, title, id, "aria-label": label, "aria-describedby": describedBy }) {
    const fileLink = useContext(FileLinkContext);
    const safe = defaultUrlTransform(href);
    if (!safe) return <span id={id}>{children}</span>;
    if (safe.startsWith("#")) return <a href={safe} id={id} title={title} aria-label={label} aria-describedby={describedBy}>{children}</a>;
    const external = /^https?:\/\//i.test(safe);
    const target = external || /^mailto:/i.test(safe) ? safe : fileLink?.(safe) || "";
    return target ? <a href={target} title={title} target={external ? "_blank" : undefined} rel={external ? "noopener noreferrer nofollow" : undefined}>{children}</a> : <span>{children}</span>;
  },
  img: function Image({ src, alt, title }) {
    const fileLink = useContext(FileLinkContext);
    return typeof src === "string" && /^https?:\/\//i.test(src) ? <img src={src} alt={alt || ""} title={title} loading="lazy" referrerPolicy="no-referrer" /> : <span className="md-image-link">{fileLink && typeof src === "string" && fileLink(src) ? <a href={fileLink(src)}>{alt || "View image file"}</a> : alt}</span>;
  },
  table: ({ children }) => <div className="md-table-scroll" tabIndex={0} role="region" aria-label="Document table"><table>{children}</table></div>,
};
for (const level of [1, 2, 3, 4, 5, 6] as const) {
  components[`h${level}`] = ({ children, id }) => {
    return createElement(`h${level}`, { id, className: `md-h md-h${level}` }, children);
  };
}
const remarkPlugins: Options["remarkPlugins"] = [remarkGfm];
const rehypePlugins: Options["rehypePlugins"] = [rehypeRaw, documentHeadings, rehypeSanitize, documentAnchors];

/** Repository and agent documents share GFM, syntax color, and sanitized HTML. */
export function Markdown({ text, fileLink }: { text: string; fileLink?: FileLink }) {
  const normalized = text.replace(/\r\n/g, "\n");
  const front = /^---\n([\s\S]*?)\n---\n?/.exec(normalized);
  const meta: [string, string][] = front ? front[1].split("\n").map((line) => /^([A-Za-z0-9_-]+):\s*(.*)$/.exec(line)).filter((match): match is RegExpExecArray => !!match).map((match) => [match[1], match[2]]) : [];
  const body = front ? normalized.slice(front[0].length) : normalized;
  // The document is parsed again only when its text changes, not each time the page polls.
  const document = useMemo(() => <ReactMarkdown remarkPlugins={remarkPlugins} rehypePlugins={rehypePlugins} components={components}>{body}</ReactMarkdown>, [body]);
  return <FileLinkContext.Provider value={fileLink}>
    <div className="md">
      {!!meta.length && <Props rows={meta.map(([key, value]) => [key, <span className={/^[0-9a-f]{40}$/.test(value) ? "mono" : ""}>{value}</span>])} />}
      {document}
    </div>
  </FileLinkContext.Provider>;
}
