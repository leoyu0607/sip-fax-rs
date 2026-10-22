/**
 * The uptime to show `nowMs` after the status was read: the server sends `uptime_secs` only when
 * the page loads the status, so the page counts on from there (`loadedAtMs`, `nowMs`: the same
 * monotonic clock, e.g. `performance.now()`).
 */
export function liveUptime(uptimeSecs: number, loadedAtMs: number, nowMs: number): number {
  return uptimeSecs + Math.max(0, Math.floor((nowMs - loadedAtMs) / 1000));
}
