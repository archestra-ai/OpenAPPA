import { SONG_BAR_INTENSITY, SONG_BEATS_MS, SONG_BEATS_PER_BAR } from "@/lib/song-beats";

/* A recording, not speech synthesis: APPA is said as a word, and every
   respelling that made a system voice land on it ("Ahpa", "Ahp-pah",
   "Op-pa") traded one part of the sound for another. The song settles it,
   and it sounds the same on every machine. */
const SONG_SRC = "/brand/openappa-check-the-flow.mp3";

/* The song outlives the page it was started from: the element lives here,
   in module state, not in the button that starts it, so a navigation away
   from the landing page neither stops the song nor the header mascot dancing
   to it. The button and the mascot both subscribe; nothing else needs to
   share a parent with them. The element is created on the first play, so a
   visitor who never clicks never downloads the clip. */

type Listener = (audio: HTMLAudioElement | null) => void;

let element: HTMLAudioElement | null = null;
let playing: HTMLAudioElement | null = null;
const listeners = new Set<Listener>();

function publish(audio: HTMLAudioElement | null) {
  if (audio === playing) return;
  playing = audio;
  for (const listener of listeners) listener(audio);
}

function audioElement(): HTMLAudioElement {
  if (!element) {
    element = new Audio(SONG_SRC);
    element.preload = "none";
    element.addEventListener("ended", () => publish(null));
    element.addEventListener("pause", () => publish(null));
  }
  return element;
}

/** Starts the song from the top. Resolves false when the browser refused to play. */
export async function playSong(): Promise<boolean> {
  const audio = audioElement();
  audio.currentTime = 0;
  try {
    await audio.play();
  } catch {
    // Nothing to recover from: no codec, or blocked media.
    return false;
  }
  publish(audio);
  return true;
}

export function stopSong() {
  if (!element) return;
  element.pause();
  element.currentTime = 0;
}

/** Calls `listener` with the playing element now and on every change. */
export function subscribeSong(listener: Listener): () => void {
  listeners.add(listener);
  listener(playing);
  return () => {
    listeners.delete(listener);
  };
}

export interface SongPosition {
  /** Index of the beat just passed. */
  beat: number;
  /** 0 at that beat, approaching 1 at the next. */
  phase: number;
  /** Position of the beat in its bar, 0 = downbeat. */
  beatInBar: number;
  /** Loudness of the surrounding bar, 0..1, crossfaded over the bar's last beat. */
  intensity: number;
}

/** Where in the beat grid a playback time falls; null before the first beat. */
export function songPosition(timeSec: number): SongPosition | null {
  const t = timeSec * 1000;
  if (t < SONG_BEATS_MS[0]) return null;
  let lo = 0;
  let hi = SONG_BEATS_MS.length - 1;
  while (lo < hi) {
    const mid = (lo + hi + 1) >> 1;
    if (SONG_BEATS_MS[mid] <= t) lo = mid;
    else hi = mid - 1;
  }
  const beat = lo;
  const start = SONG_BEATS_MS[beat];
  const next = SONG_BEATS_MS[beat + 1] ?? start + (start - SONG_BEATS_MS[beat - 1]);
  const phase = Math.min((t - start) / (next - start), 1);
  const beatInBar = beat % SONG_BEATS_PER_BAR;
  const bar = Math.floor(beat / SONG_BEATS_PER_BAR);
  const here = SONG_BAR_INTENSITY[bar] ?? 0;
  const after = SONG_BAR_INTENSITY[bar + 1] ?? 0;
  const barPhase = (beatInBar + phase) / SONG_BEATS_PER_BAR;
  // Ease into the next bar's level over this bar's last beat so a drop or a
  // chorus entry reads as a change on its downbeat, not a fade across the bar.
  const blend = Math.max(0, (barPhase - 0.75) / 0.25);
  return { beat, phase, beatInBar, intensity: here + (after - here) * blend };
}
