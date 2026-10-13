import type { ReactNode } from "react";
import { Badge } from "../components/classic";
import { Since } from "../components/time";
import { bare, type RepoView } from "../hub";
import { repoUrl } from "../route";

/** A link to a repo named in the Hub's lists: "agents" in the org, or "cloudflare/agents". */
export const repoHref = (org: string, name: string) => (name.includes("/") ? `/${name.split("/").map(encodeURIComponent).join("/")}` : repoUrl({ org, repo: name }));

/** Repos as rows: the name, what it is, where a replay comes from, and how busy it is. */
export function RepoRows({ repos, empty }: { repos: RepoView[]; empty: string }) {
  if (!repos.length) return <div className="hub-list empty">{empty}</div>;
  return (
    <div className="hub-list">
      {repos.map((r) => {
        const s = r.summary;
        return (
          <a key={`${r.org}/${r.repo}`} className="hub-row" href={repoUrl(r)}>
            <Badge name={r.repo} size={24} />
            <span className="hub-main">
              <span className="hub-title">
                {r.org} / <b>{r.repo}</b>
                {!r.public && <span className="chip">Private</span>}
              </span>
              {r.description && <span className="hub-note">{r.description}</span>}
              {r.upstream && <span className="hub-note mute">Replayed from {bare(r.upstream)}</span>}
            </span>
            <span className="hub-side">
              {s ? (
                <>
                  <span>
                    <b>{s.stats.merged}</b> merged · <b>{s.stats.open}</b> open
                  </span>
                  <span className="mute">
                    <Since ms={s.updated_ms} />
                  </span>
                </>
              ) : (
                <span className="mute">—</span>
              )}
            </span>
          </a>
        );
      })}
    </div>
  );
}

/** A plain list in a white box: a badge, a title, a note, and something on the right. */
export function HubList({ rows, empty }: { rows: { key: string; name: string; title: ReactNode; note?: ReactNode; side?: ReactNode; href?: string }[]; empty: string }) {
  if (!rows.length) return <div className="hub-list empty">{empty}</div>;
  return (
    <div className="hub-list">
      {rows.map((r) => {
        const body = (
          <>
            <Badge name={r.name} size={20} />
            <span className="hub-main">
              <span className="hub-title">{r.title}</span>
              {r.note && <span className="hub-note">{r.note}</span>}
            </span>
            <span className="hub-side">{r.side}</span>
          </>
        );
        return r.href ? (
          <a key={r.key} className="hub-row" href={r.href}>
            {body}
          </a>
        ) : (
          <div key={r.key} className="hub-row">
            {body}
          </div>
        );
      })}
    </div>
  );
}
