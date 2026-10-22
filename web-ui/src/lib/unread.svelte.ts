// The number of unread faxes, for the badge next to "Faxes" in the menu. Read from
// `GET /faxes/counts` when someone signs in, and again shortly after any event that can change it
// (several events in a row ask once).

import { get } from './api';
import type { ServerEvent } from './events';

const CHANGES = new Set(['fax.created', 'fax.finalized', 'fax.read_changed']);

export function createUnread(fetchCount: () => Promise<number>, delayMs = 300) {
  let count = $state<number | null>(null);
  let timer: ReturnType<typeof setTimeout> | null = null;
  let asked = 0;

  async function refresh(): Promise<void> {
    const mine = ++asked;
    try {
      const n = await fetchCount();
      // an older answer arriving after a newer one is dropped
      if (mine === asked) count = n;
    } catch {
      // keep what is shown; the next event asks again
    }
  }

  function soon(): void {
    if (timer) clearTimeout(timer);
    timer = setTimeout(() => {
      timer = null;
      void refresh();
    }, delayMs);
  }

  return {
    get count(): number | null {
      return count;
    },
    refresh,
    /** Feed every server event; the ones that can change the count ask again. */
    event(e: ServerEvent): void {
      if (CHANGES.has(e.type)) soon();
    },
    /** Signed out: nothing shown, nothing pending. */
    reset(): void {
      if (timer) clearTimeout(timer);
      timer = null;
      asked++;
      count = null;
    },
  };
}

export const unread = createUnread(async () => (await get<{ unread: number }>('/faxes/counts')).unread);
