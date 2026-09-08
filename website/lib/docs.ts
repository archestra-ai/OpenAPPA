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

export function isDevMode(): boolean {
  return process.env.NODE_ENV !== "production" || process.env.NEXT_PUBLIC_DEV_MODE === "true";
}

export function substituteKagentDevSnippets(content: string): string {
  let result = content;

  // 1. Controller agentImage: remote registry -> local build Never
  result = result.replaceAll(
    `  --set controller.agentImage.registry=europe-west1-docker.pkg.dev \\
  --set controller.agentImage.repository=friendly-path-465518-r6/appa-public/appa-kagent-adk \\
  --set-string controller.agentImage.tag="v$APPA_VERSION"`,
    `  --set controller.agentImage.registry=docker.io \\
  --set controller.agentImage.repository=library/appa-kagent-adk \\
  --set-string controller.agentImage.tag=dev \\
  --set controller.agentImage.pullPolicy=Never`
  );

  // 2. appa-kagent-demo: OCI pull -> local chart path with local image overrides
  result = result.replace(
    `helm upgrade --install appa-kagent-demo \\
  oci://europe-west1-docker.pkg.dev/friendly-path-465518-r6/appa-public/charts/appa-kagent-demo \\
  --version "$APPA_VERSION" -n "$KAGENT_NAMESPACE" \\
  --set-string runtime.url="http://appa-runtime.$KAGENT_NAMESPACE.svc.cluster.local:18787" \\
  --set-string modelConfig.name=default-model-config \\
  --set-string runtime.reasoningEffort=none \\
  --force-conflicts --wait --timeout 10m`,
    `helm upgrade --install appa-kagent-demo \\
  ./integrations/kagent/demo/chart \\
  -n "$KAGENT_NAMESPACE" \\
  --set-string runtime.url="http://appa-runtime.$KAGENT_NAMESPACE.svc.cluster.local:18787" \\
  --set-string modelConfig.name=default-model-config \\
  --set-string runtime.reasoningEffort=none \\
  --set tools.image.repository=docker.io/library/appa-demo-tools \\
  --set-string tools.image.tag=dev \\
  --set tools.image.pullPolicy=Never \\
  --set mocks.image.repository=docker.io/library/appa-demo-mocks \\
  --set-string mocks.image.tag=dev \\
  --set mocks.image.pullPolicy=Never \\
  --force-conflicts --wait --timeout 10m`
  );

  // 3. appa-runtime quickstart: OCI pull -> local chart path with local image override
  result = result.replace(
    `helm upgrade --install appa-runtime \\
  oci://europe-west1-docker.pkg.dev/friendly-path-465518-r6/appa-public/charts/appa-runtime \\
  --version "$APPA_VERSION" -n "$KAGENT_NAMESPACE" \\
  --set persistence.enabled=false \\
  --set config.existingConfigMap=appa-kagent-demo-policy \\
  --force-conflicts --wait --timeout 10m`,
    `helm upgrade --install appa-runtime \\
  ./charts/appa-runtime \\
  -n "$KAGENT_NAMESPACE" \\
  --set persistence.enabled=false \\
  --set image.repository=docker.io/library/appa-runtime \\
  --set-string image.tag=dev \\
  --set image.pullPolicy=Never \\
  --set config.existingConfigMap=appa-kagent-demo-policy \\
  --force-conflicts --wait --timeout 10m`
  );

  // 4. appa-runtime existing agents: OCI pull -> local chart path
  result = result.replace(
    `helm upgrade --install appa-runtime \\
  oci://europe-west1-docker.pkg.dev/friendly-path-465518-r6/appa-public/charts/appa-runtime \\
  --version "$APPA_VERSION" -n "$RUNTIME_NAMESPACE" --create-namespace \\`,
    `helm upgrade --install appa-runtime \\
  ./charts/appa-runtime \\
  -n "$RUNTIME_NAMESPACE" --create-namespace \\
  --set image.repository=docker.io/library/appa-runtime \\
  --set-string image.tag=dev \\
  --set image.pullPolicy=Never \\`
  );

  // 5. Add local build commands before helm commands in quickstart
  result = result.replace(
    `kubectl config current-context\n`,
    `# Build images and load into kind
docker build -t appa-kagent-adk:dev integrations/kagent/appa-kagent-adk
docker build -t appa-demo-tools:dev integrations/kagent/demo
docker build -t appa-demo-mocks:dev integrations/kagent/demo/mocks
docker build -f appa-runtime/Dockerfile -t appa-runtime:dev .
kind load docker-image appa-kagent-adk:dev appa-demo-tools:dev appa-demo-mocks:dev appa-runtime:dev

kubectl config current-context
`
  );

  return result;
}

export function getAllDocs(): DocPage[] {
  const files = fs.readdirSync(DOCS_DIR).filter((f) => f.endsWith(".md"));
  const docs = files.map((file) => {
    const raw = fs.readFileSync(path.join(DOCS_DIR, file), "utf-8");
    const { data, content } = matter(raw);
    const fm = data as DocFrontMatter;
    // Strip release-please markers so public docs and clipboard copies stay clean,
    // while git source files retain the markers for the release pipeline.
    let cleanContent = content.replace(/[ \t]*#\s*x-release-please-[a-z0-9_-]+/g, "");
    if (file === "kagent.md" && isDevMode()) {
      cleanContent = substituteKagentDevSnippets(cleanContent);
    }
    return {
      slug: file.replace(/\.md$/, ""),
      title: fm.title,
      category: fm.category,
      order: fm.order ?? 999,
      description: fm.description ?? "",
      content: cleanContent,
      proposal: PROPOSAL_OPEN.test(cleanContent.trimStart().split("\n", 1)[0]),
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
