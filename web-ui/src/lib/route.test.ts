import { describe, expect, it } from 'vitest';
import { href, parseRoute } from './route';

describe('routes', () => {
  it('parses the hash and builds it back', () => {
    expect(parseRoute('')).toEqual({ page: 'faxes' });
    expect(parseRoute('#/faxes/12')).toEqual({ page: 'fax', id: 12 });
    expect(parseRoute('#/faxes/0')).toEqual({ page: 'not_found' });
    expect(parseRoute('#/faxes/abc')).toEqual({ page: 'not_found' });
    expect(parseRoute('#/settings')).toEqual({ page: 'settings', section: null });
    expect(parseRoute('#/settings/trunk%3Acht')).toEqual({ page: 'settings', section: 'trunk:cht' });
    expect(parseRoute('#/users/')).toEqual({ page: 'users' });
    expect(parseRoute('#/users/x')).toEqual({ page: 'not_found' });
    expect(parseRoute('#/nope')).toEqual({ page: 'not_found' });
    for (const r of [{ page: 'fax', id: 7 }, { page: 'settings', section: 'trunk:a b' }, { page: 'audit' }] as const) {
      expect(parseRoute(href(r))).toEqual(r);
    }
    expect(href({ page: 'not_found' })).toBe('#/');
    expect(href({ page: 'settings', section: null })).toBe('#/settings');
  });
});
