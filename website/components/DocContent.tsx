import { Fragment, type AnchorHTMLAttributes, type HTMLAttributes, type ReactNode } from "react";
import ReactMarkdown from "react-markdown";
import rehypeHighlight from "rehype-highlight";
import rehypeSlug from "rehype-slug";
import remarkGfm from "remark-gfm";

import { CodeBlock } from "@/components/CodeBlock";
import { ConnectedAgentFigure } from "@/components/figures/ConnectedAgentFigure";
import { ExfiltrationFigure } from "@/components/figures/ExfiltrationFigure";
import { GuardrailFigure } from "@/components/figures/GuardrailFigure";
import { NegotiationFigure } from "@/components/figures/NegotiationFigure";
import { LogoGallery } from "@/components/LogoGallery";

/* Block directives: a line of the form :::name::: in the markdown renders
   the mapped component in place. */
const DIRECTIVES: Record<string, () => ReactNode> = {
  "logo-gallery": () => <LogoGallery />,
  "fig-connected-agent": () => <ConnectedAgentFigure />,
  "fig-exfiltration": () => <ExfiltrationFigure />,
  "fig-guardrail": () => <GuardrailFigure />,
  "fig-negotiation": () => <NegotiationFigure />,
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
