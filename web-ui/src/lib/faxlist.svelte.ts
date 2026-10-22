// The state of the fax list page, kept while the user opens a fax and comes back.

import { get, query, type Fax, type FaxPage } from './api';
import type { ServerEvent } from './events';
import { EMPTY_FILTER, filterParams, matches, upsert, type FaxFilter } from './faxes';

export const PAGE = 50;

class FaxList {
  items = $state<Fax[]>([]);
  cursor = $state<string | null>(null);
  filter = $state<FaxFilter>({ ...EMPTY_FILTER });
  loading = $state(false);
  error = $state<unknown>(null);
  loaded = $state(false);
  /** New faxes held back while the user is scrolled down (spec 6: no jump). */
  pending = $state<Fax[]>([]);
  /** Where the page was scrolled when the user left it. */
  scrollY = 0;
  private generation = 0;

  get complete(): boolean {
    return this.loaded && this.cursor === null;
  }

  async reload(): Promise<void> {
    const gen = ++this.generation;
    this.loading = true;
    this.error = null;
    this.pending = [];
    try {
      const p = await get<FaxPage>(`/faxes${query({ ...filterParams(this.filter), limit: PAGE })}`);
      if (gen !== this.generation) return;
      this.items = p.items;
      this.cursor = p.next_cursor;
      this.loaded = true;
    } catch (e) {
      if (gen === this.generation) this.error = e;
    } finally {
      if (gen === this.generation) this.loading = false;
    }
  }

  async more(): Promise<void> {
    if (this.loading || this.cursor === null) return;
    const gen = this.generation;
    this.loading = true;
    this.error = null;
    try {
      const p = await get<FaxPage>(
        `/faxes${query({ ...filterParams(this.filter), limit: PAGE, cursor: this.cursor })}`,
      );
      if (gen !== this.generation) return;
      const seen = new Set(this.items.map((f) => f.id));
      this.items = [...this.items, ...p.items.filter((f) => !seen.has(f.id))];
      this.cursor = p.next_cursor;
    } catch (e) {
      if (gen === this.generation) this.error = e;
    } finally {
      if (gen === this.generation) this.loading = false;
    }
  }

  /** Puts the held-back faxes in the list. */
  showPending(): void {
    let items = this.items;
    for (const f of this.pending) items = upsert(items, f, this.filter, this.complete);
    this.items = items;
    this.pending = [];
  }

  private put(fax: Fax): void {
    this.items = upsert(this.items, fax, this.filter, this.complete);
    this.pending = this.pending.filter((f) => f.id !== fax.id);
  }

  private patch(id: number, f: (x: Fax) => Fax): void {
    const i = this.items.findIndex((x) => x.id === id);
    if (i >= 0) {
      const next = f(this.items[i] as Fax);
      if (matches(next, this.filter)) this.items[i] = next;
      else this.items = this.items.filter((x) => x.id !== id);
    }
    this.pending = this.pending.map((x) => (x.id === id ? f(x) : x));
  }

  /** A server event; `scrolled`: the user is not at the top (new faxes wait). */
  async event(e: ServerEvent, scrolled: boolean): Promise<void> {
    const d = (e.data ?? {}) as Record<string, unknown>;
    const id = typeof d.id === 'number' ? d.id : null;
    if (id === null) return;
    switch (e.type) {
      case 'fax.created': {
        const fax = await this.fetch(id);
        if (!fax || !matches(fax, this.filter)) return;
        if (this.items.some((x) => x.id === id)) this.put(fax);
        else if (scrolled) this.pending = [fax, ...this.pending.filter((x) => x.id !== id)];
        else this.put(fax);
        return;
      }
      case 'fax.progress': {
        const pages = typeof d.pages === 'number' ? d.pages : null;
        const known = this.items.some((x) => x.id === id) || this.pending.some((x) => x.id === id);
        if (known && pages !== null) {
          this.patch(id, (x) => ({ ...x, pages: Math.max(x.pages, pages) }));
          return;
        }
        // a row the list has not seen yet (the event of its creation came before the list)
        const fax = await this.fetch(id);
        if (fax && matches(fax, this.filter) && !scrolled) this.put(fax);
        return;
      }
      case 'fax.finalized': {
        const fax = await this.fetch(id);
        if (!fax) return;
        if (this.pending.some((x) => x.id === id)) {
          this.pending = this.pending.map((x) => (x.id === id ? fax : x));
          return;
        }
        if (this.items.some((x) => x.id === id) || !scrolled) this.put(fax);
        return;
      }
      case 'fax.read_changed':
        this.patch(id, (x) => ({
          ...x,
          is_read: Boolean(d.is_read),
          read_changed_by: (d.by as string | null) ?? null,
          read_changed_at: (d.at as string | null) ?? null,
        }));
        return;
      case 'fax.note_changed':
        this.patch(id, (x) => ({
          ...x,
          note: (d.note as string | null) ?? null,
          note_updated_by: (d.by as string | null) ?? null,
          note_updated_at: (d.at as string | null) ?? null,
        }));
        return;
    }
  }

  /** A row changed here (read toggle). */
  replace(fax: Fax): void {
    this.patch(fax.id, () => fax);
  }

  private async fetch(id: number): Promise<Fax | null> {
    try {
      return await get<Fax>(`/faxes/${id}`);
    } catch {
      return null;
    }
  }
}

export const faxList = new FaxList();
