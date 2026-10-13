import { env } from "cloudflare:workers";
import type { OrgRow, OrgView, RepoView, UserRow, UserView } from "../src/hub";
import { sessionHeaders, sessionOfHeader, viewer } from "./session";
import { globalSearchRequest, type GlobalSearchResult } from "../src/global-search";

/** The nerudo Worker (the sequencer and its API), on the NERUDO service binding. */
export function nerudo(): Fetcher {
  return (env as unknown as { NERUDO: Fetcher }).NERUDO;
}

/** Pass a request on to the nerudo Worker, unchanged. Its URL keeps this site's origin,
 *  so the links nerudo makes (share, invite) point here. */
export async function forward(request: Request): Promise<Response> {
  const out = new Request(request.url, request);
  // A signed-in user's call without a key of its own goes on with the user's headers, but
  // only from this site's own pages.
  const own = request.method === "GET" || request.method === "HEAD" || request.headers.get("sec-fetch-site") === "same-origin";
  const linked = out.headers.has("x-nerudo-key") || out.headers.has("x-landseq-key") || new URL(request.url).searchParams.has("k");
  const s = own && !linked ? sessionOfHeader(request.headers.get("cookie")) : null;
  for (const [k, v] of Object.entries(sessionHeaders(s))) out.headers.set(k, v);
  return nerudo().fetch(out);
}

/** GET JSON from the sequencer. Null when it is not there (404, or no answer). */
export async function getJson<T>(path: string, headers: Record<string, string> = {}, timeoutMs = 5_000): Promise<T | null> {
  const controller = new AbortController();
  let timer: ReturnType<typeof setTimeout> | undefined;
  const read = async () => {
    try {
      const r = await nerudo().fetch(`https://nerudo.internal${path}`, { headers, signal: controller.signal });
      return r.ok ? ((await r.json()) as T) : null;
    } catch {
      return null;
    }
  };
  try {
    return await Promise.race([read(), new Promise<null>((resolve) => {
      timer = setTimeout(() => { controller.abort(); resolve(null); }, timeoutMs);
    })]);
  } finally {
    clearTimeout(timer);
  }
}

const enc = encodeURIComponent;

// The Hub filters every result using the signed-in viewer's repo access.
async function hubJson<T>(path: string): Promise<T | null> {
  return getJson<T>(path, sessionHeaders(await viewer()));
}
export const hubSearch = (query: string, owner = "", visibility = "all") => hubJson<GlobalSearchResult>(globalSearchRequest(query, owner, visibility));
export const hubOrgs = () => hubJson<{ orgs: OrgRow[]; users: UserRow[]; repos: RepoView[] }>("/v1/hub/orgs");
/** An owner's page: an org, or a user's profile. */
export const hubOwner = (owner: string) => hubJson<OrgView | UserView>(`/v1/hub/orgs/${enc(owner)}`);
export const hubOrg = async (org: string) => {
  const o = await hubOwner(org);
  return o && o.kind !== "user" ? o : null;
};
export const hubRepo = (org: string, repo: string) => hubJson<RepoView>(`/v1/hub/repos/${enc(org)}/${enc(repo)}`);
export const hubTrunk = (trunk: string) => hubJson<RepoView>(`/v1/hub/trunks/${enc(trunk)}`);
