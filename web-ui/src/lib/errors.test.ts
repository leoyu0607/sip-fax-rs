import { describe, expect, it } from 'vitest';
import { ApiError } from './api';
import { errorText, fieldErrorText } from './errors';
import { i18n } from './i18n.svelte';

describe('error texts', () => {
  it('by code, never the English message', () => {
    i18n.set('zh-TW');
    expect(errorText(new ApiError(401, 'invalid_credentials', 'wrong user or password'))).toBe('帳號或密碼錯誤');
    expect(errorText(new ApiError(423, 'account_locked', 'x', undefined, 900))).toContain('15 分鐘');
    expect(errorText(new ApiError(429, 'rate_limited', 'x', undefined, 30))).toContain('30 秒');
    expect(errorText(new ApiError(500, 'brand_new_code', 'secret detail'))).toBe('發生錯誤（brand_new_code）');
    expect(errorText('boom')).toContain('boom');
    expect(fieldErrorText({ code: 'out_of_range', message: 'x' })).toBe('超出允許的範圍');
    expect(fieldErrorText({ code: 'zzz' })).toContain('zzz');
    i18n.set('en');
    expect(errorText(new ApiError(409, 'last_admin', ''))).toContain('last administrator');
  });
});
