import { notFound } from "next/navigation";

import { DocContent } from "@/components/DocContent";
import { Logo } from "@/components/Logo";
import { DocShell } from "@/components/DocShell";
import { generateTableOfContents, getDocBySlug } from "@/lib/docs";

export default function HomePage() {
  const doc = getDocBySlug("index");
  if (!doc) notFound();

  const toc = generateTableOfContents(doc.content);

  return (
    <DocShell toc={toc}>
      <div className="landing">
        <div className="hero">
          <h1>
            {/* Fluid: the lockup shrinks to fit a phone instead of wrapping
                the mascot onto a line of its own. 36px is the design size. */}
            <Logo height="clamp(15px, calc((100vw - 64px) / 15), 36px)" />
          </h1>
          <p className="tagline">{doc.description}</p>
        </div>
        <DocContent content={doc.content} />
      </div>
    </DocShell>
  );
}
