// The signed-in account and the router state.

import { ApiError, get, post, setCsrf, type LoginOk, type User } from './api';
import { i18n, pickLocale, t } from './i18n.svelte';
import { parseRoute, type Route } from './route';
import { hasUnsaved, onBeforeUnload } from './unsaved';

let user = $state<User | null>(null);
let ready = $state(false);
let route = $state<Route>(parseRoute(typeof location === 'undefined' ? '' : location.hash));

export const session = {
  get user(): User | null {
    return user;
  },
  get ready(): boolean {
    return ready;
  },
  get isAdmin(): boolean {
    return user?.role === 'admin';
  },
};

export const router = {
  get route(): Route {
    return route;
  },
};

if (typeof window !== 'undefined') {
  // leaving a page with unsaved changes asks first; "stay" puts the address back without a new
  // history entry
  window.addEventListener('hashchange', (e) => {
    if (hasUnsaved() && !confirm(t('common.leave_unsaved'))) {
      history.replaceState(history.state, '', new URL(e.oldURL).hash || '#');
      return;
    }
    route = parseRoute(location.hash);
  });
  window.addEventListener('beforeunload', onBeforeUnload);
}

function browserLanguages(): readonly string[] {
  return typeof navigator === 'undefined' ? [] : navigator.languages;
}

/** Remembered choice of the language switch (a per-browser convenience). */
const LOCALE_KEY = 'sipfax.locale';

export function storedLocale(): string | null {
  try {
    return localStorage.getItem(LOCALE_KEY);
  } catch {
    return null;
  }
}

export function storeLocale(l: string): void {
  try {
    localStorage.setItem(LOCALE_KEY, l);
  } catch {
    // private window
  }
}

function signedIn(ok: { user: User; csrf: string }): void {
  setCsrf(ok.csrf);
  user = ok.user;
  i18n.set(pickLocale(ok.user.locale ?? storedLocale(), browserLanguages()));
}

/** Asks the server whether the cookie is still a session. */
export async function restore(): Promise<void> {
  try {
    signedIn(await get<LoginOk>('/auth/me'));
  } catch (e) {
    if (!(e instanceof ApiError) || (e.status !== 401 && e.status !== 0)) throw e;
    user = null;
    i18n.set(pickLocale(storedLocale(), browserLanguages()));
  } finally {
    ready = true;
  }
}

export async function login(username: string, password: string): Promise<void> {
  signedIn(await post<LoginOk>('/auth/login', { username, password }));
}

export async function changePassword(current: string, next: string): Promise<void> {
  signedIn(await post<LoginOk>('/auth/password', { current, new: next }));
}

export async function logout(): Promise<void> {
  try {
    await post('/auth/logout');
  } finally {
    ended();
  }
}

/** The session is gone (401, a closed event stream): back to the login. */
export function ended(): void {
  setCsrf(null);
  user = null;
}

export function updateUser(u: User): void {
  user = u;
}
