// Replace form for the order kinds the quick amend form does not cover (trailing stops, IFD / IFO entries) and for every
// knob of a conditional order (band, trigger rule, trailing, expiry, activation): the order is turned back into its ticket intent
// (replace-intent.ts), edited in the ticket's own fields, re-planned by the ticket's planner, and the planned order becomes the replacement of ONE
// atomic cancel + new order transaction. Nothing here signs: the plan goes to the confirmation flow of My orders.
import { useMemo, useState } from 'preact/hooks';
import { useServices } from '../../app/context';
import type { StrayView } from '../../data/indexer-types';
import { t } from '../../i18n';
import { issueText } from '../../i18n/issue-text';
import { buildErrorText, rawOf } from '../../i18n/build-error';
import type { CancelPlan, OrderSnapshot, ReplacementSpec } from '../../kob/cancel';
import { custodiesOf, scaleOf } from '../../kob/order-facts';
import { planOrder } from '../../kob/plan';
import { errors, isPairEnv, type OrderPlan, type PlanEnv } from '../../kob/plan-types';
import type { TokenInfo } from '../../kob/registry';
import { keyState } from '../../kob/token-state';
import type { Hex, OrderState } from '../../kob/types';
import { Banner, Button, IssueLine, Loading, Modal, RawDetails, toError } from '../kit';
import { DisclosurePanel } from '../ticket/DisclosurePanel';
import { buildDisclosureModel } from '../ticket/disclosure-model';
import { TicketFields } from '../ticket/TicketFields';
import { buildIntent, formFromIntent, layoutOf, setValue, visibleErrors, type TicketCtx, type TicketForm } from '../ticket/form-state';
import { usePairTicketEnv, useTicketEnv, useTicketPlan } from '../ticket/use-ticket';
import { describeActionError, planReplaceFor } from './actions';
import type { OrderRowModel } from './orders-model';
import { intentFromOrder } from './replace-intent';

export interface ReplaceDialogProps {
  row: OrderRowModel;
  /** current (proven) state of the order */
  state: OrderState;
  token: TokenInfo;
  /** a pair order: its quote token B (prices typed in B per whole A, the pair environment) */
  quote?: TokenInfo;
  pubkey: Hex;
  strays: readonly StrayView[];
  onClose(): void;
  /** the atomic cancel + replace plan is built: hand it to the confirmation flow */
  onPlanned(plan: CancelPlan, snapshot: OrderSnapshot): void;
}

const SYNTH_TXID = 'ff'.repeat(32);

/**
 * The planning environment of a REPLACEMENT: the order's own custody tokens and KAS come back in the same transaction, so the planner may count them
 * (as stand-in UTXOs it never spends: only the planned order state, value and carriers are kept; planCancelReplace builds the real transaction), and
 * the order itself is no own order to self-trade against. A pair order's custodies come back per token (A into the base UTXOs, B into the quote ones).
 */
export function replaceEnv(env: PlanEnv, row: OrderRowModel, state: OrderState): PlanEnv {
  const amount = row.entry.view?.current?.value ?? row.entry.resolved?.order?.amount ?? '0';
  const custodies = custodiesOf(state);
  const heldOf = (token: Hex): bigint => custodies.filter((c) => c.token === token).reduce((a, c) => a + c.amount, 0n);
  const synth = (market: PlanEnv['token'], held: bigint, index: number) => ({
    transactionId: SYNTH_TXID, index, amount: '100000000', covenantId: market.covenantId,
    state: keyState(market.family, held, env.maker, market.family === 'kron' ? null : market.extensionCommitment),
  });
  const heldA = heldOf(env.token.covenantId);
  const out: PlanEnv = {
    ...env,
    ownOrders: env.ownOrders.filter((o) => o.covenantId !== row.id),
    funding: [...env.funding, { transactionId: SYNTH_TXID, index: 0, amount: String(amount), pubkey: env.maker }],
    tokenUtxos: heldA > 0n ? [...env.tokenUtxos, synth(env.token, heldA, 1)] : env.tokenUtxos,
  };
  if (isPairEnv(env)) {
    const heldB = heldOf(env.pair.quote.covenantId);
    return { ...out, pair: { ...env.pair, quoteTokenUtxos: heldB > 0n ? [...env.pair.quoteTokenUtxos, synth(env.pair.quote, heldB, 2)] : env.pair.quoteTokenUtxos } } as PlanEnv;
  }
  return out;
}

/** The replacement spec of a planned order (its createOrder request). */
export function specOfPlan(p: OrderPlan): ReplacementSpec | null {
  const r = p.request;
  if (!r) return null;
  return { order: r.order, value: BigInt(r.value), tokenCarrier: r.tokenCarrier != null ? BigInt(r.tokenCarrier) : null, deadline: r.deadline != null ? BigInt(r.deadline) : null };
}

export function ReplaceDialog(props: ReplaceDialogProps) {
  const services = useServices();
  const { row, state, token, quote } = props;
  // a pair order re-plans against the pair environment (both tokens, the pair book); a KAS order against its token's market
  const kasEnv = useTicketEnv(token, !quote);
  const pairEnv = usePairTicketEnv(token, quote ?? token, !!quote);
  const { env: rawEnv, error: envError, refresh } = quote ? pairEnv : kasEnv;
  const rateMilli = rawEnv?.clock.rateMilli;
  // the replaced order quotes per its own scale (base units per whole token; a pair order: per whole A): the form reads and writes its prices in it
  const scale = scaleOf(state);
  const ctx: TicketCtx = useMemo(
    () => ({
      decimals: token.decimals, scale, tick: token.tick ?? 1n, tzOffsetMin: new Date().getTimezoneOffset(), ...(rateMilli ? { rateMilli } : {}),
      ...(quote ? { quoteDecimals: quote.decimals } : {}),
    }),
    [token.decimals, scale, token.tick, rateMilli, quote?.decimals],
  );
  const source = useMemo(
    () => intentFromOrder(state, { typeKey: row.typeKey, expiryKind: row.expiry.kind, nowDaa: rawEnv?.clock.daa ?? null, expiryUnix: row.expiry.approxUnix }, services.kob),
    // the clock only decides whether a future activation is kept: the first reading is enough
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [state, row.typeKey, row.expiry.kind, services.kob, !!rawEnv],
  );
  const [form, setForm] = useState<TicketForm | null>(null);
  const shownForm = form ?? (source.ok ? formFromIntent(source.intent, ctx) : null);
  const env = useMemo(() => (rawEnv ? replaceEnv(rawEnv, row, state) : null), [rawEnv, row, state]);
  const built = useMemo(() => (shownForm ? buildIntent(shownForm, ctx) : null), [shownForm, ctx]);
  const { plan, current, pending, crash } = useTicketPlan(env, built?.intent ?? null);
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState<{ text: string; raw?: string | null } | null>(null);
  const [serverIssues, setServerIssues] = useState<{ code: string; message: string; params?: Record<string, string | number | bigint> }[]>([]);

  const shownPlan = current ? plan : null;
  const planErrors = shownPlan ? errors(shownPlan) : [];
  const model = useMemo(
    () =>
      shownPlan?.disclosure && env
        ? buildDisclosureModel(shownPlan, {
            ticker: token.ticker, decimals: token.decimals, scale: shownPlan.disclosure.scale, clock: env.clock, ...(quote ? { quote: { ticker: quote.ticker, decimals: quote.decimals } } : {}),
          })
        : null,
    [shownPlan, env, token.ticker, token.decimals, quote?.ticker, quote?.decimals],
  );
  const canReview = !!shownPlan && shownPlan.ok && planErrors.length === 0 && !!shownPlan.request && !pending && !busy && !crash;

  const onChange = (id: string, value: string) => {
    setNote(null);
    setServerIssues([]);
    setForm((f) => setValue(f ?? shownForm!, id, value));
  };

  const review = async () => {
    if (!canReview || !built?.intent) return;
    setBusy(true);
    setNote(null);
    setServerIssues([]);
    try {
      // re-plan on fresh chain data, then build the atomic cancel + replacement from the node's view of the order
      const fresh = await refresh();
      if (!fresh) return setNote({ text: t('ticket.reviewNoEnv') });
      const p = planOrder(replaceEnv(fresh, row, state), built.intent);
      const spec = p.ok && errors(p).length === 0 ? specOfPlan(p) : null;
      if (!spec) return setNote({ text: t('ticket.reviewChanged') });
      const out = await planReplaceFor(services, props.pubkey, row.entry, spec, props.strays);
      if (!out.plan.ok) return setServerIssues(out.plan.issues.filter((i) => i.severity === 'error'));
      props.onPlanned(out.plan, out.snapshot);
    } catch (e) {
      const err = toError(e);
      setNote({ text: describeActionError(err), raw: err.message });
    } finally {
      setBusy(false);
    }
  };

  const layout = shownForm ? layoutOf(shownForm) : null;
  const errs = built && shownForm ? visibleErrors(built.errors).filter((e) => (shownForm.values[e.field] ?? '') !== '') : [];
  const issueCtx = { tokenDecimals: token.decimals, tokenTicker: token.ticker, ...(quote ? { quoteDecimals: quote.decimals, quoteTicker: quote.ticker } : {}) };

  return (
    <Modal
      title={t('orders.replace.title', { type: t(`orders.type.${row.typeKey}`) })}
      onClose={props.onClose}
      data-testid="replace-dialog"
      dismissable={!busy}
      footer={
        <>
          <Button onClick={props.onClose} disabled={busy} data-testid="replace-cancel">{t('common.cancel')}</Button>
          <Button variant="primary" disabled={!canReview} loading={busy} onClick={() => void review()} data-testid="replace-review">
            {t('orders.amend.review')}
          </Button>
        </>
      }
    >
      <p class="small muted">{t('orders.replace.explain')}</p>
      {row.armed === true ? <Banner tone="warn">{t('orders.amend.restartsUnarmed')}</Banner> : null}
      {row.typeKey === 'ifd' || row.typeKey === 'ifo' ? <p class="small muted" data-testid="replace-ifd-note">{t('orders.replace.ifdNote')}</p> : null}
      {!source.ok ? (
        <Banner tone="warn" data-testid="replace-unsupported">{t(`orders.replace.unsupported.${source.reason}`)}</Banner>
      ) : !shownForm || !layout ? null : (
        <div class="stack-sm tk" data-testid="replace-form" data-type={shownForm.type} data-side={shownForm.side}>
          <p class="small" data-testid="replace-type">
            <strong>{t(`ticket.type.${shownForm.type}.name`)}</strong> · {t(shownForm.side === 'buy' ? 'common.side.buy' : 'common.side.sell')}
          </p>
          <TicketFields form={shownForm} ctx={ctx} ticker={token.ticker} ids={layout.main} errors={errs} onChange={onChange} {...(quote ? { quoteTicker: quote.ticker } : {})} />
          <details class="tk-advanced" data-testid="replace-advanced">
            <summary>{t('ticket.advanced')}</summary>
            <div class="tk-advanced-body">
              <TicketFields form={shownForm} ctx={ctx} ticker={token.ticker} ids={layout.advanced} errors={errs} onChange={onChange} {...(quote ? { quoteTicker: quote.ticker } : {})} />
            </div>
          </details>
        </div>
      )}
      {envError ? (
        <Banner tone="error" data-testid="replace-env-error">
          {t('ticket.need.env')}
          <RawDetails text={envError} />
        </Banner>
      ) : null}
      {!rawEnv && !envError ? <Loading /> : null}
      {crash ? (
        <Banner tone="error">
          {t('ticket.need.crash')}
          <RawDetails text={crash} />
        </Banner>
      ) : null}
      {shownPlan && shownPlan.issues.length ? (
        <ul class="tk-issues" data-testid="replace-issues">
          {shownPlan.issues.map((i, n) => (
            <li key={`${i.code}-${n}`} class={`tk-issue tk-issue-${i.severity}`} data-code={i.code} data-severity={i.severity}>
              <span class="tk-issue-tag">{t(`ticket.severity.${i.severity}`)}</span> {issueText(i, issueCtx)}
              <RawDetails text={rawOf(i)} />
            </li>
          ))}
        </ul>
      ) : null}
      {model ? <DisclosurePanel model={model} /> : null}
      {serverIssues.length ? (
        <Banner tone="error" data-testid="replace-error">
          <ul>
            {serverIssues.map((i, n) => (
              <li key={n}><IssueLine prefix="orders.issue" issue={i} /></li>
            ))}
          </ul>
        </Banner>
      ) : null}
      {note ? (
        <Banner tone="warn" data-testid="replace-note">
          {note.text}
          <RawDetails text={note.raw ?? null} />
        </Banner>
      ) : null}
    </Modal>
  );
}

export const replaceErrorText = (e: unknown): string => buildErrorText(e instanceof Error ? e.message : String(e));
