import { defineConfig } from "vite";
import vinext from "vinext";
import { cloudflare } from "@cloudflare/vite-plugin";

// The nerudo web app: vinext (Cloudflare's Next.js API on Vite) on Workers. Its own
// Worker is the front door; the sequencer stays behind it on the NERUDO service binding.
export default defineConfig({
  plugins: [
    vinext(),
    cloudflare({
      viteEnvironment: { name: "rsc", childEnvironments: ["ssr"] },
    }),
  ],
});
