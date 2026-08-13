import { DocsSidebar } from "@/components/DocsSidebar";
import { TableOfContents } from "@/components/TableOfContents";
import { DocPagination } from "@/components/DocPagination";
import { getAllDocs, getDocsByCategory, type TocItem } from "@/lib/docs";

export function DocShell({ toc, children }: { toc: TocItem[]; children: React.ReactNode }) {
  const allDocs = getAllDocs().map(({ slug, title }) => ({ slug, title }));
  const categories = getDocsByCategory().map((category) => ({
    name: category.name,
    docs: category.docs.map(({ slug, title }) => ({ slug, title })),
  }));

  return (
    <div className="shell">
      <DocsSidebar categories={categories} toc={toc} />
      <main className="doc-main">
        {children}
        <DocPagination docs={allDocs} />
      </main>
      <TableOfContents items={toc} />
    </div>
  );
}
