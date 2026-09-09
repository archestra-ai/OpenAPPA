# Linear vertical slice

These root configs combine the complete Linear battery with the existing public
GitHub battery. Choose one of `approved-writes.toml`, `read-only.toml`,
`team-use.toml`, or `production-lockdown.toml`. They use fictional ENG-1 and Alice
mappings, no credentials, and the existing human review backend. Replace those
mappings with verified resource ACLs before connecting real tools.

The comment rule is explicitly enabled for production; the issue-edit rule is
not. This makes the distinction reviewable without enabling every mutation.
The membership-source example is in the battery README; these configs use
literal readers so configuration checks do not need provider network access.

The same policy applies through either host's server mapping. Installing a
battery selects its default profile; use source includes for alternate profiles.
Do not include multiple profiles. Run from a checkout so relative includes
resolve, or package the complete deployment with `appa bundle` for relocation.

Expected sequence with fixture outcomes:

1. `get_issue {"id":"ENG-1"}` offers the audience/trust change. Accepting it
   allows the read and narrows the trajectory to Alice with suspicious content.
2. A GitHub `issue_write` to a public repository is refused. Linear's operator
   authority cannot expand the audience, so reviewing a Linear edit cannot
   authorize this disclosure.
3. `save_comment {"issueId":"ENG-1","body":"reviewed text"}` offers human
   review. Rejection runs nothing; approval authorizes the exact call once.
4. Changing the issue, adding related-content flags, or changing reviewed text
   invalidates that authorization. A successful comment emits `linear.changed`.
5. In production lockdown, `save_issue` remains refused until its matching
   resource rule explicitly sets `production: true`.

A sanitizer is a separate deployment decision. The existing `redact-email`
backend can remove email addresses; it cannot declassify arbitrary issue text
or establish that private business information is safe to publish. Do not add a
blanket private-to-public sanitizer to make the cross-battery refusal disappear.
