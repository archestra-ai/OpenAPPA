import { SPONSOR_NOTE } from "@/lib/directive-content";

/* Rendered by the :::sponsor-note::: directive at the top of the Archestra
   page. One panel, one message: who pays for OpenAPPA, and that this changes
   nothing about who may ship it. */
export function SponsorNote() {
  return (
    <aside className="sponsor-note">
      <p className="sponsor-note-text">
        {SPONSOR_NOTE.lead} <a href={SPONSOR_NOTE.link.href}>{SPONSOR_NOTE.link.label}</a>
        {SPONSOR_NOTE.rest}
      </p>
    </aside>
  );
}
