// @ts-check
import { defineConfig } from "astro/config";
import sitemap from "@astrojs/sitemap";
import { SITE_URL } from "./src/consts.ts";

// Static output (default). Deployed to Cloudflare Pages (output: web/dist).
export default defineConfig({
  site: SITE_URL,
  trailingSlash: "ignore",
  integrations: [sitemap({ filter: (page) => !page.includes("/og") })],
  build: { inlineStylesheets: "auto" },
});
