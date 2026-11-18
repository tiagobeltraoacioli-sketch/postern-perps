import { useSyncExternalStore } from 'react';

/** A value with subscribers. Components read it with `useStore`; writers call `set` (at most once per frame for feeds). */
export class Store<T> {
  private listeners = new Set<() => void>();
  constructor(private value: T) {}
  get = () => this.value;
  set(next: T) {
    if (Object.is(next, this.value)) return;
    this.value = next;
    for (const l of [...this.listeners]) l();
  }
  update(f: (current: T) => T) {
    this.set(f(this.value));
  }
  subscribe = (listener: () => void) => {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  };
}

export function useStore<T>(store: Store<T>): T {
  return useSyncExternalStore(store.subscribe, store.get, store.get);
}

/** A store persisted to localStorage (presentation preferences only: layout, theme, form defaults). */
export function persisted<T>(key: string, initial: T, valid: (v: unknown) => v is T): Store<T> {
  let value = initial;
  try {
    const raw = localStorage.getItem(key);
    if (raw !== null) {
      const parsed: unknown = JSON.parse(raw);
      if (valid(parsed)) value = parsed;
    }
  } catch {
    /* unreadable: defaults */
  }
  const store = new Store<T>(value);
  store.subscribe(() => {
    try {
      localStorage.setItem(key, JSON.stringify(store.get()));
    } catch {
      /* in-memory only */
    }
  });
  return store;
}
