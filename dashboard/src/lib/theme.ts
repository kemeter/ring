/** Dashboard theme persistence.
 *
 * Two modes: `dark` (default) and `light`. The choice is persisted in
 * localStorage under `ring.theme`. When nothing is saved we follow the
 * `prefers-color-scheme` media query.
 *
 * The active theme is reflected as `data-theme="light"` (or absent for dark)
 * on `<html>`, so `app.css` only needs to override variables under the
 * `:root[data-theme="light"]` selector.
 */

export type Theme = 'dark' | 'light';

const STORAGE_KEY = 'ring.theme';

function systemPreference(): Theme {
  if (typeof window === 'undefined' || !window.matchMedia) {
    return 'dark';
  }
  return window.matchMedia('(prefers-color-scheme: light)').matches ? 'light' : 'dark';
}

export function loadTheme(): Theme {
  if (typeof localStorage === 'undefined') {
    return 'dark';
  }
  const stored = localStorage.getItem(STORAGE_KEY);
  if (stored === 'dark' || stored === 'light') {
    return stored;
  }
  return systemPreference();
}

export function applyTheme(theme: Theme): void {
  if (typeof document === 'undefined') {
    return;
  }
  if (theme === 'light') {
    document.documentElement.setAttribute('data-theme', 'light');
  } else {
    document.documentElement.removeAttribute('data-theme');
  }
}

export function saveTheme(theme: Theme): void {
  if (typeof localStorage === 'undefined') {
    return;
  }
  localStorage.setItem(STORAGE_KEY, theme);
}
