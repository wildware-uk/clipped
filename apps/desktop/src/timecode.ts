/**
 * Nanoseconds, and the one way this window writes a position on a timeline.
 *
 * Two screens now draw a timeline — the Editor's, over an edit document
 * (issue #71), and the playback screen's, over a recording (issue #65) — and
 * both place things by nanosecond. A second formatter would be a second answer
 * to "where is this", and the two would drift the first time one of them
 * learned about hours and the other did not.
 *
 * It lives here rather than in either screen because neither owns it. Everything
 * else about the Editor's timeline — the two kinds of time, the speed
 * arithmetic, the ruler's intervals — is `editor/timeline.ts`, and that file
 * re-exports these two so its own callers are unaffected by where they are
 * written.
 */

/** A thousand million: one second of nanoseconds. */
export const NANOS_PER_SECOND = 1_000_000_000;

/** `nanos` as `mm:ss.mmm`, or `h:mm:ss.mmm` once it runs past the hour. */
export function formatTimecode(nanos: number): string {
  const totalMs = Math.floor(nanos / 1_000_000);
  const ms = totalMs % 1000;
  const totalSeconds = Math.floor(totalMs / 1000);
  const seconds = totalSeconds % 60;
  const minutes = Math.floor(totalSeconds / 60) % 60;
  const hours = Math.floor(totalSeconds / 3600);
  const pad = (value: number, width = 2): string => String(value).padStart(width, '0');
  const withoutHours = `${pad(minutes)}:${pad(seconds)}.${pad(ms, 3)}`;
  return hours === 0 ? withoutHours : `${String(hours)}:${withoutHours}`;
}
