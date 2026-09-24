"use client";

import { createContext, Fragment, useContext, type AnchorHTMLAttributes, type HTMLAttributes, type ReactNode } from "react";
import ReactMarkdown from "react-markdown";
import rehypeHighlight from "rehype-highlight";
import rehypeSlug from "rehype-slug";
import remarkGfm from "remark-gfm";
import type { LanguageFn } from "highlight.js";
import { common } from "lowlight";

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
import { KagentFigure } from "@/components/figures/KagentFigure";
import { LabelFoldFigure } from "@/components/figures/LabelFoldFigure";
import { NegotiationFigure } from "@/components/figures/NegotiationFigure";
import { PolicyStackFigure } from "@/components/figures/PolicyStackFigure";
import { RemedyPlanFigure } from "@/components/figures/RemedyPlanFigure";
import { RuntimeOverviewFigure } from "@/components/figures/RuntimeOverviewFigure";
import { TwoEndingsFigure } from "@/components/figures/TwoEndingsFigure";
import { MascotBoard } from "@/components/MascotBoard";
import { IntegrationPaths, IntegrationCheckpoints } from "@/components/IntegrationPaths";
import { ProposalBlock } from "@/components/ProposalBlock";
import { SponsorNote } from "@/components/SponsorNote";
import { Term } from "@/components/Term";
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

/* Block directives: a line of the form :::name::: in the markdown renders
   the mapped component in place. */
const DIRECTIVES: Record<string, () => ReactNode> = {
  "advisory-signup": () => <AdvisorySignup />,
  "battery-catalog": () => <BatteryCatalog />,
  "battery-review-checklist": () => (
    <details className="my-5 rounded-lg border border-[var(--border)] bg-[var(--bg-weak)] px-4 py-3 text-sm text-[var(--text)]">
      <summary className="cursor-pointer font-semibold text-[var(--text-strong)] hover:text-[var(--accent)]">
        What should I review?
      </summary>
      <div className="mt-3 border-t border-[var(--border)] pt-3 leading-relaxed">
        <p>Open the links next to each rule and check:</p>
        <ul className="mt-2 list-disc space-y-2 pl-5">
          <li>The battery includes every tool in the server version it names.</li>
          <li>It identifies every tool that sends data or changes something.</li>
          <li>Each action sends data only to the people or services you expect.</li>
          <li>Only the right people can see each result.</li>
          <li>Data from sources you have not checked is <code>suspicious</code>, not <code>trusted</code>.</li>
          <li>The battery asks a person before every action that needs approval.</li>
          <li>The agent lists every question it could not answer from the server code or docs.</li>
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
  "fig-runtime-overview": () => <RuntimeOverviewFigure />,
  "fig-runtime-overview-v2": () => <RuntimeOverviewFigure overview />,
  "fig-two-endings": () => <TwoEndingsFigure />,
  "mascot-board": () => <MascotBoard />,
  "integration-paths": () => <IntegrationPaths />,
  "integration-checkpoints": () => <IntegrationCheckpoints />,
  "integration-details": () => (
    <div className="my-6 divide-y divide-[var(--border)] rounded-lg border border-[var(--border)] text-sm">
      {[
        ["Lifecycle events and concurrent calls", `The HTTP protocol carries one event per versioned JSON envelope. Embedded integrations submit typed events to the same runtime dispatcher.

| Event | Your integration handles |
|---|---|
| \`session_start\` | Continue on \`ack\`; stop startup on \`refuse\`. |
| \`prompt\` | Mark a turn boundary. This does not check prompt content. |
| \`tool_call\` | Run on \`allow_call\`; withhold execution on \`deny_call\` and return feedback and remedy offers. |
| \`tool_result\` | Deliver on \`ack\`; use \`deliver_value\` or \`replace_output\` instead of the original; withhold on \`block\`. |
| \`turn_end\` | Settle the turn after its tools finish. |

Keep a stable trajectory ID for the conversation and a distinct \`call_id\` for each call. Send the same call ID with its result so concurrent calls can finish in any order. Report failures as well as successes. A timeout or \`refuse\` must stop the pending flow.

Use the [event and decision types](https://github.com/archestra-ai/OpenAPPA/tree/main/appa-runtime-api) for exact payloads. The [Rust example](https://github.com/archestra-ai/OpenAPPA/tree/main/appa-example-agent) uses \`appa_runtime::hooks::handle\`; disable the runtime's default features when embedding it without the daemon.`],
        ["Remedy calls", `For a hook integration, expose APPA's control tool through \`/mcp\`. Submit its proposed call through \`tool_call\` first. A \`pass_control\` decision routes the call unchanged to APPA's remedy handler; it does not approve the blocked action by itself.`],
        ["If your agent uses subagents", `A child agent has its own context, but delegation does not bypass policy. APPA checks the launch and the return to the parent. This lets a child read sensitive data and return a cleaned result without exposing the original to the parent, when the policy permits.

Connect three additional events:

- **\`child_start\`** links the child to its approved delegating call. Preserve the returned \`spawn_binding\`; apply any context APPA supplies and stop launch on \`refuse\`.
- **\`child_end\`** submits the child's proposed answer before it leaves the child. Forward an admitted replacement instead of the original; withhold a blocked answer.
- **\`spawn_result\`** checks delivery into the parent's context. Preserve the delegating \`call_id\` and child identity, and deliver only the result APPA admits.

The adapter identifies which tools launch children. Keep each child's identity separate and route its tool calls through APPA too. Do not copy the parent's transcript into a child outside the checked path.

See [Subagent returns](/contracts#subagent-returns) for return requirements and sanitization. If your agent never delegates, these events are not needed.`],
        ["Runtime storage and operations", `The served runtime stores its trajectory event log in SQLite. Keep the database on durable storage if work must resume after a restart.

Use \`GET /health\` for liveness. The local management endpoints include \`GET /status\` for runtime status and \`POST /reload\` to reload policy from disk. These management endpoints accept only loopback requests; they are not remote administration APIs.

See [Observability · v2](/observability-v2) for current diagnostics and planned telemetry.`],
      ].map(([title, content]) => (
        <details key={title} className="px-4 py-3">
          <summary className="cursor-pointer font-semibold text-[var(--text-strong)]">{title}</summary>
          <Markdown content={content} />
        </details>
      ))}
    </div>
  ),
  "sponsor-note": () => <SponsorNote />,
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
          const render = match ? DIRECTIVES[match[1]] : undefined;
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
