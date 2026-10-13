import type { Metadata, Viewport } from "next";
import { cookies, headers } from "next/headers";
import { ExperienceProvider } from "../src/components/Experience";
import { EXPERIENCE_COOKIE } from "../src/experience";
import "../src/styles.css";
import "../src/nerudo.css";
import "../src/brand-kit.css";
import "../src/formation.css";
import "../src/reading.css";
import "../src/repo-search.css";
import "../src/global-search.css";
import "../src/repo-digest.css";

const brandMetadata: Metadata = {
  title: { default: "NERUDO — Good work takes form.", template: "%s · NERUDO" },
  description: "A Git forge for people and coding agents. Explore approaches, review the reasoning, and bring your best work together.",
  icons: { icon: "/brand/nerudo-icon.svg" },
  openGraph: {
    title: "NERUDO — Good work takes form.",
    description: "A Git forge for people and coding agents.",
    type: "website",
    siteName: "NERUDO",
    images: [{ url: "/brand/nerudo-social.png", width: 1200, height: 630, alt: "NERUDO — Good work takes form. A Git forge for people and coding agents." }],
  },
  twitter: { card: "summary_large_image", title: "NERUDO — Good work takes form.", description: "A Git forge for people and coding agents.", images: ["/brand/nerudo-social.png"] },
};

export const viewport: Viewport = { themeColor: "#F5F0E6", colorScheme: "light" };

/** Share artwork uses the current deployment's origin, including local previews. */
export async function generateMetadata(): Promise<Metadata> {
  const host = (await headers()).get("host") || "localhost";
  const local = /^(localhost|127\.0\.0\.1|\[::1\])(?::\d+)?$/i.test(host);
  return { ...brandMetadata, metadataBase: new URL(`${local ? "http" : "https"}://${host}`) };
}

export default async function RootLayout({ children }: Readonly<{ children: React.ReactNode }>) {
  const preference = (await cookies()).get(EXPERIENCE_COOKIE)?.value;
  const experience = preference === "modern" ? "modern" : "retro";
  return (
    <html lang="en" data-experience={experience}>
      <body>
        <ExperienceProvider initial={experience}>
          <a className="skip-link" href="#main-content">Skip to content</a>
          {children}
        </ExperienceProvider>
      </body>
    </html>
  );
}
