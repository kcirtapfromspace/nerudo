// The Hub: repo names (org/repo), organizations and teams. The sequencer serves only
// public repos and private repos the signed-in viewer can access.

/** A repo at a glance, from its trunk. Null when the trunk did not answer. */
export interface RepoSummary {
  trunk: string;
  head: string | null;
  public: boolean;
  stats: { merged: number; open: number; closed: number; batches: number; submitted: number };
  latest_release: { tag: string; title: string; seq: number; created_ms: number } | null;
  people: Record<string, string[]>;
  agents: string[];
  /** The newest event's time. */
  updated_ms: number | null;
}

export interface RepoView {
  org: string;
  repo: string;
  trunk: string;
  description: string | null;
  /** The repo a replay rebuilds, like https://github.com/cloudflare/agents. */
  upstream: string | null;
  public: boolean;
  created_ms: number;
  summary: RepoSummary | null;
}

export interface OrgRow {
  org: string;
  title: string | null;
  description: string | null;
  repos: number;
}

export interface UserRow {
  user: string;
  title: string | null;
  /** Repos the user owns (user/repo). */
  repos: number;
}

/** A user's profile: one name across every repo. */
export interface UserView {
  kind: "user";
  user: string;
  title: string | null;
  created_ms: number;
  /** The user's approver key, as nerudo.toml lists it. */
  key: string | null;
  /** Repos the user owns. */
  repos: RepoView[];
  /** Every repo that lists the user as a person: by name, or by key under another name (`as`).
   *  `verified`: the repo's key for that name is the user's (null: the user has no key). */
  on: { org: string; repo: string; roles: string[]; as: string | null; verified: boolean | null }[];
  orgs: { org: string; title: string | null }[];
  teams: { org: string; team: string; repos: string[] }[];
}

export interface OrgView {
  kind?: "org";
  org: string;
  title: string | null;
  description: string | null;
  created_ms: number;
  repos: RepoView[];
  teams: { team: string; members: string[]; repos: string[] }[];
  /** `user`: the name is a user's, with a profile. `joined`: a repo lists their key; without
   *  one, they have not joined nerudo and can't approve yet. */
  people: { name: string; roles: string[]; repos: string[]; user?: boolean; joined?: boolean }[];
  agents: { name: string; repos: string[] }[];
}

/** "github.com/cloudflare/agents" from "https://github.com/cloudflare/agents". */
export function bare(url: string): string {
  return url.replace(/^https?:\/\//, "").replace(/\/+$/, "").replace(/\.git$/, "");
}
