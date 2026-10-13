import { redirect } from "next/navigation";
import { hubTrunk } from "../../../../lib/nerudo";
import { RepoApp } from "../../../../lib/repo";
import { partsOf, routeUrl } from "../../../../src/route";

import { searchFromRecord } from "../../../../src/search";

type Params = { trunk: string; rest?: string[] };
type Search = { k?: string; u?: string } & Record<string, string | string[] | undefined>;

/** A repo by its trunk id: the links from before repos had names, and private repos.
 *  A repo with a public name moves to /{org}/{repo}, with the same view and keys. */
export default async function TrunkPage(props: { params: Promise<Params> | Params; searchParams: Promise<Search> | Search }) {
  const { trunk, rest = [] } = await props.params;
  const q = await props.searchParams;
  const id = decodeURIComponent(trunk);
  const named = await hubTrunk(id);
  if (named?.public) {
    redirect(routeUrl({ trunk: id, name: { org: named.org, repo: named.repo }, ...partsOf(rest), key: q.k || null, user: q.u || null, search: searchFromRecord(q) }));
  }
  return <RepoApp trunk={id} repo={null} rest={rest} k={q.k} u={q.u} search={searchFromRecord(q)} />;
}
