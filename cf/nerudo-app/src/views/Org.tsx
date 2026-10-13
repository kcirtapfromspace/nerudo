"use client";

import { Fragment, type ReactNode } from "react";
import { Tabs } from "../components/classic";
import type { OrgView } from "../hub";
import { orgUrl, teamUrl } from "../route";
import { HubList, RepoRows, repoHref } from "./Hub";

const plural = (n: number, one: string) => `${n} ${one}${n === 1 ? "" : "s"}`;

/** Repo names as links, comma separated. */
function Repos({ org, names }: { org: string; names: string[] }) {
  return (
    <>
      {names.map((n, i) => (
        <Fragment key={n}>
          {i > 0 && ", "}
          <a href={repoHref(org, n)}>{n}</a>
        </Fragment>
      ))}
    </>
  );
}

/** An org's window body: its repos, the teams that approve in them, its people and agents. */
export function OrgTabs({ org, tab }: { org: OrgView; tab: string | null }) {
  const o = org.org;
  const tabs: [string, ReactNode][] = [
    ["repos", `Repositories (${org.repos.length})`],
    ["teams", `Teams (${org.teams.length})`],
    ["people", `People (${org.people.length})`],
    ["agents", `Agents (${org.agents.length})`],
  ];
  const onTab = (t: string) => window.history.replaceState(null, "", t === "repos" ? orgUrl(o) : `${orgUrl(o)}?tab=${t}`);
  return (
    <Tabs tabs={tabs} initial={tab} onTab={onTab} label="Organization sections">
      {(t) => {
        if (t === "teams")
          return (
            <HubList
              empty="No teams yet. A team is a role in a repo's nerudo.toml approvers."
              rows={org.teams.map((tm) => ({
                key: tm.team,
                name: tm.team,
                href: teamUrl(o, tm.team),
                title: <b>{tm.team}</b>,
                note: tm.members.join(", ") || "No members",
                side: `${plural(tm.members.length, "member")} · approves in ${plural(tm.repos.length, "repo")}`,
              }))}
            />
          );
        if (t === "people")
          return (
            <HubList
              empty="No people yet. People are the approvers in a repo's nerudo.toml."
              rows={org.people.map((p) => ({
                key: p.name,
                name: p.name,
                href: p.user ? `/${encodeURIComponent(p.name)}` : undefined,
                title: <b>{p.name}</b>,
                note: (
                  <>
                    {p.roles.map((r) => (
                      <span key={r} className="chip">
                        {r}
                      </span>
                    ))}
                    {p.joined === false && <span className="mute"> not on NERUDO yet: no key, so no approvals</span>}
                  </>
                ),
                side: <Repos org={o} names={p.repos} />,
              }))}
            />
          );
        if (t === "agents")
          return (
            <HubList
              empty="No agents yet. An agent shows up when it gets its fork."
              rows={org.agents.map((a) => ({ key: a.name, name: a.name, title: <b>{a.name}</b>, side: <Repos org={o} names={a.repos} /> }))}
            />
          );
        return <RepoRows repos={org.repos} empty="No repositories in this organization are visible to you." />;
      }}
    </Tabs>
  );
}
