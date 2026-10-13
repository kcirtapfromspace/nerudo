"use client";

import { useState, type ReactNode } from "react";
import { BRAND } from "../brand";
import { Badge, Tabs } from "../components/classic";
import { ExperienceSwitch, useExperience } from "../components/Experience";
import { Mark } from "../components/Mark";
import { FormationStudy, FormingStrand } from "../components/Strand";

/** The living kit: its specimens use the same tokens and controls as the application. */
export function BrandKit() {
  const [feedback, setFeedback] = useState("");
  const { experience } = useExperience();
  async function copyColor(name: string, value: string) {
    try {
      await navigator.clipboard.writeText(value);
      setFeedback(`${name} copied: ${value}`);
    } catch {
      setFeedback(`Copy ${name} manually: ${value}`);
    }
  }
  return (
    <article className="brand-kit">
      <header className="kit-intro">
        <div>
          <p className="eyebrow">NERUDO · Brand system 01</p>
          <h1>Care in the craft.<br /><span>Clarity in the result.</span></h1>
          <p>A shared identity for two ways of working. Familiar desktop precision, or an organic modern workspace.</p>
        </div>
        <div className="kit-intro-note"><span className="kit-note-line" />Retro by default.<br />Modern by choice.</div>
        <div className="kit-intro-strand" aria-hidden="true"><FormingStrand /><span>Warmth. Tension. Good form.</span></div>
      </header>
      <nav className="kit-nav" aria-label="Brand kit sections">
        {["Identity", "Color", "Typography", "Interface", "Interactions", "Assets"].map((name) => <a key={name} href={`#kit-${name.toLowerCase()}`}>{name}</a>)}
      </nav>

      <KitSection number="01" title="Identity" note="Good work takes form.">
        <p className="kit-statement">Independent craft.<br />Considered decisions.<br /><span>Work brought together.</span></p>
        <p className="kit-body-copy">NERUDO takes its root from <a href="https://www.nihongomaster.com/japanese/dictionary/word/53853/neru-%E7%B7%B4%E3%82%8B-%E7%85%89%E3%82%8B-%E3%81%AD%E3%82%8B" target="_blank" rel="noreferrer">neru — 練る</a>: to knead, refine, or develop a plan. The discipline of a fine bowl of ramen informs our approach: prepare carefully, balance deliberately, and make the result satisfying to use.</p>
        <FormationStudy />
        <div className="kit-principles">{BRAND.principles.map((p) => <div key={p.title}><h3>{p.title}</h3><p>{p.text}</p></div>)}</div>
        <div className="logo-specimens">
          <div className="logo-plate light"><Mark size={64} /><span className="wordmark">NERUDO</span><span className="plate-caption">Primary · Kombu on Paper</span></div>
          <div className="logo-plate dark"><Mark size={64} /><span className="wordmark">NERUDO</span><span className="plate-caption">Reversed · Paper on Kombu</span></div>
        </div>
        <p className="kit-fine">The Strand mark gathers fine handmade noodles into an N. Pale dough, flour texture, slight variation, and uneven cut ends shape the display treatment. Leave one bundle-width around it. Minimum size: 16 px for the mark, 96 px for the full lockup. Use five strands in the flat mark and the three-strand version at 24 px and below. Keep the gaps open and preserve its proportions.</p>
      </KitSection>

      <KitSection number="02" title="Color" note="Warm foundations. Clear signals.">
        <div className="color-specimens">
          {BRAND.colors.map((color) => (
            <button key={color.name} className="color-specimen" onClick={() => copyColor(color.name, color.hex)} aria-label={`Copy ${color.name} ${color.hex}`}>
              <span className="color-swatch" style={{ background: color.hex, color: color.ink }}><span>{color.hex}</span><span aria-hidden="true">↗</span></span>
              <span className="color-name">{color.name}</span><span className="color-role">{color.role}</span>
            </button>
          ))}
        </div>
        <p className="kit-copy-feedback" role="status" aria-live="polite">{feedback || "Select a swatch to copy its color."}</p>
        <div className="kit-signals"><span className="chip c-landed">✓ Ready</span><span className="chip c-approval">! Needs review</span><span className="chip c-bounced">× Needs attention</span><span className="chip c-queued">→ Queued</span></div>
        <p className="kit-fine">Kombu carries primary actions. Toast is reserved for focus and small accents. Status colors always appear with a readable label or glyph; color alone never carries meaning.</p>
      </KitSection>

      <KitSection number="03" title="Typography" note="Human voice. Precise detail.">
        <div className="type-specimen"><span className="type-label">{experience === "modern" ? "Display / Fraunces / 450 / soft optical forms" : "Display / IBM Plex Sans / 500"}</span><p className="type-display">Good work takes form.</p></div>
        <div className="type-specimen"><span className="type-label">Body / IBM Plex Sans / 400</span><p className="type-body">Give every approach room to develop. Keep the reasoning close to the work, and make the next step clear.</p></div>
        <div className="type-specimen"><span className="type-label">Code / IBM Plex Mono / 400</span><p className="type-code">git push origin my-change</p></div>
        <p className="kit-fine">Retro keeps Tahoma/Verdana at 12 px and Plex Sans display type. Modern pairs soft Fraunces headlines with Plex Sans at 14 px. Code uses Plex Mono in both. Expressive type belongs to the brand; controls and dense information remain easy to scan. Fonts are hosted locally under the included SIL Open Font Licenses.</p>
      </KitSection>

      <KitSection number="04" title="Interface" note="One product. Two presentations.">
        <div className="kit-interface-heading"><div><h3>{experience === "retro" ? "The desktop, considered." : "Room for the work to breathe."}</h3><p>{experience === "retro" ? "Raised controls, compact rows, warm surfaces. The preferred first-visit experience." : "An open workbench, flowing contours, and quiet copper details. Precise controls, with the same workflows and shortcuts."}</p></div><ExperienceSwitch /></div>
        <ReviewPreview />
        <p className="kit-fine">The style switch applies across the whole site and persists between visits. Layout, spacing, and control treatments change; repository access and decisions use the same application.</p>
      </KitSection>

      <KitSection number="05" title="Interactions" note="Every detail earns its place.">
        <dl className="kit-rules">{BRAND.rules.map(([title, text]) => <div key={title}><dt>{title}</dt><dd>{text}</dd></div>)}</dl>
        <div className="voice-specimen"><span className="type-label">Voice / calm, exact, welcoming</span><p>“This approach needs one maintainer signature.”</p><p>“Comment posted.”</p><p>“We couldn’t load this review. Try again.”</p></div>
        <p className="kit-fine">Use the familiar product words: repository, pull request, task, review, agent, release. The food inspiration shapes the care we take with the experience.</p>
      </KitSection>

      <KitSection number="06" title="Assets" note="Ready to use.">
        <div className="asset-list">
          <Asset href="/brand/nerudo-brand-kit.zip" title="Complete brand kit" detail="ZIP · logos, share artwork, tokens, fonts, and licenses" />
          <Asset href="/brand/nerudo-mark.svg" title="Strand mark" detail="Transparent SVG · one color" />
          <Asset href="/brand/nerudo-strand-material.svg" title="Noodle bundle" detail="Transparent SVG · flour-dusted display treatment" />
          <Asset href="/brand/nerudo-social.png" title="Share image" detail="PNG · 1200 × 630 · link previews" />
          <Asset href="/brand/nerudo-wordmark.svg" title="Primary lockup" detail="Outlined SVG · no font dependency" />
          <Asset href="/brand/nerudo-wordmark-reversed.svg" title="Reversed lockup" detail="Outlined SVG · light strokes" />
          <Asset href="/brand/nerudo-icon.svg" title="Application icon" detail="SVG · 64 × 64" />
          <Asset href="/brand/nerudo-tokens.json" title="Design tokens & guidelines" detail="JSON · colors, type, motion, experience rules" />
        </div>
        <p className="kit-fine">NERUDO carries through the product: the website, CLI, configuration, and integrations share one name. Existing credentials and repository history continue to work as they migrate.</p>
      </KitSection>
    </article>
  );
}

function KitSection({ number, title, note, children }: { number: string; title: string; note: string; children: ReactNode }) {
  return <section className="kit-section" id={`kit-${title.toLowerCase()}`}><header className="kit-section-heading"><span className="section-number">{number}</span><h2>{title}</h2><p>{note}</p></header><div className="kit-section-body">{children}</div></section>;
}

function Asset({ href, title, detail }: { href: string; title: string; detail: string }) {
  return <a href={href} download><span><b>{title}</b><span>{detail}</span></span><span aria-hidden="true">↓</span></a>;
}

const EXAMPLE_APPROACHES = [
  { id: "42", title: "Format receipt prices", agent: "ember", state: "review", label: "Needs review", summary: "Use the project’s existing formatter so every receipt follows the same currency rules.", rationale: "Reusing the formatter keeps receipt totals consistent with the checkout. This approach changes two callers and leaves the public API intact.", code: 'const total = formatMoney(receipt.total, receipt.currency);', files: "receipts.ts · receipts.test.ts", checks: "Both example checks passed" },
  { id: "41", title: "Refresh cached summaries", agent: "sage", state: "ready", label: "Ready", summary: "Invalidate the cached summary when a contribution lands.", rationale: "Invalidate at the point of change, so readers share the same current summary without polling more often.", code: 'await summaryCache.invalidate(repository.id);', files: "summary.ts", checks: "The example check passed" },
  { id: "39", title: "Clarify access roles", agent: "cedar", state: "ready", label: "Ready", summary: "Explain who can read, review, and choose an approach.", rationale: "Use the same role names in help text and review prompts to make each permission easy to understand.", code: 'const reviewHint = "One maintainer signature is required.";', files: "access.ts · help.ts", checks: "Both example checks passed" },
] as const;

/** Local-only examples: no backend calls or real review actions. */
function ReviewPreview() {
  const [find, setFind] = useState("");
  const [scope, setScope] = useState("all");
  const [selected, setSelected] = useState("42");
  const [reviewed, setReviewed] = useState<Record<string, boolean>>({});
  const [chosen, setChosen] = useState<Record<string, boolean>>({});
  const items = EXAMPLE_APPROACHES.filter((item) => (scope === "all" || item.state === scope) && `${item.title} ${item.agent} ${item.id}`.toLowerCase().includes(find.toLowerCase()));
  const active = items.find((item) => item.id === selected) ?? items[0];
  return (
    <div className="review-preview">
      <header className="preview-chrome"><Mark /><span className="wordmark">NERUDO</span><span className="preview-caption">Example workspace · local preview</span></header>
      <div className="preview-repobar"><span><span className="mute">studio / </span><b>receipts</b></span><span className="chip">Example</span></div>
      <div className="preview-layout">
        <aside className="preview-list" aria-label="Example pull requests">
          <div className="preview-tools"><input type="search" aria-label="Filter example pull requests" placeholder="Find an approach…" value={find} onChange={(e) => setFind(e.target.value)} /><select aria-label="Example status" value={scope} onChange={(e) => setScope(e.target.value)}><option value="all">All approaches</option><option value="review">Needs review</option><option value="ready">Ready</option></select></div>
          <div className="preview-rows">{items.map((item) => <button className={`preview-row${active?.id === item.id ? " selected" : ""}`} key={item.id} aria-pressed={active?.id === item.id} onClick={() => setSelected(item.id)}><Badge name={item.agent} size={20} /><span><b>{item.title}</b><span className="preview-row-note">#{item.id} · {item.agent} · {chosen[item.id] ? "Selected in preview" : item.label}</span></span><span className={`preview-state t-${item.state === "ready" ? "landed" : "approval"}`} aria-hidden="true">{chosen[item.id] ? "✓" : item.state === "ready" ? "✓" : "!"}</span></button>)}</div>
          {!items.length && <div className="preview-empty"><b>No matching approaches.</b><button className="link" onClick={() => { setFind(""); setScope("all"); }}>Clear filters</button></div>}
          <div className="preview-list-foot">{items.length} approach{items.length === 1 ? "" : "es"}</div>
        </aside>
        <div className="preview-detail">
          {active ? <>
            <div className="preview-detail-heading"><p className="type-label">Pull request #{active.id}</p><h3>{active.title}</h3><span className={`chip ${chosen[active.id] ? "c-landed" : active.state === "ready" ? "c-landed" : "c-approval"}`}>{chosen[active.id] ? "✓ Selected in preview" : active.label}</span></div>
            <Tabs key={active.id} tabs={[["overview", "Overview"], ["changes", "Changes"], ["reasoning", "Reasoning"]]}>
              {(tab) => tab === "changes" ? <><p className="mute small">{active.files}</p><pre className="preview-code"><code>{active.code}</code></pre><p className="kit-fine">Illustrative code for this brand specimen.</p></> : tab === "reasoning" ? <><p className="preview-summary">{active.rationale}</p><div className="preview-reason-note"><b>The decision stays with the work.</b><p>Plans and alternatives should remain easy to inspect before an approach is chosen.</p></div></> : <><p className="preview-summary">{active.summary}</p><ul className="checks"><li className="check ck-pass"><span className="ck-glyph">✓</span><b>Checks</b><span className="ck-detail">{active.checks}</span></li><li className="check"><span className="ck-glyph">≡</span><b>Context</b><span className="ck-detail">Reasoning recorded</span></li></ul><label className="preview-review"><input type="checkbox" checked={!!reviewed[active.id]} disabled={!!chosen[active.id]} onChange={(e) => setReviewed((r) => ({ ...r, [active.id]: e.target.checked }))} />I’ve examined this example approach</label><div className="preview-actions"><span className="mute small" role="status">{chosen[active.id] ? "Choice recorded locally." : "Examine the approach to continue."}</span><button className="btn primary" disabled={!reviewed[active.id] || !!chosen[active.id]} onClick={() => setChosen((c) => ({ ...c, [active.id]: true }))}>{chosen[active.id] ? "Selected" : "Select in preview"}</button></div></>}
            </Tabs>
          </> : <div className="preview-empty"><b>Nothing selected.</b><p>Clear the filters to examine an approach.</p></div>}
        </div>
      </div>
      <footer className="preview-foot">Preview only. Choices stay on this page and do not affect a repository.</footer>
    </div>
  );
}
