"use client";

import { useEffect } from "react";

import { playSong } from "@/lib/song";

/* `?song` on any site URL starts the song, and with it the ship video
   (SongShip follows playback). Browsers refuse sound before the visitor has
   interacted with the page, so when the play with sound is refused the song
   starts muted: the video plays at once and lib/song brings the sound in on
   the visitor's first click or key press. */
const PARAM = "song";

export function SongDeepLink() {
  useEffect(() => {
    const url = new URL(window.location.href);
    if (!url.searchParams.has(PARAM)) return;
    // Drop the parameter so a reload or a shared copy of this URL from the
    // address bar does not start the song again. With it gone, a remount
    // (Strict Mode runs every effect twice) finds nothing to do.
    url.searchParams.delete(PARAM);
    window.history.replaceState(window.history.state, "", url);
    void playSong({ fromLink: true }).then((played) => {
      if (!played) void playSong({ muted: true, fromLink: true });
    });
  }, []);

  return null;
}
