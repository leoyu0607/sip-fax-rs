import { afterEach, describe, expect, it } from 'vitest';
import { applyTheme, setTheme, storedTheme } from './theme';

afterEach(() => {
  localStorage.clear();
  document.documentElement.removeAttribute('data-theme');
});

describe('theme', () => {
  it('follows the system when nothing (or something unknown) is stored', () => {
    expect(storedTheme()).toBe('system');
    localStorage.setItem('sipfax.theme', 'purple');
    expect(storedTheme()).toBe('system');
  });

  it('stores a choice and puts it on <html>; system removes both', () => {
    setTheme('dark');
    expect(storedTheme()).toBe('dark');
    expect(document.documentElement.getAttribute('data-theme')).toBe('dark');
    setTheme('light');
    expect(storedTheme()).toBe('light');
    expect(document.documentElement.getAttribute('data-theme')).toBe('light');
    setTheme('system');
    expect(localStorage.getItem('sipfax.theme')).toBeNull();
    expect(document.documentElement.hasAttribute('data-theme')).toBe(false);
  });

  it('applies to the element it is given', () => {
    const el = document.createElement('div');
    applyTheme('dark', el);
    expect(el.dataset.theme).toBe('dark');
    applyTheme('system', el);
    expect(el.hasAttribute('data-theme')).toBe(false);
  });
});
