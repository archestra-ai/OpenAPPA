/* The repository is not public yet, so this is deliberately not a link: a
   <span> cannot be clicked, focused, or opened in a new tab, which is the
   honest rendering of "Soon". When the repo goes public, restore the anchor
   here — https://github.com/archestra-ai/OpenAPPA — and both the header and
   the drawer follow. */
export function GitHubSoon() {
  return (
    <span className="gh-link" aria-disabled="true" title="The repository is not public yet">
      GitHub
      <span className="ribbon">Soon</span>
    </span>
  );
}
