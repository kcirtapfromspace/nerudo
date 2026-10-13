import { cookies } from "next/headers";
import { COOKIE, LEGACY_COOKIE, parseSession, type Session } from "./session-cookie";
export * from "./session-cookie";

/** Who is signed in on this request (server components only). */
export async function viewer(): Promise<Session | null> {
  const jar = await cookies();
  return parseSession(jar.get(COOKIE)?.value ?? jar.get(LEGACY_COOKIE)?.value);
}
