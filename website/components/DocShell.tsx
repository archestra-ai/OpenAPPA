import { DocsSidebar } from "@/components/DocsSidebar";
import { TableOfContents } from "@/components/TableOfContents";
import { getDocsByCategory, type TocItem } from "@/lib/docs";

export function DocShell({ toc, children }: { toc: TocItem[]; children: React.ReactNode }) {
  const categories = getDocsByCategory().map((category) => ({
    name: category.name,
    docs: category.docs.map(({ slug, title }) => ({ slug, title })),
  }));

  return (
    <div className="shell">
      <DocsSidebar categories={categories} />
      <main className="doc-main">{children}</main>
      <TableOfContents items={toc} />
    </div>
  );
}
