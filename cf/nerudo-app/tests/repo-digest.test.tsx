import { beforeEach, describe, expect, it, vi } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { Api, useDetail, type RepoDigestResult } from "../src/api";
import { RepoDigest } from "../src/views/RepoDigest";

vi.mock("../src/api", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../src/api")>();
  return { ...actual, useDetail: vi.fn() };
});

const api = new Api("sample", null, null);
const reload = vi.fn();
const digest: RepoDigestResult = {
  commit: "16c26fd654977ed2d85da19cfcc3850d830596e3", at: null, method: "extension-based",
  total_files: 26, total_bytes: 53_248, source_files: 15, source_bytes: 12_288,
  excluded_files: 3, skipped_files: 1, truncated: false,
  categories: [{ name: "source", files: 12, bytes: 10_240 }, { name: "tests", files: 3, bytes: 2048 }, { name: "docs", files: 5, bytes: 8192 }, { name: "config", files: 2, bytes: 2048 }],
  languages: [{ name: "TypeScript", bytes: 9216, files: 12, extensions: [".ts", ".tsx"] }, { name: "Rust", bytes: 3072, files: 3, extensions: [".rs"] }],
  file_types: [{ extension: ".ts", files: 12, bytes: 9216 }, { extension: ".rs", files: 3, bytes: 3072 }, { extension: "", files: 1, bytes: 1024 }],
  directories: [],
  stack: [{ name: "React", kind: "framework", paths: ["web/package.json"] }, { name: "Cargo", kind: "tool", paths: ["Cargo.toml"] }],
  important_files: [{ path: "README.md", label: "Introduction", kind: "readme" }, { path: "web/package.json", label: "Project manifest", kind: "manifest" }],
  readme: { path: "README.md", text: "# Sample", truncated: false },
};
function render() { return renderToStaticMarkup(<RepoDigest api={api} stamp="head:1" openFile={vi.fn()} onSearch={vi.fn()} />); }

beforeEach(() => { vi.clearAllMocks(); vi.mocked(useDetail).mockReturnValue({ data: digest, error: null, loading: false, reload }); });

describe("repository digest", () => {
  it("keeps totals and measured composition compact without detailed filter breakdowns", () => {
    const html = render();
    expect(html).toContain("<b>26</b> tracked files");
    expect(html).toContain("<b>12 KiB</b> source");
    expect(html).toContain("Source bytes · extension estimate");
    expect(html).toContain('aria-label="Find TypeScript files, 75% of source bytes"');
    expect(html).toContain('aria-label="Find Rust files, 25% of source bytes"');
    expect(html).not.toContain("File categories");
    expect(html).not.toContain("File types");
    expect(html).not.toContain("digest-grid");
    expect(html).not.toContain("<select");
  });

  it("links detected tools to tracked evidence and names important files", () => {
    const html = render();
    expect(html).toContain('aria-label="Open web/package.json, evidence for React"');
    expect(html).toContain('aria-label="Open Cargo.toml, evidence for Cargo"');
    expect(html).toContain('title="Project manifest">web/package.json</button>');
    expect(html).not.toContain("README.md</button>");
    expect(html).toContain('title="Commit 16c26fd654977ed2d85da19cfcc3850d830596e3.');
  });

  it("identifies shortened composition lists while preserving complete totals and remaining language bytes", () => {
    vi.mocked(useDetail).mockReturnValue({ data: { ...digest, truncated: true, languages: [digest.languages[0]] }, error: null, loading: false, reload });
    const html = render();
    expect(html).toContain("<b>26</b> tracked files");
    expect(html).toContain("Totals cover the full tracked tree.");
    expect(html).toContain("Other</span><span class=\"digest-percentage\">25%");
  });

  it("limits visible languages, evidence, and quick links so the README can follow immediately", () => {
    const names = ["TypeScript", "Rust", "Go", "Python", "JavaScript", "Java", "Swift", "Kotlin"];
    const languages = names.map((name) => ({ name, bytes: 1024, files: 1, extensions: [] }));
    const stack = names.map((name) => ({ name, kind: "language" as const, paths: [`${name}/manifest`] }));
    const importantFiles = names.map((name) => ({ path: `${name}/manifest`, label: "Manifest", kind: "manifest" as const }));
    vi.mocked(useDetail).mockReturnValue({ data: { ...digest, source_bytes: 8192, languages, stack, important_files: importantFiles }, error: null, loading: false, reload });
    const html = render();
    expect([...html.matchAll(/aria-label="Find /g)]).toHaveLength(6);
    expect([...html.matchAll(/class="digest-evidence"/g)]).toHaveLength(6);
    const quickFiles = html.split('aria-label="Important files"')[1];
    expect([...quickFiles.matchAll(/<button /g)]).toHaveLength(4);
    expect(html).toContain("Other</span><span class=\"digest-percentage\">25%");
  });

  it("keeps a previously loaded digest visible when a refresh fails", () => {
    vi.mocked(useDetail).mockReturnValue({ data: digest, error: "Repository unavailable", loading: false, reload });
    const html = render();
    expect(html).toContain("Couldn’t refresh digest.");
    expect(html).toContain("Try again");
    expect(html).toContain("Source language composition");
  });

  it("renders distinct loading, unavailable, and empty states", () => {
    vi.mocked(useDetail).mockReturnValue({ data: null, error: null, loading: true, reload });
    expect(render()).toContain('aria-busy="true"');
    expect(render()).toContain("Reading repository…");
    vi.mocked(useDetail).mockReturnValue({ data: null, error: "Access denied", loading: false, reload });
    expect(render()).toContain("Digest unavailable");
    expect(render()).toContain("Try again");
    vi.mocked(useDetail).mockReturnValue({ data: { ...digest, total_files: 0 }, error: null, loading: false, reload });
    expect(render()).toContain("No files yet.");
    expect(render()).not.toContain("Source language composition");
  });
});
