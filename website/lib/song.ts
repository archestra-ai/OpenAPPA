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

/** What the video and the dancing mascot follow: the song's clock. */
export interface SongPlayback {
  readonly currentTime: number;
  readonly duration: number;
}

/** The song's length, until the browser reports the clip's. */
export const SONG_SECONDS = 126;

type Listener = (playback: SongPlayback | null) => void;
type MutedListener = (muted: boolean | null) => void;

let element: HTMLAudioElement | null = null;
let playing: SongPlayback | null = null;
const listeners = new Set<Listener>();
/* Whether a start from the `/nfc` link is muted; null for any other start,
   which has no mute button. */
let linkMuted: boolean | null = null;
const mutedListeners = new Set<MutedListener>();

function publish(playback: SongPlayback | null) {
  if (playback === null) setLinkMuted(null);
  if (playback === playing) return;
  playing = playback;
  for (const listener of listeners) listener(playback);
}

function setLinkMuted(muted: boolean | null) {
  if (muted === linkMuted) return;
  linkMuted = muted;
  for (const listener of mutedListeners) listener(muted);
}

function audioElement(): HTMLAudioElement {
  if (!element) {
    element = new Audio(SONG_SRC);
    element.preload = "none";
    element.addEventListener("ended", () => publish(null));
    element.addEventListener("pause", () => {
      // A pause while the silent clock runs is the clock taking over, not
      // the song ending.
      if (!silent) publish(null);
    });
  }
  return element;
}

/** Starts the song from the top. Resolves false when the browser refused to play. */
export async function playSong(): Promise<boolean> {
  stopSilent();
  const audio = audioElement();
  audio.muted = false;
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

/**
 * Starts the song for a visitor who arrived on the `/nfc` link, with a mute
 * button on the video. Browsers refuse sound before the visitor has
 * interacted with the page, and some refuse even a muted play of media that
 * is not on screen (iOS Safari, or any browser in Low Power Mode). So when
 * the play with sound is refused, the video runs on a silent clock instead
 * of the audio, and the visitor's first tap, click or key press, or the
 * mute button, starts the audio at the clock's time.
 */
export async function playSongFromLink(): Promise<void> {
  if (await playSong()) {
    setLinkMuted(false);
    return;
  }
  startSilent();
}

export function stopSong() {
  if (silent) {
    stopSilent();
    publish(null);
    return;
  }
  if (!element) return;
  element.pause();
  element.currentTime = 0;
}

/** Mutes or unmutes a song started from the `/nfc` link. Call it from a
 * click handler: the unmute that starts the audio needs the gesture. */
export function setSongMuted(muted: boolean) {
  if (linkMuted === null) return;
  if (!muted && silent) {
    startAudioFromSilent();
    return;
  }
  if (!element) return;
  element.muted = muted;
  setLinkMuted(muted);
}

/** Calls `listener` with the playing song now and on every change. */
export function subscribeSong(listener: Listener): () => void {
  listeners.add(listener);
  listener(playing);
  return () => {
    listeners.delete(listener);
  };
}

/** Calls `listener` with whether a `/nfc` start is muted (null for any
 * other start, or none) now and on every change. */
export function subscribeSongMuted(listener: MutedListener): () => void {
  mutedListeners.add(listener);
  listener(linkMuted);
  return () => {
    mutedListeners.delete(listener);
  };
}

/* ---------- the silent clock ---------- */

/* The video's clock while the audio is not allowed to play: wall time since
   the start. It ends with the song, as the audio would. */
let silent: { start: number; end: ReturnType<typeof setTimeout> } | null = null;
let starting = false;

const silentPlayback: SongPlayback = {
  get currentTime() {
    return silent ? (performance.now() - silent.start) / 1000 : 0;
  },
  duration: SONG_SECONDS,
};

function startSilent() {
  stopSilent();
  silent = {
    start: performance.now(),
    end: setTimeout(() => {
      stopSilent();
      publish(null);
    }, SONG_SECONDS * 1000),
  };
  publish(silentPlayback);
  setLinkMuted(true);
  waitForGesture();
}

function stopSilent() {
  stopWaitingForGesture();
  if (!silent) return;
  clearTimeout(silent.end);
  silent = null;
}

/* Called inside a gesture's handler, synchronously: iOS only lets a play
   start with sound from there. The audio takes over the clock once it
   plays; if the play is refused, the clock keeps running and the next
   gesture tries again. */
function startAudioFromSilent() {
  if (starting) return;
  starting = true;
  stopWaitingForGesture();
  const audio = audioElement();
  audio.muted = false;
  audio.currentTime = silentPlayback.currentTime;
  setLinkMuted(false);
  audio.play().then(
    () => {
      starting = false;
      // Browsers that ignore a seek before the clip has loaded start at 0.
      const at = silentPlayback.currentTime;
      if (Math.abs(audio.currentTime - at) > 0.25) audio.currentTime = at;
      stopSilent();
      publish(audio);
    },
    () => {
      starting = false;
      if (!silent) return;
      setLinkMuted(true);
      waitForGesture();
    },
  );
}

/* Gestures that let the audio start. `touchend` for iOS, which sends no
   click for a tap on most of the page. A gesture on the mute button is left
   to the button. SongShip keeps the video on a gesture while the song is
   muted, so the tap that brings the sound in does not also dismiss it. */
const GESTURES = ["click", "touchend", "keydown"] as const;
let unmuteOnGesture: ((event: Event) => void) | null = null;

function waitForGesture() {
  stopWaitingForGesture();
  unmuteOnGesture = (event) => {
    if (event.target instanceof Element && event.target.closest("[data-song-mute]")) return;
    startAudioFromSilent();
  };
  for (const type of GESTURES) window.addEventListener(type, unmuteOnGesture, true);
}

function stopWaitingForGesture() {
  if (!unmuteOnGesture) return;
  for (const type of GESTURES) window.removeEventListener(type, unmuteOnGesture, true);
  unmuteOnGesture = null;
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
