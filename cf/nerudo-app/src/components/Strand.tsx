"use client";

import { useId, useState, type CSSProperties } from "react";
import { STRAND_STAGES, STRAND_MATERIAL_WIDTH, STRAND_BOUNDS } from "../strand";

const STAGES = [
  { name: "Prepare", detail: "Fine strands. Room to explore.", description: "Loose noodle folds carry the slight irregularity of work made by hand." },
  { name: "Refine", detail: "Tension draws the strands together.", description: "Each noodle follows its own curve as the bundle takes the shape of an N." },
  { name: "Form", detail: "Handmade texture. A measured form.", description: "Eleven flour-dusted noodles share one clear N contour, with fine gaps and uneven cut ends." },
] as const;

/** Fine handmade noodles at display scale; fewer strands keep the small logo readable. */
export function FormingStrand({ stage = 2, guides = false }: { stage?: number; guides?: boolean }) {
  const id = useId().replaceAll(":", "");
  return (
    <svg className="forming-strand" viewBox="0 0 80 74" fill="none" aria-hidden="true">
      <defs>
        <linearGradient id={`${id}-dough`} x1="12" y1="11" x2="53" y2="54" gradientUnits="userSpaceOnUse">
          <stop stopColor="#EEEBDC" /><stop offset=".4" stopColor="#D9D3BA" /><stop offset=".75" stopColor="#C5BCA0" /><stop offset="1" stopColor="#E3DFCE" />
        </linearGradient>
        <filter id={`${id}-flour`} x="-5%" y="-5%" width="110%" height="110%" colorInterpolationFilters="sRGB">
          <feTurbulence type="fractalNoise" baseFrequency="2.4" numOctaves="3" seed="17" result="grain" />
          <feColorMatrix in="grain" type="matrix" values="0 0 0 0 1 0 0 0 0 1 0 0 0 0 .95 .7 .7 .7 0 -.94" result="flour" />
          <feComposite in="flour" in2="SourceGraphic" operator="in" result="dust" />
          <feBlend in="SourceGraphic" in2="dust" mode="screen" />
        </filter>
      </defs>
      <g className={`strand-geometry stage-${stage}`} transform="translate(6 5)">
        <g className={`strand-guides${guides ? " shown" : ""}`} stroke="currentColor" strokeWidth=".12">
          <rect x={STRAND_BOUNDS.x} y={STRAND_BOUNDS.y} width={STRAND_BOUNDS.width} height={STRAND_BOUNDS.height} strokeDasharray=".8 1.3" />
          <path d={`M${STRAND_BOUNDS.x} 4V0 M${STRAND_BOUNDS.x + STRAND_BOUNDS.width} 4V0 M${STRAND_BOUNDS.x} 2H${STRAND_BOUNDS.x + STRAND_BOUNDS.width} M4 ${STRAND_BOUNDS.y}H0 M4 ${STRAND_BOUNDS.y + STRAND_BOUNDS.height}H0 M2 ${STRAND_BOUNDS.y}V${STRAND_BOUNDS.y + STRAND_BOUNDS.height}`} />
        </g>
        <g filter={`url(#${id}-flour)`} strokeLinecap="butt" strokeLinejoin="round">
          {STRAND_STAGES[stage].map((path, index) => <g key={index} style={{ "--strand-path": `path('${path}')` } as CSSProperties}>
            <path className="strand-material" d={path} stroke="#A69B7D" strokeOpacity=".45" strokeWidth="1.08" transform="translate(.08 .12)" />
            <path className="strand-material" d={path} stroke={`url(#${id}-dough)`} strokeWidth={STRAND_MATERIAL_WIDTH} />
            <path className="strand-material" d={path} stroke="#F4F1E4" strokeOpacity=".6" strokeWidth=".22" transform="translate(-.13 -.08)" />
          </g>)}
        </g>
      </g>
    </svg>
  );
}

export function FormationStudy() {
  const [stage, setStage] = useState(2);
  const step = STAGES[stage];
  return (
    <figure className="formation-study">
      <div className="formation-art"><span className="formation-overline">The Strand / made by hand</span><FormingStrand stage={stage} guides={stage === 2} /><span className="formation-measure">Fine strands · measured form</span></div>
      <figcaption className="formation-caption">
        <div><span className="type-label">From possibility to precision</span><h3>Dough in motion.<br />Drawn into form.</h3><p>The warmth of ramen. The discipline of a forge. Fine noodles gathered with care, then shaped into a clear N.</p></div>
        <div className="formation-controls" role="group" aria-label="Logo formation stage">{STAGES.map((item, index) => <button key={item.name} aria-pressed={stage === index} onClick={() => setStage(index)}><span aria-hidden="true">0{index + 1}</span>{item.name}</button>)}</div>
        <div className="formation-feedback" role="status" aria-live="polite"><b>{step.detail}</b><p>{step.description}</p></div>
      </figcaption>
    </figure>
  );
}
