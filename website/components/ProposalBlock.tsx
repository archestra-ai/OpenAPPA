import type { ReactNode } from "react";

import { type Proposal, proposalSlug } from "@/lib/proposals";

/* A proposal is marked for its full length by the rail, so a reader arriving
   from a deep link cannot mistake it for current reference material. The
   header itself stays deliberately small: proposals can be long, but their
   chrome should not compete with their contents. */

/* An ISO date parsed by Date() is UTC midnight, which renders as the previous
   day west of Greenwich; the locale and time zone are pinned so the server and
   the client agree. */
function formatDate(date: string): string {
  const parts = /^(\d{4})-(\d{2})-(\d{2})$/.exec(date);
  if (!parts) return date;
  const [, year, month, day] = parts;
  return new Date(Date.UTC(Number(year), Number(month) - 1, Number(day))).toLocaleDateString(
    "en-US",
    { year: "numeric", month: "long", day: "numeric", timeZone: "UTC" },
  );
}

export function ProposalBlock({ proposal, children }: { proposal: Proposal; children: ReactNode }) {
  const { name, date, author } = proposal;
  const id = proposalSlug(name);
  return (
    <aside className="proposal" id={id} aria-label={`${name} proposal`}>
      <header className="proposal-header">
        <span className="proposal-badge">Proposal</span>
        {(author || date) && (
          <p className="proposal-meta">
            {author && <span>{author}</span>}
            {author && date && <span aria-hidden="true">·</span>}
            {date && <time dateTime={date}>{formatDate(date)}</time>}
          </p>
        )}
      </header>
      <div className="proposal-body">{children}</div>
    </aside>
  );
}
