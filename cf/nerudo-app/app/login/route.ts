import { getJson } from "../../lib/nerudo";
import { COOKIE, LEGACY_COOKIE, parseSession } from "../../lib/session";

/** /login?u=NAME&k=KEY (a sign-in link) signs in and opens the user's profile.
 *  /login?out=1 signs out. A wrong link signs nobody in. A link followed from another
 *  site does nothing either: that is how someone else's link would sign a visitor in as
 *  them, or sign them out. A pasted or typed link, and this site's own pages, work. */
export async function GET(request: Request): Promise<Response> {
  const url = new URL(request.url);
  const secure = url.protocol === "https:" ? "; Secure" : "";
  const go = (path: string, cookie: string) => {
    const headers = new Headers({ Location: new URL(path, url).toString() });
    headers.append("Set-Cookie", cookie);
    headers.append("Set-Cookie", `${LEGACY_COOKIE}=; Path=/; Max-Age=0; HttpOnly; SameSite=Lax${secure}`);
    return new Response(null, { status: 303, headers });
  };
  const from = request.headers.get("sec-fetch-site");
  if (from !== null && from !== "none" && from !== "same-origin") {
    return new Response("Open the sign-in link itself: paste it into the address bar.", { status: 403, headers: { "content-type": "text/plain; charset=utf-8" } });
  }
  if (url.searchParams.has("out")) return go("/", `${COOKIE}=; Path=/; Max-Age=0; HttpOnly; SameSite=Lax${secure}`);
  const s = parseSession(`${url.searchParams.get("u") ?? ""}:${url.searchParams.get("k") ?? ""}`);
  const me = s && (await getJson<{ user: string }>("/v1/hub/me", { "X-Nerudo-User": s.user, "X-Nerudo-Key": s.key }));
  if (!s || !me) return go("/?signin=wrong", `${COOKIE}=; Path=/; Max-Age=0; HttpOnly; SameSite=Lax${secure}`);
  const month = 30 * 24 * 3600;
  return go(`/${encodeURIComponent(me.user)}`, `${COOKIE}=${encodeURIComponent(`${s.user}:${s.key}`)}; Path=/; Max-Age=${month}; HttpOnly; SameSite=Lax${secure}`);
}
