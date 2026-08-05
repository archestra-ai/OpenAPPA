import { getMcpDocs } from "@/lib/mcp-content";

/* The whole documentation as one file, for `curl` and LLM ingestion
   (https://llmstxt.org): an index up top, then every page in full. Built
   from the content/docs *.md files on every request — the docs menu is the
   catalog, so exactly the pages it lists are served. */

export const dynamic = "force-dynamic";

export async function GET() {
  const docs = getMcpDocs();
  const index = [
    "# OpenAPPA",
    "",
    "> A deterministic policy engine for LLM agents — tracking data origins and enforcing information flow before tool calls dispatch.",
    "",
    "## Docs",
    "",
    ...docs.map((doc) => `- [${doc.title}](${doc.url}): ${doc.description}`),
    "",
    "## Optional",
    "",
    "- [MCP server](/mcp): the same docs as an auth-less MCP server (streamable HTTP)",
  ].join("\n");
  const body = docs.map((doc) => `# ${doc.title}\n\n> ${doc.description}\n\n${doc.markdown}`).join("\n\n---\n\n");
  return new Response(`${index}\n\n---\n\n${body}\n`, {
    headers: { "Content-Type": "text/markdown; charset=utf-8" },
  });
}
