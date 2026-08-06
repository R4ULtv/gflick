// @ts-check

import cloudflare from "@astrojs/cloudflare";
import { defineConfig, fontProviders, sessionDrivers } from "astro/config";
import tailwindcss from "@tailwindcss/vite";

// https://astro.build/config
export default defineConfig({
  adapter: cloudflare({ imageService: "compile" }),
  session: {
    // The site does not use Astro sessions. An in-memory driver prevents the
    // Cloudflare adapter from provisioning an unused SESSION KV namespace.
    driver: sessionDrivers.lruCache({
      max: 1,
    }),
  },
  site: "https://www.gflick.app",
  vite: {
    plugins: [tailwindcss()],
  },
  trailingSlash: "never",
  build: {
    // Emits `blog/post.html` instead of `blog/post/index.html`, keeping URLs
    // slash-less to match the previously indexed Next.js routes.
    format: "file",
  },
  fonts: [
    {
      provider: fontProviders.local(),
      name: "Anton",
      cssVariable: "--font-anton",
      fallbacks: ["Impact", "Haettenschweiler", "sans-serif"],
      options: {
        variants: [
          {
            src: ["./src/assets/fonts/Anton/Anton.woff2"],
            weight: "400",
            style: "normal",
            display: "swap",
          },
        ],
      },
    },
    {
      provider: fontProviders.local(),
      name: "Geist",
      cssVariable: "--font-geist",
      fallbacks: ["sans-serif"],
      options: {
        variants: [
          {
            src: ["./src/assets/fonts/Geist/Geist.woff2"],
            weight: "400 500",
            style: "normal",
            display: "swap",
          },
        ],
      },
    },
    {
      provider: fontProviders.local(),
      name: "Geist Mono",
      cssVariable: "--font-geist-mono",
      fallbacks: ["monospace"],
      options: {
        variants: [
          {
            src: ["./src/assets/fonts/Geist/GeistMono.woff2"],
            weight: "400 500",
            style: "normal",
            display: "swap",
          },
        ],
      },
    },
  ],
});
