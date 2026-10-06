// The orientation of one market as component state: the remembered choice (localStorage, per market) over the convention.
import { useState } from 'preact/hooks';
import { readInverted, writeInverted } from './orientation';

/** `[inverted, toggle, set]`: `defaultInverted` applies until the user flips the market once; from then on the remembered choice wins. */
export function useInverted(market: string, defaultInverted: boolean): [boolean, () => void, (v: boolean) => void] {
  const [stored, setStored] = useState<boolean | null>(() => readInverted(market));
  const inverted = stored ?? defaultInverted;
  const set = (next: boolean) => {
    setStored(next);
    writeInverted(market, next);
  };
  return [inverted, () => set(!inverted), set];
}
