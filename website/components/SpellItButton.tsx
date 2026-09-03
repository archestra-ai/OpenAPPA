"use client";

import { useEffect, useRef, useState } from "react";

/* A recording, not speech synthesis: APPA is said as a word, and every
   respelling that made a system voice land on it ("Ahpa", "Ahp-pah",
   "Op-pa") traded one part of the sound for another. The file settles it,
   and it sounds the same on every machine. */
const AUDIO_SRC = "/brand/openappa-pronunciation.mp3";

/* The play triangle, drawn on the same pixel grid as the wordmark: an 8-cell
   column of 3px steps rather than a smooth hypotenuse. Used twice — as the
   glyph in the button, and (as a data URI, in globals.css) as the cursor over
   it. */
const TRIANGLE =
  "M6 0 L9 0 L9 3 L12 3 L12 6 L15 6 L15 9 L18 9 L18 15 L15 15 L15 18 L12 18 L12 21 L9 21 L9 24 L6 24 Z";

export function SpellItButton() {
  const audioRef = useRef<HTMLAudioElement | null>(null);
  const [playing, setPlaying] = useState(false);

  useEffect(() => {
    // A clip left running survives a client-side navigation away from the
    // page, so stop it on unmount.
    const audio = audioRef.current;
    return () => audio?.pause();
  }, []);

  function toggle() {
    const audio = audioRef.current;
    if (!audio) return;
    if (playing) {
      audio.pause();
      audio.currentTime = 0;
      setPlaying(false);
      return;
    }
    // Rewind first: a second click after the clip ended would otherwise
    // resume from the end and play nothing.
    audio.currentTime = 0;
    setPlaying(true);
    // Nothing to recover from — a rejected play() (no codec, blocked media)
    // just means the button goes back to idle.
    audio.play().catch(() => setPlaying(false));
  }

  return (
    <button
      type="button"
      className="spell-it"
      onClick={toggle}
      aria-label={playing ? "Stop saying OpenAPPA" : "Hear how OpenAPPA is pronounced"}
      data-speaking={playing || undefined}
    >
      <svg
        viewBox="0 0 24 24"
        width="9"
        height="9"
        aria-hidden="true"
        shapeRendering="crispEdges"
        className="spell-it-glyph"
      >
        {playing ? (
          <>
            <rect x="6" y="3" width="4" height="18" fill="currentColor" />
            <rect x="14" y="3" width="4" height="18" fill="currentColor" />
          </>
        ) : (
          <path d={TRIANGLE} fill="currentColor" />
        )}
      </svg>
      <span className="spell-it-say">How to spell &ldquo;OpenAPPA&rdquo;</span>
      {/* Metadata only: a 34KB clip nobody clicks should not cost every
          visitor a download on the landing page. */}
      <audio
        ref={audioRef}
        src={AUDIO_SRC}
        preload="metadata"
        onEnded={() => setPlaying(false)}
        onPause={() => setPlaying(false)}
      />
    </button>
  );
}
