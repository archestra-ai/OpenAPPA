import { termDefinition } from "@/lib/terms";

export interface SearchResult {
  id: string;
  title: string;
  subtitle?: string;
  type: "doc" | "section" | "rule" | "term";
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
    url: "/docs/how-it-works",
    description: "The whole model in one sitting — what OpenAPPA guarantees and what it costs.",
  },
  {
    slug: "contracts",
    title: "Policy reference",
    category: "Get started",
    url: "/docs/contracts",
    description: "Declarations, syntax, and rules for OpenAPPA policy TOML files.",
  },
  {
    slug: "evaluation",
    title: "Evaluating OpenAPPA",
    category: "Get started",
    url: "/docs/evaluation",
    description: "Empirical evaluation and bench-corp benchmark results.",
  },
];

const STATIC_SECTIONS = [
  { title: "OpenAPPA enforces information-flow policy before tool dispatch", url: "/docs/how-it-works#openappa-enforces-information-flow-policy-before-tool-dispatch", docTitle: "How OpenAPPA works" },
  { title: "Labels only move one way", url: "/docs/how-it-works#labels-only-move-one-way", docTitle: "How OpenAPPA works" },
  { title: "Reading data costs the agent reach", url: "/docs/how-it-works#reading-data-costs-the-agent-reach", docTitle: "How OpenAPPA works" },
  { title: "A child's narrowing dies with it", url: "/docs/how-it-works#a-childs-narrowing-dies-with-it", docTitle: "How OpenAPPA works" },
  { title: "Engine refusals enumerate every valid remedy", url: "/docs/how-it-works#engine-refusals-enumerate-every-valid-remedy", docTitle: "How OpenAPPA works" },
  { title: "Unknown labels propagate until a requirement checks them", url: "/docs/how-it-works#unknown-labels-propagate-until-a-requirement-checks-them", docTitle: "How OpenAPPA works" },
  { title: "Model guarantees depend on four explicit assumptions", url: "/docs/how-it-works#model-guarantees-depend-on-four-explicit-assumptions", docTitle: "How OpenAPPA works" },
  { title: "Set operators", url: "/docs/contracts#set-operators", docTitle: "Policy reference" },
  { title: "What to check when reviewing", url: "/docs/contracts#what-to-check-when-reviewing", docTitle: "Policy reference" },
  { title: "Tools", url: "/docs/contracts#tools", docTitle: "Policy reference" },
  { title: "Authorities", url: "/docs/contracts#authorities", docTitle: "Policy reference" },
  { title: "Sanitizers", url: "/docs/contracts#sanitizers", docTitle: "Policy reference" },
  { title: "Casts", url: "/docs/contracts#casts", docTitle: "Policy reference" },
  { title: "Empirical evaluation", url: "/docs/evaluation#empirical-evaluation-results", docTitle: "Evaluating OpenAPPA" },
];

const STATIC_RULES = [
  { id: "LBL-6", title: "LBL-6: Restrictive Delta Invariant", url: "/docs/contracts#tools", snippet: "A tool's delta can only narrow audience or lower trust." },
  { id: "CHK-9", title: "CHK-9: Dynamic Recipient Audience Check", url: "/docs/contracts#tools", snippet: "Requires recipient matching using dynamic placeholders like $recipient." },
  { id: "CHK-15", title: "CHK-15: Dual-Gate Contract Evaluation", url: "/docs/contracts#tools", snippet: "Evaluates both delta narrowing and requires gates on a single dispatch." },
  { id: "RUL-1", title: "RUL-1: Authority Approval Invariant", url: "/docs/contracts#authorities", snippet: "Authority approvals clear gaps for one dispatch without raising overall label." },
  { id: "RUL-8", title: "RUL-8: Resolver Context Logging", url: "/docs/contracts#authorities", snippet: "Dynamic authority resolvers receive call digest and log decision verbatim." },
  { id: "SAN-4", title: "SAN-4: Sanitizer Transition Mandate", url: "/docs/contracts#sanitizers", snippet: "Defines exact label transition for scrubbed data." },
  { id: "SAN-6", title: "SAN-6: Sanitizer Audit Trail", url: "/docs/contracts#sanitizers", snippet: "Log records transition name and sanitizer ID." },
  { id: "SAN-7", title: "SAN-7: Cast Resolution Types", url: "/docs/contracts#casts", snippet: "Resolves Unknown label dimensions via constant or resolver." },
  { id: "SAN-8", title: "SAN-8: Cast Ceiling Re-validation", url: "/docs/contracts#casts", snippet: "Engine re-validates resolver response against declared ceiling." },
  { id: "CFG-8", title: "CFG-8: Mandatory Set Operators", url: "/docs/contracts#set-operators", snippet: "Set declarations without explicit operators fail policy load." },
  { id: "UNK-5", title: "UNK-5: Unannotated Output State", url: "/docs/contracts#what-to-check-when-reviewing", snippet: "Unannotated tool outputs enter in Unknown state." },
];

const GLOSSARY_TERMS = [
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
  "exactly",
  "includes",
  "cap",
  "may_add",
  "attention",
  "mandate",
  "can_raise_trust_to",
  "can_add_readers",
  "can_waive",
  "attends",
  "scope",
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

  // Match Rule IDs
  for (const rule of STATIC_RULES) {
    if (rule.id.toLowerCase().includes(q) || rule.title.toLowerCase().includes(q)) {
      results.push({
        id: `rule-${rule.id}`,
        title: rule.title,
        subtitle: "Spec Invariant",
        type: "rule",
        url: rule.url,
        snippet: rule.snippet,
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
          url: "/docs/contracts#what-to-check-when-reviewing",
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
