import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { Fax } from './api';
import { EMPTY_FILTER } from './faxes';
import { faxList } from './faxlist.svelte';

function fax(id: number, started: string, o: Partial<Fax> = {}): Fax {
  return {
    id, call_id: `c${id}`, trunk: 'cht', ani: '02', ani_source: null, dnis: '0255', dnis_source: null,
    remote_ident: null, started_at: started, answered_at: null, ended_at: null, duration_sec: null,
    hangup_direction: null, hangup_code: null, sip_status: null, t30_code: null, fax_status: 'received', pages: 1,
    resolution: null, bit_rate: null, compression: null, ecm: null, has_tiff: true, has_pdf: true, tiff_size: null,
    pdf_size: null, files_purged_at: null, is_read: false, read_changed_by: null, read_changed_at: null, note: null,
    note_updated_by: null, note_updated_at: null, created_at: started, updated_at: started, ...o,
  };
}

// a fake server: the list pages and the rows by id
let rows = new Map<number, Fax>();
let pages: { items: Fax[]; next_cursor: string | null }[] = [];
let urls: string[] = [];

beforeEach(() => {
  rows = new Map();
  pages = [];
  urls = [];
  faxList.filter = { ...EMPTY_FILTER };
  faxList.items = [];
  faxList.pending = [];
  faxList.cursor = null;
  faxList.loaded = false;
  vi.stubGlobal('fetch', async (url: string) => {
    urls.push(url);
    const m = /\/faxes\/(\d+)$/.exec(url);
    if (m) {
      const r = rows.get(Number(m[1]));
      return r ? Response.json(r) : Response.json({ error: { code: 'not_found', message: '' } }, { status: 404 });
    }
    return Response.json(pages.shift() ?? { items: [], next_cursor: null });
  });
});
afterEach(() => vi.unstubAllGlobals());

const ids = () => faxList.items.map((f) => f.id);

describe('fax list', () => {
  it('U07 loads pages with the filter and the cursor', async () => {
    pages = [
      { items: [fax(3, '2026-10-03T00:00:00Z'), fax(2, '2026-10-02T00:00:00Z')], next_cursor: 'k1' },
      { items: [fax(2, '2026-10-02T00:00:00Z'), fax(1, '2026-10-01T00:00:00Z')], next_cursor: null },
    ];
    faxList.filter = { ...EMPTY_FILTER, read: 'unread', q: 'acme' };
    await faxList.reload();
    expect(urls[0]).toBe('/api/v1/faxes?is_read=false&q=acme&limit=50');
    expect(faxList.complete).toBe(false);
    await faxList.more();
    expect(urls[1]).toContain('cursor=k1');
    expect(ids()).toEqual([3, 2, 1]); // the duplicate of the boundary is dropped
    expect(faxList.complete).toBe(true);
    await faxList.more(); // nothing more
    expect(urls.length).toBe(2);
  });

  it('U07 a new fax goes on top at the top of the page, waits when scrolled (spec 6)', async () => {
    pages = [{ items: [fax(1, '2026-10-01T00:00:00Z')], next_cursor: null }];
    await faxList.reload();
    rows.set(2, fax(2, '2026-10-02T00:00:00Z', { fax_status: 'receiving', pages: 0 }));
    await faxList.event({ seq: 1, type: 'fax.created', data: { id: 2 } }, false);
    expect(ids()).toEqual([2, 1]);
    rows.set(3, fax(3, '2026-10-03T00:00:00Z', { fax_status: 'receiving', pages: 0 }));
    await faxList.event({ seq: 2, type: 'fax.created', data: { id: 3 } }, true);
    expect(ids()).toEqual([2, 1]);
    expect(faxList.pending.map((f) => f.id)).toEqual([3]);
    // progress of a waiting row updates it where it waits
    await faxList.event({ seq: 3, type: 'fax.progress', data: { id: 3, pages: 2 } }, true);
    expect(faxList.pending[0]?.pages).toBe(2);
    rows.set(3, fax(3, '2026-10-03T00:00:00Z', { pages: 3 }));
    await faxList.event({ seq: 4, type: 'fax.finalized', data: { id: 3 } }, true);
    expect(faxList.pending[0]?.fax_status).toBe('received');
    faxList.showPending();
    expect(ids()).toEqual([3, 2, 1]);
    expect(faxList.pending).toEqual([]);
  });

  it('U07 progress and finalisation update the row in place', async () => {
    pages = [{ items: [fax(5, '2026-10-05T00:00:00Z', { fax_status: 'receiving', pages: 0 })], next_cursor: null }];
    await faxList.reload();
    for (const p of [1, 2, 3]) await faxList.event({ seq: p, type: 'fax.progress', data: { id: 5, pages: p } }, false);
    expect(faxList.items[0]?.pages).toBe(3);
    await faxList.event({ seq: 9, type: 'fax.progress', data: { id: 5, pages: 2 } }, false); // never backwards
    expect(faxList.items[0]?.pages).toBe(3);
    rows.set(5, fax(5, '2026-10-05T00:00:00Z', { pages: 3 }));
    await faxList.event({ seq: 10, type: 'fax.finalized', data: { id: 5 } }, false);
    expect(faxList.items[0]?.fax_status).toBe('received');
  });

  it('U07 read and note changes; a row that stops matching the filter leaves', async () => {
    pages = [{ items: [fax(1, '2026-10-01T00:00:00Z'), fax(2, '2026-10-02T00:00:00Z')], next_cursor: null }];
    faxList.filter = { ...EMPTY_FILTER, read: 'unread' };
    await faxList.reload();
    await faxList.event({ seq: 1, type: 'fax.note_changed', data: { id: 1, note: 'hi', by: 'amy', at: 't' } }, false);
    expect(faxList.items.find((f) => f.id === 1)?.note).toBe('hi');
    await faxList.event({ seq: 2, type: 'fax.read_changed', data: { id: 2, is_read: true, by: 'amy', at: 't' } }, false);
    expect(ids()).toEqual([1]);
    faxList.replace({ ...faxList.items[0]!, note: 'local' });
    expect(faxList.items[0]?.note).toBe('local');
  });

  it('U07 a created fax that does not match, or cannot be read, is ignored', async () => {
    pages = [{ items: [], next_cursor: null }];
    faxList.filter = { ...EMPTY_FILTER, status: 'failed' };
    await faxList.reload();
    rows.set(7, fax(7, '2026-10-07T00:00:00Z', { fax_status: 'receiving' }));
    await faxList.event({ seq: 1, type: 'fax.created', data: { id: 7 } }, false);
    await faxList.event({ seq: 2, type: 'fax.created', data: { id: 404 } }, false);
    await faxList.event({ seq: 3, type: 'fax.created', data: {} }, false);
    expect(ids()).toEqual([]);
  });

  it('U07 a progress event of an unknown row fetches it', async () => {
    pages = [{ items: [], next_cursor: null }];
    await faxList.reload();
    rows.set(8, fax(8, '2026-10-08T00:00:00Z', { fax_status: 'receiving', pages: 1 }));
    await faxList.event({ seq: 1, type: 'fax.progress', data: { id: 8, pages: 1 } }, false);
    expect(ids()).toEqual([8]);
  });

  it('U07 a failed load keeps the error', async () => {
    vi.stubGlobal('fetch', async () => Response.json({ error: { code: 'unavailable', message: '' } }, { status: 503 }));
    await faxList.reload();
    expect(faxList.error).toMatchObject({ code: 'unavailable' });
    expect(faxList.loading).toBe(false);
  });
});
