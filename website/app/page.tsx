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
            <Logo height={36} />
          </h1>
          <p className="tagline">{doc.description}</p>
        </div>
        <DocContent content={doc.content} />
      </div>
    </DocShell>
  );
}
