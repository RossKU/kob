// Replace form for the order kinds the quick amend form does not cover, which are trailing stops
// and IFD / IFO entries, and for every knob of a conditional order: band, trigger rule, trailing,
// expiry and activation. The order is turned back into its ticket intent (replace-intent.ts),
// edited in the ticket's own fields and re-planned by the ticket's planner. The planned order
// becomes the replacement in one atomic cancel + new order transaction. Nothing here signs. The
// plan goes to the confirmation flow of My orders.
import { useMemo, useState } from 'preact/hooks';
import { useServices } from '../../app/context';
import type { StrayView } from '../../data/indexer-types';
import { t } from '../../i18n';
import { issueText } from '../../i18n/issue-text';
import { buildErrorText, rawOf } from '../../i18n/build-error';
import type { CancelPlan, OrderSnapshot } from '../../kob/cancel';
import { daaToUnix } from '../../kob/daa';
import { scaleOf } from '../../kob/order-facts';
import { planOrder } from '../../kob/plan';
import { errors } from '../../kob/plan-types';
import { tickerUnit, type TokenInfo } from '../../kob/registry';
import type { Hex, OrderState } from '../../kob/types';
import { Banner, Button, HelpTip, IssueLine, Loading, Modal, RawDetails, toError } from '../kit';
import { DisclosurePanel } from '../ticket/DisclosurePanel';
import { buildDisclosureModel } from '../ticket/disclosure-model';
import { TicketFields } from '../ticket/TicketFields';
import { buildIntent, formFromIntent, layoutOf, setValue, visibleErrors, type TicketCtx, type TicketForm } from '../ticket/form-state';
import { usePairTicketEnv, useTicketEnv, useTicketPlan } from '../ticket/use-ticket';
import { describeActionError, planReplaceFor } from './actions';
import type { OrderRowModel } from './orders-model';
import { intentFromOrder } from './replace-intent';
import { replaceEnv, specOfPlan } from './replace-env';

export interface ReplaceDialogProps {
  row: OrderRowModel;
  /** Current, proven state of the order. */
  state: OrderState;
  token: TokenInfo;
  /** A pair order's quote token B. Prices are typed in B per whole A, in the pair environment. */
  quote?: TokenInfo;
  pubkey: Hex;
  strays: readonly StrayView[];
  onClose(): void;
  /** The atomic cancel + replace plan is built. Hand it to the confirmation flow. */
  onPlanned(plan: CancelPlan, snapshot: OrderSnapshot): void;
}

// The planning environment of a replacement and the spec of its plan live in replace-env.ts, shared
// with the TN10 soak's pair amend.
export { replaceEnv, specOfPlan } from './replace-env';

/** KAS on the order UTXO now. It is the indexer's current output, else the node-resolved order. */
const orderValueOf = (row: OrderRowModel): bigint | null => {
  const v = row.entry.view?.current?.value ?? row.entry.resolved?.order?.amount ?? null;
  return v === null ? null : BigInt(v);
};

export function ReplaceDialog(props: ReplaceDialogProps) {
  const services = useServices();
  const { row, state, token, quote } = props;
  // A ticker that reads as KAS carries 8 + 8 hex of the id (`tickerUnit`).
  const unit = tickerUnit(token.ticker, token.covenantId);
  const quoteUnit = quote ? tickerUnit(quote.ticker, quote.covenantId) : null;
  // A pair order re-plans against the pair environment, with both tokens and the pair book. A KAS
  // order re-plans against its token's market.
  const kasEnv = useTicketEnv(token, !quote);
  const pairEnv = usePairTicketEnv(token, quote ?? token, !!quote);
  const { env: rawEnv, error: envError, errorSource: envSource, refresh } = quote ? pairEnv : kasEnv;
  const rateMilli = rawEnv?.clock.rateMilli;
  // The replaced order quotes per its own scale, in base units per whole token or, for a pair
  // order, per whole A. The form reads and writes its prices in it.
  const scale = scaleOf(state);
  const ctx: TicketCtx = useMemo(
    () => ({
      decimals: token.decimals, scale, tick: token.tick ?? 1n, tzOffsetMin: new Date().getTimezoneOffset(), ...(rateMilli ? { rateMilli } : {}),
      ...(quote ? { quoteDecimals: quote.decimals } : {}),
    }),
    [token.decimals, scale, token.tick, rateMilli, quote?.decimals],
  );
  const source = useMemo(
    () => {
      const clock = rawEnv?.clock ?? null;
      const daaToUnixOf = clock ? (daa: bigint) => daaToUnix(clock, daa) : undefined;
      return intentFromOrder(
        state,
        {
          typeKey: row.typeKey, expiryKind: row.expiry.kind, nowDaa: clock?.daa ?? null, expiryUnix: row.expiry.approxUnix, daaToUnix: daaToUnixOf, clock,
          value: orderValueOf(row), ...(token.tick ? { tick: token.tick } : {}),
        },
        services.kob,
      );
    },
    // The clock only decides whether a future activation is kept, so the first reading is enough.
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
  // the field help of the form, off at first as on the ticket
  const [helpOn, setHelpOn] = useState(false);

  const shownPlan = current ? plan : null;
  const planErrors = shownPlan ? errors(shownPlan) : [];
  const model = useMemo(
    () =>
      shownPlan?.disclosure && env
        ? buildDisclosureModel(shownPlan, {
            ticker: unit, decimals: token.decimals, scale: shownPlan.disclosure.scale, clock: env.clock, ...(quote ? { quote: { ticker: quoteUnit!, decimals: quote.decimals } } : {}),
          })
        : null,
    [shownPlan, env, unit, token.decimals, quoteUnit, quote?.decimals],
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
      // Re-plan on fresh chain data, then build the atomic cancel + replacement from the node's
      // view of the order.
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

  const layout = shownForm ? layoutOf(shownForm, ctx) : null;
  const errs = built && shownForm ? visibleErrors(built.errors).filter((e) => (shownForm.values[e.field] ?? '') !== '') : [];
  const issueCtx = { tokenDecimals: token.decimals, tokenTicker: unit, ...(quote ? { quoteDecimals: quote.decimals, quoteTicker: quoteUnit! } : {}) };

  return (
    <Modal
      title={t('orders.replace.title', { type: t(`orders.type.${row.typeKey}`) })}
      help={<HelpTip label={t('orders.replace.title', { type: t(`orders.type.${row.typeKey}`) })} data-testid="replace-help">{t('orders.replace.explain')}</HelpTip>}
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
      {row.armed === true ? <Banner tone="warn">{t('orders.amend.restartsUnarmed')}</Banner> : null}
      {row.typeKey === 'ifd' || row.typeKey === 'ifo' ? <p class="small muted" data-testid="replace-ifd-note">{t('orders.replace.ifdNote')}</p> : null}
      {row.typeKey === 'dutch' ? <p class="small muted" data-testid="replace-dutch-note">{t('orders.replace.dutchNote')}</p> : null}
      {!source.ok ? (
        <Banner tone="warn" data-testid="replace-unsupported">{t(`orders.replace.unsupported.${source.reason}`)}</Banner>
      ) : !shownForm || !layout ? null : (
        <div class={`stack-sm tk${helpOn ? '' : ' tk-help-off'}`} data-testid="replace-form" data-type={shownForm.type} data-side={shownForm.side}>
          {/* the field help is off at first, as on the ticket: the "?" at the end of this row shows it */}
          <div class="tk-siderow">
            <p class="small grow" style="margin:0" data-testid="replace-type">
              <strong>{t(`ticket.type.${shownForm.type}.name`)}</strong> · {t(shownForm.side === 'buy' ? 'common.side.buy' : 'common.side.sell')}
            </p>
            <button
              type="button"
              class="tk-help-toggle"
              aria-pressed={helpOn ? 'true' : 'false'}
              title={t(helpOn ? 'ticket.helpToggle.hide' : 'ticket.helpToggle.show')}
              aria-label={t('ticket.helpToggle.label')}
              data-testid="replace-help-toggle"
              onClick={() => setHelpOn(!helpOn)}
            >
              ?
            </button>
          </div>
          <TicketFields form={shownForm} ctx={ctx} ticker={unit} ids={layout.main} errors={errs} onChange={onChange} {...(quote ? { quoteTicker: quoteUnit! } : {})} />
          <details class="tk-advanced" data-testid="replace-advanced">
            <summary>{t('ticket.advanced')}</summary>
            <div class="tk-advanced-body">
              <TicketFields form={shownForm} ctx={ctx} ticker={unit} ids={layout.advanced} errors={errs} onChange={onChange} {...(quote ? { quoteTicker: quoteUnit! } : {})} />
            </div>
          </details>
        </div>
      )}
      {envError ? (
        <Banner tone="error" data-testid="replace-env-error">
          {t(`ticket.need.env.${envSource ?? 'other'}`)}
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
      {/* only what stops the replacement, as on the ticket: the planner's warnings and notes are not shown */}
      {shownPlan && shownPlan.issues.some((i) => i.severity === 'error') ? (
        <ul class="tk-issues" data-testid="replace-issues">
          {shownPlan.issues.filter((i) => i.severity === 'error').map((i, n) => (
            <li key={`${i.code}-${n}`} class="tk-issue tk-issue-error" data-code={i.code} data-severity={i.severity}>
              {issueText(i, issueCtx)}
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
