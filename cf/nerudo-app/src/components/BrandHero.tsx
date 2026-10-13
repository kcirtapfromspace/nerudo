import { Mark } from "./Mark";
import { FormingStrand } from "./Strand";

/** The public promise, with the identity expressed as material taking shape. */
export function BrandHero({ compact = false, demoHref }: { compact?: boolean; demoHref?: string }) {
  return (
    <section className={`brand-hero${compact ? " compact" : ""}`} aria-labelledby="brand-headline">
      <div className="brand-hero-copy">
        <p className="eyebrow">A Git forge for people and coding agents</p>
        <h1 id="brand-headline">Good work{compact ? " " : <br />}<span className="hero-form">takes form<span className="brand-period">.</span></span></h1>
        <p className="brand-lede">Let coding agents work in parallel. Compare their reasoning, integrate changes by declared rules, and approve the work that needs your judgment.</p>
        {demoHref && <div className="hero-actions">
          <a className="btn primary" href={demoHref}>Follow the agent demo <span aria-hidden="true">→</span></a>
          <a className="hero-secondary" href="/nerudo/nerudo/repo/file%3ABENCHMARKS.md">Read the measurements <span aria-hidden="true">↗</span></a>
        </div>}
        {!compact && <div className="hero-actions">
          <a className="btn primary" href="#repositories">Explore repositories <span aria-hidden="true">↗</span></a>
          <a className="hero-secondary" href="/settings/brand">Meet NERUDO <span aria-hidden="true">→</span></a>
        </div>}
      </div>
      <div className="brand-hero-art" aria-hidden="true">
        <div className="hero-art-grid"><Mark size={180} /></div>
        <div className="hero-material"><span className="hero-material-label">A study in good form / 01</span><FormingStrand guides /><span className="hero-material-foot">Fluid in the making.<br />Precise in the result.</span></div>
        <span className="art-caption">Prepare · Refine · Bring together</span>
      </div>
      <div className="hero-process" aria-hidden="true"><span>01 / Prepare</span><span>02 / Refine</span><span>03 / Bring together</span><span className="hero-process-line" /></div>
    </section>
  );
}
