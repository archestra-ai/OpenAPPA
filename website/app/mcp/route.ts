import { createMcpHandler } from "mcp-handler";
import { z } from "zod";

import { getMcpDoc, getMcpDocs, searchMcpDocs } from "@/lib/mcp-content";
import { GLOSSARY_TERMS } from "@/lib/search";
import { termDefinition } from "@/lib/terms";

/* Auth-less MCP server exposing the OpenAPPA documentation over streamable
   HTTP at /mcp. Content is the same markdown the site renders (lib/docs.ts),
   so the server cannot drift from the website. */

function text(value: string) {
  return { content: [{ type: "text" as const, text: value }] };
}

const handler = createMcpHandler(
  (server) => {
    server.registerTool(
      "list_docs",
      {
        title: "List OpenAPPA docs",
        description:
          "Map of the OpenAPPA documentation: every page's slug, title, description, and section headings. Call this first to orient yourself, then read_doc for full pages.",
        inputSchema: z.object({}),
      },
      async () => {
        const lines = getMcpDocs().map((doc) => {
          const sections = doc.sections.map((s) => `  - ${s.heading} (section: "${s.anchor}")`).join("\n");
          return `## ${doc.title}\nslug: "${doc.slug}"\n${doc.description}\n${sections}`;
        });
        return text(lines.join("\n\n"));
      },
    );

    server.registerTool(
      "read_doc",
      {
        title: "Read an OpenAPPA doc",
        description:
          "Full markdown of a documentation page by slug, or a single section of it when `section` (a heading anchor from list_docs) is given.",
        inputSchema: z.object({
          slug: z.string().describe('Page slug from list_docs, e.g. "how-it-works"'),
          section: z.string().optional().describe('Optional heading anchor from list_docs, e.g. "labels-only-move-one-way"'),
        }),
      },
      async ({ slug, section }) => {
        const doc = getMcpDoc(slug);
        if (!doc) {
          const known = getMcpDocs()
            .map((d) => `"${d.slug}"`)
            .join(", ");
          return text(`No doc with slug "${slug}". Known slugs: ${known}.`);
        }
        if (section) {
          const found = doc.sections.find((s) => s.anchor === section);
          if (!found) {
            const known = doc.sections.map((s) => `"${s.anchor}"`).join(", ");
            return text(`No section "${section}" in "${slug}". Known sections: ${known}.`);
          }
          return text(found.markdown);
        }
        return text(`# ${doc.title}\n\n${doc.markdown}`);
      },
    );

    server.registerTool(
      "search_docs",
      {
        title: "Search OpenAPPA docs",
        description:
          "Full-text search across all documentation pages. Returns matching sections with the slug and heading anchor to pass to read_doc, plus a snippet.",
        inputSchema: z.object({
          query: z.string().describe("Search terms, e.g. 'remedy plans' or 'audience intersection'"),
        }),
      },
      async ({ query }) => {
        const hits = searchMcpDocs(query);
        if (hits.length === 0) return text(`No matches for "${query}". Try list_docs for the page map.`);
        const lines = hits.map((h) => {
          const where = h.heading ? `${h.title} › ${h.heading} (slug: "${h.slug}", section: "${h.anchor}")` : `${h.title} (slug: "${h.slug}")`;
          return `- ${where}\n  ${h.snippet}`;
        });
        return text(lines.join("\n"));
      },
    );

    server.registerTool(
      "define_term",
      {
        title: "Define an OpenAPPA term",
        description:
          "Glossary definition of an OpenAPPA policy/model term exactly as the docs define it — e.g. delta, requires, audience, attention, Unknown, trajectory.",
        inputSchema: z.object({
          term: z.string().describe('The term, e.g. "delta" or "may_add"'),
        }),
      },
      async ({ term }) => {
        const definition = termDefinition(term.trim());
        if (definition) return text(`${term.trim()}: ${definition}`);
        const known = GLOSSARY_TERMS.join(", ");
        return text(`No glossary entry for "${term}". Known terms: ${known}.`);
      },
    );

    server.registerPrompt(
      "explain_openappa",
      {
        title: "Explain OpenAPPA",
        description: "Prime the assistant with the full 'How OpenAPPA works' introduction.",
        argsSchema: z.object({}),
      },
      () => {
        const doc = getMcpDoc("how-it-works");
        return {
          messages: [
            {
              role: "user" as const,
              content: {
                type: "text" as const,
                text: `Read the following OpenAPPA introduction, then answer my questions grounded in it.\n\n${doc ? doc.markdown : "(doc unavailable)"}`,
              },
            },
          ],
        };
      },
    );

    for (const doc of getMcpDocs()) {
      server.registerResource(
        `doc-${doc.slug}`,
        `openappa://docs/${doc.slug}`,
        {
          title: doc.title,
          description: doc.description,
          mimeType: "text/markdown",
        },
        async (uri) => ({
          contents: [{ uri: uri.href, mimeType: "text/markdown", text: `# ${doc.title}\n\n${doc.markdown}` }],
        }),
      );
    }
  },
  {
    serverInfo: { name: "openappa-docs", version: "1.0.0" },
  },
);

export { handler as GET, handler as POST, handler as DELETE };
