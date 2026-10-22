// The fax list: its filter (the query of `GET /faxes`) and how a live change finds its row.

import type { Fax, FaxStatus } from './api';

export const FAX_STATUSES: readonly FaxStatus[] = ['receiving', 'received', 'partial', 'failed', 'no_fax'];

export interface FaxFilter {
  read: 'all' | 'unread' | 'read';
  status: FaxStatus | '';
  /** `YYYY-MM-DD`, local date (inclusive). */
  from: string;
  /** `YYYY-MM-DD`, local date (inclusive). */
  to: string;
  ani: string;
  dnis: string;
  trunk: string;
  q: string;
}

export const EMPTY_FILTER: FaxFilter = {
  read: 'all',
  status: '',
  from: '',
  to: '',
  ani: '',
  dnis: '',
  trunk: '',
  q: '',
};

export function isEmpty(f: FaxFilter): boolean {
  return (Object.keys(EMPTY_FILTER) as (keyof FaxFilter)[]).every((k) => f[k] === EMPTY_FILTER[k]);
}

/** Start of the local day `YYYY-MM-DD`, plus `days`, as RFC 3339 UTC. */
export function localDay(date: string, days = 0): string | null {
  const m = /^(\d{4})-(\d{2})-(\d{2})$/.exec(date);
  if (!m) return null;
  const d = new Date(Number(m[1]), Number(m[2]) - 1, Number(m[3]) + days);
  return Number.isNaN(d.getTime()) ? null : d.toISOString();
}

/** The parameters of `GET /faxes` for `f` (the dates are whole local days). */
export function filterParams(f: FaxFilter): Record<string, string> {
  const p: Record<string, string> = {};
  if (f.read !== 'all') p.is_read = f.read === 'read' ? 'true' : 'false';
  if (f.status !== '') p.status = f.status;
  const from = localDay(f.from);
  if (from) p.from = from;
  const to = localDay(f.to, 1);
  if (to) p.to = to;
  for (const k of ['ani', 'dnis', 'trunk', 'q'] as const) {
    const v = f[k].trim();
    if (v !== '') p[k] = v;
  }
  return p;
}

/** Whether `fax` belongs in a list filtered by `f` (the server's rules, for live rows). */
export function matches(fax: Fax, f: FaxFilter): boolean {
  if (f.read === 'read' && !fax.is_read) return false;
  if (f.read === 'unread' && fax.is_read) return false;
  if (f.status !== '' && fax.fax_status !== f.status) return false;
  const started = Date.parse(fax.started_at);
  const from = localDay(f.from);
  if (from && started < Date.parse(from)) return false;
  const to = localDay(f.to, 1);
  if (to && started >= Date.parse(to)) return false;
  const ani = f.ani.trim();
  if (ani !== '' && !(fax.ani ?? '').startsWith(ani)) return false;
  const dnis = f.dnis.trim();
  if (dnis !== '' && !(fax.dnis ?? '').startsWith(dnis)) return false;
  const trunk = f.trunk.trim();
  if (trunk !== '' && fax.trunk !== trunk) return false;
  const q = f.q.trim().toLowerCase();
  if (q !== '') {
    const hay = [fax.ani, fax.dnis, fax.remote_ident, fax.note].map((v) => (v ?? '').toLowerCase());
    if (!hay.some((v) => v.includes(q))) return false;
  }
  return true;
}

/** The list order of the server: `started_at` then `id`, newest first. */
export function compare(a: Fax, b: Fax): number {
  const d = Date.parse(b.started_at) - Date.parse(a.started_at);
  return d !== 0 ? d : b.id - a.id;
}

/** `items` with `fax` put in place (replaced, inserted in order, or removed when it no longer
 * matches). Rows older than the last loaded one are left to the next page. */
export function upsert(items: Fax[], fax: Fax, f: FaxFilter, complete: boolean): Fax[] {
  const rest = items.filter((x) => x.id !== fax.id);
  if (!matches(fax, f)) return rest;
  const last = rest[rest.length - 1];
  if (!complete && last !== undefined && compare(last, fax) < 0) return rest;
  const i = rest.findIndex((x) => compare(x, fax) > 0);
  if (i < 0) rest.push(fax);
  else rest.splice(i, 0, fax);
  return rest;
}

/** Translation key of a hangup code (`fax.hangup.<code>`). */
export function hangupKey(code: string | null): string | null {
  return code === null ? null : `fax.hangup.${code}`;
}
