// The amount slider under the ticket's amount box: 0..100 % of Max in 1 % steps, with snap buttons at 0 / 25 / 50 / 75 / 100 % (amount-slider.ts).
// Keyboard: the range takes the arrow keys (1 %), Page Up / Down, Home / End; every snap point is a button.
import { t } from '../../i18n';
import { SLIDER_POINTS, amountPercent, pickAmount, pointEnabled } from './amount-slider';

export interface AmountSliderProps {
  /** what Max gives now (base units); null while it is not known (no wallet, no environment, being computed) */
  max: bigint | null;
  /** the amount of the box (base units), null when empty or malformed */
  amount: bigint | null;
  /** the order's own minimum fill (base units), null = the default */
  minFill: bigint | null;
  /** the amount text of `base` units in the box's unit, for the screen reader */
  describe(base: bigint): string;
  /** sets the amount (null: empties the box) */
  onPick(base: bigint | null): void;
  disabled?: boolean;
}

export function AmountSlider(p: AmountSliderProps) {
  const off = !!p.disabled || p.max === null || p.max <= 0n;
  const pct = amountPercent(p.amount, p.max) ?? 0;
  const pick = (v: number) => {
    if (!off) p.onPick(pickAmount(v, p.max, p.minFill));
  };
  const valueText = off || p.amount === null ? `${pct}%` : t('ticket.slider.value', { pct, amount: p.describe(p.amount) });
  return (
    <div class="tk-slider" data-testid="order-amount-slider" data-pct={off ? '' : String(pct)}>
      <input
        type="range"
        class="tk-slider-range"
        min={0}
        max={100}
        step={1}
        value={pct}
        disabled={off}
        aria-label={t('ticket.slider.aria')}
        aria-valuetext={valueText}
        onInput={(e) => pick(Number((e.currentTarget as HTMLInputElement).value))}
        data-testid="order-amount-range"
      />
      <div class="tk-slider-points" role="group" aria-label={t('ticket.slider.points')}>
        {SLIDER_POINTS.map((v) => {
          const ok = !off && pointEnabled(v, p.max, p.minFill);
          return (
            <button
              key={v}
              type="button"
              class="tk-slider-pt"
              aria-pressed={!off && pct === v}
              disabled={!ok}
              title={!off && !ok ? t('ticket.slider.belowMin') : undefined}
              onClick={() => pick(v)}
              data-testid={`order-amount-pct-${v}`}
            >
              {v}%
            </button>
          );
        })}
      </div>
    </div>
  );
}
