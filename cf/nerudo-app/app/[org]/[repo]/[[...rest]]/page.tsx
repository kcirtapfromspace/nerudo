import { notFound, redirect } from "next/navigation";
import { hubRepo } from "../../../../lib/nerudo";
import { RepoApp } from "../../../../lib/repo";
import { isOrg, isRepo, repoUrl } from "../../../../src/route";

import { searchFromRecord } from "../../../../src/search";

type Params = { org: string; repo: string; rest?: string[] };
type Search = { k?: string; u?: string; [key: string]: string | string[] | undefined };

/** A repo by its public name: /{org}/{repo}, /{org}/{repo}/{view}/{id}, /{org}/{repo}/pulls/{n}. */
export default async function RepoPage(props: { params: Promise<Params>; searchParams: Promise<Search> }) {
  const p = await props.params;
  const q = await props.searchParams;
  const org = decodeURIComponent(p.org);
  const repo = decodeURIComponent(p.repo);
  // The clone URL, opened in a browser: show the repo.
  if (isOrg(org) && repo.endsWith(".git") && isRepo(repo.slice(0, -4)) && !p.rest?.length) redirect(repoUrl({ org, repo: repo.slice(0, -4) }));
  const r = isOrg(org) && isRepo(repo) ? await hubRepo(org, repo) : null;
  if (!r) notFound();
  if (r.org !== org || r.repo !== repo) {
    const query = new URLSearchParams();
    for (const [key, value] of Object.entries(q)) {
      for (const item of Array.isArray(value) ? value : value === undefined ? [] : [value]) query.append(key, item);
    }
    const rest = (p.rest ?? []).map(encodeURIComponent).join("/");
    redirect(`${repoUrl({ org: r.org, repo: r.repo })}${rest ? `/${rest}` : ""}${query.size ? `?${query}` : ""}`);
  }
  return <RepoApp trunk={r.trunk} repo={r} rest={p.rest ?? []} k={q.k} u={q.u} search={searchFromRecord(q)} />;
}
