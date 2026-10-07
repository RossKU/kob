// The input fields of the ticket, rendered from the layout of `form-state.ts`. Thin: every rule (parsing, rounding, which fields exist) lives there.
import type { ComponentChildren } from 'preact';
import { useState } from 'preact/hooks';
import { has, t } from '../../i18n';
import { formatUnits, safeRounding } from '../../kob/units';
import { invertedToNativePriceText, nativeToInvertedPriceText } from '../market/orientation';
import { Checkbox, Field, FieldShell, Segmented } from '../kit';
import {
  BASE_SUFFIX, FIELDS, FIELD_DEFAULTS, TOUCH_MIN, TOUCH_PRESETS, amountBaseOf, amountPriceOf, kasTextOf, parseTouch, priceInfo,
  type FieldError, type FieldSpec, type Side, type TicketCtx, type TicketForm,
} from './form-state';

export interface TicketFieldsProps {
  form: TicketForm;
  ctx: TicketCtx;
  ticker: string;
  ids: string[];
  /** parse errors of non-empty fields (the caller filters out "required") */
  errors: FieldError[];
  onChange(id: string, value: string): void;
  /** "max" button of the amount field */
  onMax?: () => void;
  maxLabel?: string;
  disabled?: boolean;
  /**
   * the market is shown inverted (KAS/TOKEN, orientation.ts): the PRICE inputs take tokens per KAS (`name` per KAS). The form still keeps native text
   * (KAS per token): what is typed is converted on input, rounded the way that never makes the limit worse, and the unit hint says what the order is.
   */
  flip?: FlipCtx;
  /** a token/token pair: the ticker of the quote token B the prices are typed in (B per whole `ticker`); absent = KAS. Tips stay KAS. */
  quoteTicker?: string;
}

/** What the fields need to speak the shown (inverted) market: the token's name, its scale and a KAS price per whole token to turn amounts into KAS. */
export interface FlipCtx {
  name: string;
  /** a reference price (the state price: sompi per whole token of `scale` base units): the form's own price when it parses, else the best in the book; null when there is none */
  price?: bigint | null;
  /** `price` is the form's own price (else the book's) */
  ownPrice?: boolean;
  scale?: bigint;
  decimals?: number;
}

/** Exit legs of an if-done order are on the OPPOSITE side of the entry: their prices round the other way. */
export const sideOfField = (form: TicketForm, id: string): Side => (id.startsWith('exit.') && id !== 'exit.tip' && id !== 'exit.minFill' ? (form.side === 'buy' ? 'sell' : 'buy') : form.side);

export const labelKey = (type: string, id: string): string => (has(`ticket.label.${id}.${type}`) ? `ticket.label.${id}.${type}` : `ticket.label.${id}`);

const UNIT_KEY: Partial<Record<FieldSpec['kind'], string>> = { percent: 'ticket.unit.percent', seconds: 'ticket.unit.seconds', minutes: 'ticket.unit.minutes' };

function errorText(e: FieldError): string {
  return t(`ticket.err.${e.code}`, (e.params ?? {}) as Record<string, string | number>);
}

/** The basis of a KAS amount's conversion as words (`ticket.kasAmt.basis.*`). */
const BASIS_KEY = { own: 'ticket.kasAmt.basis.own', stop: 'ticket.kasAmt.basis.stop', exit: 'ticket.kasAmt.basis.exit', book: 'ticket.kasAmt.basis.book' } as const;

/**
 * The line under a KAS amount box (a `kas` form): the exact token amount the order is built with and the exact KAS it is worth at the price the
 * amount converts at, or what is still missing to know it.
 */
function KasAmountHint(props: { form: TicketForm; ctx: TicketCtx; id: string; ticker: string; testid: string }) {
  const { form, ctx, id } = props;
  const base = amountBaseOf(form, ctx, id);
  if (!base.ok || base.value <= 0n) {
    const p = amountPriceOf(form, ctx, id);
    return !p && (form.values[id] ?? '') !== '' ? <span class="tk-sub" data-testid={`${props.testid}-tokens`}>{t('ticket.kasAmt.needPrice', { ticker: props.ticker })}</span> : null;
  }
  const p = amountPriceOf(form, ctx, id);
  return (
    <span class="tk-sub" data-testid={`${props.testid}-tokens`} data-base={base.value.toString()}>
      {p
        ? t('ticket.kasAmt.is', { kas: kasTextOf(form, ctx, id, base.value), tokens: formatUnits(base.value, ctx.decimals, { group: ',' }), ticker: props.ticker, basis: t(BASIS_KEY[p.basis]) })
        : t('ticket.kasAmt.kept', { tokens: formatUnits(base.value, ctx.decimals, { group: ',' }), ticker: props.ticker })}
    </span>
  );
}

export function TicketFields(props: TicketFieldsProps) {
  const { form, ctx, ticker } = props;
  /** the text an amount box shows: the KAS value of an exact amount kept for it (a `kas` form), else what was typed */
  const amountText = (id: string): string => {
    const kept = form.values[id + BASE_SUFFIX];
    return form.kas && kept !== undefined ? kasTextOf(form, ctx, id, BigInt(kept)) : form.values[id] ?? '';
  };
  /** the unit of an amount box: KAS in a `kas` form, else the token */
  const amountUnit = form.kas ? 'KAS' : ticker;
  const errOf = (id: string) => props.errors.find((e) => e.field === id);
  // what the user typed in an inverted price box (the form's own text is the converted native price): kept while it still produces the form's text
  const [typed, setTyped] = useState<Record<string, string>>({});
  return (
    <>
      {props.ids.map((id) => {
        const spec = FIELDS[id];
        if (!spec) return null;
        const value = form.values[id] ?? '';
        const e = errOf(id);
        // the inverted market drops "(per token)": the unit next to the box says what the amount is
        const label = t(props.flip && has(`ticket.label.${id}.inv`) ? `ticket.label.${id}.inv` : labelKey(form.type, id));
        // an inverted market has its own wording where the native help speaks of buying / selling the token
        const helpKey = props.flip && has(`ticket.help.${id}.inv`) ? `ticket.help.${id}.inv` : `ticket.help.${id}`;
        const help = has(helpKey) ? t(helpKey, { ticker }) : undefined;
        const phKey = has(`ticket.ph.${id}.${form.type}`) ? `ticket.ph.${id}.${form.type}` : `ticket.ph.${id}`;
        const defaultHint = value === '' && FIELD_DEFAULTS[id] === undefined && has(phKey) ? t(phKey) : undefined;
        const common = { label, error: e ? errorText(e) : null, 'data-testid': spec.testid, disabled: props.disabled };

        if (spec.kind === 'bool') {
          return (
            <Checkbox key={id} checked={value === 'true'} onChange={(c) => props.onChange(id, c ? 'true' : '')} label={label} hint={help} data-testid={spec.testid} disabled={props.disabled} />
          );
        }
        if (spec.kind === 'select') {
          return (
            <FieldShell key={id} label={label} hint={help} error={common.error}>
              {({ id: cid, describedBy, invalid }) => (
                <select
                  id={cid}
                  class="select"
                  value={value || FIELD_DEFAULTS[id] || spec.options?.[0]}
                  aria-describedby={describedBy}
                  aria-invalid={invalid ? 'true' : undefined}
                  data-testid={spec.testid}
                  disabled={props.disabled}
                  onChange={(ev) => props.onChange(id, (ev.currentTarget as HTMLSelectElement).value)}
                >
                  {(spec.options ?? []).map((o) => (
                    <option key={o} value={o}>
                      {t(`ticket.opt.${id}.${o}`)}
                    </option>
                  ))}
                </select>
              )}
            </FieldShell>
          );
        }
        if (spec.kind === 'touch') {
          // trigger threshold: presets (the order's minimum fill / 25% / 50% / 100% of its own amount) plus a custom token amount, all in the one text value
          const shown = value === '' && form.values[id + BASE_SUFFIX] === undefined ? (FIELD_DEFAULTS[id] ?? TOUCH_MIN) : value;
          const preset = (TOUCH_PRESETS as readonly string[]).includes(shown) ? shown : '';
          const order = amountBaseOf(form, ctx, 'amount');
          const resolved = shown.endsWith('%') ? parseTouch(shown, order.ok ? order.value : undefined, ctx.decimals) : null;
          const tok = (v: bigint): string => `${formatUnits(v, ctx.decimals, { group: ',' })} ${ticker}`;
          const touchHint = (
            <>
              {resolved && resolved.ok && resolved.value !== null && order.ok ? (
                <span class="tk-sub" data-testid={`${spec.testid}-resolved`}>{t('ticket.touch.resolved', { amount: tok(resolved.value), order: tok(order.value) })}</span>
              ) : null}
              {shown === TOUCH_MIN ? <span class="tk-sub" data-testid={`${spec.testid}-min`}>{t('ticket.touch.minHint')}</span> : null}
              {form.kas && preset === '' ? <KasAmountHint form={form} ctx={ctx} id={id} ticker={ticker} testid={spec.testid} /> : null}
              {help ? <span class="tk-help">{help}</span> : null}
              <span class="tk-help">{t('ticket.touch.tradeoff')}</span>
            </>
          );
          return (
            <FieldShell key={id} label={label} hint={touchHint} error={common.error}>
              {({ id: cid, describedBy, invalid }) => (
                <div class="tk-touch" data-testid={`${spec.testid}-presets`}>
                  <Segmented<string>
                    aria-label={label}
                    value={preset}
                    disabled={props.disabled}
                    options={TOUCH_PRESETS.map((o) => ({ value: o, label: o === TOUCH_MIN ? t('ticket.touch.preset.min') : o, 'data-testid': `${spec.testid}-preset-${o.replace('%', '')}` }))}
                    onChange={(v) => props.onChange(id, v)}
                  />
                  <span class="tk-suffix">
                    <input
                      id={cid}
                      class="input"
                      inputMode="decimal"
                      value={preset !== '' ? '' : amountText(id)}
                      placeholder={t('ticket.touch.custom')}
                      aria-describedby={describedBy}
                      aria-invalid={invalid ? 'true' : undefined}
                      data-testid={spec.testid}
                      disabled={props.disabled}
                      onInput={(ev) => props.onChange(id, (ev.currentTarget as HTMLInputElement).value)}
                    />
                    <span class="tk-unit">{amountUnit}</span>
                  </span>
                </div>
              )}
            </FieldShell>
          );
        }
        if (spec.kind === 'datetime') {
          return <Field key={id} {...common} type="datetime-local" value={value} onValue={(v) => props.onChange(id, v)} hint={help} />;
        }

        const flipped = props.flip && spec.kind === 'price' ? props.flip : null;
        // the help of a price box speaks of KAS per token: an inverted box says what the order is in its own hint instead
        const helpShown = flipped ? undefined : help;
        let hint: ComponentChildren = helpShown;
        let suffix: ComponentChildren = undefined;
        let shownValue = value;
        let onValue = (v: string) => props.onChange(id, v);
        if (flipped) {
          const rounding = safeRounding(sideOfField(form, id)) as 'up' | 'down';
          const t0 = typed[id];
          shownValue = t0 !== undefined && invertedToNativePriceText(t0, rounding) === value ? t0 : nativeToInvertedPriceText(value);
          onValue = (v) => {
            setTyped((m) => ({ ...m, [id]: v }));
            props.onChange(id, invertedToNativePriceText(v, rounding));
          };
        }
        let inputMode: 'decimal' | 'numeric' = 'decimal';
        if (spec.kind === 'price' || spec.kind === 'delta' || spec.kind === 'tip') {
          // a price is shown per KAS in the inverted market; a tip or a distance is an amount of KAS per token, which it says in words
          // a pair prices in its quote token B; a tip is KAS on every market
          const quote = spec.kind === 'tip' ? 'KAS' : props.quoteTicker ?? 'KAS';
          suffix = <span class="tk-unit">{flipped ? `${flipped.name} / KAS` : props.flip ? t('ticket.flip.tipUnit', { name: props.flip.name }) : `${quote} / ${ticker}`}</span>;
          if (props.flip && spec.kind === 'tip' && value !== '') {
            const f = props.flip;
            const tip = Number(value);
            if (f.price && f.scale && f.decimals !== undefined && Number.isFinite(tip) && tip > 0) {
              const kasPerToken = (Number(f.price) / 1e8) * (10 ** f.decimals / Number(f.scale));
              if (kasPerToken > 0) {
                const pct = ((tip / kasPerToken) * 100).toPrecision(2);
                hint = (
                  <>
                    <span class="tk-sub" data-testid={`${spec.testid}-share`}>{t('ticket.flip.tipShare', { pct, basis: t(f.ownPrice ? 'ticket.flip.qtyOwn' : 'ticket.flip.qtyBook') })}</span>
                    {helpShown ? <span class="tk-help">{helpShown}</span> : null}
                  </>
                );
              }
            }
          }
          if (spec.kind === 'price' && value !== '') {
            const info = priceInfo(value, sideOfField(form, id), ctx);
            if (info) {
              hint = (
                <>
                  {flipped ? <span class="tk-sub" data-testid={`${spec.testid}-native`}>{t('ticket.flip.executesAs', { price: info.perToken, ticker })}</span> : null}
                  {info.rounded ? <span class="tk-rounded" data-testid={`${spec.testid}-rounded`}>{t('ticket.priceRounded', { price: info.perToken, ticker, quote: props.quoteTicker ?? 'KAS' })}</span> : null}
                  {helpShown ? <span class="tk-help">{helpShown}</span> : null}
                </>
              );
            }
          }
        } else if (spec.kind === 'kas') {
          suffix = <span class="tk-unit">KAS</span>;
        } else {
          const unit = UNIT_KEY[spec.kind];
          if (unit) suffix = <span class="tk-unit">{t(unit)}</span>;
          inputMode = spec.kind === 'percent' || spec.kind === 'minutes' ? 'decimal' : 'numeric';
        }
        if (spec.kind === 'amount') {
          // an amount counts the base of the shown pair: the token natively (whole-token units, its decimals), KAS in a `kas` form
          suffix = <span class="tk-unit">{amountUnit}</span>;
          inputMode = 'decimal';
          if (form.kas) {
            shownValue = amountText(id);
            hint = (
              <>
                <KasAmountHint form={form} ctx={ctx} id={id} ticker={ticker} testid={spec.testid} />
                {helpShown ? <span class="tk-help">{helpShown}</span> : null}
              </>
            );
          }
        }
        if (id === 'amount' && props.onMax) {
          suffix = (
            <>
              <span class="tk-unit">{amountUnit}</span>
              <button type="button" class="btn btn-sm" onClick={props.onMax} disabled={props.disabled} data-testid="order-amount-max" aria-label={t('ticket.max.aria')}>
                {props.maxLabel ?? t('ticket.max')}
              </button>
            </>
          );
        }
        return (
          <Field
            key={id}
            {...common}
            value={shownValue}
            onValue={onValue}
            inputMode={inputMode}
            placeholder={defaultHint ?? (FIELD_DEFAULTS[id] !== undefined && FIELDS[id]?.kind !== 'select' ? FIELD_DEFAULTS[id] : undefined)}
            hint={hint}
            suffix={suffix ? <span class="tk-suffix">{suffix}</span> : undefined}
          />
        );
      })}
    </>
  );
}
