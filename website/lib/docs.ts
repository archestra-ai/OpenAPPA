import fs from "fs";
import path from "path";

import GithubSlugger from "github-slugger";
import matter from "gray-matter";

const DOCS_DIR = path.join(process.cwd(), "content", "docs");

export interface DocFrontMatter {
  title: string;
  category: string;
  order?: number;
  description?: string;
}

export interface DocPage {
  slug: string;
  title: string;
  category: string;
  order: number;
  description: string;
  content: string;
}

export interface TocItem {
  id: string;
  text: string;
  level: 2 | 3;
}

export interface DocCategory {
  name: string;
  docs: DocPage[];
}

export function getAllDocs(): DocPage[] {
  const files = fs.readdirSync(DOCS_DIR).filter((f) => f.endsWith(".md"));
  const docs = files.map((file) => {
    const raw = fs.readFileSync(path.join(DOCS_DIR, file), "utf-8");
    const { data, content } = matter(raw);
    const fm = data as DocFrontMatter;
    return {
      slug: file.replace(/\.md$/, ""),
      title: fm.title,
      category: fm.category,
      order: fm.order ?? 999,
      description: fm.description ?? "",
      content,
    };
  });
  return docs.sort((a, b) => a.order - b.order || a.title.localeCompare(b.title));
}

export function getDocBySlug(slug: string): DocPage | undefined {
  return getAllDocs().find((doc) => doc.slug === slug);
}

export function getDocsByCategory(): DocCategory[] {
  const categories: DocCategory[] = [];
  for (const doc of getAllDocs()) {
    let category = categories.find((c) => c.name === doc.category);
    if (!category) {
      category = { name: doc.category, docs: [] };
      categories.push(category);
    }
    category.docs.push(doc);
  }
  return categories;
}

export function generateTableOfContents(content: string): TocItem[] {
  const slugger = new GithubSlugger();
  const items: TocItem[] = [];
  let inCodeBlock = false;
  for (const line of content.split("\n")) {
    if (line.trimStart().startsWith("```")) {
      inCodeBlock = !inCodeBlock;
      continue;
    }
    if (inCodeBlock) continue;
    const match = line.match(/^(#{2,3})\s+(.+)$/);
    if (!match) continue;
    const text = match[2].replace(/`/g, "").trim();
    items.push({
      id: slugger.slug(text),
      text,
      level: match[1].length as 2 | 3,
    });
  }
  return items;
}
