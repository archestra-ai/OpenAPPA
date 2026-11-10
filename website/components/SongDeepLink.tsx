"use client";

import { useEffect } from "react";

import { playSongFromLink } from "@/lib/song";
import { takeTrinketParams } from "@/lib/trinket";

/* `/nfc` (app/(site)/nfc) starts the song on arrival, and with it the ship
   video (SongShip follows playback). lib/song says what happens when the
   browser refuses the sound. */
const PATH = "/nfc";

export function SongDeepLink() {
  useEffect(() => {
    if (window.location.pathname !== PATH) return;
    // Put the address back to `/` so a reload or a link copied from the
    // address bar does not start the song again. With the path changed, a
    // remount (Strict Mode runs every effect twice) finds nothing to do.
    // The trinket's parameters leave the address here too (lib/trinket). This
    // effect runs before the root layout's pageview effect, so the pageview
    // URL never carries them.
    const search = takeTrinketParams(window.location.search);
    window.history.replaceState(window.history.state, "", "/" + search + window.location.hash);
    void playSongFromLink();
  }, []);

  return null;
}
