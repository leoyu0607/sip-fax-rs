// Translations (spec 6, i18n): `src/locales/{zh-TW,en}.json`. The account's locale wins, then
// the browser's languages, then zh-TW.

import en from '../locales/en.json';
import zhTW from '../locales/zh-TW.json';

export const LOCALES = ['zh-TW', 'en'] as const;
export type Locale = (typeof LOCALES)[number];

type Tree = { [k: string]: string | Tree };

export const DICTS: Record<Locale, Tree> = { 'zh-TW': zhTW as Tree, en: en as Tree };

/** Every key of a translation tree, dotted (`fax.status.received`). */
export function flatten(t: Tree, prefix = ''): Map<string, string> {
  const out = new Map<string, string>();
  for (const [k, v] of Object.entries(t)) {
    const key = prefix === '' ? k : `${prefix}.${k}`;
    if (typeof v === 'string') out.set(key, v);
    else for (const [kk, vv] of flatten(v, key)) out.set(kk, vv);
  }
  return out;
}

const FLAT: Record<Locale, Map<string, string>> = {
  'zh-TW': flatten(DICTS['zh-TW']),
  en: flatten(DICTS.en),
};

function asLocale(s: string | null | undefined): Locale | null {
  if (!s) return null;
  const l = s.toLowerCase();
  if (l === 'en' || l.startsWith('en-')) return 'en';
  if (l === 'zh-tw' || l === 'zh-hant' || l.startsWith('zh-hant') || l === 'zh' || l.startsWith('zh-')) return 'zh-TW';
  return null;
}

/** The locale to use: the account's, else the first browser language we have, else zh-TW. */
export function pickLocale(account: string | null | undefined, browser: readonly string[]): Locale {
  const a = asLocale(account);
  if (a) return a;
  for (const b of browser) {
    const l = asLocale(b);
    if (l) return l;
  }
  return 'zh-TW';
}

/** `{name}` placeholders of `text` replaced by `params`. */
export function interpolate(text: string, params?: Record<string, string | number>): string {
  if (!params) return text;
  return text.replace(/\{(\w+)\}/g, (m, k: string) => (k in params ? String(params[k]) : m));
}

/** The text of `key` in `locale`, or the key itself when it has none (visible in tests). */
export function translate(locale: Locale, key: string, params?: Record<string, string | number>): string {
  const v = FLAT[locale].get(key);
  return v === undefined ? key : interpolate(v, params);
}

/** Whether `key` has a text (for the optional ones: a hangup code, a validation code). */
export function hasKey(key: string): boolean {
  return FLAT['zh-TW'].has(key);
}

// ------------------------------------------------------------------------------ state

let current = $state<Locale>('zh-TW');

export const i18n = {
  get locale(): Locale {
    return current;
  },
  set(l: Locale) {
    current = l;
    if (typeof document !== 'undefined') document.documentElement.lang = l;
  },
};

/** The text of `key` in the current locale (reactive in templates). */
export function t(key: string, params?: Record<string, string | number>): string {
  return translate(current, key, params);
}

// ------------------------------------------------------------------------------ formats

/** A time of the API (RFC 3339 UTC) in the browser's time zone (or `timeZone`). */
export function formatTime(iso: string | null | undefined, locale: Locale, timeZone?: string): string {
  if (!iso) return '';
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return iso;
  return new Intl.DateTimeFormat(locale, {
    year: 'numeric',
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
    second: '2-digit',
    hour12: false,
    timeZone,
  }).format(d);
}

/** The calendar day of `d` in `timeZone`, as `2026-10-23`. */
function dayKey(d: Date, timeZone?: string): string {
  return new Intl.DateTimeFormat('en-CA', { year: 'numeric', month: '2-digit', day: '2-digit', timeZone }).format(d);
}

/**
 * A short time for a list: `today 19:24`, `yesterday 19:24`, `10/21 19:24` this year, the whole
 * date before that (`words`: the translated "today" and "yesterday").
 */
export function formatWhen(
  iso: string | null | undefined,
  locale: Locale,
  words: { today: string; yesterday: string },
  now: Date = new Date(),
  timeZone?: string,
): string {
  if (!iso) return '';
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return iso;
  const hm = new Intl.DateTimeFormat(locale, { hour: '2-digit', minute: '2-digit', hour12: false, timeZone }).format(d);
  const day = dayKey(d, timeZone);
  if (day === dayKey(now, timeZone)) return `${words.today} ${hm}`;
  if (day === dayKey(new Date(now.getTime() - 86_400_000), timeZone)) return `${words.yesterday} ${hm}`;
  const sameYear = day.slice(0, 4) === dayKey(now, timeZone).slice(0, 4);
  const date = new Intl.DateTimeFormat(locale, { year: sameYear ? undefined : 'numeric', month: '2-digit', day: '2-digit', timeZone }).format(d);
  return `${date} ${hm}`;
}

/** `1.2 MB`, up to TB (a disk's size) */
export function formatBytes(n: number | null | undefined): string {
  if (n === null || n === undefined) return '';
  if (n < 1024) return `${n} B`;
  const units = ['KB', 'MB', 'GB', 'TB'];
  let v = n / 1024;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i++;
  }
  return `${v.toFixed(1)} ${units[i]}`;
}

/** `1:05:03` / `5:03` */
export function formatDuration(secs: number | null | undefined): string {
  if (secs === null || secs === undefined) return '';
  const s = Math.max(0, Math.round(secs));
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const ss = String(s % 60).padStart(2, '0');
  return h > 0 ? `${h}:${String(m).padStart(2, '0')}:${ss}` : `${m}:${ss}`;
}

export function fmt(iso: string | null | undefined): string {
  return formatTime(iso, current);
}

/** [`formatWhen`] in the current language. */
export function when(iso: string | null | undefined, now?: Date): string {
  return formatWhen(iso, current, { today: t('time.today'), yesterday: t('time.yesterday') }, now);
}
