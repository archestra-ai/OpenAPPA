import { SONG_BAR_INTENSITY, SONG_BEATS_MS, SONG_BEATS_PER_BAR } from "@/lib/song-beats";

/* The song button owns the <audio>; the header mascot, in another tree,
   dances to it. This is the one line between them: the button publishes the
   playing element, the mascot subscribes. Module state rather than context
   because the two never share a parent below the root layout. */

type Listener = (audio: HTMLAudioElement | null) => void;

let playing: HTMLAudioElement | null = null;
const listeners = new Set<Listener>();

export function publishSong(audio: HTMLAudioElement | null) {
  if (audio === playing) return;
  playing = audio;
  for (const listener of listeners) listener(audio);
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
