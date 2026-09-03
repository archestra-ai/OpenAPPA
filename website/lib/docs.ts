import fs from "fs";
import path from "path";

import GithubSlugger from "github-slugger";
import matter from "gray-matter";

import { PROPOSAL_CLOSE, PROPOSAL_OPEN, proposalSlug } from "@/lib/proposals";

const DOCS_DIR = path.join(process.cwd(), "content", "docs");

export interface DocFrontMatter {
  title: string;
  category: string;
  order?: number;
  description?: string;
  sidebar?: boolean;
  breadcrumb?: string;
}

export interface DocPage {
  slug: string;
  title: string;
  category: string;
  order: number;
  description: string;
  content: string;
  proposal: boolean;
  sidebar: boolean;
  breadcrumb?: string;
}

export interface TocItem {
  id: string;
  text: string;
  level: 2 | 3;
  proposal?: true;
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
      proposal: PROPOSAL_OPEN.test(content.trimStart().split("\n", 1)[0]),
      sidebar: fm.sidebar ?? true,
      breadcrumb: fm.breadcrumb,
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
    if (!doc.sidebar) continue;
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
  let inProposal = false;
  let inHeader = false;
  for (const line of content.split("\n")) {
    if (line.trimStart().startsWith("```")) {
      inCodeBlock = !inCodeBlock;
      continue;
    }
    if (inCodeBlock) continue;

    /* A proposal contributes its own name and nothing else: the headings
       inside it structure the proposal, not the page. */
    if (inProposal) {
      if (PROPOSAL_CLOSE.test(line)) inProposal = false;
      else if (line.trim() === "") inHeader = false;
      else if (inHeader) {
        const name = /^name\s*:\s*(.+)$/.exec(line.trim());
        if (name) {
          items.push({ id: proposalSlug(name[1]), text: name[1].trim(), level: 3, proposal: true });
        }
      }
      continue;
    }
    if (PROPOSAL_OPEN.test(line)) {
      inProposal = true;
      inHeader = true;
      continue;
    }

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
