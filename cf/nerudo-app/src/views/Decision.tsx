import type { Decision } from "../api";
import { Box, Props } from "../components/classic";
import { Clock } from "../components/time";

const OUTCOME: Record<Decision["outcome"], string> = { merge: "No extra step", review: "Needs a review", sign: "Needs a signature" };

/** What the decision model answered about this PR, and what nerudo did with it. */
export function DecisionBox({ decisions }: { decisions?: Decision[] }) {
  const d = decisions?.find((x) => x.point === "risk");
  if (!d) return null;
  const why =
    d.basis === "error"
      ? `the model could not be asked (${d.error ?? "no answer"}), so NERUDO ${d.outcome === "merge" ? "went on" : "asked a person"}`
      : d.basis === "unsure"
        ? `no option reached ${d.threshold.toFixed(2)}; the top was ${d.choice} at ${d.p.toFixed(2)}`
        : `${d.choice} at ${d.p.toFixed(2)}, threshold ${d.threshold.toFixed(2)}`;
  return (
    <Box title="Decision model">
      <Props
        rows={[
          [
            "Risk",
            <span key="r">
              <b>{OUTCOME[d.outcome]}</b> <span className="mute">· {why}</span>
            </span>,
          ],
          [
            "Probabilities",
            d.basis === "error" ? (
              <span key="p" className="mute">
                none
              </span>
            ) : (
              <span key="p" style={{ display: "grid", gridTemplateColumns: "56px 130px 40px", gap: "2px 6px", alignItems: "center" }}>
                {["merge", "review", "sign"].map((o) => (
                  <Bar key={o} label={o} p={d.probabilities[o] ?? 0} on={o === d.choice} />
                ))}
              </span>
            ),
          ],
          ["Confidence", d.basis === "error" ? "—" : d.confidence.toFixed(2)],
          [
            "Model",
            <span key="m">
              <span className="mono">
                {d.provider}/{d.model}
              </span>{" "}
              <span className="mute">· {d.calibrated ? "calibrated probabilities" : "self-reported, not calibrated"}</span>
            </span>,
          ],
          ["Asked", <Clock key="t" ms={d.at_ms} />],
        ]}
      />
      <p className="mute small" style={{ marginTop: 6 }}>
        The model can only add a review or a signature. The rules and thresholds in nerudo.toml decide the rest.
      </p>
    </Box>
  );
}

function Bar({ label, p, on }: { label: string; p: number; on: boolean }) {
  return (
    <>
      <span className={on ? "" : "mute"}>{label}</span>
      <span style={{ height: 10, background: "var(--white)", border: "1px solid var(--shade)" }}>
        <span style={{ display: "block", height: "100%", width: `${Math.round(p * 100)}%`, background: on ? "var(--title)" : "var(--shade)" }} />
      </span>
      <span className="mono">{p.toFixed(2)}</span>
    </>
  );
}
