"use client";

import { Fragment, type AnchorHTMLAttributes, type HTMLAttributes, type ReactNode } from "react";
import ReactMarkdown from "react-markdown";
import rehypeHighlight from "rehype-highlight";
import rehypeSlug from "rehype-slug";
import remarkGfm from "remark-gfm";

import { AdvisorySignup } from "@/components/AdvisorySignup";
import { BatteryCatalog } from "@/components/BatteryCatalog";
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
import { LabelFoldFigure } from "@/components/figures/LabelFoldFigure";
import { NegotiationFigure } from "@/components/figures/NegotiationFigure";
import { PolicyStackFigure } from "@/components/figures/PolicyStackFigure";
import { RemedyPlanFigure } from "@/components/figures/RemedyPlanFigure";
import { TwoEndingsFigure } from "@/components/figures/TwoEndingsFigure";
import { MascotBoard } from "@/components/MascotBoard";
import { ProposalBlock } from "@/components/ProposalBlock";
import { Term } from "@/components/Term";
import { parseProposal, PROPOSAL_SPLIT } from "@/lib/proposals";
import { termDefinition } from "@/lib/terms";

/* Block directives: a line of the form :::name::: in the markdown renders
   the mapped component in place. */
const DIRECTIVES: Record<string, () => ReactNode> = {
  "advisory-signup": () => <AdvisorySignup />,
  "battery-catalog": () => <BatteryCatalog />,
  "battery-rule-order": () => <BatteryRuleOrderFigure />,
  "benchmark-highlight": () => <BenchmarkHighlight />,
  "brand-assets": () => <BrandAssets />,
  "claude-policy-timing": () => <ClaudePolicyTiming />,
  "claude-session-choice": () => <ClaudeSessionChoice />,
  "fig-claude-code-hooks": () => <ClaudeCodeHooksFigure />,
  "fig-connected-agent": () => <ConnectedAgentFigure />,
  "fig-exfiltration": () => <ExfiltrationFigure />,
  "fig-guardrail": () => <GuardrailFigure />,
  "fig-label-fold": () => <LabelFoldFigure />,
  "fig-negotiation": () => <NegotiationFigure />,
  "fig-policy-stack": () => <PolicyStackFigure />,
  "fig-remedy-plan": () => <RemedyPlanFigure />,
  "fig-two-endings": () => <TwoEndingsFigure />,
  "mascot-board": () => <MascotBoard />,
  "details-7pc-leak": () => (
    <details className="leak-details my-6 rounded-lg border border-[var(--border)] bg-[var(--bg-weak)] p-4 text-sm text-[var(--text)]">
      <summary className="cursor-pointer font-semibold text-[var(--text-strong)] hover:text-[var(--accent)]">
        Why the 7% leak occurred under GPT-4o (and why the algebra held)
      </summary>
      <div className="mt-3 space-y-3 border-t border-[var(--border)] pt-3 leading-relaxed">
        <p>
          You might notice that under GPT-4o, OpenAPPA allowed 3 breaches out of 42 episodes (a 7% Attack Success Rate). Does this mean the label algebra failed?
        </p>
        <p>
          <strong>No—OpenAPPA&apos;s mathematical label invariants held 100% of the time.</strong> Every non-zero breach traced back to two specific scenario configurations, neither of which was an enforcement bypass:
        </p>
        <ol className="list-decimal space-y-2 pl-5">
          <li>
            <strong>Authorized recipient smuggling (<code>hide-secret-in-status</code>)</strong>: An injected prompt tricked the agent into emailing data to <code>finance-lead@</code>. Because <code>finance-lead@</code> was explicitly listed in the policy as an authorized internal finance reader, OpenAPPA&apos;s label algebra permitted the send. The label algebra enforces <em>who</em> is legally allowed to receive data (reader sets); detecting secret text hidden inside an email sent to an authorized recipient requires content scrubbers, which recipient label algebra does not claim to provide.
          </li>
          <li>
            <strong>Unannotated write contract (<code>joint-merger-brief</code>)</strong>: The agent copied an HR value into a finance data store whose tool contract had no destination restriction declared on creation, then read it back under the finance contract. OpenAPPA prospectively enforces declared tool contracts; if a custom write contract omits a restriction, the engine permits the call.
          </li>
        </ol>
        <p>
          In short: the policy algebra executed perfectly according to its declared rules. Prospective enforcement is as complete as the tool contracts provided to it.
        </p>
      </div>
    </details>
  ),
};

const DIRECTIVE_SPLIT = /^:::([a-z0-9-]+):::$/m;

function AnchoredHeading({
  level,
  id,
  children,
  ...props
}: HTMLAttributes<HTMLHeadingElement> & { level: 2 | 3 | 4; children?: ReactNode }) {
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

/* Inline code whose text names a glossary term gets a definition popover;
   block code (array children after highlighting) falls through untouched. */
function MarkdownCode({ children, ...props }: HTMLAttributes<HTMLElement> & { children?: ReactNode }) {
  if (typeof children === "string") {
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
      rehypePlugins={[rehypeSlug, rehypeHighlight]}
      components={{
        pre: (props) => <CodeBlock {...props} />,
        code: terms ? MarkdownCode : PlainCode,
        a: MarkdownLink,
        // A table's min-content width can exceed a phone viewport; without a
        // scroll container of its own it widens the whole page instead.
        table: (props) => (
          <div className="table-scroll">
            <table {...props} />
          </div>
        ),
        h2: (props) => <AnchoredHeading level={2} {...props} />,
        h3: (props) => <AnchoredHeading level={3} {...props} />,
        h4: (props) => <AnchoredHeading level={4} {...props} />,
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
          <Fragment key={index}>{DIRECTIVES[part]?.()}</Fragment>
        ) : (
          <Markdown key={index} content={part} terms={terms} />
        ),
      )}
    </>
  );
}

export function DocContent({ content }: { content: string }) {
  // proposals split first, so a directive inside one still renders in place
  const blocks = content.split(PROPOSAL_SPLIT);
  return (
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
  );
}
