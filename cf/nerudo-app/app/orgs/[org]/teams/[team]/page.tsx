import { notFound } from "next/navigation";
import { hubOrg } from "../../../../../lib/nerudo";
import { Crumbs, Window } from "../../../../../src/components/Window";
import { isOrg, orgUrl } from "../../../../../src/route";
import { HubList, RepoRows } from "../../../../../src/views/Hub";

export const dynamic = "force-dynamic";

/** A team: a role in the approvers of the org's repos. Its members, and where it approves. */
export default async function TeamPage(props: { params: Promise<{ org: string; team: string }> }) {
  const p = await props.params;
  const org = decodeURIComponent(p.org);
  const team = decodeURIComponent(p.team);
  const o = isOrg(org) ? await hubOrg(org) : null;
  const t = o?.teams.find((x) => x.team === team);
  if (!o || !t) notFound();
  const named = (n: string) => (n.includes("/") ? n : `${o.org}/${n}`);
  const repos = o.repos.filter((r) => t.repos.map(named).includes(`${r.org}/${r.repo}`));
  return (
    <Window title={`${o.org} / ${t.team}`} wide>
      <Crumbs items={[["Explore", "/"], [o.org, orgUrl(o.org)], ["Teams", `${orgUrl(o.org)}?tab=teams`], [t.team, null]]} />
      <h1>{t.team}</h1>
      <p className="lede">A team is a role in the approvers of a repo&apos;s nerudo.toml. Its members approve the pull requests that need the role.</p>
      <fieldset className="box">
        <legend>Members ({t.members.length})</legend>
        <HubList empty="No members." rows={t.members.map((m) => ({ key: m, name: m, title: <b>{m}</b> }))} />
      </fieldset>
      <fieldset className="box">
        <legend>Approves in ({t.repos.length})</legend>
        <RepoRows repos={repos} empty="No public repos." />
      </fieldset>
    </Window>
  );
}
