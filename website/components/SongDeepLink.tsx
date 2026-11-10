"use client";

import { useEffect } from "react";

import { playSong } from "@/lib/song";

/* `/nfc` (app/(site)/nfc) starts the song on arrival, and with it the ship
   video (SongShip follows playback). Browsers refuse sound before the
   visitor has interacted with the page, so when the play with sound is
   refused the song starts muted: the video plays at once, the mute button
   on it pulses, and lib/song brings the sound in on the visitor's first
   click or key press. */
const PATH = "/nfc";

export function SongDeepLink() {
  useEffect(() => {
    if (window.location.pathname !== PATH) return;
    // Put the address back to `/` so a reload or a link copied from the
    // address bar does not start the song again. With the path changed, a
    // remount (Strict Mode runs every effect twice) finds nothing to do.
    window.history.replaceState(window.history.state, "", "/" + window.location.search + window.location.hash);
    void playSong({ fromLink: true }).then((played) => {
      if (!played) void playSong({ muted: true, fromLink: true });
    });
  }, []);

  return null;
}
