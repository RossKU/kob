// The views' side of the optional USD display. app/fiat.ts decides and app/kas-price.ts fetches.
// This file holds:
// - which currency a value is shown in right now,
// - the one-line note that says so,
// - the loop that keeps the price fresh while USD is chosen.
import { useEffect, useMemo, useState } from 'preact/hooks';
import { onVisibilityChange } from '../ui/kit/hooks';
import { useServices } from './context';
import { readCurrencyPref, useDisplayCurrency } from './currency-pref';
import { estimateNote, fallbackNotice, fiatOf, resolveFiat, type Fiat, type FiatState, type Rate } from './fiat';
import { kasPrice, startKasPriceFeed, type PriceSnapshot } from './kas-price';

function usePriceSnapshot(): PriceSnapshot {
  const [snap, setSnap] = useState(() => kasPrice.snapshot);
  useEffect(() => {
    setSnap(kasPrice.snapshot);
    return kasPrice.subscribe(() => setSnap(kasPrice.snapshot));
  }, []);
  return snap;
}

/**
 * What is shown now: KAS, USD, or KAS with the reason USD is not in force. It is judged again as
 * the quote ages.
 */
export function useFiatState(): FiatState {
  const { config } = useServices();
  const [currency] = useDisplayCurrency();
  const snap = usePriceSnapshot();
  const testnet = config.network === 'testnet-10';
  return useMemo(() => resolveFiat({ currency, feed: snap, nowMs: snap.nowMs, testnet }), [currency, snap, testnet]);
}

/**
 * The USD display in force, or null to show KAS. It is stable while the quote does not change, so a
 * view that memoises on it is not redone every tick.
 */
export function useFiat(): Fiat | null {
  const f = fiatOf(useFiatState());
  const quote = f?.quote;
  const testnet = f?.testnet;
  return useMemo(() => f, [quote, testnet]);
}

/**
 * The KAS price in USD per KAS, for a figure that needs it whatever the display currency. One is
 * the thin-book marker of the market views, "under $100".
 *
 * It is the fresh quote's rate, else null, and never a stale or guessed one. Mounting the view
 * keeps the price loop running (`kasPrice.want`) although KAS is shown. It is stable while the
 * quote does not change.
 */
export function useKasRate(): Rate | null {
  const snap = usePriceSnapshot();
  useEffect(() => kasPrice.want(), []);
  const f = fiatOf(resolveFiat({ currency: 'usd', feed: snap, nowMs: snap.nowMs, testnet: false }));
  const quote = f?.quote;
  return useMemo(() => f?.rate ?? null, [quote]);
}

/**
 * The USD display in force right now, read outside a component, such as for a toast made when an
 * event arrives. Null means KAS.
 */
export function currentFiat(testnet: boolean, nowMs = Date.now()): Fiat | null {
  return fiatOf(resolveFiat({ currency: readCurrencyPref(), feed: kasPrice.snapshot, nowMs, testnet }));
}

/**
 * The loop that reads the price. It is mounted once for the app, and runs only while USD is the
 * display currency or a market view wants the rate.
 */
export function KasPriceFeed() {
  const { indexer } = useServices();
  const [currency] = useDisplayCurrency();
  // It also runs while a market view wants the rate for its thin-book marker (`useKasRate`).
  const [wanted, setWanted] = useState(() => kasPrice.isWanted);
  useEffect(() => {
    setWanted(kasPrice.isWanted);
    return kasPrice.subscribeWanted(() => setWanted(kasPrice.isWanted));
  }, []);
  const active = currency === 'usd' || wanted;
  useEffect(() => {
    if (!active) return;
    return startKasPriceFeed({ indexer, onVisibility: onVisibilityChange });
  }, [indexer, active]);
  return null;
}

/**
 * The one muted line under a view that shows values. Nothing shows while KAS is the display
 * currency.
 *
 * With `full`, which only Settings sets, it says what the USD is: the source, how old it is, "an
 * estimate, not a quote", and on testnet that its KAS has no value. Any other page says only why it
 * shows KAS although USD was chosen.
 */
export function FiatNote(props: { class?: string; full?: boolean }) {
  const s = useFiatState();
  if (s.kind === 'kas') return null;
  if (s.kind !== 'fallback' && !props.full) return null;
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
