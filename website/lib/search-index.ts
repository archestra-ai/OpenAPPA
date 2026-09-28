import { docUrl, getTextDocs } from "@/lib/doc-text";
import type { SearchIndex } from "@/lib/search";
import { TERM_NAMES } from "@/lib/terms";

/* Server-side only: reads the filesystem through lib/docs.ts, so it is
   reached through app/search-index/route.ts and never from a client
   component.

   The search index is derived from the same text form of content/docs/*.md
   that /llms.txt and the MCP server serve (lib/doc-text.ts), plus
   lib/terms.ts, so a new page, a renamed heading, a directive's copy, or a
   new term is searchable without anyone editing a list. Pages, headings,
   section bodies, and glossary terms are indexed. */

/** Section body as plain text: images, fences, list markers,
    and table rules dropped; links reduced to their text; bold and italic
    markers removed. Underscores stay, so identifiers such as `trust_below`
    remain searchable as typed. */
function plainText(body: string): string {
  return body
    .replace(/^```.*$/gm, "")
    .replace(/!\[[^\]]*\]\([^)]*\)/g, "")
    .replace(/\[([^\]]+)\]\([^)]*\)/g, "$1")
    .replace(/^\s*\|?\s*:?-+:?\s*(\|\s*:?-+:?\s*)*\|?\s*$/gm, "")
    .replace(/^\s*[-*+]\s+/gm, "")
    .replace(/\*\*(.+?)\*\*/g, "$1")
    .replace(/(^|\s)\*(\S[^*\n]*?)\*(?=[\s.,;:)]|$)/g, "$1$2")
    .replace(/[`|]/g, " ")
    .replace(/\s+/g, " ")
    .trim();
}

export function buildSearchIndex(): SearchIndex {
  const docs = getTextDocs();
  const index: SearchIndex = { docs: [], sections: [], terms: [] };

  /* Where each term is first mentioned as a chip. The policy-review guide is
     the reference for the vocabulary, so it is searched first. */
  const termSections = new Map<string, string>();
  const ordered = [...docs].sort((a, b) => Number(b.slug === "contracts") - Number(a.slug === "contracts"));

  for (const doc of ordered) {
    for (const section of doc.sections) {
      const sectionUrl = `${doc.url}#${section.anchor}`;
      index.sections.push({ url: sectionUrl, title: section.heading, docTitle: doc.title, text: plainText(section.body) });
      for (const term of TERM_NAMES) {
        if (!termSections.has(term) && section.body.includes(`\`${term}\``)) termSections.set(term, sectionUrl);
      }
    }
  }

  /* Pages keep the sidebar order, whatever order the term scan used. */
  index.docs = docs.map((doc) => ({
    slug: doc.slug,
    title: doc.title,
    category: doc.category,
    url: doc.url,
    description: doc.description,
    text: plainText(doc.intro),
  }));
  index.terms = TERM_NAMES.map((term) => ({ term, url: termSections.get(term) ?? docUrl("contracts") }));
  return index;
}
