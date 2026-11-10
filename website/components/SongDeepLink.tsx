"use client";

import { useEffect, useState } from "react";

import { playSong } from "@/lib/song";

/* `?song` on any site URL starts the song, and with it the ship video
   (SongShip follows playback). Browsers refuse sound before the visitor
   has interacted with the page, so when the play is refused the link shows
   one button in its place: the link was the visitor asking for the song,
   so that click plays it without the hero button's confirm step. */
const PARAM = "song";

export function SongDeepLink() {
  const [blocked, setBlocked] = useState(false);
  const [loading, setLoading] = useState(false);

  useEffect(() => {
    const url = new URL(window.location.href);
    if (!url.searchParams.has(PARAM)) return;
    // Drop the parameter so a reload or a shared copy of this URL from the
    // address bar does not start the song again.
    url.searchParams.delete(PARAM);
    window.history.replaceState(window.history.state, "", url);
    let cancelled = false;
    void playSong().then((played) => {
      if (!cancelled && !played) setBlocked(true);
    });
    return () => {
      cancelled = true;
    };
  }, []);

  if (!blocked) return null;

  async function play() {
    setLoading(true);
    try {
      if (await playSong()) setBlocked(false);
    } finally {
      setLoading(false);
    }
  }

  return (
    <div className="song-deep-link" role="dialog" aria-label="Play the OpenAPPA song">
      <button
        type="button"
        className="song-deep-link-play"
        onClick={() => void play()}
        disabled={loading}
        aria-busy={loading || undefined}
        autoFocus
      >
        <svg viewBox="0 0 24 24" width="40" height="40" aria-hidden="true">
          <circle cx="12" cy="12" r="10.5" fill="none" stroke="currentColor" strokeWidth="1.5" />
          <path d="M10 7.5 L17 12 L10 16.5 Z" fill="currentColor" />
        </svg>
        <span>{loading ? "Loading..." : "Play “Check the Flow”"}</span>
      </button>
      <button type="button" className="song-deep-link-dismiss" onClick={() => setBlocked(false)}>
        Not now
      </button>
    </div>
  );
}
