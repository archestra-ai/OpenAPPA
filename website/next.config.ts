import type { NextConfig } from "next";

const nextConfig: NextConfig = {
  // The MCP and llms.txt routes read doc markdown from disk on every request;
  // make sure the content directory ships with their serverless functions.
  outputFileTracingIncludes: {
    "/mcp": ["./content/**/*"],
    "/llms.txt": ["./content/**/*"],
  },
  // Doc pages live at the root (/contracts, not /docs/contracts). Links
  // already shared under the old prefix keep working.
  async redirects() {
    return [
      { source: "/docs", destination: "/", permanent: true },
      { source: "/docs/:slug", destination: "/:slug", permanent: true },
    ];
  },
};

export default nextConfig;
