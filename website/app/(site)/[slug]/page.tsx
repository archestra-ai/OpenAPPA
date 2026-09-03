import type { Metadata } from "next";
import Link from "next/link";
import { notFound, redirect } from "next/navigation";

import { DocContent } from "@/components/DocContent";
import { DocShell } from "@/components/DocShell";
import { UnderConstruction } from "@/components/UnderConstruction";
import { generateTableOfContents, getAllDocs, getDocBySlug } from "@/lib/docs";

interface Props {
  params: Promise<{ slug: string }>;
}

export function generateStaticParams() {
  return getAllDocs()
    .filter((doc) => doc.slug !== "index")
    .map((doc) => ({ slug: doc.slug }));
}

export async function generateMetadata({ params }: Props): Promise<Metadata> {
  const { slug } = await params;
  const doc = getDocBySlug(slug);
  if (!doc) return {};
  return { title: doc.title, description: doc.description };
}

export default async function DocPage({ params }: Props) {
  const { slug } = await params;
  if (slug === "index") redirect("/");

  const doc = getDocBySlug(slug);
  if (!doc) notFound();

  const toc = generateTableOfContents(doc.content);

  return (
    <DocShell toc={toc}>
      {doc.breadcrumb && (
        <nav className="doc-breadcrumb" aria-label="Breadcrumb">
          <Link href="/available-batteries">Available batteries</Link>
          <span aria-hidden="true">/</span>
          <span aria-current="page">{doc.breadcrumb}</span>
        </nav>
      )}
      <div className="prose">
        <h1>{doc.title}</h1>
      </div>
      {doc.description && <p className="doc-description">{doc.description}</p>}
      {doc.content.trim() === "" ? <UnderConstruction /> : <DocContent content={doc.content} />}
    </DocShell>
  );
}
