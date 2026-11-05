// The standalone-mode state for the views (data/standalone.ts): a component that shows what depends on it (the banner, the book label, the "since"
// notes of the charts and figures) reads it here and re-renders when it changes.
import { useEffect, useState } from 'preact/hooks';
import type { StandaloneController, StandaloneState } from '../../data/standalone';

/** The standalone state now (null: no standalone mode in these services), kept current. */
export function useStandalone(c: StandaloneController | null | undefined): StandaloneState | null {
  const [state, setState] = useState<StandaloneState | null>(c ? c.state : null);
  useEffect(() => {
    if (!c) return setState(null);
    setState(c.state);
    return c.subscribe(setState);
  }, [c]);
  return state;
}

/** `HH:MM` (local time) of a moment, for "since HH:MM". */
export function clockText(ms: number): string {
  const d = new Date(ms);
  return `${String(d.getHours()).padStart(2, '0')}:${String(d.getMinutes()).padStart(2, '0')}`;
}
