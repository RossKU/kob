// The input fields of the ticket, rendered from the layout of `form-state.ts`. It is thin: every
// rule (parsing, rounding, which fields exist) lives there.
import { Fragment, type ComponentChildren } from 'preact';
import { useState } from 'preact/hooks';
import { has, t } from '../../i18n';
import { formatUnits, safeRounding } from '../../kob/units';
import { formatLocalTime, nextUtcMidnight } from '../../kob/daa';
import { kasWords, tokenWords } from '../../signer/words';
import { invertedToNativePriceText, nativeToInvertedPriceText } from '../market/orientation';
import { Checkbox, Dropdown, Field, FieldShell, Segmented, unitParts } from '../kit';
import { AmountSlider, type AmountSliderProps } from './AmountSlider';
import {
  BASE_SUFFIX, FIELDS, FIELD_DEFAULTS, fieldDefault, TOUCH_MIN, TOUCH_PRESETS, amountBaseOf, amountPriceOf, baseToKas, kasTextOf, parseTouch, priceInfo,
  type FieldError, type FieldSpec, type Side, type TicketCtx, type TicketForm,
} from './form-state';

export interface TicketFieldsProps {
  form: TicketForm;
  ctx: TicketCtx;
  ticker: string;
  ids: string[];
  /** Parse errors of non-empty fields. The caller filters out "required". */
  errors: FieldError[];
  onChange(id: string, value: string): void;
  /**
   * Max: fills the amount with the largest the balance allows. The "Available" figure of the amount
   * label row is its button.
   */
  onMax?: () => void;
  /**
   * "Available 988.41 KAS": the balance this order spends, one right-aligned line under the amount
   * box. A tap does `onMax`.
   */
  available?: { amount: string; unit: string };
  /**
   * A sell: held units the wallet cannot spend here, one muted line under Available. The reason is
   * its tooltip.
   */
  unusable?: { text: string; reason: string };
  /** The 0..100 % of Max slider under the amount field. */
  slider?: AmountSliderProps;
  disabled?: boolean;
  /** A market order: its price row stays, disabled ("Market price"), before the slippage box. */
  marketPrice?: boolean;
  /**
   * The market is shown inverted (KAS/TOKEN, orientation.ts), so the price inputs take tokens per
   * KAS (`name` per KAS). The form still keeps native text (KAS per token). What is typed is
   * converted on input and rounded the way that never makes the limit worse, and the unit hint says
   * what the order is.
   */
  flip?: FlipCtx;
  /**
   * A token/token pair: the ticker of the quote token B the prices are typed in (B per whole
   * `ticker`). Absent means KAS. Tips stay KAS.
   */
  quoteTicker?: string;
  /**
   * A pair: the tip fields whose typed tip is below what a netting fill needs (pair-common
   * `pairTipBelowDefault`). A hint says a crossing order may wait.
   */
  lowTips?: readonly string[];
  /**
   * The value an empty select shows when its default is not static (Delivery: the wallet default of
   * Settings).
   */
  defaults?: Readonly<Record<string, string>>;
  /**
   * What an empty box shows as its placeholder when its default is not static (the Top-up tip of
   * the token's program).
   */
  placeholders?: Readonly<Record<string, string>>;
  /** One muted line under a field (Delivery: what Collected means. Top-up tip: the tip total). */
  subs?: Readonly<Record<string, { text: string; testid: string }>>;
  /**
   * The options a select offers when they depend on the ticket (Bought tokens on a token pair: no
   * "Added to one UTXO").
   */
  options?: Readonly<Record<string, readonly string[]>>;
  /** Something at the right end of a select's label row (Delivery: its "What's this?" link). */
  asides?: Readonly<Record<string, ComponentChildren>>;
  /** The executor tip option: what the tip it set pays in all, one line under it. */
  tipAutoNote?: string;
}

/**
 * What the fields need to speak the shown (inverted) market: the token's name, its scale, and a KAS
 * price per whole token to turn amounts into KAS.
 */
export interface FlipCtx {
  name: string;
  /**
   * A reference price (the state price: sompi per whole token of `scale` base units). It is the
   * form's own price when it parses, else the best in the book, and null when there is none.
   */
  price?: bigint | null;
  /** `price` is the form's own price (else the book's). */
  ownPrice?: boolean;
  scale?: bigint;
  decimals?: number;
}

/**
 * Exit legs of an if-done order are on the opposite side of the entry, so their prices round the
 * other way.
 */
export const sideOfField = (form: TicketForm, id: string): Side => (id.startsWith('exit.') && id !== 'exit.tip' && id !== 'exit.minFill' ? (form.side === 'buy' ? 'sell' : 'buy') : form.side);

export const labelKey = (type: string, id: string): string => (has(`ticket.label.${id}.${type}`) ? `ticket.label.${id}.${type}` : `ticket.label.${id}`);

const UNIT_KEY: Partial<Record<FieldSpec['kind'], string>> = { percent: 'ticket.unit.percent', seconds: 'ticket.unit.seconds', minutes: 'ticket.unit.minutes' };

function errorText(e: FieldError): string {
  return t(`ticket.err.${e.code}`, (e.params ?? {}) as Record<string, string | number>);
}

/** A field hint on one line (CSS ellipsis). The full text is the tooltip. */
function Help(props: { text: string }) {
  return <span class="tk-help" title={props.text}>{props.text}</span>;
}
/** The user guide where stops are explained (a stop can be triggered by someone's own trade). */
const STOP_DOCS_URL = 'https://github.com/RossKU/kob/blob/master/docs/user-guide.md#5-limit-and-market-orders';

const helpOf = (text: string | undefined): ComponentChildren => (text ? <Help text={text} /> : undefined);

/**
 * The one short line under a KAS amount box (a `kas` form): the token amount the order is built
 * with and, where it matters, the KAS it stands for. A price the user typed converts exactly ("=
 * 8.6169 TUSD at your price"). A buy's KAS is the most it spends (the worst price plus the tip),
 * and a sell's is what it receives about at the book. When the value is not known it says what is
 * still missing. It is null when there is nothing to say yet.
 */
export function kasHintText(form: TicketForm, ctx: TicketCtx, id: string, ticker: string): { text: string; base?: bigint } | null {
  const base = amountBaseOf(form, ctx, id);
  const p = amountPriceOf(form, ctx, id);
  if (!base.ok || base.value <= 0n) return !p && (form.values[id] ?? '') !== '' ? { text: t('ticket.kasAmt.needPrice', { ticker }) } : null;
  const tokens = tokenWords(base.value, ctx.decimals);
  if (!p) return { text: t('ticket.kasAmt.kept', { tokens, ticker }), base: base.value };
  if (p.basis === 'own' || p.basis === 'stop' || p.basis === 'exit') return { text: t(`ticket.kasAmt.at.${p.basis}`, { tokens, ticker }), base: base.value };
  // A buy spends at most the KAS typed (rounded up, so it never looks cheaper). A sell at the book
  // receives about it.
  const atMost = p.basis !== 'book';
  const key = atMost ? 'ticket.kasAmt.spendsAtMost' : form.side === 'buy' ? 'ticket.kasAmt.spendsAbout' : 'ticket.kasAmt.receivesAbout';
  return { text: t(key, { tokens, ticker, kas: kasWords(baseToKas(base.value, p.price, ctx.scale), atMost ? 'up' : 'nearest') }), base: base.value };
}

function KasAmountHint(props: { form: TicketForm; ctx: TicketCtx; id: string; ticker: string; testid: string }) {
  const h = kasHintText(props.form, props.ctx, props.id, props.ticker);
  if (!h) return null;
  return <span class="tk-sub" data-testid={`${props.testid}-tokens`} {...(h.base !== undefined ? { 'data-base': h.base.toString() } : {})} title={h.text}><span key={h.text}>{h.text}</span></span>;
}

export function TicketFields(props: TicketFieldsProps) {
  const { form, ctx, ticker } = props;
  /**
   * The long help of a field (what the "?" of the title row shows under it). An inverted market has
   * its own wording where the native help speaks of buying or selling the token. A market order's
   * Fills box has its own on either view, because it speaks of transactions, not of a limit's
   * escrow. The plain amount help says what the label and the unit beside the box say, so it is
   * not shown.
   */
  const helpOf0 = (id: string): string | undefined => {
    const helpKey = props.quoteTicker && has(`ticket.help.${id}.pair`) ? `ticket.help.${id}.pair` : (id === 'maxFills' || id === 'direct') && has(`ticket.help.${id}.${form.type}`) ? `ticket.help.${id}.${form.type}` : props.flip && has(`ticket.help.${id}.inv`) ? `ticket.help.${id}.inv` : id.startsWith('ladder.') && has(`ticket.help.${id}.${form.type}`) ? `ticket.help.${id}.${form.type}` : `ticket.help.${id}`;
    return helpKey !== 'ticket.help.amount' && has(helpKey) ? t(helpKey, { ticker, quote: props.quoteTicker ?? 'KAS', local: formatLocalTime(nextUtcMidnight(Date.now())) }) : undefined;
  };
  /**
   * The text an amount box shows: the KAS value of an exact amount kept for it (a `kas` form), else
   * what was typed.
   */
  const amountText = (id: string): string => {
    const kept = form.values[id + BASE_SUFFIX];
    return form.kas && kept !== undefined ? kasTextOf(form, ctx, id, BigInt(kept)) : form.values[id] ?? '';
  };
  /** The unit of an amount box: KAS in a `kas` form, else the token. */
  const amountUnit = form.kas ? 'KAS' : ticker;
  const errOf = (id: string) => props.errors.find((e) => e.field === id);
  // What the user typed in an inverted price box (the form's own text is the converted native
  // price). It is kept while it still produces the form's text.
  const [typed, setTyped] = useState<Record<string, string>>({});
  /**
   * A market order has no price of its own. Its price row stays, disabled, saying "Market price",
   * so the rows (and the button) do not move between Limit and Market (the usual exchange layout).
   */
  const marketPriceRow = (
    <Field
      key="marketPrice"
      label={t('ticket.label.price')}
      value={t('ticket.marketPrice')}
      disabled
      readOnly
      data-testid="order-price-market"
      suffix={<span class="tk-suffix"><span class="tk-unit">{unitParts(props.flip ? t('ticket.unit.per', { unit: props.flip.name, per: 'KAS' }) : t('ticket.unit.per', { unit: props.quoteTicker ?? 'KAS', per: ticker }))}</span></span>}
    />
  );
  const fieldOf = (id: string): ComponentChildren => {
    {
        const spec = FIELDS[id];
        if (!spec) return null;
        const value = form.values[id] ?? '';
        const e = errOf(id);
        // The inverted market drops "(per token)", because the unit next to the box says what the
        // amount is.
        const label = t(props.flip && has(`ticket.label.${id}.inv`) ? `ticket.label.${id}.inv` : labelKey(form.type, id));
        const help = helpOf0(id);
        const phKey = has(`ticket.ph.${id}.${form.type}`) ? `ticket.ph.${id}.${form.type}` : props.quoteTicker && has(`ticket.ph.${id}.pair`) ? `ticket.ph.${id}.pair` : `ticket.ph.${id}`;
        // A token without decimals takes whole numbers, so its amount box says "0", not "0.0".
        const wholeAmount = spec.kind === 'amount' && !form.kas && ctx.decimals === 0 && phKey === 'ticket.ph.amount';
        const defaultHint = value === '' && FIELD_DEFAULTS[id] === undefined && has(phKey) ? (wholeAmount ? t('ticket.amountWholePh') : t(phKey)) : undefined;
        const common = { label, error: e ? errorText(e) : null, 'data-testid': spec.testid, disabled: props.disabled };
        const sub = props.subs?.[id];
        const subLine = sub ? <span class="tk-sub" data-testid={sub.testid} title={sub.text}><span key={sub.text}>{sub.text}</span></span> : null;

        if (spec.kind === 'bool') {
          const note = id === 'tipAuto' && value === 'true' && props.tipAutoNote ? (
            <>
              <span class="tk-sub" data-testid="field-tipAuto-total">{props.tipAutoNote}</span>
              <span class="tk-sub tk-help" data-testid="field-tipAuto-partial">{t('ticket.tipAuto.partial')}</span>
            </>
          ) : null;
          return (
            <Checkbox key={id} checked={value === 'true'} onChange={(c) => props.onChange(id, c ? 'true' : '')} label={label} hint={note ? <>{note}{helpOf(help)}</> : helpOf(help)} data-testid={spec.testid} disabled={props.disabled} />
          );
        }
        if (spec.kind === 'select') {
          // A pair order is never filled from the wallet, so its crossing choices start at the
          // auction.
          const pairNoTake = !!props.quoteTicker && id === 'crossing';
          return (
            <FieldShell key={id} label={label} labelAside={props.asides?.[id]} hint={subLine ? <>{subLine}{helpOf(help)}</> : helpOf(help)} error={common.error}>
              {({ id: cid, labelId, describedBy, invalid }) => (
                <Dropdown
                  id={cid}
                  labelledBy={labelId}
                  describedBy={describedBy}
                  invalid={invalid}
                  value={value && !(pairNoTake && value === 'take') ? value : pairNoTake ? 'auction' : props.defaults?.[id] || fieldDefault(id, form.type) || spec.options?.[0] || ''}
                  options={(props.options?.[id] ?? spec.options ?? []).filter((o) => !(pairNoTake && o === 'take')).map((o) => ({ value: o, label: t(has(`ticket.opt.${id}.${o}.${form.type}`) ? `ticket.opt.${id}.${o}.${form.type}` : `ticket.opt.${id}.${o}`) }))}
                  disabled={props.disabled}
                  onChange={(v) => props.onChange(id, v)}
                  data-testid={spec.testid}
                />
              )}
            </FieldShell>
          );
        }
        if (spec.kind === 'touch') {
          // Trigger threshold: presets (the order's minimum fill, 25%, 50% or 100% of its own
          // amount) plus a custom token amount, all in the one text value.
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
              <span class="tk-sub" data-testid={`${spec.testid}-hunt`}>
                {t('ticket.touch.hunt')} <a href={STOP_DOCS_URL} target="_blank" rel="noopener noreferrer">{t('ticket.touch.hunt.link')}</a>
              </span>
              {helpOf(help)}
              {helpOf(t('ticket.touch.tradeoff'))}
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
                  <span class="tk-touch-custom">
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
                    <span class="tk-suffix"><span class="tk-unit">{amountUnit}</span></span>
                  </span>
                </div>
              )}
            </FieldShell>
          );
        }
        if (spec.kind === 'datetime') {
          return <Field key={id} {...common} type="datetime-local" value={value} onValue={(v) => props.onChange(id, v)} hint={helpOf(help)} />;
        }

        const flipped = props.flip && spec.kind === 'price' ? props.flip : null;
        // The help of a price box speaks of KAS per token. An inverted box says what the order is
        // in its own hint instead.
        const helpShown = flipped ? undefined : help;
        let hint: ComponentChildren = helpOf(helpShown);
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
          // A price is shown per KAS in the inverted market. A tip or a distance is an amount of
          // KAS per token, which it says in words. It is always "X per Y", never the pair notation
          // "TOKEN / KAS", because that reads as tokens per KAS. A pair prices in its quote token
          // B, and a tip is KAS on every market.
          const quote = spec.kind === 'tip' ? 'KAS' : props.quoteTicker ?? 'KAS';
          suffix = <span class="tk-unit">{unitParts(flipped ? t('ticket.unit.per', { unit: flipped.name, per: 'KAS' }) : props.flip ? t('ticket.flip.tipUnit', { name: props.flip.name }) : t('ticket.unit.per', { unit: quote, per: ticker }))}</span>;
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
                    {helpOf(helpShown)}
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
                  {helpOf(helpShown)}
                </>
              );
            }
          }
        } else if (spec.kind === 'kas') {
          suffix = <span class="tk-unit">{has(`ticket.unit.${id}`) ? t(`ticket.unit.${id}`) : 'KAS'}</span>;
          if (subLine) hint = <>{subLine}{helpOf(helpShown)}</>;
        } else {
          const unit = UNIT_KEY[spec.kind];
          if (unit) suffix = <span class="tk-unit">{t(unit)}</span>;
          inputMode = spec.kind === 'percent' || spec.kind === 'minutes' ? 'decimal' : 'numeric';
        }
        if (spec.kind === 'tip' && props.lowTips?.includes(id)) {
          hint = (
            <>
              <span class="tk-sub" data-testid={`${spec.testid}-low`}>{t('ticket.tipLow.pair')}</span>
              {hint}
            </>
          );
        }
        if (spec.kind === 'amount') {
          // An amount counts the base of the shown pair: the token natively (whole-token units, its
          // decimals), KAS in a `kas` form.
          suffix = <span class="tk-unit">{amountUnit}</span>;
          inputMode = 'decimal';
          if (form.kas) {
            shownValue = amountText(id);
            hint = (
              <>
                <KasAmountHint form={form} ctx={ctx} id={id} ticker={ticker} testid={spec.testid} />
                {helpOf(helpShown)}
              </>
            );
          }
        }
        // The balance this order spends is the Max button, at the right end of the label row.
        const availableBtn = id === 'amount' && props.onMax && props.available ? (
          <button type="button" class="tk-available" onClick={props.onMax} disabled={props.disabled} title={t('ticket.max.aria')} data-testid="order-available">
            <span class="tk-avail-label">{t('ticket.available')}</span>{' '}
            <span class="tk-line-num" key={props.available.amount}>{props.available.amount}</span>{' '}
            <span class="tk-line-unit">{props.available.unit}</span>
          </button>
        ) : undefined;
        const field = (
          <Field
            key={id}
            {...common}
            value={shownValue}
            onValue={onValue}
            inputMode={inputMode}
            placeholder={props.placeholders?.[id] ?? defaultHint ?? (FIELD_DEFAULTS[id] !== undefined && FIELDS[id]?.kind !== 'select' ? FIELD_DEFAULTS[id] : undefined)}
            hint={hint}
            suffix={suffix ? <span class="tk-suffix">{suffix}</span> : undefined}
          />
        );
        return id === 'amount' && (props.slider || availableBtn) ? (
          <Fragment key={id}>
            {field}
            {/* one line of fixed height under the box: what this order can spend (the Max button), at the right end. What the amount costs stands in the
                ticket's footer, over the button (OrderTicket.tsx "Total") */}
            <div class="tk-avail-line" data-testid="order-available-line">{availableBtn}</div>
            {availableBtn && props.unusable ? <div class="tk-unusable" title={props.unusable.reason} data-testid="order-unusable">{props.unusable.text}</div> : null}
            {props.slider ? <AmountSlider {...props.slider} disabled={props.disabled || props.slider.disabled} /> : null}
          </Fragment>
        ) : (
          field
        );
    }
  };
  /**
   * A field with its long help as the tooltip (`title`). The help under the fields is off unless
   * the title row's "?" asks for it, so what stays is the tooltip over the label and the box.
   * `display: contents` (ticket.css) keeps the wrapper out of the layout, so the form's gap and the
   * field's own rows are untouched. An inverted price box has no help of its own, because its hint
   * says what the order is. A trigger threshold adds its trade-off.
   */
  const withTip = (id: string, node: ComponentChildren): ComponentChildren => {
    const spec = FIELDS[id];
    if (!node || !spec) return node;
    const flipped = props.flip && spec.kind === 'price';
    const parts = [flipped ? undefined : helpOf0(id), spec.kind === 'touch' ? t('ticket.touch.tradeoff') : undefined].filter(Boolean) as string[];
    return parts.length ? <div class="tk-tip" key={id} title={parts.join(' ')}>{node}</div> : node;
  };
  return <>{props.ids.map((id) => (props.marketPrice && id === 'slippageBps' ? <Fragment key={id}>{marketPriceRow}{withTip(id, fieldOf(id))}</Fragment> : withTip(id, fieldOf(id))))}</>;
}
