"use client";

import { useEffect, useRef, useState } from "react";

import { playSong, stopSong, subscribeSong } from "@/lib/song";

/* How long the "are you sure?" label waits for the second click before the
   button goes back to idle. Long enough to read the question, short enough
   that a stale confirm is never one stray click from a song. */
const CONFIRM_TIMEOUT_MS = 5000;

export function SpellItButton() {
  const confirmTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const [playing, setPlaying] = useState(false);
  const [confirming, setConfirming] = useState(false);
  // True from the confirming click until the browser starts playback: the
  // clip is fetched on first play, so the button says so and takes no clicks.
  const [loading, setLoading] = useState(false);

  // The song lives in lib/song and keeps playing when this page is left; a
  // button mounted while it plays shows the stop state straight away.
  useEffect(() => subscribeSong((audio) => setPlaying(audio !== null)), []);

  useEffect(() => {
    return () => {
      if (confirmTimer.current) clearTimeout(confirmTimer.current);
    };
  }, []);

  function clearConfirm() {
    if (confirmTimer.current) {
      clearTimeout(confirmTimer.current);
      confirmTimer.current = null;
    }
    setConfirming(false);
  }

  async function toggle() {
    if (loading) return;
    if (playing) {
      stopSong();
      return;
    }
    // First click only arms the button: the song is loud and long enough that
    // an accidental click should not start it.
    if (!confirming) {
      setConfirming(true);
      confirmTimer.current = setTimeout(() => {
        confirmTimer.current = null;
        setConfirming(false);
      }, CONFIRM_TIMEOUT_MS);
      return;
    }
    clearConfirm();
    setLoading(true);
    try {
      await playSong();
    } finally {
      setLoading(false);
    }
  }

  const label = loading
    ? "Loading the OpenAPPA song"
    : playing
      ? "Stop singing OpenAPPA"
      : confirming
        ? "Confirm: play the OpenAPPA song"
        : "Hear how OpenAPPA is sung";

  return (
    <button
      type="button"
      className="spell-it"
      onClick={() => void toggle()}
      onBlur={clearConfirm}
      disabled={loading}
      aria-label={label}
      aria-busy={loading || undefined}
      data-speaking={playing || undefined}
      data-confirming={confirming || undefined}
      data-loading={loading || undefined}
    >
      <svg
        viewBox="0 0 24 24"
        width="24"
        height="24"
        aria-hidden="true"
        className="spell-it-glyph"
      >
        <circle cx="12" cy="12" r="10.5" fill="none" stroke="currentColor" strokeWidth="1.5" />
        {playing ? (
          <>
            <rect x="8" y="8" width="3" height="8" fill="currentColor" />
            <rect x="13" y="8" width="3" height="8" fill="currentColor" />
          </>
        ) : (
          <path d="M10 7.5 L17 12 L10 16.5 Z" fill="currentColor" />
        )}
      </svg>
      <span className="spell-it-say">
        {loading
          ? "Loading..."
          : confirming
            ? "The song will play, are you sure?"
            : "How to sing “OpenAPPA”"}
      </span>
    </button>
  );
}
