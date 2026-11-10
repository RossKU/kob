// The views' side of the optional USD display (app/fiat.ts decides, app/kas-price.ts fetches): which currency a value is shown in right now, the one-line note that
// says so, and the loop that keeps the price fresh while USD is chosen.
import { useEffect, useMemo, useState } from 'preact/hooks';
import { onVisibilityChange } from '../ui/kit/hooks';
import { useServices } from './context';
import { useDisplayCurrency } from './currency-pref';
import { estimateNote, fallbackNotice, fiatOf, resolveFiat, type Fiat, type FiatState } from './fiat';
import { kasPrice, startKasPriceFeed, type PriceSnapshot } from './kas-price';

function usePriceSnapshot(): PriceSnapshot {
  const [snap, setSnap] = useState(() => kasPrice.snapshot);
  useEffect(() => {
    setSnap(kasPrice.snapshot);
    return kasPrice.subscribe(() => setSnap(kasPrice.snapshot));
  }, []);
  return snap;
}

/** What is shown now: KAS, USD, or KAS with the reason USD is not in force. Re-judged as the quote ages. */
export function useFiatState(): FiatState {
  const { config } = useServices();
  const [currency] = useDisplayCurrency();
  const snap = usePriceSnapshot();
  const testnet = config.network === 'testnet-10';
  return useMemo(() => resolveFiat({ currency, feed: snap, nowMs: snap.nowMs, testnet }), [currency, snap, testnet]);
}

/** The USD display in force, or null (show KAS). Stable while the quote does not change, so a view that memoises on it is not redone every tick. */
export function useFiat(): Fiat | null {
  const f = fiatOf(useFiatState());
  const quote = f?.quote;
  const testnet = f?.testnet;
  return useMemo(() => f, [quote, testnet]);
}

/** The loop that reads the price: mounted once for the app, running only while USD is the display currency. */
export function KasPriceFeed() {
  const { indexer } = useServices();
  const [currency] = useDisplayCurrency();
  useEffect(() => {
    if (currency !== 'usd') return;
    return startKasPriceFeed({ indexer, onVisibility: onVisibilityChange });
  }, [indexer, currency]);
  return null;
}

/**
 * The one muted line under a view that shows values: what the USD is (the source, how old, "an estimate, not a quote", and on testnet that its KAS has no value),
 * or why KAS is shown although USD was chosen. Nothing while KAS is the display currency.
 */
export function FiatNote(props: { class?: string }) {
  const s = useFiatState();
  if (s.kind === 'kas') return null;
  const cls = `small muted fiat-note${props.class ? ` ${props.class}` : ''}`;
  if (s.kind === 'fallback') {
    return (
      <p class={cls} data-testid="fiat-notice" data-fiat="fallback" data-reason={s.reason}>
        {fallbackNotice(s)}
      </p>
    );
  }
  return (
    <p class={cls} data-testid="fiat-note" data-fiat="usd" data-source={s.fiat.quote.source}>
      {estimateNote(s.fiat, s.ageS)}
    </p>
  );
}
