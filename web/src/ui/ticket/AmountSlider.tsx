// The amount slider under the ticket's amount box, the width of the ticket: 0..100 % of Max in 1 % steps. Tick marks at 0 / 25 / 50 / 75 / 100 % with
// their labels in one row directly under the track, aligned to the ticks; the labels are the buttons that set that point (amount-slider.ts). While the
// thumb is dragged it snaps to a tick within +-3 % and a small bubble above it reads the value; the filled track is the colour of the side (green for a
// buy, red for a sell). Keyboard: the range takes the arrow keys (1 %), Home / End, Page Up / Page Down move it by 25 %. The thumb's touch target is 44 px on
// a phone. Percent of Max is what it always was (of the available balance, or of the KAS budget when buying).
import { useEffect, useRef, useState } from 'preact/hooks';
import { t } from '../../i18n';
import { PAGE_STEP, SLIDER_POINTS, pickAmount, picked, pointEnabled, snapPct, trackPercent, type SliderMemory } from './amount-slider';

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
  /** the side of the order: the filled track is green for a buy, red for a sell */
  side?: 'buy' | 'sell';
  disabled?: boolean;
}

export function AmountSlider(p: AmountSliderProps) {
  const off = !!p.disabled || p.max === null || p.max <= 0n;
  // the thumb keeps its position through ticks: it is converted from the amount once, when the amount changes (amount-slider.ts `trackPercent`)
  const memory = useRef<SliderMemory | null>(null);
  memory.current = trackPercent(memory.current, p.amount, p.max);
  const pct = memory.current.pct;
  const [dragging, setDragging] = useState(false);
  const drag = useRef(false);
  // the pointer is released anywhere, also off the thumb or after the finger left the page
  useEffect(() => {
    const end = () => {
      if (!drag.current) return;
      drag.current = false;
      setDragging(false);
    };
    window.addEventListener('pointerup', end);
    window.addEventListener('pointercancel', end);
    return () => {
      window.removeEventListener('pointerup', end);
      window.removeEventListener('pointercancel', end);
    };
  }, []);
  const pick = (v: number) => {
    if (off) return;
    const a = pickAmount(v, p.max, p.minFill);
    memory.current = picked(v, a);
    p.onPick(a);
  };
  const valueText = off || p.amount === null ? `${pct}%` : t('ticket.slider.value', { pct, amount: p.describe(p.amount) });
  return (
    <div class="tk-slider" data-testid="order-amount-slider" data-pct={String(pct)} data-side={p.side ?? 'buy'} data-dragging={dragging ? '1' : '0'} style={`--p:${pct / 100}`}>
      <div class="tk-slider-track">
        <output class="tk-slider-bubble" aria-hidden="true" hidden={!dragging || off} data-testid="order-amount-bubble">{`${pct}%`}</output>
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
          onPointerDown={() => {
            drag.current = true;
            setDragging(true);
          }}
          onInput={(e) => {
            const input = e.currentTarget as HTMLInputElement;
            let v = Number(input.value);
            // magnetic: only while the thumb is dragged (the keyboard moves by exact steps)
            if (drag.current) {
              const s = snapPct(v);
              if (s !== v) {
                v = s;
                input.value = String(s);
              }
            }
            pick(v);
          }}
          onKeyDown={(e) => {
            if (e.key !== 'PageUp' && e.key !== 'PageDown') return;
            e.preventDefault();
            pick(Math.max(0, Math.min(100, pct + (e.key === 'PageUp' ? PAGE_STEP : -PAGE_STEP))));
          }}
          data-testid="order-amount-range"
        />
      </div>
      <div class="tk-slider-ticks" role="group" aria-label={t('ticket.slider.points')}>
        {SLIDER_POINTS.map((v) => {
          const ok = !off && pointEnabled(v, p.max, p.minFill);
          return (
            <button
              key={v}
              type="button"
              class="tk-slider-pt"
              style={`--v:${v / 100}`}
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
