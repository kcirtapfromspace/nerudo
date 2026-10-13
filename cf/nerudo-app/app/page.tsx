import { hubOrgs } from "../lib/nerudo";
import { Window } from "../src/components/Window";
import { BrandHero } from "../src/components/BrandHero";
import { RepoExplorer } from "../src/views/RepoExplorer";
import { discoveryOf } from "../src/repo-discovery";

export const dynamic = "force-dynamic";

/** One global search across the people, organizations and code this viewer can read. */
export default async function Explore(props: { searchParams: Promise<{ signin?: string; q?: string; owner?: string; visibility?: string; sort?: string; page?: string }> }) {
  const parameters = await props.searchParams;
  const { signin } = parameters;
  const initial = discoveryOf(new URLSearchParams(Object.entries(parameters).filter((entry): entry is [string, string] => typeof entry[1] === "string")));
  const list = await hubOrgs();
  return (
    <Window title="Explore" wide>
      <BrandHero compact demoHref={list?.repos.some((repo) => repo.org === "nerudo" && repo.repo === "demo") ? "/nerudo/demo/tasks/T-5" : undefined} />
      {signin === "wrong" && <p className="notice">That sign-in link is not right, or it is old. Ask for a new one (nerudo user --name YOU).</p>}
      <RepoExplorer repos={list?.repos ?? []} orgs={list?.orgs ?? []} users={list?.users ?? []} initial={initial} directoryError={!list} />
      <details className="signin-guide foot" id="access">
        <summary>How to access a private repository</summary>
        <p>Open your sign-in link to use one identity across your repositories. A private repository also opens with its share link or your personal link.</p>
        <p className="mute small">Create a sign-in link with <code>nerudo user</code>, a share link with <code>nerudo share</code>, or a personal link with <code>nerudo invite</code>.</p>
      </details>
    </Window>
  );
}
