import { STRAND_MARK, STRAND_COMPACT } from "../strand";

/** A gathered noodle bundle, simplified at small sizes. */
export function Mark({ size = 18 }: { size?: number }) {
  const mark = size <= 24 ? STRAND_COMPACT : STRAND_MARK;
  return (
    <svg width={size} height={size} viewBox="0 0 64 64" aria-hidden="true" className="mark" fill="none" stroke="currentColor" strokeWidth={mark.strokeWidth} strokeLinecap="round" strokeLinejoin="round">
      {mark.paths.map((path, index) => <path key={index} d={path} />)}
    </svg>
  );
}
