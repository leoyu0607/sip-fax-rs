import { describe, expect, it } from 'vitest';
import { liveUptime } from './clock';

describe('liveUptime', () => {
  it('counts on from the uptime that was read', () => {
    expect(liveUptime(100, 5000, 5000)).toBe(100);
    expect(liveUptime(100, 5000, 5999)).toBe(100);
    expect(liveUptime(100, 5000, 6000)).toBe(101);
    expect(liveUptime(100, 5000, 65_000)).toBe(160);
  });
  it('never goes back when the clock reads earlier', () => {
    expect(liveUptime(100, 5000, 4000)).toBe(100);
  });
});
