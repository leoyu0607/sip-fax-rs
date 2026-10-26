import { describe, expect, it } from 'vitest';
import { DICTS, flatten, formatBytes, formatDuration, formatTime, formatWhen, interpolate, pickLocale, translate } from './i18n.svelte';

describe('locales', () => {
  it('U01 both locale files have the same keys and no empty text', () => {
    const zh = flatten(DICTS['zh-TW']);
    const en = flatten(DICTS.en);
    expect([...zh.keys()].sort()).toEqual([...en.keys()].sort());
    for (const [k, v] of [...zh, ...en]) expect(v.trim(), k).not.toBe('');
  });

  it('every {placeholder} of a text is in the other language too', () => {
    const zh = flatten(DICTS['zh-TW']);
    const en = flatten(DICTS.en);
    const names = (s: string) => [...s.matchAll(/\{(\w+)\}/g)].map((m) => m[1]).sort();
    for (const [k, v] of zh) expect(names(en.get(k) ?? ''), k).toEqual(names(v));
  });
});

describe('pickLocale', () => {
  it('prefers the account, then the browser, then zh-TW', () => {
    expect(pickLocale('en', ['zh-TW'])).toBe('en');
    expect(pickLocale(null, ['fr-FR', 'en-GB'])).toBe('en');
    expect(pickLocale(null, ['zh-Hant-TW'])).toBe('zh-TW');
    expect(pickLocale(undefined, ['fr'])).toBe('zh-TW');
    expect(pickLocale('xx', [])).toBe('zh-TW');
  });
});

describe('translate', () => {
  it('fills placeholders and shows the key of a missing text', () => {
    expect(translate('en', 'fax.page_n', { page: 3 })).toBe('Page 3');
    expect(translate('zh-TW', 'fax.page_n', { page: 3 })).toBe('第 3 頁');
    expect(translate('en', 'no.such.key')).toBe('no.such.key');
    expect(interpolate('{a} {b}', { a: 1 })).toBe('1 {b}');
  });
});

describe('formats', () => {
  it('U03 times are shown in the browser time zone', () => {
    const iso = '2026-10-22T16:30:05Z';
    expect(formatTime(iso, 'en', 'Asia/Taipei')).toContain('00:30:05');
    expect(formatTime(iso, 'en', 'Asia/Taipei')).toContain('10/23/2026');
    expect(formatTime(iso, 'en', 'UTC')).toContain('16:30:05');
    expect(formatTime(iso, 'zh-TW', 'Asia/Taipei')).toContain('2026/10/23');
    expect(formatTime(null, 'en')).toBe('');
    expect(formatTime('garbage', 'en')).toBe('garbage');
  });

  it('bytes and durations', () => {
    expect(formatBytes(null)).toBe('');
    expect(formatBytes(512)).toBe('512 B');
    expect(formatBytes(2048)).toBe('2.0 KB');
    expect(formatBytes(3 * 1024 * 1024)).toBe('3.0 MB');
    expect(formatBytes(957531.1 * 1024 * 1024)).toBe('935.1 GB');
    expect(formatBytes(2.5 * 1024 ** 4)).toBe('2.5 TB');
    expect(formatBytes(3 * 1024 ** 5)).toBe('3072.0 TB');
    expect(formatDuration(null)).toBe('');
    expect(formatDuration(65)).toBe('1:05');
    expect(formatDuration(3723)).toBe('1:02:03');
  });
});

describe('formatWhen', () => {
  const words = { today: 'Today', yesterday: 'Yesterday' };
  const now = new Date('2026-10-23T04:00:00Z'); // 12:00 in Taipei
  const w = (iso: string, tz = 'Asia/Taipei', locale: 'en' | 'zh-TW' = 'en') => formatWhen(iso, locale, words, now, tz);
  it('says today and yesterday by the calendar day of the time zone', () => {
    expect(w('2026-10-23T01:05:00Z')).toBe('Today 09:05');
    // 23:59 in Taipei the day before, but the same UTC day as now
    expect(w('2026-10-22T15:59:00Z')).toBe('Yesterday 23:59');
    // 00:00 in Taipei today, though it is the day before in UTC
    expect(w('2026-10-22T16:00:00Z')).toBe('Today 00:00');
  });
  it('gives the date without the year this year, and with it before', () => {
    expect(w('2026-10-21T04:00:00Z')).toBe('10/21 12:00');
    expect(w('2025-12-31T04:00:00Z')).toBe('12/31/2025 12:00');
    expect(w('2025-12-31T04:00:00Z', 'Asia/Taipei', 'zh-TW')).toBe('2025/12/31 12:00');
  });
  it('passes empty and broken values through like formatTime', () => {
    expect(formatWhen(null, 'en', words, now)).toBe('');
    expect(formatWhen('garbage', 'en', words, now)).toBe('garbage');
  });
});

describe('lastDay', () => {
  it('is the day before an end at midnight, in the current language', async () => {
    const { lastDay, i18n } = await import('./i18n.svelte');
    i18n.set('en');
    const end = new Date(2100, 0, 1, 0, 0, 0).toISOString();
    expect(lastDay(end)).toBe('12/31/2099');
    expect(lastDay(null)).toBe('');
    expect(lastDay('not a time')).toBe('not a time');
  });
});
