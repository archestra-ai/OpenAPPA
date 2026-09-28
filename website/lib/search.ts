import { termDefinition } from "@/lib/terms";

/* Client-safe search over the index that lib/search-index.ts builds from the
   markdown on the server and app/search-index/route.ts serves as JSON. The
   modal fetches it the first time search opens, so no page carries it, and
   matches in the browser. Term definitions are not in the index:
   lib/terms.ts already ships to the client for the term popovers, so the
   index carries only where each term is introduced. */

export interface IndexedDoc {
  slug: string;
  title: string;
  category: string;
  url: string;
  description: string;
  /** Text above the page's first heading, as plain text. */
  text: string;
}

export interface IndexedSection {
  /** Page URL with the heading's anchor, e.g. "/contracts#audiences". */
  url: string;
  title: string;
  docTitle: string;
  /** Section body as plain text, markdown syntax stripped and whitespace collapsed. */
  text: string;
}

export interface IndexedTerm {
  term: string;
  /** The section that first mentions the term as a code chip. */
  url: string;
}

export interface SearchIndex {
  docs: IndexedDoc[];
  sections: IndexedSection[];
  terms: IndexedTerm[];
}

export interface SearchResult {
  id: string;
  title: string;
  subtitle?: string;
  type: "doc" | "section" | "term";
  url: string;
  snippet?: string;
}

const MAX_RESULTS = 12;
const SNIPPET_RADIUS = 80;

/* Every query word must appear in the title or the body. A title that starts
   with the whole query ranks first, then a title containing it, then a title
   containing every word, then a body-only match. */
function score(query: string, words: string[], title: string, body: string): number {
  const t = title.toLowerCase();
  const b = body.toLowerCase();
  if (!words.every((w) => t.includes(w) || b.includes(w))) return 0;
  if (t.startsWith(query)) return 4;
  if (t.includes(query)) return 3;
  if (words.every((w) => t.includes(w))) return 2;
  return 1;
}

/** The body around its first query word, or its opening when the title matched. */
function snippet(words: string[], body: string): string | undefined {
  if (!body) return undefined;
  const lower = body.toLowerCase();
  const hits = words.map((w) => lower.indexOf(w)).filter((i) => i >= 0);
  const at = hits.length > 0 ? Math.min(...hits) : 0;
  const start = Math.max(0, at - SNIPPET_RADIUS);
  const end = Math.min(body.length, at + SNIPPET_RADIUS * 2);
  return `${start > 0 ? "…" : ""}${body.slice(start, end).trim()}${end < body.length ? "…" : ""}`;
}

export function searchDocs(index: SearchIndex, query: string): SearchResult[] {
  const q = query.trim().toLowerCase();
  if (!q) return [];
  const words = q.split(/\s+/);

  const scored: { result: SearchResult; score: number; rank: number }[] = [];

  for (const doc of index.docs) {
    const s = score(q, words, doc.title, `${doc.description} ${doc.text}`);
    if (s === 0) continue;
    /* The description is the page's summary; the intro is quoted only when
       the match is there and nowhere in the summary. */
    const inDescription = words.some((w) => doc.description.toLowerCase().includes(w));
    scored.push({
      score: s,
      rank: 0,
      result: {
        id: `doc-${doc.slug}`,
        title: doc.title,
        subtitle: doc.category,
        type: "doc",
        url: doc.url,
        snippet: s === 1 && !inDescription ? snippet(words, doc.text) : doc.description,
      },
    });
  }

  for (const sec of index.sections) {
    const s = score(q, words, sec.title, sec.text);
    if (s === 0) continue;
    scored.push({
      score: s,
      rank: 1,
      result: {
        id: `sec-${sec.url}`,
        title: sec.title,
        subtitle: `${sec.docTitle} section`,
        type: "section",
        url: sec.url,
        snippet: snippet(words, sec.text),
      },
    });
  }

  for (const term of index.terms) {
    const definition = termDefinition(term.term) ?? "";
    const s = score(q, words, term.term, definition);
    if (s === 0) continue;
    scored.push({
      score: s,
      rank: 2,
      result: { id: `term-${term.term}`, title: term.term, subtitle: "Glossary term", type: "term", url: term.url, snippet: definition },
    });
  }

  return scored
    .sort((a, b) => b.score - a.score || a.rank - b.rank)
    .slice(0, MAX_RESULTS)
    .map((s) => s.result);
}
