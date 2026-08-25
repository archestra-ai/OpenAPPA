import { termDefinition } from "@/lib/terms";

export interface SearchResult {
  id: string;
  title: string;
  subtitle?: string;
  type: "doc" | "section" | "term";
  url: string;
  snippet?: string;
}

const STATIC_DOCS = [
  {
    slug: "index",
    title: "What is OpenAPPA",
    category: "Get started",
    url: "/",
    description: "A deterministic policy engine for LLM agents — tracking data origins and enforcing information flow before tool calls dispatch.",
  },
  {
    slug: "how-it-works",
    title: "How OpenAPPA works",
    category: "Get started",
    url: "/how-it-works",
    description: "The whole model in one sitting — what OpenAPPA guarantees and what it costs.",
  },
  {
    slug: "contracts",
    title: "Policy reference",
    category: "Get started",
    url: "/contracts",
    description: "Declarations, syntax, and rules for OpenAPPA policy TOML files.",
  },
  {
    slug: "evaluation",
    title: "Evaluating OpenAPPA",
    category: "Get started",
    url: "/evaluation",
    description: "Empirical evaluation and bench-corp benchmark results.",
  },
];

const STATIC_SECTIONS = [
  { title: "OpenAPPA enforces information-flow policy before tool dispatch", url: "/how-it-works#openappa-enforces-information-flow-policy-before-tool-dispatch", docTitle: "How OpenAPPA works" },
  { title: "Labels only move one way", url: "/how-it-works#labels-only-move-one-way", docTitle: "How OpenAPPA works" },
  { title: "Reading data costs the agent reach", url: "/how-it-works#reading-data-costs-the-agent-reach", docTitle: "How OpenAPPA works" },
  { title: "A child's narrowing dies with it", url: "/how-it-works#a-childs-narrowing-dies-with-it", docTitle: "How OpenAPPA works" },
  { title: "Engine refusals enumerate every valid remedy", url: "/how-it-works#engine-refusals-enumerate-every-valid-remedy", docTitle: "How OpenAPPA works" },
  { title: "Unknown labels propagate until a consumer checks them", url: "/how-it-works#unknown-labels-propagate-until-a-consumer-checks-them", docTitle: "How OpenAPPA works" },
  { title: "Model guarantees depend on four explicit assumptions", url: "/how-it-works#model-guarantees-depend-on-four-explicit-assumptions", docTitle: "How OpenAPPA works" },
  { title: "Set operators", url: "/contracts#set-operators", docTitle: "Policy reference" },
  { title: "What to check when reviewing", url: "/contracts#what-to-check-when-reviewing", docTitle: "Policy reference" },
  { title: "Tools", url: "/contracts#tools", docTitle: "Policy reference" },
  { title: "Authorities", url: "/contracts#authorities", docTitle: "Policy reference" },
  { title: "Sanitizers", url: "/contracts#sanitizers", docTitle: "Policy reference" },
  { title: "Casts", url: "/contracts#casts", docTitle: "Policy reference" },
  { title: "Empirical evaluation", url: "/evaluation#summary-of-empirical-findings", docTitle: "Evaluating OpenAPPA" },
];

export const GLOSSARY_TERMS = [
  "delta",
  "requires",
  "audience",
  "trust",
  "trusted",
  "suspicious",
  "public",
  "internal",
  "egress",
  "mutation",
  "effects",
  "emits",
  "contains",
  "within",
  "excludes",
  "attention",
  "permits",
  "trust_below",
  "audience_missing",
  "effects_containing",
  "tags",
  "constant",
  "resolver",
  "may_cast",
  "return_sanitizer",
  "narrowing",
  "remedy_plans",
  "unestablished",
  "Unknown",
  "trajectory",
];

export function searchDocs(query: string): SearchResult[] {
  const q = query.trim().toLowerCase();
  if (!q) return [];

  const results: SearchResult[] = [];

  // Match static docs
  for (const doc of STATIC_DOCS) {
    if (doc.title.toLowerCase().includes(q) || doc.description.toLowerCase().includes(q)) {
      results.push({
        id: `doc-${doc.slug}`,
        title: doc.title,
        subtitle: doc.category,
        type: "doc",
        url: doc.url,
        snippet: doc.description,
      });
    }
  }

  // Match section headings
  for (const sec of STATIC_SECTIONS) {
    if (sec.title.toLowerCase().includes(q)) {
      results.push({
        id: `sec-${sec.url}`,
        title: sec.title,
        subtitle: `${sec.docTitle} section`,
        type: "section",
        url: sec.url,
      });
    }
  }

  // Match Glossary Terms
  for (const term of GLOSSARY_TERMS) {
    if (term.toLowerCase().includes(q)) {
      const def = termDefinition(term);
      if (def) {
        results.push({
          id: `term-${term}`,
          title: term,
          subtitle: "Glossary Term",
          type: "term",
          url: "/contracts#what-to-check-when-reviewing",
          snippet: def,
        });
      }
    }
  }

  // Deduplicate and return top 10 results
  const seen = new Set<string>();
  return results.filter((item) => {
    if (seen.has(item.id)) return false;
    seen.add(item.id);
    return true;
  }).slice(0, 10);
}
