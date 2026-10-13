import { notFound, redirect } from "next/navigation";
import { hubOwner } from "../../lib/nerudo";
import { viewer } from "../../lib/session";
import { Crumbs, Window } from "../../src/components/Window";
import { isOrg } from "../../src/route";
import { OrgTabs } from "../../src/views/Org";
import { UserProfile } from "../../src/views/User";

export const dynamic = "force-dynamic";

/** An owner: an org (its public repos, the teams that approve in them, its people and
 *  agents), or a user's profile. Orgs and users share one set of names. */
export default async function OwnerPage(props: { params: Promise<{ org: string }>; searchParams: Promise<{ tab?: string }> }) {
  const owner = decodeURIComponent((await props.params).org);
  const { tab } = await props.searchParams;
  const o = isOrg(owner) ? await hubOwner(owner) : null;
  if (!o) notFound();
  if (o.kind !== "user" && o.org !== owner) redirect(`/${encodeURIComponent(o.org)}${tab ? `?tab=${encodeURIComponent(tab)}` : ""}`);
  if (o.kind === "user") {
    const me = await viewer();
    return (
      <Window title={o.user} wide>
        <Crumbs items={[["Explore", "/"], [o.user, null]]} />
        <UserProfile u={o} mine={me?.user === o.user} tab={tab ?? null} />
      </Window>
    );
  }
  return (
    <Window title={o.org} wide>
      <Crumbs items={[["Explore", "/"], [o.org, null]]} />
      <h1>{o.title || o.org}</h1>
      {o.description && <p className="lede">{o.description}</p>}
      <div className="org-tabs">
        <OrgTabs org={o} tab={tab ?? null} />
      </div>
    </Window>
  );
}
