import { viewer } from "../../lib/session";

/** Who is signed in, on a page outside a repo: a link to their profile, and sign out. */
export async function Me() {
  const s = await viewer();
  if (!s) return null;
  return (
    <span className="win-me">
      <a href={`/${encodeURIComponent(s.user)}`}>{s.user}</a>
      <span className="sep">·</span>
      <a href="/login?out=1">Sign out</a>
    </span>
  );
}
