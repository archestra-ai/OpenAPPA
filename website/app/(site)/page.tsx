import { notFound } from "next/navigation";

import { DocContent } from "@/components/DocContent";
import { DocShell } from "@/components/DocShell";
import { SpellItButton } from "@/components/SpellItButton";
import { generateTableOfContents, getDocBySlug } from "@/lib/docs";

export default function HomePage() {
  const doc = getDocBySlug("index");
  if (!doc) notFound();

  const toc = generateTableOfContents(doc.content);

  return (
    <DocShell toc={toc}>
      <div className="landing">
        <div className="hero">
          {/* Title as text, not the lockup: the header already carries the
              wordmark, and two of them stacked read as a duplicate. */}
          <h1>{doc.title}</h1>
          <p className="tagline">{doc.description}</p>
          <SpellItButton />
        </div>
        <DocContent content={doc.content} />
      </div>
    </DocShell>
  );
}
