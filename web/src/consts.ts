// Central site config. Replace SITE_URL with the real domain once it is acquired
// (used for canonical URLs, Open Graph tags, and the generated sitemap).
export const SITE_URL = "https://getrumor.dev";

export const SITE_NAME = "rumor";
export const SITE_TAGLINE = "Multi-process TUI orchestrator";
export const SITE_DESCRIPTION =
  "rumor runs your whole dev stack in one terminal — each process in its own tab, " +
  "with dependency gating, retry policies, dynamic ports, and searchable logs. " +
  "A single JSON file, a single window.";

export const REPO_URL = "https://github.com/ornitech/rumor";
export const REPO_OWNER = "ornitech";

// The canonical installer lives at the repo root; /install.sh redirects to it
// (see public/_redirects) so the homepage command can use the bare domain.
export const INSTALL_CURL = `curl -fsSL ${SITE_URL}/install.sh | sh`;
export const INSTALL_RAW = `https://raw.githubusercontent.com/${REPO_OWNER}/rumor/main/install.sh`;

// Optional Cloudflare Web Analytics token. Leave empty to omit the beacon.
export const CF_ANALYTICS_TOKEN = "";
