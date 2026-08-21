/* A proposal is material that is not true of the current model yet. It is
   authored as a fenced container so it reads as plain markdown outside the
   site, and so no part of it can be mistaken for reference:

     :::proposal
     name: Resolver interface
     date: 2026-08-21
     author: Innokentii Konstantinov

     Body markdown…
     :::

   A key/value header runs to the first blank line; the body is ordinary
   markdown until the closing fence. */

export interface Proposal {
  name: string;
  date: string;
  author: string;
  body: string;
}

/* A proposal container spans several lines, so it cannot collide with the
   single-line :::directive::: form. */
export const PROPOSAL_SPLIT = /^:::proposal[ \t]*\n([\s\S]*?)\n:::[ \t]*$/m;

export const PROPOSAL_OPEN = /^:::proposal[ \t]*$/;
export const PROPOSAL_CLOSE = /^:::[ \t]*$/;

/* The id comes from the name alone, never from the status. A proposal that is
   accepted becomes an ordinary heading with the same slug, so links into it
   survive the promotion. */
export function proposalSlug(name: string): string {
  return name
    .toLowerCase()
    .trim()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "");
}

/* A missing field degrades to an omitted line, never to a broken page. */
export function parseProposal(source: string): Proposal {
  const separator = source.indexOf("\n\n");
  const header = separator === -1 ? source : source.slice(0, separator);
  const body = separator === -1 ? "" : source.slice(separator + 2);

  const fields = new Map<string, string>();
  for (const line of header.split("\n")) {
    const field = /^([a-z]+)\s*:\s*(.*)$/.exec(line.trim());
    if (field) fields.set(field[1], field[2].trim());
  }

  return {
    name: fields.get("name") ?? "Proposal",
    date: fields.get("date") ?? "",
    author: fields.get("author") ?? "",
    body,
  };
}
