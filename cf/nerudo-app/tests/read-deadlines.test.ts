import { afterEach, describe, expect, it, vi } from "vitest";
import { Api } from "../src/api";

afterEach(() => {
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

describe("repository read deadlines", () => {
  it("ends a stalled read and permits the next refresh", async () => {
    vi.useFakeTimers();
    const fetcher = vi.fn()
      .mockImplementationOnce((_url, init: RequestInit) => new Promise((_resolve, reject) => {
        init.signal!.addEventListener("abort", () => reject(init.signal!.reason), { once: true });
      }))
      .mockResolvedValueOnce(new Response(JSON.stringify({ seq: 2 }), { status: 200 }));
    vi.stubGlobal("fetch", fetcher);
    const api = new Api("example", null, null);
    const failed = api.get("state").catch((error: Error) => error.message);
    await vi.advanceTimersByTimeAsync(15_000);
    expect(await failed).toContain("too long");
    expect(await api.get("state")).toEqual({ seq: 2 });
    expect(vi.getTimerCount()).toBe(0);
  });

  it("keeps caller cancellation distinct from the read deadline", async () => {
    vi.useFakeTimers();
    const caller = new AbortController();
    vi.stubGlobal("fetch", (_url: string, init: RequestInit) => new Promise((_resolve, reject) => {
      init.signal!.addEventListener("abort", () => reject(init.signal!.reason), { once: true });
    }));
    const canceled = new Api("example", null, null).get("state", caller.signal).catch((error: Error) => error.message);
    caller.abort(new Error("search changed"));
    expect(await canceled).toBe("search changed");
    expect(vi.getTimerCount()).toBe(0);
  });
});
