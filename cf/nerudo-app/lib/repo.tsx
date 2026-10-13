import { getJson } from "./nerudo";
import { sessionHeaders, viewer } from "./session";
import { changeOfNumber, keyHeaders, type ChangeDetail, type Snapshot, type TreeResult, type RepoDigestResult } from "../src/api";
import { Window } from "../src/components/Window";
import type { RepoView } from "../src/hub";
import { partsOf, prNumberOf, type Route } from "../src/route";
import { Shell } from "../src/Shell";
import { Availability } from "../src/components/Availability";
import { rootReadme } from "../src/readme";
import type { RepoSearchOptions } from "../src/search";

/** One repo's app, for /{org}/{repo}/... and /app/{trunk}/... alike. The first snapshot
 *  comes from the sequencer on the server, so the page arrives filled in; the shell keeps
 *  it live from there. A public repo reads with no key. A link's key (?k=) comes first;
 *  without one, the signed-in user's cookie goes with every call, and never into a URL. */
export async function RepoApp(props: { trunk: string; repo: RepoView | null; rest: string[]; k?: string; u?: string; search?: RepoSearchOptions }) {
  const key = props.k || null;
  const user = key ? props.u || null : null;
  const session = key ? null : await viewer();
  if (!key && !session && !props.repo?.public) return <LinkNeeded />;
  let { view, id } = partsOf(props.rest);
  const headers = key ? keyHeaders(key, user) : sessionHeaders(session);
  const api = `/t/${encodeURIComponent(props.trunk)}/api`;
  const [snap, root, digest] = await Promise.all([
    getJson<Snapshot>(`${api}/state`, headers),
    view === "repo" ? getJson<TreeResult>(`${api}/tree?path=`, headers) : Promise.resolve(null),
    // Composition can read many files. Its client panel fills in independently so a
    // cold digest cannot hold the complete repository page open.
    view === "repo" ? getJson<RepoDigestResult>(`${api}/digest`, headers, 1_500) : Promise.resolve(null),
  ]);
  if (!snap && !key && !props.repo?.public) return <LinkNeeded />;
  // /pulls/12 is PR #12. The snapshot lists the recent ones; the API finds the others.
  const n = prNumberOf(view, id);
  if (n !== null) id = (snap && changeOfNumber(snap, n)) ?? (await getJson<ChangeDetail>(`${api}/change/${n}`, headers))?.change.change_id ?? id;
  const name = props.repo ? { org: props.repo.org, repo: props.repo.repo } : null;
  const route: Route = { trunk: props.trunk, name, view, id, key, user, search: props.search };
  const readmePath = root ? rootReadme(root) : null;
  const readme = readmePath ? await getJson<TreeResult>(`${api}/tree?path=${encodeURIComponent(readmePath)}`, headers) : null;
  return <Shell initialRoute={route} initialSnap={snap} initialRoot={root} initialReadme={readme} initialDigest={digest} repo={props.repo} viewer={session?.user ?? null} />;
}

function LinkNeeded() {
  return (
    <Window title="Private repository">
      <Availability title="A link is needed." message="This repository is private. Open its share or personal link, or sign in as someone who has access." />
    </Window>
  );
}
