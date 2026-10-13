import { createElement } from "react";
import { describe, expect, it, vi } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";

// The components that the Markdown view gives react-markdown, one entry for each render.
const given: unknown[] = [];
vi.mock("react-markdown", async (original) => {
  const real = await original<typeof import("react-markdown")>();
  return { ...real, default: (props: Parameters<typeof real.default>[0]) => { given.push(props.components); return createElement(real.default, props); } };
});

const { Markdown } = await import("../src/components/Markdown");
const { MermaidBlock, rememberDiagram } = await import("../src/components/Mermaid");

describe("a document that renders again", () => {
  it("gives react-markdown the same components each time, so diagrams and tables do not mount again", () => {
    const text = "| a |\n|---|\n| 1 |\n\n```mermaid\nflowchart LR\n  A --> B\n```\n";
    const first = renderToStaticMarkup(<Markdown text={text} fileLink={(href) => `/one/${href}`} />);
    const second = renderToStaticMarkup(<Markdown text={text} fileLink={(href) => `/two/${href}`} />);
    expect(given).toHaveLength(2);
    expect(given[1]).toBe(given[0]);
    expect(first).toContain("md-table-scroll");
    expect(second).toContain("md-diagram-pending");
  });
  it("still makes repo links with the fileLink of each render", () => {
    expect(renderToStaticMarkup(<Markdown text="[a](docs/a.md)" fileLink={(href) => `/one/${href}`} />)).toContain('href="/one/docs/a.md"');
    expect(renderToStaticMarkup(<Markdown text="[a](docs/a.md)" fileLink={(href) => `/two/${href}`} />)).toContain('href="/two/docs/a.md"');
    expect(renderToStaticMarkup(<Markdown text="![plan](docs/plan.png)" fileLink={(href) => `/two/${href}`} />)).toContain('href="/two/docs/plan.png"');
  });
  it("shows a diagram that this tab drew before at once, not its source", () => {
    const code = "flowchart LR\n  C --> D";
    expect(renderToStaticMarkup(<MermaidBlock code={code} />)).toContain("md-diagram-pending");
    rememberDiagram(code, { src: "data:image/svg+xml;charset=utf-8,%3Csvg%2F%3E", width: 120 });
    const html = renderToStaticMarkup(<MermaidBlock code={code} />);
    expect(html).toContain('<figure class="md-diagram">');
    expect(html).toContain("width:120px");
    expect(html).not.toContain("md-diagram-pending");
  });
});
