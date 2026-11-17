import { useCallback, useEffect, useState } from 'react';

/** Presentation preference only: which of the two finished themes is shown. */
export type Theme = 'dark' | 'light';
export const THEME_KEY = 'postern.theme.v1';
const THEME_COLOR: Record<Theme, string> = { dark: '#0b0e0f', light: '#f2f0e9' };

const isTheme = (value: unknown): value is Theme => value === 'dark' || value === 'light';

/** The stored choice; without one, the system preference; dark when neither is known. */
export function initialTheme(): Theme {
  try {
    const stored = localStorage.getItem(THEME_KEY);
    if (isTheme(stored)) return stored;
  } catch {
    /* Storage unavailable: fall through to the system preference. */
  }
  try {
    return window.matchMedia?.('(prefers-color-scheme: light)').matches ? 'light' : 'dark';
  } catch {
    return 'dark';
  }
}

export function applyTheme(theme: Theme) {
  document.documentElement.dataset.theme = theme;
  document.querySelector('meta[name="theme-color"]')?.setAttribute('content', THEME_COLOR[theme]);
}

export function useTheme(): [Theme, () => void] {
  const [theme, setTheme] = useState<Theme>(() => {
    const current = document.documentElement.dataset.theme;
    return isTheme(current) ? current : initialTheme();
  });
  useEffect(() => applyTheme(theme), [theme]);
  useEffect(() => {
    const sync = (event: StorageEvent) => {
      if (event.key === THEME_KEY && isTheme(event.newValue)) setTheme(event.newValue);
    };
    window.addEventListener('storage', sync);
    return () => window.removeEventListener('storage', sync);
  }, []);
  const toggle = useCallback(() => {
    setTheme((current) => {
      const next = current === 'dark' ? 'light' : 'dark';
      try {
        localStorage.setItem(THEME_KEY, next);
      } catch {
        /* Continue with an in-memory preference. */
      }
      return next;
    });
  }, []);
  return [theme, toggle];
}
