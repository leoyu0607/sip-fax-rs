import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { setCsrf } from './api';
import { i18n } from './i18n.svelte';
import { changePassword, ended, login, logout, restore, session, storeLocale, storedLocale, updateUser } from './session.svelte';

const agent = { id: 1, username: 'amy', display_name: null, role: 'agent', locale: null, disabled: false, must_change_password: false, last_login_at: null, created_at: '2026-10-22T00:00:00Z' };

function stub(routes: Record<string, { status: number; body?: unknown }>) {
  const calls: { url: string; method: string; csrf: string | null }[] = [];
  vi.stubGlobal('fetch', async (url: string, init: RequestInit = {}) => {
    const r = routes[url.replace('/api/v1', '')] ?? { status: 404, body: { error: { code: 'not_found', message: 'x' } } };
    const h = new Headers(init.headers);
    calls.push({ url, method: init.method ?? 'GET', csrf: h.get('X-Sipfax-Csrf') });
    return new Response(r.status === 204 ? null : JSON.stringify(r.body), { status: r.status });
  });
  return calls;
}

beforeEach(() => {
  localStorage.clear();
  ended();
  i18n.set('zh-TW');
});
afterEach(() => {
  vi.unstubAllGlobals();
  setCsrf(null);
});

describe('U07 session', () => {
  it('restore: a live cookie signs in with the account language; the CSRF token is kept', async () => {
    const calls = stub({ '/auth/me': { status: 200, body: { user: { ...agent, locale: 'en' }, csrf: 'tok' } }, '/auth/logout': { status: 204 } });
    expect(session.ready).toBe(false);
    await restore();
    expect(session.ready).toBe(true);
    expect(session.user?.username).toBe('amy');
    expect(session.isAdmin).toBe(false);
    expect(i18n.locale).toBe('en');
    await logout();
    const out = calls.find((c) => c.url.endsWith('/auth/logout'));
    expect(out?.csrf).toBe('tok');
    expect(session.user).toBeNull();
  });

  it('restore: 401 means signed out, with the language of this browser', async () => {
    storeLocale('en');
    stub({ '/auth/me': { status: 401, body: { error: { code: 'unauthenticated', message: 'x' } } } });
    await restore();
    expect(session.ready).toBe(true);
    expect(session.user).toBeNull();
    expect(i18n.locale).toBe('en');
  });

  it('restore: another failure is not hidden as "signed out"', async () => {
    stub({ '/auth/me': { status: 500, body: { error: { code: 'internal', message: 'x' } } } });
    await expect(restore()).rejects.toMatchObject({ code: 'internal' });
    expect(session.ready).toBe(true);
  });

  it('login and a password change replace the account; an admin is recognised', async () => {
    stub({
      '/auth/login': { status: 200, body: { user: { ...agent, role: 'admin', must_change_password: true }, csrf: 'a' } },
      '/auth/password': { status: 200, body: { user: { ...agent, role: 'admin', must_change_password: false }, csrf: 'b' } },
    });
    await login('amy', 'pw');
    expect(session.isAdmin).toBe(true);
    expect(session.user?.must_change_password).toBe(true);
    await changePassword('pw', 'new password 1');
    expect(session.user?.must_change_password).toBe(false);
    updateUser({ ...agent, role: 'admin', display_name: 'Amy' });
    expect(session.user?.display_name).toBe('Amy');
  });

  it('logout ends the session even when the server cannot be reached', async () => {
    stub({ '/auth/login': { status: 200, body: { user: agent, csrf: 'a' } } });
    await login('amy', 'pw');
    stub({ '/auth/logout': { status: 500, body: { error: { code: 'internal', message: 'x' } } } });
    await expect(logout()).rejects.toBeDefined();
    expect(session.user).toBeNull();
  });

  it('the remembered language survives a blocked storage', () => {
    storeLocale('en');
    expect(storedLocale()).toBe('en');
    vi.spyOn(Storage.prototype, 'getItem').mockImplementation(() => {
      throw new Error('blocked');
    });
    vi.spyOn(Storage.prototype, 'setItem').mockImplementation(() => {
      throw new Error('blocked');
    });
    expect(storedLocale()).toBeNull();
    expect(() => storeLocale('zh-TW')).not.toThrow();
    vi.restoreAllMocks();
  });
});
