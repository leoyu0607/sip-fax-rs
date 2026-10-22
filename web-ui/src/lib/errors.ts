// The text of an API error: by its code (spec 3: the English message is never shown).

import { ApiError } from './api';
import { hasKey, t } from './i18n.svelte';

export function errorText(e: unknown): string {
  if (e instanceof ApiError) {
    const key = `error.${e.code}`;
    if (hasKey(key)) {
      const secs = e.retryAfter ?? 0;
      return t(key, { minutes: Math.max(1, Math.ceil(secs / 60)), seconds: secs });
    }
    return t('error.unknown', { code: e.code });
  }
  return t('error.unknown', { code: String(e) });
}

/** The text of a field error of a 422 (`{code, message}`), by its code. */
export function fieldErrorText(v: unknown): string {
  const code = typeof v === 'object' && v !== null && 'code' in v ? String((v as { code: unknown }).code) : String(v);
  const key = `validation.${code}`;
  return hasKey(key) ? t(key) : t('validation.unknown', { code });
}
