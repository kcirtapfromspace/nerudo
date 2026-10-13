"use client";

import { useEffect, useState } from "react";
import { ago, clock } from "../util";

// Times depend on the clock and the viewer's time zone, so the server can't render them
// the way the browser will. They appear once the page is live in the browser.
let live = false;

export function useMounted(): boolean {
  const [mounted, setMounted] = useState(live);
  useEffect(() => {
    live = true;
    setMounted(true);
  }, []);
  return mounted;
}

export function Ago({ ms }: { ms: number | null | undefined }) {
  return <>{useMounted() ? ago(ms) : ""}</>;
}

export function Clock({ ms }: { ms: number | null | undefined }) {
  return <>{useMounted() ? clock(ms) : ""}</>;
}

/** "5m ago", once the page is live. */
export function Since({ ms }: { ms: number | null | undefined }) {
  const mounted = useMounted();
  return <>{mounted && ms ? `${ago(ms)} ago` : ""}</>;
}
