/* The one-sentence version of a comparison page, shown under its title. The
   sentence comes from the page's `oversimplified` front matter. A fieldset
   because its legend is the one element browsers draw across a border. */
export function Oversimplified({ text }: { text: string }) {
  return (
    <fieldset className="oversimplified">
      <legend className="oversimplified-label">TLDR</legend>
      {text}
    </fieldset>
  );
}
