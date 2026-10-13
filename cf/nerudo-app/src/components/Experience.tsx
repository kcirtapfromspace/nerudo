"use client";

import { createContext, useContext, useState, type ReactNode } from "react";

import { EXPERIENCE_COOKIE, type Experience } from "../experience";
const Context = createContext<{ experience: Experience; setExperience: (value: Experience) => void } | null>(null);

/** One presentation preference for the whole site. Retro is the first-visit default. */
export function ExperienceProvider({ initial, children }: { initial: Experience; children: ReactNode }) {
  const [experience, setValue] = useState(initial);
  const setExperience = (value: Experience) => {
    const focused = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    const anchorY = window.scrollY > 0 && focused?.closest(".experience-switch") ? focused.getBoundingClientRect().top : null;
    setValue(value);
    document.documentElement.dataset.experience = value;
    // Read on the server as well, so navigation and reloads never flash the other interface.
    document.cookie = `${EXPERIENCE_COOKIE}=${value}; Path=/; Max-Age=31536000; SameSite=Lax${location.protocol === "https:" ? "; Secure" : ""}`;
    // A presentation change must not move the control the person is using off screen.
    if (anchorY !== null && focused?.isConnected) window.scrollBy({ top: focused.getBoundingClientRect().top - anchorY, behavior: "instant" });
  };
  return <Context.Provider value={{ experience, setExperience }}>{children}</Context.Provider>;
}

export function useExperience() {
  const value = useContext(Context);
  if (!value) throw new Error("ExperienceProvider is required");
  return value;
}

export function ExperienceSwitch({ className = "" }: { className?: string }) {
  const { experience, setExperience } = useExperience();
  return (
    <div className={`experience-switch ${className}`} role="group" aria-label="Interface style">
      {(["retro", "modern"] as const).map((value) => (
        <button key={value} type="button" aria-pressed={experience === value} onClick={() => setExperience(value)} title={value === "retro" ? "Retro interface · default" : "Modern interface"}>
          {value === "retro" ? "Retro" : "Modern"}
        </button>
      ))}
    </div>
  );
}
