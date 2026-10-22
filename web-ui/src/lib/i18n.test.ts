import { describe, expect, it } from 'vitest';
import { DICTS, flatten, formatBytes, formatDuration, formatTime, interpolate, pickLocale, translate } from './i18n.svelte';

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
    expect(formatDuration(null)).toBe('');
    expect(formatDuration(65)).toBe('1:05');
    expect(formatDuration(3723)).toBe('1:02:03');
  });
});
