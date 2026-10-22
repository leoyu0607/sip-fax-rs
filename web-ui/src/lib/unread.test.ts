import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { createUnread } from './unread.svelte';

const ev = (type: string) => ({ seq: 1, type, at: '', data: {} });

describe('unread counter', () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it('reads the count, and once again after a burst of events that change it', async () => {
    let n = 3;
    const fetch = vi.fn(async () => n);
    const u = createUnread(fetch, 300);
    expect(u.count).toBeNull();
    await u.refresh();
    expect(u.count).toBe(3);

    n = 5;
    u.event(ev('fax.created'));
    u.event(ev('fax.progress')); // does not change the count
    u.event(ev('fax.finalized'));
    u.event(ev('fax.read_changed'));
    await vi.advanceTimersByTimeAsync(299);
    expect(fetch).toHaveBeenCalledTimes(1);
    await vi.advanceTimersByTimeAsync(1);
    expect(fetch).toHaveBeenCalledTimes(2);
    expect(u.count).toBe(5);

    u.event(ev('fax.note_changed'));
    u.event(ev('status.changed'));
    await vi.advanceTimersByTimeAsync(1000);
    expect(fetch).toHaveBeenCalledTimes(2);
  });

  it('keeps the shown count when a request fails, and drops an answer older than the last ask', async () => {
    const answers: Array<(n: number) => void> = [];
    let fail = true;
    const u = createUnread(
      () =>
        fail
          ? Promise.reject(new Error('down'))
          : new Promise<number>((resolve) => answers.push(resolve)),
      10,
    );
    await u.refresh();
    expect(u.count).toBeNull();
    fail = false;
    const first = u.refresh();
    const second = u.refresh();
    answers[1]!(7);
    await second;
    answers[0]!(4);
    await first;
    expect(u.count).toBe(7);
  });

  it('reset forgets the count and the pending ask', async () => {
    const fetch = vi.fn(async () => 2);
    const u = createUnread(fetch, 10);
    await u.refresh();
    u.event(ev('fax.created'));
    u.reset();
    await vi.advanceTimersByTimeAsync(50);
    expect(u.count).toBeNull();
    expect(fetch).toHaveBeenCalledTimes(1);
  });
});
