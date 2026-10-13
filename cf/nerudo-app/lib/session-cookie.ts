export const COOKIE = "nerudo_signin";
export const LEGACY_COOKIE = "landseq_signin";
const NAME = /^[a-z0-9][a-z0-9-]{0,38}$/;
const KEY = /^[0-9a-f]{32}$/;

export interface Session {
  user: string;
  key: string;
}

/** The session in a cookie value "user:key", if it is well formed. */
export function parseSession(value: string | undefined | null): Session | null {
  const parts = (value ?? "").split(":");
  const [user, key] = parts;
  return parts.length === 2 && user && key && NAME.test(user) && KEY.test(key) ? { user, key } : null;
}

/** The session in a Cookie header. */
export function sessionOfHeader(header: string | null): Session | null {
  const values = new Map<string, string>();
  for (const part of (header ?? "").split(";")) {
    const [k, ...v] = part.trim().split("=");
    if ((k === COOKIE || k === LEGACY_COOKIE) && !values.has(k)) values.set(k, v.join("="));
  }
  const value = values.get(COOKIE) ?? values.get(LEGACY_COOKIE);
  try { return parseSession(value === undefined ? null : decodeURIComponent(value)); }
  catch { return null; }
}

export function sessionHeaders(s: Session | null): Record<string, string> {
  return s ? { "X-Nerudo-User": s.user, "X-Nerudo-Key": s.key } : {};
}
