/** Toasts: short confirmations of what happened. Never the only place a required text appears. */
import { useEffect } from 'react';
import { Store, useStore } from '../lib/store';

export type ToastTone = 'ok' | 'info' | 'warn' | 'danger';
type Toast = { id: number; tone: ToastTone; title: string; text?: string };
const toasts = new Store<Toast[]>([]);
let nextId = 0;

export function toast(tone: ToastTone, title: string, text?: string) {
  const id = ++nextId;
  toasts.update((list) => [...list.slice(-3), { id, tone, title, text }]);
  setTimeout(() => dismiss(id), tone === 'danger' ? 12_000 : 6_000);
}
export const dismiss = (id: number) => toasts.update((list) => list.filter((t) => t.id !== id));

export function Toaster({ events }: { events?: Store<{ id: number; kind: string; text: string }[]> }) {
  const list = useStore(toasts);
  return (
    <div className="toaster" role="status" aria-live="polite">
      {events && <FeedEvents events={events} />}
      {list.map((t) => (
        <div key={t.id} className={`toast toast-${t.tone}`}>
          <span className="toast-lamp" aria-hidden="true" />
          <div>
            <strong>{t.title}</strong>
            {t.text && <p>{t.text}</p>}
          </div>
          <button type="button" className="icon-button toast-close" aria-label="Dismiss" onClick={() => dismiss(t.id)}>
            ×
          </button>
        </div>
      ))}
    </div>
  );
}

/** Announces fills and liquidations from the account channel once each. */
function FeedEvents({ events }: { events: Store<{ id: number; kind: string; text: string }[]> }) {
  const list = useStore(events);
  useEffect(() => {
    const last = list[list.length - 1];
    if (!last) return;
    toast(last.kind === 'liquidation' ? 'danger' : 'ok', last.kind === 'liquidation' ? 'Liquidation' : 'Fill', last.text);
  }, [list]);
  return null;
}
