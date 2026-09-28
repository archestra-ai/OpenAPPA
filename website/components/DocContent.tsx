"use client";

import { createContext, Fragment, useContext, type AnchorHTMLAttributes, type HTMLAttributes, type ReactNode } from "react";
import ReactMarkdown from "react-markdown";
import rehypeHighlight from "rehype-highlight";
import rehypeSlug from "rehype-slug";
import remarkGfm from "remark-gfm";
import type { LanguageFn } from "highlight.js";
import { common } from "lowlight";

import { AdvisorySignup } from "@/components/AdvisorySignup";
import { BatteryCardsContext, BatteryCatalog } from "@/components/BatteryCatalog";
import { BenchmarkHighlight } from "@/components/BenchmarkHighlight";
import { BrandAssets } from "@/components/BrandKit";
import {
  ClaudePolicyTiming,
  ClaudeSessionChoice,
} from "@/components/ClaudeCodeStory";
import { CodeBlock } from "@/components/CodeBlock";
import { BatteryRuleOrderFigure } from "@/components/figures/BatteriesFigures";
import { ClaudeCodeHooksFigure } from "@/components/figures/ClaudeCodeHooksFigure";
import { ConnectedAgentFigure } from "@/components/figures/ConnectedAgentFigure";
import { ExfiltrationFigure } from "@/components/figures/ExfiltrationFigure";
import { GuardrailFigure } from "@/components/figures/GuardrailFigure";
import { KagentFigure } from "@/components/figures/KagentFigure";
import { LabelFoldFigure } from "@/components/figures/LabelFoldFigure";
import { NegotiationFigure } from "@/components/figures/NegotiationFigure";
import { PolicyStackFigure } from "@/components/figures/PolicyStackFigure";
import { RemedyPlanFigure } from "@/components/figures/RemedyPlanFigure";
import { RuntimeOverviewFigure } from "@/components/figures/RuntimeOverviewFigure";
import { TwoEndingsFigure } from "@/components/figures/TwoEndingsFigure";
import { MascotBoard } from "@/components/MascotBoard";
import { IntegrationPaths } from "@/components/IntegrationPaths";
import { ProposalBlock } from "@/components/ProposalBlock";
import { SponsorNote } from "@/components/SponsorNote";
import { Term } from "@/components/Term";
import { YouTubeEmbed } from "@/components/YouTubeEmbed";
import { BATTERY_REVIEW_CHECKLIST, isDirectiveName, type DirectiveName } from "@/lib/directive-content";
import type { BatteryCard } from "@/lib/docs";
import { parseProposal, PROPOSAL_SPLIT } from "@/lib/proposals";
import { termDefinition } from "@/lib/terms";

const appaTraceLanguage: LanguageFn = (hljs) => ({
  name: "OpenAPPA replay trace",
  aliases: ["appa"],
  contains: [
    hljs.COMMENT("#", "$"),
    {
      scope: "title.function",
      begin: /^[A-Za-z_][A-Za-z0-9_.-]*(?=\s*\{)/m,
    },
    {
      scope: "attr",
      begin: /[A-Za-z_][A-Za-z0-9_-]*(?=\s*:)/,
    },
    {
      scope: "keyword",
      begin: /\bexpect\b/,
    },
    {
      scope: "literal",
      begin: /\b(?:allow|deny|offer)\b/,
    },
    {
      scope: "string",
      begin: /"/,
      end: /"/,
      contains: [hljs.BACKSLASH_ESCAPE],
    },
    hljs.NUMBER_MODE,
  ],
});

/** `backticks` in shared directive copy become <code>. */
function inlineCode(text: string): ReactNode {
  return text.split(/(`[^`]+`)/).map((part, i) =>
    part.startsWith("`") ? <code key={i}>{part.slice(1, -1)}</code> : <Fragment key={i}>{part}</Fragment>,
  );
}

/* Block directives: a line of the form :::name::: in the markdown renders
   the mapped component in place. */
const DIRECTIVES: Record<DirectiveName, () => ReactNode> = {
  "advisory-signup": () => <AdvisorySignup />,
  "battery-catalog": () => <BatteryCatalog />,
  "battery-review-checklist": () => (
    <details className="my-5 rounded-lg border border-[var(--border)] bg-[var(--bg-weak)] px-4 py-3 text-sm text-[var(--text)]">
      <summary className="cursor-pointer font-semibold text-[var(--text-strong)] hover:text-[var(--accent)]">
        {BATTERY_REVIEW_CHECKLIST.summary}
      </summary>
      <div className="mt-3 border-t border-[var(--border)] pt-3 leading-relaxed">
        <p>{BATTERY_REVIEW_CHECKLIST.intro}</p>
        <ul className="mt-2 list-disc space-y-2 pl-5">
          {BATTERY_REVIEW_CHECKLIST.items.map((item) => (
            <li key={item}>{inlineCode(item)}</li>
          ))}
        </ul>
      </div>
    </details>
  ),
  "battery-rule-order": () => <BatteryRuleOrderFigure />,
  "benchmark-highlight": () => <BenchmarkHighlight />,
  "brand-assets": () => <BrandAssets />,
  "claude-policy-timing": () => <ClaudePolicyTiming />,
  "claude-session-choice": () => <ClaudeSessionChoice />,
  "fig-claude-code-hooks": () => <ClaudeCodeHooksFigure />,
  "fig-connected-agent": () => <ConnectedAgentFigure />,
  "fig-exfiltration": () => <ExfiltrationFigure />,
  "fig-guardrail": () => <GuardrailFigure />,
  "fig-kagent": () => <KagentFigure />,
  "fig-label-fold": () => <LabelFoldFigure />,
  "fig-negotiation": () => <NegotiationFigure />,
  "fig-policy-stack": () => <PolicyStackFigure />,
  "fig-remedy-plan": () => <RemedyPlanFigure />,
  "fig-runtime-overview": () => <RuntimeOverviewFigure overview />,
  "fig-two-endings": () => <TwoEndingsFigure />,
  "mascot-board": () => <MascotBoard />,
  "integration-paths": () => <IntegrationPaths />,
  "sponsor-note": () => <SponsorNote />,
  "video-how-it-works": () => (
    <YouTubeEmbed title="How OpenAPPA works" videoId="XKdN90IYy0Y" />
  ),
};

const DIRECTIVE_SPLIT = /^:::([a-z0-9-]+):::$/m;

function AnchoredHeading({
  level,
  id,
  children,
  ...props
}: HTMLAttributes<HTMLHeadingElement> & { level: 2 | 3 | 4 | 5; children?: ReactNode }) {
  const Tag = `h${level}` as const;
  return (
    <Tag id={id} {...props}>
      {children}
      {id && (
        <a href={`#${id}`} className="heading-anchor" aria-label="Link to this section">
          #
        </a>
      )}
    </Tag>
  );
}

const InDocTable = createContext(false);

/* Inline code outside tables whose text names a glossary term gets a definition popover;
   block code (array children after highlighting) falls through untouched. */
function MarkdownCode({ children, ...props }: HTMLAttributes<HTMLElement> & { children?: ReactNode }) {
  const inTable = useContext(InDocTable);
  if (!inTable && typeof children === "string") {
    const definition = termDefinition(children);
    if (definition !== undefined) return <Term chip={children} definition={definition} />;
  }
  return <code {...props}>{children}</code>;
}

/* A proposal may reuse an implemented key with a different meaning, and the
   glossary defines the implemented one. Popovers stay off inside a proposal
   rather than contradicting the text they annotate. */
function PlainCode({ children, ...props }: HTMLAttributes<HTMLElement> & { children?: ReactNode }) {
  return <code {...props}>{children}</code>;
}

function MarkdownLink({ href, children, ...props }: AnchorHTMLAttributes<HTMLAnchorElement>) {
  const isExternal = href?.startsWith("http");
  return (
    <a
      href={href}
      {...(isExternal ? { target: "_blank", rel: "noreferrer" } : {})}
      {...props}
    >
      {children}
    </a>
  );
}

function Markdown({ content, terms = true }: { content: string; terms?: boolean }) {
  return (
    <ReactMarkdown
      remarkPlugins={[remarkGfm]}
      rehypePlugins={[
        rehypeSlug,
        [rehypeHighlight, { languages: { ...common, appa: appaTraceLanguage } }],
      ]}
      components={{
        p: ({ children }) => {
          // Indented directives stay inside their Markdown list item.
          const match = typeof children === "string" ? children.match(/^:::([a-z0-9-]+):::$/) : null;
          const render = match && isDirectiveName(match[1]) ? DIRECTIVES[match[1]] : undefined;
          return render ? <>{render()}</> : <p>{children}</p>;
        },
        pre: (props) => <CodeBlock {...props} />,
        code: terms ? MarkdownCode : PlainCode,
        a: MarkdownLink,
        // A table's min-content width can exceed a phone viewport; without a
        // scroll container of its own it widens the whole page instead.
        table: (props) => (
          <InDocTable.Provider value={true}>
            <div className="table-scroll">
              <table {...props} />
            </div>
          </InDocTable.Provider>
        ),
        h2: (props) => <AnchoredHeading level={2} {...props} />,
        h3: (props) => <AnchoredHeading level={3} {...props} />,
        h4: (props) => <AnchoredHeading level={4} {...props} />,
        h5: (props) => <AnchoredHeading level={5} {...props} />,
      }}
    >
      {content}
    </ReactMarkdown>
  );
}

function MarkdownWithDirectives({ content, terms = true }: { content: string; terms?: boolean }) {
  // split() with a captured group interleaves markdown chunks and directive names
  const parts = content.split(DIRECTIVE_SPLIT);
  return (
    <>
      {parts.map((part, index) =>
        index % 2 === 1 ? (
          <Fragment key={index}>{isDirectiveName(part) && DIRECTIVES[part]()}</Fragment>
        ) : (
          <Markdown key={index} content={part} terms={terms} />
        ),
      )}
    </>
  );
}

/** `batteries` feeds :::battery-catalog:::; pages without it pass nothing. */
export function DocContent({ content, batteries = [] }: { content: string; batteries?: BatteryCard[] }) {
  // proposals split first, so a directive inside one still renders in place
  const blocks = content.split(PROPOSAL_SPLIT);
  return (
    <BatteryCardsContext.Provider value={batteries}>
      <div className="prose">
        {blocks.map((block, index) => {
          if (index % 2 === 0) return <MarkdownWithDirectives key={index} content={block} />;
          const proposal = parseProposal(block);
          return (
            <ProposalBlock key={index} proposal={proposal}>
              <MarkdownWithDirectives content={proposal.body} terms={false} />
            </ProposalBlock>
          );
        })}
      </div>
    </BatteryCardsContext.Provider>
  );
}
