export const BRAND = {
  name: "NERUDO",
  tagline: "Good work takes form.",
  descriptor: "A Git forge for people and coding agents.",
  promise: "Explore approaches, review the reasoning, and bring your best work together.",
  principles: [
    { title: "Prepare with care", text: "Make context, intent, and constraints easy to understand before work begins." },
    { title: "Refine with judgment", text: "Show alternatives and the reasoning behind a choice. Readiness should be visible." },
    { title: "Bring work together", text: "Keep independent contributions coherent through clear order, review, and feedback." },
  ],
  colors: [
    { name: "Porcelain", hex: "#F5F0E6", role: "Canvas", ink: "#25302B" },
    { name: "Paper", hex: "#FFFDF8", role: "Reading surfaces", ink: "#25302B" },
    { name: "Rice", hex: "#E9E2D5", role: "Controls & secondary surfaces", ink: "#25302B" },
    { name: "Kombu", hex: "#284A3B", role: "Primary actions & identity", ink: "#FFFDF8" },
    { name: "Toast", hex: "#985436", role: "Focus & small accents", ink: "#FFFDF8" },
    { name: "Ink", hex: "#25302B", role: "Text & strong contrast", ink: "#FFFDF8" },
  ],
  rules: [
    ["One clear next step", "Give each decision one primary action. Keep secondary actions quiet and nearby."],
    ["Readiness has a reason", "Pair every state with words and a glyph. Explain what is needed and who can act."],
    ["Keep context in place", "Review and refine inline. Preserve selection, scroll position, and the user’s work."],
    ["Make feedback specific", "Say what changed. Keep progress, empty, error, and permission states distinct."],
    ["Keyboard is a first-class path", "Keep shortcuts in both experiences. Show focus clearly and retain predictable tab order."],
    ["Motion follows the action", "Use 120 ms for hover and 180 ms for interface feedback. The strand takes 680 ms to settle into form after a deliberate choice. Honor reduced motion; useful actions respond immediately."],
  ],
} as const;
