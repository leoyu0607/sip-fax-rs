import { describe, expect, it } from 'vitest';
import type { Fax } from './api';
import { EMPTY_FILTER, compare, filterParams, isEmpty, localDay, matches, upsert, type FaxFilter } from './faxes';

function fax(id: number, started: string, o: Partial<Fax> = {}): Fax {
  return {
    id, call_id: `c${id}`, trunk: 'cht', ani: '0227001234', ani_source: null, dnis: '025570001', dnis_source: null,
    remote_ident: null, started_at: started, answered_at: null, ended_at: null, duration_sec: null,
    hangup_direction: null, hangup_code: null, sip_status: null, t30_code: null, fax_status: 'received', pages: 1,
    resolution: null, bit_rate: null, compression: null, ecm: null, has_tiff: true, has_pdf: true, tiff_size: null,
    pdf_size: null, files_purged_at: null, is_read: false, read_changed_by: null, read_changed_at: null, note: null,
    note_updated_by: null, note_updated_at: null, created_at: started, updated_at: started, ...o,
  };
}

const f = (o: Partial<FaxFilter>): FaxFilter => ({ ...EMPTY_FILTER, ...o });

describe('filter', () => {
  it('builds the query of GET /faxes', () => {
    expect(filterParams(EMPTY_FILTER)).toEqual({});
    expect(isEmpty(EMPTY_FILTER)).toBe(true);
    expect(isEmpty(f({ q: 'x' }))).toBe(false);
    const p = filterParams(f({ read: 'unread', status: 'failed', ani: ' 02 ', dnis: '', trunk: 'cht', q: 'acme', from: '2026-10-01', to: '2026-10-02' }));
    expect(p).toMatchObject({ is_read: 'false', status: 'failed', ani: '02', trunk: 'cht', q: 'acme' });
    expect(p.dnis).toBeUndefined();
    // whole local days: to is the start of the next day
    expect(p.from).toBe(new Date(2026, 9, 1).toISOString());
    expect(p.to).toBe(new Date(2026, 9, 3).toISOString());
    expect(filterParams(f({ read: 'read' })).is_read).toBe('true');
    expect(localDay('bad')).toBeNull();
  });

  it('matches like the server', () => {
    const x = fax(1, new Date(2026, 9, 1, 12).toISOString(), { remote_ident: 'ACME Sales', note: '請回電' });
    expect(matches(x, EMPTY_FILTER)).toBe(true);
    expect(matches(x, f({ read: 'read' }))).toBe(false);
    expect(matches(x, f({ read: 'unread' }))).toBe(true);
    expect(matches(x, f({ status: 'failed' }))).toBe(false);
    expect(matches(x, f({ from: '2026-10-02' }))).toBe(false);
    expect(matches(x, f({ to: '2026-09-30' }))).toBe(false);
    expect(matches(x, f({ from: '2026-10-01', to: '2026-10-01' }))).toBe(true);
    expect(matches(x, f({ ani: '022' }))).toBe(true);
    expect(matches(x, f({ ani: '7001' }))).toBe(false);
    expect(matches(x, f({ dnis: '0255' }))).toBe(true);
    expect(matches(x, f({ dnis: '9' }))).toBe(false);
    expect(matches(x, f({ trunk: 'other' }))).toBe(false);
    expect(matches(x, f({ q: 'acme sales' }))).toBe(true);
    expect(matches(x, f({ q: '回電' }))).toBe(true);
    expect(matches(x, f({ q: '7001' }))).toBe(true);
    expect(matches(x, f({ q: 'nothing' }))).toBe(false);
  });

  it('upsert keeps the server order and the loaded window', () => {
    const a = fax(1, '2026-10-01T10:00:00Z');
    const b = fax(2, '2026-10-01T09:00:00Z');
    const c = fax(3, '2026-10-01T09:00:00Z');
    expect(compare(c, b)).toBeLessThan(0); // same time: higher id first
    let items = [a, b];
    items = upsert(items, c, EMPTY_FILTER, true);
    expect(items.map((x) => x.id)).toEqual([1, 3, 2]);
    // older than the last loaded row and more pages exist: left to the next page
    const old = fax(4, '2026-09-01T00:00:00Z');
    expect(upsert(items, old, EMPTY_FILTER, false).map((x) => x.id)).toEqual([1, 3, 2]);
    expect(upsert(items, old, EMPTY_FILTER, true).map((x) => x.id)).toEqual([1, 3, 2, 4]);
    // replaced in place, removed when it stops matching
    expect(upsert(items, { ...a, pages: 9 }, EMPTY_FILTER, true)[0]?.pages).toBe(9);
    expect(upsert(items, { ...a, is_read: true }, f({ read: 'unread' }), true).map((x) => x.id)).toEqual([3, 2]);
  });
});
