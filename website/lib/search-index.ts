import { generateSections, getAllDocs } from "@/lib/docs";
import type { SearchIndex } from "@/lib/search";
import { TERM_NAMES } from "@/lib/terms";

/* Server-side only: reads the filesystem through lib/docs.ts, so it is
   reached through app/search-index/route.ts and never from a client
   component.

   The search index is derived from content/docs/*.md and lib/terms.ts, so a
   new page, a renamed heading, or a new term is searchable without anyone
   editing a list. Pages, headings, section bodies, and glossary terms are
   indexed. */

function docUrl(slug: string): string {
  return slug === "index" ? "/" : `/${slug}`;
}

/** Section body as plain text: directives, images, fences, list markers,
    and table rules dropped; links reduced to their text; bold and italic
    markers removed. Underscores stay, so identifiers such as `trust_below`
    remain searchable as typed. */
function plainText(body: string): string {
  return body
    .replace(/^:::[a-z-]+:::$/gm, "")
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
  const docs = getAllDocs().filter((doc) => Boolean(doc.title) && Boolean(doc.category));
  const index: SearchIndex = { docs: [], sections: [], terms: [] };

  /* Where each term is first mentioned as a chip. The policy-review guide is
     the reference for the vocabulary, so it is searched first. */
  const termSections = new Map<string, string>();
  const ordered = [...docs].sort((a, b) => Number(b.slug === "contracts") - Number(a.slug === "contracts"));

  for (const doc of ordered) {
    const url = docUrl(doc.slug);
    for (const section of generateSections(doc.content)) {
      const sectionUrl = `${url}#${section.id}`;
      index.sections.push({ url: sectionUrl, title: section.text, docTitle: doc.title, text: plainText(section.body) });
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
    url: docUrl(doc.slug),
    description: doc.description,
  }));
  index.terms = TERM_NAMES.map((term) => ({ term, url: termSections.get(term) ?? docUrl("contracts") }));
  return index;
}
