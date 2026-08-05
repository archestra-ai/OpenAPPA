import type { NextConfig } from "next";

const nextConfig: NextConfig = {
  // The MCP and llms.txt routes read doc markdown from disk on every request;
  // make sure the content directory ships with their serverless functions.
  outputFileTracingIncludes: {
    "/mcp": ["./content/**/*"],
    "/llms.txt": ["./content/**/*"],
  },
};

export default nextConfig;
