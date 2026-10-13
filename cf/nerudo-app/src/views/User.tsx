"use client";

import type { ReactNode } from "react";
import { Badge, Tabs } from "../components/classic";
import type { UserView } from "../hub";
import { orgUrl, repoUrl, teamUrl } from "../route";
import { HubList, RepoRows } from "./Hub";

const plural = (n: number, one: string) => `${n} ${one}${n === 1 ? "" : "s"}`;

/** A user's profile: one name across every repo, as on a git host. The repos the user owns
 *  (user/repo), the orgs and teams they are in, and every repo that lists them. */
export function UserProfile({ u, mine, tab }: { u: UserView; mine: boolean; tab: string | null }) {
  const tabs: [string, ReactNode][] = [
    ["repos", `Repositories (${u.repos.length})`],
    ["on", `Works on (${u.on.length})`],
    ["orgs", `Organizations (${u.orgs.length})`],
    ["teams", `Teams (${u.teams.length})`],
  ];
  const base = `/${encodeURIComponent(u.user)}`;
  const onTab = (t: string) => window.history.replaceState(null, "", t === "repos" ? base : `${base}?tab=${t}`);
  return (
    <>
      <div className="profile">
        <Badge name={u.user} size={56} />
        <div>
          <h1>{u.user}</h1>
          {u.title && <p className="lede">{u.title}</p>}
          <p className="mute small">
            {u.key ? (
              <>
                Approver key <code>{u.key.slice(0, 16)}…</code>: a repo that lists {u.user} with this key lists this user.
              </>
            ) : (
              <>No approver key: repos list {u.user} by name only.</>
            )}
            {mine && <> · You are signed in as {u.user}.</>}
          </p>
        </div>
      </div>
      <div className="org-tabs">
        <Tabs tabs={tabs} initial={tab} onTab={onTab} label="Profile sections">
          {(t) => {
            if (t === "on")
              return (
                <HubList
                  empty={`No repo lists ${u.user} yet. A repo lists a person as an approver in its nerudo.toml.`}
                  rows={u.on.map((r) => ({
                    key: `${r.org}/${r.repo}`,
                    name: r.repo,
                    href: repoUrl(r),
                    title: (
                      <>
                        {r.org} / <b>{r.repo}</b>
                      </>
                    ),
                    note: (
                      <>
                        {r.roles.map((x) => (
                          <span key={x} className="chip">
                            {x}
                          </span>
                        ))}
                        {r.as && <span className="mute"> as {r.as}, with {u.user}&apos;s key</span>}
                      </>
                    ),
                    side:
                      r.verified === false ? (
                        <span className="t-bounced" title={`This repo's ${r.as ?? u.user} has another key: someone else with the same name`}>
                          another key
                        </span>
                      ) : r.verified ? (
                        <span className="t-landed" title="The repo's key for this name is this user's">
                          ✓ key
                        </span>
                      ) : null,
                  }))}
                />
              );
            if (t === "orgs")
              return (
                <HubList
                  empty={`${u.user} is in no org yet.`}
                  rows={u.orgs.map((o) => ({ key: o.org, name: o.org, href: orgUrl(o.org), title: <b>{o.title || o.org}</b>, note: o.title ? o.org : null }))}
                />
              );
            if (t === "teams")
              return (
                <HubList
                  empty={`${u.user} is in no team yet. A team is a role in a repo's nerudo.toml approvers.`}
                  rows={u.teams.map((tm) => ({
                    key: `${tm.org}/${tm.team}`,
                    name: tm.team,
                    href: teamUrl(tm.org, tm.team),
                    title: (
                      <>
                        {tm.org} / <b>{tm.team}</b>
                      </>
                    ),
                    side: `approves in ${plural(tm.repos.length, "repo")}`,
                  }))}
                />
              );
            return <RepoRows repos={u.repos} empty={`No repositories owned by ${u.user} are visible to you.`} />;
          }}
        </Tabs>
      </div>
    </>
  );
}
