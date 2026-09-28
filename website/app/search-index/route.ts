import { buildSearchIndex } from "@/lib/search-index";

/* The docs search index as JSON, fetched by the search modal the first time
   it opens. Built from the content/docs *.md files on every request, like
   /llms.txt, so the dev server reflects edits without a restart; the
   Cache-Control header keeps repeat opens off the server in production. */

export const dynamic = "force-dynamic";

export async function GET() {
  return Response.json(buildSearchIndex(), {
    headers: { "Cache-Control": "public, max-age=300, stale-while-revalidate=3600" },
  });
}
