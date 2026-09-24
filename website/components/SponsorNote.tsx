import { PixelMark } from "@/components/Logo";

/* Rendered by the :::sponsor-note::: directive at the top of the Archestra
   page. One panel, one message: who pays for OpenAPPA, and that this changes
   nothing about who may ship it. */
export function SponsorNote() {
  return (
    <aside className="sponsor-note">
      <PixelMark className="sponsor-note-mark" size={56} />
      <p className="sponsor-note-text">
        The development of OpenAPPA is sponsored by Archestra. We intentionally made OpenAPPA
        vendor-agnostic: the engine, the batteries, and this website belong to no single product. If
        you are looking to ship OpenAPPA as part of your product, don&apos;t hesitate to{" "}
        <a href="https://github.com/archestra-ai/openappa/tree/main/website/content/docs">
          contribute a page to this website&apos;s repository
        </a>
        , and your product will be listed here 👋. Let&apos;s promote deterministic guardrails
        together!
      </p>
    </aside>
  );
}
