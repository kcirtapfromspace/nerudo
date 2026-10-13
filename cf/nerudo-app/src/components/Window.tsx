import type { ReactNode } from "react";
import { Mark } from "./Mark";
import { Me } from "./Me";
import { ExperienceSwitch } from "./Experience";

export { Mark };

/** A page outside a repo (Explore, an org, a team): one window on the desktop. */
export function Window({ title, children, wide }: { title?: ReactNode; children: ReactNode; wide?: boolean }) {
  return (
    <main className="front" id="main-content" tabIndex={-1}>
      <div className={`front-window${wide ? " wide" : ""}`}>
        <div className="modal-title">
          <span className="win-title">
            <a className="home" href="/">
              <Mark />
              <span className="wordmark">NERUDO</span>
            </a>
            {title && <span className="win-sub">{title}</span>}
          </span>
          <div className="window-tools"><ExperienceSwitch /><Me /></div>
        </div>
        <div className="front-body">
          {children}
          <footer className="front-footer"><a href="/">Explore repositories</a><span>Good work takes form.</span><a href="/settings/brand">Brand & interface</a></footer>
        </div>
      </div>
    </main>
  );
}

/** "Explore / cloudflare / Teams": where this page sits. */
export function Crumbs({ items }: { items: [ReactNode, string | null][] }) {
  return (
    <nav className="crumbs" aria-label="Breadcrumb">
      {items.map(([label, href], i) => (
        <span key={i}>
          {i > 0 && <span className="crumb-sep">/</span>}
          {href ? <a href={href}>{label}</a> : <b>{label}</b>}
        </span>
      ))}
    </nav>
  );
}
