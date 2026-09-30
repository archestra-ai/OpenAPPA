import type { ReactNode } from "react";


/* The one-sentence version of a comparison page, shown under its title. The
   sentence comes from the page's `oversimplified` front matter. A fieldset
   because its legend is the one element browsers draw across a border. A page
   may add a figure under the sentence, such as the benchmark chart. */
export function Oversimplified({ text, children }: { text: string; children?: ReactNode }) {
  return (
    <fieldset className="oversimplified">
      <legend className="oversimplified-label">TLDR</legend>
      {text}
      {children}
    </fieldset>
  );
}
