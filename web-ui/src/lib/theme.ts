// The colour theme: following the system (prefers-color-scheme), or chosen by the person. Kept in
// this browser only (localStorage), like the language before sign-in; `data-theme` on <html> picks
// the colours in app.css.

export const THEMES = ['system', 'light', 'dark'] as const;
export type Theme = (typeof THEMES)[number];

const THEME_KEY = 'sipfax.theme';

export function storedTheme(): Theme {
  try {
    const t = localStorage.getItem(THEME_KEY);
    return (THEMES as readonly string[]).includes(t ?? '') ? (t as Theme) : 'system';
  } catch {
    return 'system';
  }
}

export function applyTheme(t: Theme, root: HTMLElement = document.documentElement): void {
  if (t === 'system') root.removeAttribute('data-theme');
  else root.setAttribute('data-theme', t);
}

export function setTheme(t: Theme): void {
  applyTheme(t);
  try {
    if (t === 'system') localStorage.removeItem(THEME_KEY);
    else localStorage.setItem(THEME_KEY, t);
  } catch {
    // private window: the choice lasts until the page is closed
  }
}
