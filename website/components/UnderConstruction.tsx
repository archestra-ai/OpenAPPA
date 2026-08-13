import Link from "next/link";

import { PixelMark } from "@/components/Logo";

/* Filler for doc pages whose markdown body is still empty: hazard tape,
   one mascot on the job, and a pointer at the pages that do exist. Rendered
   automatically by the doc page when a body has no content, so it retires
   itself the moment real content lands. */

export function UnderConstruction() {
  return (
    <div className="under-construction">
      <div className="uc-stripes" aria-hidden />
      <PixelMark size={84} />
      <div className="uc-label">Under construction</div>
      <p className="uc-text">
        There is nothing here yet — this page is being written, and the mascot is on it. In the meantime,{" "}
        <Link href="/how-it-works">How it works</Link> is the best place to start.
      </p>
      <div className="uc-stripes" aria-hidden />
    </div>
  );
}
