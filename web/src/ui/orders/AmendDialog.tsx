import { useMemo, useState } from 'preact/hooks';
import { useServices } from '../../app/context';
import type { StrayView } from '../../data/indexer-types';
import { t, tIssue } from '../../i18n';
import type { CancelPlan, OrderSnapshot } from '../../kob/cancel';
import { tickerUnit, type TokenInfo } from '../../kob/registry';
import type { Hex, OrderState } from '../../kob/types';
import { stopWorstPrice } from '../../kob/orders/cond-legs';
import { formatUnits, parseKas, parsePricePerToken, safeRounding, tokenPriceToStatePrice, tryParseKas, tryParseUnits } from '../../kob/units';
import { Banner, Button, Field, HelpTip, IssueLine, Modal, kasText, pricePerTokenText, toError, unitParts } from '../kit';
import { planAmendFor, describeActionError } from './actions';
import { buildReplacement, type AmendInput, type AmendKind } from './amend';
import type { OrderRowModel } from './orders-model';

export interface AmendDialogProps {
  row: OrderRowModel;
  kind: AmendKind;
  /** Current, proven state of the order. */
  state: OrderState;
  token: TokenInfo | undefined;
  pubkey: Hex;
  strays: readonly StrayView[];
  onClose(): void;
  /** The atomic cancel + replace plan is built. Hand it to the confirmation flow. */
  onPlanned(plan: CancelPlan, snapshot: OrderSnapshot): void;
  /**
   * Opens the full form (band, trigger rule, expiry, activation and more), re-planned as a
   * cancel-replace. When absent there is no such link.
   */
  onMore?(): void;
}

type FieldKey = 'price' | 'stop' | 'stopLimit' | 'amount' | 'tip' | 'addKas';

/**
 * Minimal snapshot for the synchronous form validation. The builders only read the state, the KAS
 * amount and the deadline.
 */
function previewSnapshot(row: OrderRowModel, state: OrderState): OrderSnapshot {
  const amount = row.entry.view?.current?.value ?? row.entry.resolved?.order?.amount ?? '0';
  return {
    covenantId: row.id, order: { transactionId: '00'.repeat(32), index: 0, amount, covenantId: row.id, state }, custody: null, strays: [], refundDueDaa: null,
    deadline: row.expiry.deadlineUnix, source: 'indexer',
  };
}

/**
 * Amend form: new price, amount and tip for a plain limit, or take-profit and stop prices for a
 * conditional order. The result is one atomic transaction (cancel + new order) that goes through
 * the same pre-sign screen as everything else. An amended armed stop restarts unarmed.
 */
export function AmendDialog(props: AmendDialogProps) {
  const services = useServices();
  const { row, kind, state, token } = props;
  const s = state.state as unknown as Record<string, string>;
  const sell = row.side === 'sell';
  // The order's prices are sompi per whole token of its `scale` base units. Amounts are base units
  // of the token.
  const scale = BigInt(s.scale);
  const decimals = token?.decimals ?? 0;
  const hasTp = kind === 'limit' || (kind === 'cond' && BigInt(s.tpPrice) > 0n);
  const hasStop = kind === 'cond' && BigInt(s.stopPrice) > 0n;
  const perToken = token !== undefined;

  const priceText = (price: bigint | null): string => (price === null ? '' : perToken ? pricePerTokenText(price, token!.decimals, scale) : kasText(price));
  const initial: Record<FieldKey, string> = useMemo(
    () => ({
      // an order stored as an auction keeps its start in `price` and its limit in `priceEnd`: the
      // limit is what it trades at once the schedule has run (R9FC2-1)
      price: priceText(kind === 'limit' ? (BigInt(s.slope ?? 0) !== 0n && BigInt(s.priceEnd ?? 0) > 0n ? BigInt(s.priceEnd) : BigInt(s.price)) : BigInt(s.tpPrice)),
      stop: hasStop ? priceText(BigInt(s.stopPrice)) : '',
      // The stop leg's worst price, which is the limit of a stop-limit. It is kept fixed when
      // typed. Otherwise the band keeps its percent and moves with the stop.
      stopLimit: hasStop ? priceText(stopWorstPrice(sell ? 'sell' : 'buy', BigInt(s.stopPrice), BigInt(s.slipBps))) : '',
      amount: 'amountLeft' in s ? formatUnits(BigInt(s.amountLeft), decimals) : '',
      tip: kasText(BigInt(s.tip)),
      addKas: '',
    }),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [row.id],
  );
  const [form, setForm] = useState<Record<FieldKey, string>>(initial);
  // A GTC can be renewed. The new order gets a fresh 90-day expiry, while a GTC otherwise ends 90
  // days after its placement, fills or not.
  const canRenew = row.expiry.kind === 'gtc';
  const [renew, setRenew] = useState(row.renewalDue && canRenew);
  const [busy, setBusy] = useState(false);
  const [failure, setFailure] = useState<string | null>(null);
  const [serverIssues, setServerIssues] = useState<{ code: string; message: string; params?: Record<string, string | number | bigint> }[]>([]);
  const set = (k: FieldKey, v: string) => {
    setForm((f) => ({ ...f, [k]: v }));
    setFailure(null);
    setServerIssues([]);
  };

  const parsePrice = (text: string): bigint | null => {
    try {
      return perToken ? parsePricePerToken(text, token!.decimals, scale, safeRounding(row.side)) : parseKas(text);
    } catch {
      return null;
    }
  };

  // Only the fields the user actually changed are sent. An untouched price keeps its exact current
  // value, with no display rounding round trip.
  const parsed = useMemo(() => {
    const errors: Partial<Record<FieldKey, string>> = {};
    const input: AmendInput = {};
    if (form.price !== initial.price) {
      const p = parsePrice(form.price);
      if (p === null) errors.price = t('orders.amend.error.number');
      else input.price = p;
    }
    if (hasStop && form.stop !== initial.stop) {
      const p = parsePrice(form.stop);
      if (p === null) errors.stop = t('orders.amend.error.number');
      else input.stop = p;
    }
    if (hasStop && form.stopLimit !== initial.stopLimit) {
      const p = parsePrice(form.stopLimit);
      if (p === null) errors.stopLimit = t('orders.amend.error.number');
      else input.stopLimit = p;
    }
    if (form.amount !== initial.amount && 'amountLeft' in s) {
      const v = tryParseUnits(form.amount, decimals);
      if (v === null || v <= 0n) errors.amount = t('orders.amend.error.amount', { decimals });
      else input.amount = v;
    }
    if (!sell && kind === 'limit' && form.addKas.trim() !== '') {
      const v = tryParseKas(form.addKas);
      if (v === null || v < 0n) errors.addKas = t('orders.amend.error.number');
      else if (v > 0n) input.addKas = v;
    }
    if (form.tip !== initial.tip) {
      const v = tryParseKas(form.tip);
      if (v === null) errors.tip = t('orders.amend.error.number');
      else input.tip = tokenPriceToStatePrice(v, decimals, scale, 'nearest');
    }
    if (renew && canRenew) input.renew = true;
    return { errors, input };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [form, renew]);

  const changed = Object.keys(parsed.input).length > 0;
  const preview = useMemo(
    () => (Object.keys(parsed.errors).length || !changed ? null : buildReplacement(previewSnapshot(row, state), kind, parsed.input, { tick: token?.tick ?? undefined, nowDaa: 0n, kob: services.kob })),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [parsed, kind],
  );
  const fieldIssue = (field: string) => preview?.issues.find((i) => i.severity === 'error' && i.field === field);
  const canReview = changed && Object.keys(parsed.errors).length === 0 && !!preview && preview.ok;

  const review = async () => {
    setBusy(true);
    setFailure(null);
    setServerIssues([]);
    try {
      const out = await planAmendFor(services, props.pubkey, row.entry, kind, parsed.input, props.strays);
      if (!out.amend.ok || !out.plan) {
        setServerIssues(out.amend.issues.filter((i) => i.severity === 'error'));
        return;
      }
      if (!out.plan.ok) {
        setServerIssues(out.plan.issues.filter((i) => i.severity === 'error'));
        return;
      }
      props.onPlanned(out.plan, out.snapshot);
    } catch (e) {
      setFailure(describeActionError(toError(e)));
    } finally {
      setBusy(false);
    }
  };

  // KAS per whole token. For an order of a token nothing names, it is per the whole token of its
  // price scale, `scale`, which My orders shows it with.
  const unit = t('orders.unit.kasPer', { ticker: token ? tickerUnit(token.ticker, token.covenantId) : '?' });
  const errText = (k: FieldKey, field: string): string | null => {
    if (parsed.errors[k]) return parsed.errors[k]!;
    const i = fieldIssue(field);
    return i ? tIssue('orders.amend', i) : null;
  };

  return (
    <Modal
      title={t('orders.amend.title')}
      help={
        <HelpTip label={t('orders.amend.title')} data-testid="amend-help">
          <p>{t('orders.amend.explain')}</p>
          {!('amountLeft' in s) && kind !== 'limit' ? <p>{t('orders.amend.budgetKept')}</p> : null}
        </HelpTip>
      }
      onClose={props.onClose}
      data-testid="amend-dialog"
      dismissable={!busy}
      footer={
        <>
          <Button onClick={props.onClose} disabled={busy} data-testid="amend-cancel">{t('common.cancel')}</Button>
          <Button variant="primary" disabled={!canReview} loading={busy} onClick={() => void review()} data-testid="amend-review">
            {t('orders.amend.review')}
          </Button>
        </>
      }
    >
      {kind === 'cond' && row.armed === true ? <Banner tone="warn">{t('orders.amend.restartsUnarmed')}</Banner> : null}
      <div class="stack-sm">
        {hasTp ? (
          <Field
            label={unitParts(t(kind === 'limit' ? 'orders.amend.price' : 'orders.amend.takeProfit', { unit }))}
            value={form.price}
            onValue={(v) => set('price', v)}
            inputMode="decimal"
            error={errText('price', 'price')}
            data-testid="amend-price"
          />
        ) : null}
        {hasStop ? (
          <Field label={unitParts(t('orders.amend.stop', { unit }))} value={form.stop} onValue={(v) => set('stop', v)} inputMode="decimal" error={errText('stop', 'stop')} data-testid="amend-stop" />
        ) : null}
        {hasStop ? (
          <Field
            label={unitParts(t('orders.amend.stopLimit', { unit }))}
            value={form.stopLimit}
            onValue={(v) => set('stopLimit', v)}
            inputMode="decimal"
            error={errText('stopLimit', 'stopLimit')}
            help={<HelpTip label={t('orders.amend.stopLimit', { unit })}>{t('orders.amend.stopLimitHint')}</HelpTip>}
            data-testid="amend-stop-limit"
          />
        ) : null}
        {'amountLeft' in s ? (
          <Field
            label={t('orders.amend.amount', { ticker: token ? tickerUnit(token.ticker, token.covenantId) : '?' })}
            value={form.amount}
            onValue={(v) => set('amount', v)}
            inputMode="decimal"
            error={errText('amount', 'amount')}
            help={sell ? <HelpTip label={t('orders.amend.amount', { ticker: token ? tickerUnit(token.ticker, token.covenantId) : '?' })}>{t('orders.amend.amountHint')}</HelpTip> : undefined}
            data-testid="amend-amount"
          />
        ) : kind === 'limit' ? (
          <Field
            label={t('orders.amend.addKas')}
            value={form.addKas}
            onValue={(v) => set('addKas', v)}
            inputMode="decimal"
            error={errText('addKas', 'addKas')}
            help={row.unfundable ? undefined : <HelpTip label={t('orders.amend.addKas')}>{t('orders.amend.addKasHint')}</HelpTip>}
            hint={row.unfundable ? t('orders.amend.addKasHintUnfunded') : undefined}
            data-testid="amend-add-kas"
          />
        ) : null}
        {canRenew ? (
          <label class="row small ord-check" style="gap:8px" data-testid="amend-renew-label">
            <input type="checkbox" checked={renew} onChange={(e) => { setRenew((e.currentTarget as HTMLInputElement).checked); setServerIssues([]); }} data-testid="amend-renew" />
            <span>{t('orders.amend.renew', { days: 90 })}</span>
          </label>
        ) : null}
        <Field label={t('orders.amend.tip')} value={form.tip} onValue={(v) => set('tip', v)} inputMode="decimal" error={errText('tip', 'tip')} help={<HelpTip label={t('orders.amend.tip')}>{t('orders.amend.tipHint')}</HelpTip>} data-testid="amend-tip" />
      </div>
      {preview && preview.ok && preview.next ? (
        <p class="small" data-testid="amend-preview">
          {t('orders.amend.preview', { price: preview.next.price !== null ? priceText(preview.next.price) : '-', unit })}
          {preview.next.stopWorst !== undefined ? (
            <>
              {' '}
              <span data-testid="amend-stop-worst">{t('orders.amend.previewStopWorst', { price: priceText(preview.next.stopWorst), unit })}</span>
            </>
          ) : null}
          {preview.next.value !== undefined && preview.next.maxAmount !== undefined ? (
            <>
              {' '}
              <span data-testid="amend-budget">
                {t('orders.amend.previewBudget', { kas: kasText(preview.next.value), amount: `${formatUnits(preview.next.maxAmount, decimals, { group: ',' })} ${token ? tickerUnit(token.ticker, token.covenantId) : ''}`.trim() })}
              </span>
            </>
          ) : null}
        </p>
      ) : null}
      {serverIssues.length ? (
        <Banner tone="error" data-testid="amend-error">
          <ul>
            {serverIssues.map((i, n) => (
              <li key={n}><IssueLine prefix={i.code.includes('.') ? 'orders.issue' : 'orders.amend'} issue={i} /></li>
            ))}
          </ul>
        </Banner>
      ) : null}
      {failure ? <Banner tone="error" data-testid="amend-failure">{failure}</Banner> : null}
      {props.onMore ? (
        <p class="small">
          <Button small variant="ghost" onClick={props.onMore} disabled={busy} data-testid="amend-more">
            {t('orders.amend.more')}
          </Button>
        </p>
      ) : null}
    </Modal>
  );
}
