import { kasPerTokenToUsd, rateTitle, sompiToUsd, usdText, usdTextFit, type Fiat } from '../../app/fiat';
import { pow10, SOMPI_PER_KAS } from '../../kob/units';
import { amountText, groupDigits, scaleOfDecimals, type AmountKind } from './format';

export interface AmountProps {
  kind: AmountKind;
  /** sompi (kas), a state price in sompi per whole token of `scale` base units (price) or token base units (token); null / undefined renders a dash */
  value: bigint | null | undefined;
  /** token decimals (token, price) */
  decimals?: number;
  /** the price's scale: base units per whole token of its denominator (price; default 10^decimals capped at 10^9) */
  scale?: bigint;
  /** cap the fraction digits (dense tables) */
  maxFraction?: number;
  /** exactly this many fraction digits, trailing zeros kept: the fixed decimals of a column (decimal points line up) */
  fraction?: number;
  /** unit text after the number; `true` = the default unit of the kind (KAS) */
  unit?: string | boolean;
  /** thousands separators (display only: the title always carries the exact plain decimal) */
  group?: boolean;
  /** `+` sign for positive values (deltas) */
  signed?: boolean;
  /**
   * The USD display in force (app/fiat.ts, `useFiat()`): a `kas` or `price` value is shown as dollars (`$1,066.77`, compact past `width` characters) with its exact KAS figure and
   * the rate used in the title; a `token` value is never converted. null / absent: KAS as before. The one place a read-only KAS value turns into dollars.
   */
  fiat?: Fiat | null;
  /** characters of the dollar text before it turns compact (`$56.61M`); default: never compact */
  width?: number;
  class?: string;
  'data-testid'?: string;
}

/** The dollars of a `kas` or `price` value, exactly (the KAS value is a rational of the rate). */
function usdOf(props: AmountProps, fiat: Fiat) {
  const v = props.value as bigint;
  if (props.kind === 'kas') return sompiToUsd(v, fiat.rate);
  const decimals = props.decimals ?? 0;
  const scale = props.scale ?? scaleOfDecimals(decimals);
  // KAS per whole token = price x 10^decimals / (scale x 1e8)
  return kasPerTokenToUsd({ num: v * pow10(decimals), den: (scale > 0n ? scale : 1n) * SOMPI_PER_KAS }, fiat.rate);
}

/**
 * A money amount, exact (bigint arithmetic through units.ts). The visible text is a plain decimal without grouping by default so a
 * selection pastes as a number; the `title` and `data-value` carry the exact value. With `fiat` a KAS value reads as dollars and the title keeps the KAS figure.
 */
export function Amount(props: AmountProps) {
  const missing = props.value === null || props.value === undefined;
  if (props.fiat && !missing && (props.kind === 'kas' || props.kind === 'price')) {
    const usd = usdOf(props, props.fiat);
    const text = props.width !== undefined ? usdTextFit(usd, props.width) : usdText(usd);
    const shown = props.signed && (props.value as bigint) > 0n ? `+${text}` : text;
    const exactKas = amountText({ ...props, fraction: undefined, maxFraction: undefined });
    const kasUnit = typeof props.unit === 'string' ? props.unit : 'KAS';
    const title = [text !== usdText(usd) ? usdText(usd) : null, `${groupDigits(exactKas)} ${kasUnit}`, rateTitle(props.fiat)].filter(Boolean).join(' · ');
    return (
      <span class={`amount num${props.class ? ` ${props.class}` : ''}`} translate={false} title={title} data-value={exactKas} data-currency="usd" data-testid={props['data-testid']}>
        {shown}
      </span>
    );
  }
  const text = amountText(props);
  // the title / data-value carry the exact value (trimmed), whatever fixed decimals the visible text of a column uses
  const plain = missing ? '' : props.fraction !== undefined ? amountText({ ...props, fraction: undefined }) : text;
  const shown = props.group ? groupDigits(text) : text;
  const withSign = props.signed && !missing && (props.value as bigint) > 0n ? `+${shown}` : shown;
  const defaultUnit = props.kind === 'kas' ? 'KAS' : '';
  const unit = typeof props.unit === 'string' ? props.unit : props.unit === false ? '' : defaultUnit;
  return (
    <span class={`amount num${props.class ? ` ${props.class}` : ''}`} translate={false} title={plain || undefined} data-value={plain || undefined} data-testid={props['data-testid']}>
      {withSign}
      {unit && !missing ? <span class="amount-unit">{unit}</span> : null}
    </span>
  );
}
