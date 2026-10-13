import { describe, expect, it } from "vitest";
import { parseSession, sessionHeaders, sessionOfHeader } from "../lib/session-cookie";

const key = "0123456789abcdef0123456789abcdef";
const session = { user: "alice", key };

describe("sign-in migration", () => {
  it("reads either generation of cookie and emits the new headers", () => {
    expect(sessionOfHeader(`landseq_signin=alice%3A${key}`)).toEqual(session);
    expect(sessionOfHeader(`nerudo_signin=alice%3A${key}`)).toEqual(session);
    expect(sessionHeaders(session)).toEqual({ "X-Nerudo-User": "alice", "X-Nerudo-Key": key });
  });
  it("prefers the new cookie regardless of header order", () => {
    for (const header of [`landseq_signin=bob:${key}; nerudo_signin=alice:${key}`, `nerudo_signin=alice:${key}; landseq_signin=bob:${key}`]) {
      expect(sessionOfHeader(header)).toEqual(session);
    }
    expect(sessionOfHeader(`nerudo_signin=bad; landseq_signin=alice:${key}`)).toBeNull();
  });
  it("rejects malformed encoding and credentials without throwing", () => {
    expect(sessionOfHeader("nerudo_signin=%E0%A4%A")).toBeNull();
    expect(parseSession(`alice:${key}:extra`)).toBeNull();
    expect(parseSession(`ALICE:${key}`)).toBeNull();
    expect(sessionOfHeader(null)).toBeNull();
  });
});
