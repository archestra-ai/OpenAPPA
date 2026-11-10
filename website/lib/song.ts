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

type Listener = (audio: HTMLMediaElement | null) => void;

let element: HTMLMediaElement | null = null;
let playing: HTMLMediaElement | null = null;
let fromLink = false;
const listeners = new Set<Listener>();

function publish(audio: HTMLMediaElement | null) {
  if (audio === playing) return;
  playing = audio;
  for (const listener of listeners) listener(audio);
}

function audioElement(): HTMLMediaElement {
  if (!element) {
    // A <video>, not an Audio: with no user gesture yet, Chrome refuses
    // even a muted play of an audio element but allows a muted video, which
    // is what lets the `?song` link start without a click. It plays the mp3
    // the same way and is never attached to the page.
    const video = document.createElement("video");
    video.playsInline = true;
    video.preload = "none";
    video.src = SONG_SRC;
    element = video;
    element.addEventListener("ended", () => publish(null));
    element.addEventListener("pause", () => {
      stopWaitingForGesture();
      publish(null);
    });
  }
  return element;
}

/** Starts the song from the top. Resolves false when the browser refused to play.
 *
 * `muted` is for a start without a user gesture (the `?song` link): browsers
 * refuse sound before the visitor has interacted with the page but allow a
 * muted start, so the video plays at once and the sound comes on with the
 * visitor's first click or key press anywhere. `fromLink` marks a start
 * from that link, which gets a mute button on the video (SongShip). */
export async function playSong(options: { muted?: boolean; fromLink?: boolean } = {}): Promise<boolean> {
  const audio = audioElement();
  stopWaitingForGesture();
  fromLink = options.fromLink ?? false;
  audio.muted = options.muted ?? false;
  audio.currentTime = 0;
  try {
    await audio.play();
  } catch {
    // Nothing to recover from: no codec, or blocked media.
    return false;
  }
  if (audio.muted) waitForGesture(audio);
  publish(audio);
  return true;
}

export function stopSong() {
  if (!element) return;
  element.pause();
  element.currentTime = 0;
}

/** Whether the song playing now was started by the `?song` link. */
export function songStartedFromLink(): boolean {
  return fromLink;
}

/** Mutes or unmutes the song; an unmute ends the wait for the first gesture. */
export function setSongMuted(muted: boolean) {
  if (!element) return;
  stopWaitingForGesture();
  element.muted = muted;
}

/** Calls `listener` with the playing element now and on every change. */
export function subscribeSong(listener: Listener): () => void {
  listeners.add(listener);
  listener(playing);
  return () => {
    listeners.delete(listener);
  };
}

/* Unmuting needs the user activation a click or key press grants.
   `click`, not `pointerdown`: SongShip dismisses the video on a pointerdown
   outside it unless the song is still muted, so the click that brings the
   sound in leaves the video playing. A gesture on the mute button is left
   to the button, so its click toggles once rather than twice. */
const GESTURES = ["click", "keydown"] as const;
let unmuteOnGesture: ((event: Event) => void) | null = null;

function waitForGesture(audio: HTMLMediaElement) {
  unmuteOnGesture = (event) => {
    if (event.target instanceof Element && event.target.closest("[data-song-mute]")) return;
    audio.muted = false;
    stopWaitingForGesture();
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
