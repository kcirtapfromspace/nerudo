"use client";

import { useEffect, useRef, useState } from "react";
import { repoUrl, type RepoName } from "../route";
import { copy } from "../util";
import { useMounted } from "./time";

/** The clone URL, and how to open a pull request with plain git. */
export function CloneBox({ name }: { name: RepoName }) {
  const mounted = useMounted();
  const url = `${mounted ? window.location.origin : ""}${repoUrl(name)}.git`;
  const [copied, setCopied] = useState(false);
  return (
    <fieldset className="box clone">
      <legend>Clone</legend>
      <div className="cmd">
        <code>{url}</code>
        <button
          className="btn"
          onClick={() =>
            copy(url).then(() => {
              setCopied(true);
              setTimeout(() => setCopied(false), 1500);
            })
          }
        >
          {copied ? "Copied" : "Copy"}
        </button>
      </div>
      <p className="clone-hint">Push a branch to open a pull request:</p>
      <pre className="clone-steps">
        {`git clone ${url}\ncd ${name.repo}\ngit switch -c my-change\ngit commit -am "My change"\ngit push origin my-change`}
      </pre>
      <p className="mute small">git asks for a name and password when you push: your user name, and the k of your sign-in link (or of a personal link).</p>
    </fieldset>
  );
}

/** The Clone button in the repo bar; the box opens under it. */
export function CloneButton({ name }: { name: RepoName }) {
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!open) return;
    const away = (e: MouseEvent) => !ref.current?.contains(e.target as Node) && setOpen(false);
    const esc = (e: KeyboardEvent) => e.key === "Escape" && setOpen(false);
    document.addEventListener("mousedown", away);
    document.addEventListener("keydown", esc);
    return () => {
      document.removeEventListener("mousedown", away);
      document.removeEventListener("keydown", esc);
    };
  }, [open]);
  return (
    <div className="clone-wrap" ref={ref}>
      <button className={`btn${open ? " on" : ""}`} aria-expanded={open} onClick={() => setOpen((o) => !o)}>
        Clone ▾
      </button>
      {open && (
        <div className="clone-pop">
          <CloneBox name={name} />
        </div>
      )}
    </div>
  );
}
