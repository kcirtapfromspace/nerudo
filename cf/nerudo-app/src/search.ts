/** Search state lives in the URL so reloads, back navigation and shared links agree. */
export interface RepoSearchOptions {
  mode: "all" | "files" | "content";
  query: string;
  extension: string;
  language: string;
  category: string;
  include: string;
  exclude: string;
  ignored: boolean;
  caseSensitive: boolean;
  wholeWord: boolean;
  regex: boolean;
  fuzzy: boolean;
}

export const defaultSearch = (mode: RepoSearchOptions["mode"] = "all"): RepoSearchOptions => ({
  mode, query: "", extension: "", language: "", category: "", include: "", exclude: "",
  ignored: false, caseSensitive: false, wholeWord: false, regex: false, fuzzy: true,
});

const fields = { query: "q", extension: "ext", language: "language", category: "type", include: "include", exclude: "exclude" } as const;
const flags = { ignored: "ignored", caseSensitive: "case", wholeWord: "word", regex: "regex" } as const;

export function writeSearch(params: URLSearchParams, search: RepoSearchOptions) {
  params.set("mode", search.mode);
  for (const [field, name] of Object.entries(fields)) {
    const value = search[field as keyof typeof fields];
    if (value) params.set(name, value);
  }
  for (const [field, name] of Object.entries(flags)) if (search[field as keyof typeof flags]) params.set(name, "1");
  if (!search.fuzzy) params.set("fuzzy", "0");
}

export function readSearch(params: URLSearchParams): RepoSearchOptions | undefined {
  if (!params.has("mode") && !params.has("q")) return undefined;
  const search = defaultSearch(params.get("mode") === "content" ? "content" : params.get("mode") === "files" ? "files" : "all");
  for (const [field, name] of Object.entries(fields)) search[field as keyof typeof fields] = (params.get(name) || "").slice(0, 512);
  for (const [field, name] of Object.entries(flags)) search[field as keyof typeof flags] = params.get(name) === "1";
  search.fuzzy = params.get("fuzzy") !== "0";
  return search;
}

export function searchFromRecord(record: Record<string, string | string[] | undefined>) {
  const params = new URLSearchParams();
  for (const [key, value] of Object.entries(record)) if (typeof value === "string") params.set(key, value);
  return readSearch(params);
}

export function searchRequest(search: RepoSearchOptions) {
  const params = new URLSearchParams();
  writeSearch(params, effectiveSearch(search));
  params.set("limit", "100");
  return `search?${params}`;
}

/** Optional power-user qualifiers keep the everyday interface to one field. */
export function effectiveSearch(search: RepoSearchOptions): RepoSearchOptions {
  const result = { ...search };
  // Explicit regex and exact queries from scoped result links are already parsed.
  if (search.regex || !search.fuzzy) return result;
  let qualified = false;
  let regexQuery: string | undefined;
  result.query = replaceQueryQualifiers(search.query, ["ext", "type", "language", "lang", "path", "include", "exclude", "case", "word", "regex"], (_, key, quoted, plain) => {
    const value = quoted ?? plain ?? "";
    const name = key.toLowerCase();
    const field = ({ ext: "extension", language: "language", lang: "language", path: "include", include: "include", exclude: "exclude" } as const)[name as "ext" | "language" | "lang" | "path" | "include" | "exclude"];
    if (field) result[field] = result[field] ? `${result[field]},${value}` : value;
    else if (name === "type") {
      const category = ["source", "tests", "docs", "config", "assets", "other"].includes(value.toLowerCase());
      result[category ? "category" : "extension"] = value.toLowerCase();
    } else if (name === "regex") { result.regex = true; regexQuery = value; }
    else if (name === "case") result.caseSensitive = ["yes", "true", "1"].includes(value.toLowerCase());
    else if (name === "word") result.wholeWord = ["yes", "true", "1"].includes(value.toLowerCase());
    qualified = true;
    return "";
  });
  if (qualified) result.query = result.query.trim();
  if (regexQuery !== undefined) result.query = regexQuery;
  if (/^"[\s\S]*"$/.test(result.query)) { result.query = result.query.slice(1, -1); result.fuzzy = false; }
  return result;
}

/** Qualifier-like text inside a quoted phrase remains literal search text. */
export function replaceQueryQualifiers(query: string, keys: string[], replace: (full: string, key: string, quoted: string | undefined, plain: string | undefined) => string) {
  const inQuote: boolean[] = [];
  let quoted = false;
  let escaped = false;
  for (let index = 0; index < query.length; index++) {
    inQuote[index] = quoted;
    if (query[index] === '"' && !escaped) quoted = !quoted;
    escaped = query[index] === "\\" && !escaped;
  }
  const pattern = new RegExp(`(?:^|\\s)(${keys.join("|")}):(?:"((?:\\\\.|[^"])*)"|(\\S+))`, "gi");
  return query.replace(pattern, (full: string, key: string, value: string | undefined, plain: string | undefined, offset: number) => inQuote[offset] ? full : replace(full, key, value?.replace(/\\"/g, '"'), plain));
}

/** Merge overlapping highlights before rendering; offsets from the API are UTF-16. */
export function highlightParts(text: string, ranges: [number, number][]): { text: string; match: boolean }[] {
  const merged: [number, number][] = [];
  for (const [start, end] of [...ranges].sort((a, b) => a[0] - b[0])) {
    const a = Math.max(0, Math.min(text.length, start));
    const b = Math.max(a, Math.min(text.length, end));
    if (a === b) continue;
    const last = merged.at(-1);
    if (last && a <= last[1]) last[1] = Math.max(last[1], b);
    else merged.push([a, b]);
  }
  const parts: { text: string; match: boolean }[] = [];
  let at = 0;
  for (const [start, end] of merged) {
    if (start > at) parts.push({ text: text.slice(at, start), match: false });
    parts.push({ text: text.slice(start, end), match: true });
    at = end;
  }
  if (at < text.length) parts.push({ text: text.slice(at), match: false });
  return parts;
}
