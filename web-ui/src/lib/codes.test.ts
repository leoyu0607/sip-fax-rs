import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';
import { DICTS, flatten } from './i18n.svelte';

// api-codes.json is written by the backend test crates/web/tests/codes.rs (U02): every API
// error code, field-error code, hangup_code, fax_status and delivery status.
const codes = JSON.parse(readFileSync('src/lib/api-codes.json', 'utf8')) as Record<string, string[]>;

const prefix: Record<string, string> = {
  error: 'error.',
  validation: 'validation.',
  hangup_code: 'fax.hangup.',
  fax_status: 'fax.status.',
  delivery_status: 'delivery.status.',
};

describe('U02 every code of the backend has a text in both languages', () => {
  it('the list has the kinds this test knows', () => {
    expect(Object.keys(codes).sort()).toEqual(Object.keys(prefix).sort());
    for (const [kind, list] of Object.entries(codes)) expect(list.length, kind).toBeGreaterThan(0);
  });

  for (const locale of ['zh-TW', 'en'] as const) {
    it(`${locale}: error, field, hangup, fax status and delivery status codes`, () => {
      const flat = flatten(DICTS[locale]);
      const missing: string[] = [];
      for (const [kind, list] of Object.entries(codes)) {
        for (const c of list) {
          const key = prefix[kind] + c;
          const text = flat.get(key);
          if (text === undefined || text.trim() === '') missing.push(key);
        }
      }
      expect(missing).toEqual([]);
    });
  }

  it('the texts of the two languages differ (nothing was copied untranslated)', () => {
    const zh = flatten(DICTS['zh-TW']);
    const en = flatten(DICTS.en);
    for (const [kind, list] of Object.entries(codes)) {
      for (const c of list) {
        const key = prefix[kind] + c;
        expect(zh.get(key), key).not.toBe(en.get(key));
      }
    }
  });
});
