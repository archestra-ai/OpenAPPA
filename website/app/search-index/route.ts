import { buildSearchIndex } from "@/lib/search-index";

/* The docs search index as JSON, fetched by the search modal the first time
   it opens. Rendered at build from the content/docs *.md files, like
   /llms.txt, so every deploy ships an index of exactly the docs it deploys and
   a doc the text renderers refuse fails the build; the dev server still runs
   it on every request. */

export const dynamic = "force-static";

export async function GET() {
  return Response.json(buildSearchIndex());
}
