import { amountText, groupDigits, type AmountKind } from './format';

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
  class?: string;
  'data-testid'?: string;
}

/**
 * A money amount, exact (bigint arithmetic through units.ts). The visible text is a plain decimal without grouping by default so a
 * selection pastes as a number; the `title` and `data-value` carry the exact value.
 */
export function Amount(props: AmountProps) {
  const text = amountText(props);
  const missing = props.value === null || props.value === undefined;
  // the title / data-value carry the exact value (trimmed), whatever fixed decimals the visible text of a column uses
  const plain = missing ? '' : props.fraction !== undefined ? amountText({ ...props, fraction: undefined }) : text;
  const shown = props.group ? groupDigits(text) : text;
  const withSign = props.signed && !missing && (props.value as bigint) > 0n ? `+${shown}` : shown;
  const defaultUnit = props.kind === 'kas' ? 'KAS' : '';
  const unit = typeof props.unit === 'string' ? props.unit : props.unit === false ? '' : defaultUnit;
  return (
    <span class={`amount num${props.class ? ` ${props.class}` : ''}`} title={plain || undefined} data-value={plain || undefined} data-testid={props['data-testid']}>
      {withSign}
      {unit && !missing ? <span class="amount-unit">{unit}</span> : null}
    </span>
  );
}
