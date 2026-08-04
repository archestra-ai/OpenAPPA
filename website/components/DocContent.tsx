import { Fragment, type AnchorHTMLAttributes, type HTMLAttributes, type ReactNode } from "react";
import ReactMarkdown from "react-markdown";
import rehypeHighlight from "rehype-highlight";
import rehypeSlug from "rehype-slug";
import remarkGfm from "remark-gfm";

import { CodeBlock } from "@/components/CodeBlock";
import { ConnectedAgentFigure } from "@/components/figures/ConnectedAgentFigure";
import { ExfiltrationFigure } from "@/components/figures/ExfiltrationFigure";
import { GuardrailFigure } from "@/components/figures/GuardrailFigure";
import { LabelFoldFigure } from "@/components/figures/LabelFoldFigure";
import { NegotiationFigure } from "@/components/figures/NegotiationFigure";
import { RemedyPlanFigure } from "@/components/figures/RemedyPlanFigure";
import { TwoEndingsFigure } from "@/components/figures/TwoEndingsFigure";
import { LogoGallery } from "@/components/LogoGallery";
import { Term } from "@/components/Term";
import { termDefinition } from "@/lib/terms";

/* Block directives: a line of the form :::name::: in the markdown renders
   the mapped component in place. */
const DIRECTIVES: Record<string, () => ReactNode> = {
  "logo-gallery": () => <LogoGallery />,
  "fig-connected-agent": () => <ConnectedAgentFigure />,
  "fig-exfiltration": () => <ExfiltrationFigure />,
  "fig-guardrail": () => <GuardrailFigure />,
  "fig-label-fold": () => <LabelFoldFigure />,
  "fig-negotiation": () => <NegotiationFigure />,
  "fig-remedy-plan": () => <RemedyPlanFigure />,
  "fig-two-endings": () => <TwoEndingsFigure />,
};

const DIRECTIVE_SPLIT = /^:::([a-z-]+):::$/m;

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

function Markdown({ content }: { content: string }) {
  return (
    <ReactMarkdown
      remarkPlugins={[remarkGfm]}
      rehypePlugins={[rehypeSlug, rehypeHighlight]}
      components={{
        pre: (props) => <CodeBlock {...props} />,
        code: MarkdownCode,
        a: MarkdownLink,
        h2: (props) => <AnchoredHeading level={2} {...props} />,
        h3: (props) => <AnchoredHeading level={3} {...props} />,
        h4: (props) => <AnchoredHeading level={4} {...props} />,
      }}
    >
      {content}
    </ReactMarkdown>
  );
}

export function DocContent({ content }: { content: string }) {
  // split() with a captured group interleaves markdown chunks and directive names
  const parts = content.split(DIRECTIVE_SPLIT);
  return (
    <div className="prose">
      {parts.map((part, index) =>
        index % 2 === 1 ? (
          <Fragment key={index}>{DIRECTIVES[part]?.()}</Fragment>
        ) : (
          <Markdown key={index} content={part} />
        ),
      )}
    </div>
  );
}
