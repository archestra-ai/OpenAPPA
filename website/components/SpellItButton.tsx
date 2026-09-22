"use client";

import { useEffect, useRef, useState } from "react";

/* A recording, not speech synthesis: APPA is said as a word, and every
   respelling that made a system voice land on it ("Ahpa", "Ahp-pah",
   "Op-pa") traded one part of the sound for another. The song settles it,
   and it sounds the same on every machine. */
const AUDIO_SRC = "/brand/openappa-check-the-flow.mp3";

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
      aria-label={playing ? "Stop singing OpenAPPA" : "Hear how OpenAPPA is sung"}
      data-speaking={playing || undefined}
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
      <span className="spell-it-say">How to sing &ldquo;OpenAPPA&rdquo;</span>
      {/* Metadata only: a ~2.8MB song nobody clicks should not cost every
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
