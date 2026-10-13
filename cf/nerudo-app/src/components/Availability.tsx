import { Mark } from "./Mark";

/** A clear route back from unavailable pages and private repositories. */
export function Availability({ title, message }: { title: string; message: string }) {
  return (
    <section className="availability">
      <div className="availability-mark" aria-hidden="true"><Mark size={80} /></div>
      <div className="availability-copy">
        <p className="eyebrow">Repository access</p>
        <h1>{title}</h1>
        <p className="lede">{message}</p>
        <div className="availability-actions">
          <a className="btn primary" href="/">Explore repositories</a>
          <a href="/#access">Access help <span aria-hidden="true">→</span></a>
        </div>
      </div>
    </section>
  );
}
