import { afterEach, describe, expect, it, vi } from 'vitest';
import { ApiError, errorOf, get, post, put, query, setCsrf, setOnUnauthenticated } from './api';

afterEach(() => {
  vi.unstubAllGlobals();
  setCsrf(null);
  setOnUnauthenticated(null);
});

function stub(status: number, body: unknown, headers: Record<string, string> = {}) {
  const calls: { url: string; init: RequestInit }[] = [];
  vi.stubGlobal('fetch', async (url: string, init: RequestInit) => {
    calls.push({ url, init });
    return new Response(status === 204 ? null : typeof body === 'string' ? body : JSON.stringify(body), { status, headers });
  });
  return calls;
}

describe('api client', () => {
  it('U07 GET has no CSRF header; changes are JSON with the token', async () => {
    const calls = stub(200, { ok: 1 });
    setCsrf('tok');
    expect(await get('/x')).toEqual({ ok: 1 });
    await put('/y', { a: 1 });
    await post('/z');
    expect(calls[0]!.url).toBe('/api/v1/x');
    expect((calls[0]!.init.headers as Record<string, string>)['X-Sipfax-Csrf']).toBeUndefined();
    const h = calls[1]!.init.headers as Record<string, string>;
    expect(h['X-Sipfax-Csrf']).toBe('tok');
    expect(h['Content-Type']).toBe('application/json');
    expect(calls[1]!.init.body).toBe('{"a":1}');
    expect(calls[2]!.init.body).toBe('{}');
  });

  it('U07 errors keep the code, fields and Retry-After', async () => {
    stub(423, { error: { code: 'account_locked', message: 'm', fields: { a: 1 } } }, { 'Retry-After': '900' });
    const e = await get('/x').catch((x: unknown) => x);
    expect(e).toBeInstanceOf(ApiError);
    expect(e).toMatchObject({ status: 423, code: 'account_locked', fields: { a: 1 }, retryAfter: 900 });
    const plain = await errorOf(new Response('oops', { status: 502 }));
    expect(plain.code).toBe('http_502');
  });

  it('U07 401 tells the app the session is gone (not for the login itself)', async () => {
    let gone = 0;
    setOnUnauthenticated(() => (gone += 1));
    stub(401, { error: { code: 'unauthenticated', message: '' } });
    await get('/faxes').catch(() => {});
    await post('/auth/login', {}).catch(() => {});
    expect(gone).toBe(1);
  });

  it('U07 a network failure is code network; 204 is undefined', async () => {
    vi.stubGlobal('fetch', async () => {
      throw new TypeError('down');
    });
    await expect(get('/x')).rejects.toMatchObject({ code: 'network', status: 0 });
    stub(204, null);
    expect(await post('/auth/logout')).toBeUndefined();
  });

  it('U07 query skips empty values', () => {
    expect(query({ a: 1, b: '', c: null, d: undefined, e: false })).toBe('?a=1&e=false');
    expect(query({})).toBe('');
  });
});
