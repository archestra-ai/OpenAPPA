import type { NextConfig } from "next";

const nextConfig: NextConfig = {
  // The MCP and llms.txt routes read doc markdown from disk on every request;
  // make sure the content directory ships with their serverless functions.
  outputFileTracingIncludes: {
    "/mcp": ["./content/**/*"],
    "/llms.txt": ["./content/**/*"],
  },
  // PostHog is reached through this origin rather than directly. Two reasons:
  // requests to a posthog.com hostname are blocked by most content blockers,
  // which silently loses a large share of a technical audience, and keeping the
  // traffic first-party means no third-party cookie is involved at all.
  // Ingestion is the EU region, so reader data does not leave it.
  async rewrites() {
    return [
      // Static assets and the array config are served from the assets host,
      // which is a different hostname than the ingestion endpoint below.
      {
        source: "/ingest/static/:path*",
        destination: "https://eu-assets.i.posthog.com/static/:path*",
      },
      {
        source: "/ingest/array/:path*",
        destination: "https://eu-assets.i.posthog.com/array/:path*",
      },
      {
        source: "/ingest/:path*",
        destination: "https://eu.i.posthog.com/:path*",
      },
    ];
  },
  // Doc pages live at the root (/contracts, not /docs/contracts). Links
  // already shared under the old prefix keep working.
  async redirects() {
    return [
      { source: "/docs", destination: "/", permanent: true },
      { source: "/docs/:slug", destination: "/:slug", permanent: true },
      { source: "/chat", destination: "/playground", permanent: true },
      {
        source: "/paper",
        destination: "https://arxiv.org/abs/2607.24625",
        permanent: false,
      },
    ];
  },
};

export default nextConfig;
