// The order-entry form for one token: EVERY order type of docs/spec/order-types.md.
//
// A thin renderer: the form keeps text (`form-state.ts` turns it into an Intent), the planner (`kob/plan.ts`) validates it against the protocol and
// wallet rules and builds the transaction, `disclosure-model.ts` explains what the order costs and does, and the confirmation screen
// (`ui/confirm`) decodes the built transaction once more before anything is signed. Review is enabled only for a built, error-free plan.
import { useEffect, useMemo, useRef, useState } from 'preact/hooks';
import { useServices, useWallet } from '../../app/context';
import { explorerTxUrl } from '../../config';
import { has, t } from '../../i18n';
import { issueText, keyedText } from '../../i18n/issue-text';
import { buildErrorText, rawOf } from '../../i18n/build-error';
import { planOrder, type Intent } from '../../kob/plan';
import { errors, isPairEnv, type OrderPlan } from '../../kob/plan-types';
import type { TokenInfo } from '../../kob/registry';
import type { ExpectedSigning } from '../../kob/decode';
import type { BuiltTx } from '../../kob/types';
import { formatKas, formatUnits } from '../../kob/units';
import { Banner, Button, CopyText, Loading, RawDetails, Segmented } from '../kit';
import { ConfirmSign, type ConfirmResult } from '../confirm/ConfirmSign';
import { oppositeSide, pairLabel } from '../market/orientation';
import { needsOpenCaution, OpenTokenCaution, PowersWarning } from '../market/TokenBadges';
import { DisclosurePanel } from './DisclosurePanel';
import { useCovenantSignGate } from './CovenantSignGate';
import { TicketFields } from './TicketFields';
import { buildDisclosureModel } from './disclosure-model';
import {
  TYPE_GROUPS, buildIntent, formatAmount, formatPrice, initialForm, layoutOf, maxAmount, parsePrice, setSide, setValue, sidesOf, switchType, visibleErrors,
  type OrderTypeId, type Side, type TicketCtx, type TicketForm,
} from './form-state';
import { expectedFromPlan } from './plan-expected';
import { expandLadder, ladderOfForm, ladderTotals } from './ladder';
import { RegistryNote } from '../shell/RegistryNote';
import { usePairTicketEnv, useTicketEnv, useTicketPlan } from './use-ticket';
import './ticket.css';

export interface OrderTicketProps {
  /** the token traded (the BASE token A of a pair) */
  token: TokenInfo;
  /**
   * a token/token pair A/B: the QUOTE token B. The ticket then offers exactly the same order types, prices are typed in B per whole A, amounts in A,
   * tips stay KAS; the planner builds pair orders (KobPair / KobCondPair / KobIfdPair) against the pair environment (both tokens, the pair book).
   * Absent = the token's KAS market.
   */
  quote?: TokenInfo;
  /** optional prefill from the book (click on a level) */
  /** `price`: the state price (sompi per whole token); `amount`: base units */
  prefill?: { side?: 'buy' | 'sell'; price?: bigint; amount?: bigint; type?: OrderTypeId };
  /** template capabilities of the token program (indexer `powers`): freeze / seize put escrowed orders at the issuer's discretion */
  powers?: readonly string[];
  /**
   * the market is shown inverted (KAS/TOKEN): the side buttons and the price inputs follow the shown pair (Buy = buy KAS = sell the token). Display only:
   * the form keeps native terms and builds the very same intent either way (orientation.ts).
   */
  inverted?: boolean;
}

interface ConfirmState {
  built: BuiltTx;
  expected: ExpectedSigning;
  title: string;
  label: string;
}

const DEFAULT_CARRIER = 10n * 100_000_000n;
const sum = (xs: bigint[]): bigint => xs.reduce((a, b) => a + b, 0n);

export function OrderTicket(props: OrderTicketProps) {
  const { token, quote } = props;
  // a pair is never shown inverted here: the pair page flips by swapping base and quote (another pair, another route)
  const inverted = !quote && !!props.inverted;
  const name = token.ticker;
  /** the ticker the prices are in: KAS, or the pair's quote token B */
  const quoteTicker = quote ? quote.ticker : 'KAS';
  const services = useServices();
  const wallet = useWallet();
  const tradable = token.tradable && token.program !== null && (!quote || (quote.tradable && quote.program !== null));
  // the order scale of this token (10^decimals, at most 10^9; kob-wasm defaultScale): every price is per that many base units
  const scale = 10n ** BigInt(Math.min(token.decimals, 9));
  const walletReady = !!wallet.info && !!wallet.adapter && !wallet.networkMismatch;
  const enabled = tradable && walletReady;

  const [form, setForm] = useState<TicketForm>(() => initialForm('limit', props.prefill?.side ?? 'sell'));
  const [confirm, setConfirm] = useState<ConfirmState | null>(null);
  const [reviewing, setReviewing] = useState(false);
  // a plain note, or a refusal: its sentence with the raw builder text behind "Details"
  const [reviewNote, setReviewNote] = useState<string | { text: string; raw: string } | null>(null);
  const [result, setResult] = useState<{ txid: string; label: string; note?: string } | null>(null);
  // a ladder in progress: the intents of every level and the txids placed so far (each level is its own transaction, signed in turn)
  const run = useRef<{ intents: Intent[]; txids: string[] } | null>(null);

  // one of the two environments is live: the token's KAS market, or the pair A/B (both tokens' UTXOs, the pair book, own pair orders)
  const kasEnv = useTicketEnv(token, enabled && !quote);
  const pairEnv = usePairTicketEnv(token, quote ?? token, enabled && !!quote);
  const { env: rawEnv, error: envError, loading: envLoading, refresh } = quote ? pairEnv : kasEnv;
  // guards whose indexer data could not be read block the plan until the user acknowledges
  const [ackGuards, setAckGuards] = useState(false);
  const env = useMemo(() => (rawEnv ? { ...rawEnv, guardsAcknowledged: ackGuards } : null), [rawEnv, ackGuards]);
  const rateMilli = env?.clock.rateMilli;
  const ctx: TicketCtx = useMemo(
    () => ({
      decimals: token.decimals, scale, tick: token.tick ?? 1n, tzOffsetMin: new Date().getTimezoneOffset(), ...(rateMilli ? { rateMilli } : {}),
      ...(quote ? { quoteDecimals: quote.decimals } : {}),
    }),
    [token.decimals, scale, token.tick, rateMilli, quote?.decimals],
  );
  const built = useMemo(() => buildIntent(form, ctx), [form, ctx]);
  const { plan, current, pending, crash } = useTicketPlan(enabled ? env : null, built.intent);

  // prefill from the book: a price implies a limit-style form, a side the matching order type
  const prefillKey = props.prefill ? `${props.prefill.type ?? ''}|${props.prefill.side ?? ''}|${props.prefill.price ?? ''}|${props.prefill.amount ?? ''}` : '';
  useEffect(() => {
    const p = props.prefill;
    if (!p) return;
    setForm((f0) => {
      let f = p.type ? switchType(f0, p.type) : f0;
      if (p.side && !sidesOf(f.type).includes(p.side)) f = switchType(f, 'limit');
      if (p.side) f = setSide(f, p.side);
      if (p.price !== undefined) {
        if (!layoutOf(f).main.includes('price')) f = switchType(f, 'limit');
        f = setValue(f, 'price', formatPrice(p.price, ctx));
      }
      if (p.amount !== undefined) f = setValue(f, 'amount', formatAmount(p.amount, token.decimals));
      return f;
    });
  }, [prefillKey]);

  const ladderSet = useMemo(() => ladderOfForm(form, ctx), [form, ctx]);
  const ladder = useMemo(() => (ladderSet && built.intent ? expandLadder(built.intent, ladderSet.levels, ladderSet.step, ctx.tick) : null), [ladderSet, built.intent, ctx.tick]);

  const layout = layoutOf(form);
  const errs = visibleErrors(built.errors).filter((e) => (form.values[e.field] ?? '') !== '');
  const ticker = token.ticker;

  // balances the wallet can spend (the planner does the exact funding check)
  const tokenBalance = env ? sum(env.tokenUtxos.map((u) => BigInt(u.state.amount))) : 0n;
  const kasBalance = env ? sum(env.funding.map((u) => BigInt(u.amount))) : 0n;
  const pairPart = env && isPairEnv(env) ? env.pair : null;
  /** a pair: the quote token B the wallet can spend (a buy pays in B) */
  const quoteBalance = pairPart ? sum(pairPart.quoteTokenUtxos.map((u) => BigInt(u.state.amount))) : 0n;
  const refPrice = (() => {
    const own = parsePrice(form.values.price ?? form.values.displayedPrice ?? '', form.side, ctx);
    if (own.ok) return own.value.price;
    return env?.book.asks[0]?.price ?? env?.book.bids[0]?.price ?? 0n;
  })();
  // what the inverted fields need to speak KAS: the price of the form (else the book's best), per whole token of `scale` base units
  const ownPrice = parsePrice(form.values.price ?? form.values.displayedPrice ?? '', form.side, ctx).ok;
  const flip = { name, price: refPrice > 0n ? refPrice : null, ownPrice, scale, decimals: token.decimals };
  const onMax = () => {
    const tip = built.intent && 'tip' in built.intent && built.intent.tip ? built.intent.tip : 0n;
    // a pair: a sell uses the A balance, a buy the B balance at the price (B per whole A; the KAS tip is not part of it), less one base unit
    // of B per possible fill (the escrow slack of a bid)
    const n = quote
      ? form.side === 'sell'
        ? tokenBalance
        : refPrice > 0n && quoteBalance > 4n
          ? ((quoteBalance - 4n) * scale) / refPrice
          : 0n
      : maxAmount({ side: form.side, tokenBalance, kasBalance, scale, allInPrice: refPrice + tip, carrier: env?.carrier ?? DEFAULT_CARRIER });
    setForm((f) => setValue(f, 'amount', n > 0n ? formatAmount(n, token.decimals) : ''));
  };

  const onChange = (id: string, value: string) => {
    setReviewNote(null);
    setForm((f) => setValue(f, id, value));
  };
  const onType = (type: OrderTypeId) => {
    setReviewNote(null);
    setForm((f) => switchType(f, type));
  };
  const onSide = (side: Side) => {
    setReviewNote(null);
    setForm((f) => setSide(f, side));
  };

  const shownPlan: OrderPlan | null = enabled && current ? plan : null;
  const planErrors = shownPlan ? errors(shownPlan) : [];
  const model = useMemo(
    () =>
      shownPlan?.disclosure && env
        ? buildDisclosureModel(shownPlan, {
            ticker, decimals: token.decimals, scale: shownPlan.disclosure.scale, clock: env.clock, ...(quote ? { quote: { ticker: quote.ticker, decimals: quote.decimals } } : {}),
          })
        : null,
    [shownPlan, env, ticker, token.decimals, quote?.ticker, quote?.decimals],
  );
  const ladderTot = useMemo(() => (shownPlan && ladder ? ladderTotals(shownPlan, ladder) : null), [shownPlan, ladder]);
  // C5-10: a wallet not known to sign covenant inputs (every cancel needs one) passes a one-time test signature before its first order
  const gate = useCovenantSignGate(enabled ? env : null);
  const canReview = enabled && gate.ready && !!shownPlan && shownPlan.ok && planErrors.length === 0 && !!shownPlan.built && !pending && !reviewing && !confirm && !crash && (!ladder || ladder.ok);

  // an inverted market words the type where the native name / help speaks of buying or selling the token
  const typeKey = (id: OrderTypeId, part: 'name' | 'help'): string => (inverted && has(`ticket.type.${id}.${part}Inv`) ? `ticket.type.${id}.${part}Inv` : `ticket.type.${id}.${part}`);
  const typeName = t(typeKey(form.type, 'name'), { name });
  // the side as the shown pair names it: buying KAS in KAS/TOKEN is selling the token
  const shownSide: Side = inverted ? oppositeSide(form.side) : form.side;
  const sideName = t(shownSide === 'buy' ? 'common.side.buy' : 'common.side.sell');
  // the confirmation names the pair too: an inverted Buy is a sale of the token (the decoded card below says exactly what the transaction does)
  const confirmSide = inverted ? `${sideName} ${pairLabel(name, true)}` : sideName;

  /** Plans level `index` of the run against freshly read coins and opens its confirmation. A level that cannot be planned ends the run. */
  const prepareLevel = async (index: number): Promise<void> => {
    const r = run.current;
    if (!r) return;
    const levels = r.intents.length;
    const laddered = levels > 1;
    const stop = (note: string) => {
      run.current = null;
      setReviewNote(laddered && r.txids.length > 0 ? t('ticket.ladder.stoppedAt', { level: index + 1, levels, placed: r.txids.length }) : note);
    };
    setReviewing(true);
    setReviewNote(null);
    try {
      // re-read the chain first: the coins may have moved since the last refresh (a ladder level has just spent some), and the transaction to sign must be built from fresh data
      const fresh = await refresh();
      if (!fresh) return stop(t('ticket.reviewNoEnv'));
      const p = planOrder({ ...fresh, guardsAcknowledged: ackGuards }, r.intents[index]!);
      if (!p.ok || !p.built || errors(p).length > 0) return stop(t('ticket.reviewChanged'));
      const title = laddered ? t('ticket.ladder.confirmTitle', { level: index + 1, levels, side: confirmSide, type: typeName }) : t('ticket.confirmTitle', { side: confirmSide, type: typeName });
      const label = laddered ? `${form.type} ${form.side} ${index + 1}/${levels}` : `${form.type} ${form.side}`;
      setConfirm({ built: p.built, expected: expectedFromPlan(p), title, label });
    } catch (e) {
      run.current = null;
      const raw = e instanceof Error ? e.message : String(e);
      setReviewNote({ text: buildErrorText(raw), raw });
    } finally {
      setReviewing(false);
    }
  };

  const review = async () => {
    if (!canReview || !built.intent) return;
    run.current = { intents: ladder?.ok ? ladder.levels.map((l) => l.intent) : [built.intent], txids: [] };
    await prepareLevel(0);
  };

  const onConfirmClose = (r: ConfirmResult) => {
    const c = confirm;
    const lr = run.current;
    setConfirm(null);
    if (r.status === 'submitted') {
      if (lr) lr.txids.push(r.txid);
      const levels = lr?.intents.length ?? 1;
      if (lr && lr.txids.length < levels) {
        // the next level: its own plan, its own confirmation and signature
        void prepareLevel(lr.txids.length);
        return;
      }
      run.current = null;
      const label = levels > 1 ? `${form.type} ${form.side} x${levels}` : c?.label ?? '';
      setResult({ txid: r.txid, label, ...(levels > 1 ? { note: t('ticket.ladder.done', { levels }) } : {}) });
      // the order is placed: the next one starts empty (the price, type and side stay: the next click is usually a variation)
      setForm((f) => setValue(f, 'amount', ''));
    } else {
      run.current = null;
      if (lr && lr.txids.length > 0) setReviewNote(t('ticket.ladder.stopped', { placed: lr.txids.length, levels: lr.intents.length }));
      else if (r.status === 'failed') setReviewNote(t('ticket.replanNote'));
    }
    void refresh();
  };

  // ---- status lines: what is needed and what is missing
  const needs: { code: string; text: string; tone: 'info' | 'warn' | 'error'; action?: 'retry'; raw?: string }[] = [];
  if (!wallet.info) needs.push({ code: 'wallet', text: t('ticket.need.wallet'), tone: 'info' });
  else if (wallet.networkMismatch) needs.push({ code: 'network', text: t('ticket.need.network', { wallet: wallet.info.network, app: services.config.network }), tone: 'error' });
  if (enabled && envError) needs.push({ code: 'env', text: t('ticket.need.env'), tone: 'error', action: 'retry', raw: envError });
  if (crash) needs.push({ code: 'crash', text: t('ticket.need.crash'), tone: 'error', raw: crash });
  if (enabled && !envError && built.intent === null && built.errors.some((e) => e.code === 'required')) needs.push({ code: 'incomplete', text: t('ticket.need.incomplete'), tone: 'info' });

  const issues = shownPlan?.issues ?? [];
  const issueCtx = { tokenDecimals: token.decimals, tokenTicker: ticker, ...(quote ? { quoteDecimals: quote.decimals, quoteTicker: quote.ticker } : {}) };

  if (!tradable) {
    const reason = token.untradableReason;
    return (
      <section class="tk" data-testid="order-ticket" data-state="untradable">
        <h2>{t('ticket.title')}</h2>
        <Banner tone="warn" title={t('ticket.untradable.title', { ticker })} data-testid="order-untradable">
          <p>{reason ? keyedText('untradable', reason) : t('ticket.untradable.generic')}</p>
          <p class="muted">{t('ticket.untradable.hint')}</p>
        </Banner>
      </section>
    );
  }

  return (
    <section class="tk" data-testid="order-ticket" data-type={form.type} data-side={form.side} data-shown-side={shownSide} data-inverted={inverted ? '1' : '0'} aria-busy={pending || envLoading ? 'true' : 'false'}>
      <div class="row-between">
        <h2>{t('ticket.title')}</h2>
        {pending || reviewing ? (
          <span class="tk-pending" data-testid="order-pending">
            <Loading text={t('ticket.pending')} />
          </span>
        ) : null}
      </div>

      {result ? (
        <Banner tone="ok" title={t('ticket.result.title')} onDismiss={() => setResult(null)} data-testid="order-result">
          <p>{t('ticket.result.text', { label: result.label })}</p>
          {result.note ? <p data-testid="order-result-ladder">{result.note}</p> : null}
          <p>
            <CopyText value={result.txid} short={false} href={explorerTxUrl(services.config, result.txid)} data-testid="order-result-txid" />
          </p>
        </Banner>
      ) : null}

      <div class="tk-form">
        {inverted ? (
          <p class="small muted" data-testid="ticket-flip-note">
            {t('ticket.flip.note', { pair: pairLabel(name, true), ticker, name })}
          </p>
        ) : null}
        <Segmented<Side>
          block
          aria-label={t('ticket.side')}
          value={shownSide}
          onChange={(s) => onSide(inverted ? oppositeSide(s) : s)}
          options={(['buy', 'sell'] as Side[]).map((s) => ({
            value: s,
            label: t(s === 'buy' ? 'common.buy' : 'common.sell'),
            tone: s,
            disabled: !sidesOf(form.type).includes(inverted ? oppositeSide(s) : s),
            'data-testid': s === 'buy' ? 'order-side-buy' : 'order-side-sell',
          }))}
        />

        <div class="field">
          <label for="order-type">{t('ticket.type.label')}</label>
          <select id="order-type" class="select" value={form.type} data-testid="order-type" onChange={(e) => onType((e.currentTarget as HTMLSelectElement).value as OrderTypeId)}>
            {TYPE_GROUPS.map((g) => (
              <optgroup key={g.group} label={t(`ticket.group.${g.group}`)}>
                {g.types.map((id) => (
                  <option key={id} value={id}>
                    {t(typeKey(id, 'name'), { name })}
                  </option>
                ))}
              </optgroup>
            ))}
          </select>
          <div class="field-hint" data-testid="order-type-help">
            {t(typeKey(form.type, 'help'), { name })}
          </div>
        </div>

        {enabled ? (
          <p class="tk-balances" data-testid="order-balances">
            {env
              ? quote
                ? t('ticket.balancePair', {
                    tokens: `${formatUnits(tokenBalance, token.decimals, { group: ',' })} ${ticker}`,
                    quote: `${formatUnits(quoteBalance, quote.decimals, { group: ',' })} ${quote.ticker}`,
                    kas: `${formatKas(kasBalance, { group: ',' })} KAS`,
                  })
                : t('ticket.balance', { tokens: `${formatUnits(tokenBalance, token.decimals, { group: ',' })} ${ticker}`, kas: `${formatKas(kasBalance, { group: ',' })} KAS` })
              : t('ticket.balanceLoading')}
          </p>
        ) : null}

        <TicketFields
          form={form} ctx={ctx} ticker={ticker} ids={layout.main} errors={errs} onChange={onChange} onMax={enabled && env ? onMax : undefined}
          {...(inverted ? { flip } : {})} {...(quote ? { quoteTicker } : {})}
        />

        <details class="tk-advanced" data-testid="order-advanced">
          <summary>{t('ticket.advanced')}</summary>
          <div class="tk-advanced-body">
            <TicketFields form={form} ctx={ctx} ticker={ticker} ids={layout.advanced} errors={errs} onChange={onChange} {...(inverted ? { flip } : {})} {...(quote ? { quoteTicker } : {})} />
          </div>
        </details>
      </div>

      <div class="tk-needs" data-testid="order-needs">
        {needs.map((n) => (
          <Banner
            key={n.code}
            tone={n.tone}
            data-testid={`order-need-${n.code}`}
            actions={n.action === 'retry' ? <Button small onClick={() => void refresh()} data-testid="order-retry">{t('common.retry')}</Button> : undefined}
          >
            {n.text}
            <RawDetails text={n.raw} />
          </Banner>
        ))}
      </div>

      {enabled && env?.guardsUnavailable?.length ? (
        <Banner tone="warn" title={t('ticket.guards.title')} data-testid="order-guards-unavailable">
          <p>{t('ticket.guards.text', { guards: env.guardsUnavailable.join(', ') })}</p>
          <label class="row" style="gap:6px">
            <input type="checkbox" checked={ackGuards} onChange={(e) => setAckGuards((e.currentTarget as HTMLInputElement).checked)} data-testid="order-guards-ack" />
            <span>{t('ticket.guards.ack')}</span>
          </label>
        </Banner>
      ) : null}

      <div class="tk-issues" data-testid="order-issues" aria-live="polite">
        {issues.length > 0 ? (
          <ul>
            {issues.map((i, n) => (
              <li key={`${i.code}-${n}`} class={`tk-issue tk-issue-${i.severity}`} data-code={i.code} data-severity={i.severity} data-testid={`order-issue-${i.code}`}>
                <span class="tk-issue-tag">{t(`ticket.severity.${i.severity}`)}</span> {issueText(i, issueCtx)}
                <RawDetails text={rawOf(i)} />
              </li>
            ))}
          </ul>
        ) : null}
      </div>

      <RegistryNote data-testid="ticket-registry-note" />
      <OpenTokenCaution token={token} data-testid="ticket-open-caution" />
      <PowersWarning powers={props.powers} data-testid="ticket-powers-warning" />

      {ladder ? (
        <div class="tk-ladder" data-testid="order-ladder">
          <h3>{t('ticket.ladder.title', { levels: ladder.levels.length })}</h3>
          {ladder.ok ? (
            <>
              <ol data-testid="order-ladder-levels">
                {ladder.levels.map((l) => (
                  <li key={l.level} data-testid={`order-ladder-level-${l.level}`}>
                    {t(l.exitStop !== null ? 'ticket.ladder.rowStop' : 'ticket.ladder.row', {
                      level: l.level,
                      entry: `${formatPrice(l.entryPrice, ctx)} ${quoteTicker}`,
                      exit: l.exitTakeProfit !== null ? `${formatPrice(l.exitTakeProfit, ctx)} ${quoteTicker}` : '-',
                      stop: l.exitStop !== null ? `${formatPrice(l.exitStop, ctx)} ${quoteTicker}` : '-',
                    })}
                  </li>
                ))}
              </ol>
              {ladderTot ? (
                <p data-testid="order-ladder-total">
                  {t('ticket.ladder.total', { amount: `${formatUnits(ladderTot.amount, token.decimals, { group: ',' })} ${ticker}`, levels: ladder.levels.length, kas: `${formatKas(ladderTot.kasLocked, { group: ',' })} KAS` })}
                </p>
              ) : null}
              {env && ladderTot && form.side === 'buy' && !quote && ladderTot.kasLocked > kasBalance ? (
                <p class="tk-issue tk-issue-warning" data-testid="order-ladder-funds">
                  {t('ticket.ladder.fundsKas', { kas: `${formatKas(ladderTot.kasLocked, { group: ',' })} KAS`, have: `${formatKas(kasBalance, { group: ',' })} KAS` })}
                </p>
              ) : null}
              {env && ladderTot && (!quote || form.side === 'sell') && ladderTot.tokens > tokenBalance ? (
                <p class="tk-issue tk-issue-warning" data-testid="order-ladder-funds">
                  {t('ticket.ladder.fundsTokens', { need: formatUnits(ladderTot.tokens, token.decimals, { group: ',' }), have: formatUnits(tokenBalance, token.decimals, { group: ',' }), ticker })}
                </p>
              ) : null}
              <p class="muted">{t('ticket.ladder.separate', { levels: ladder.levels.length })}</p>
            </>
          ) : (
            <ul data-testid="order-ladder-errors">
              {ladder.errors.map((e, n) => (
                <li key={n} class="tk-issue tk-issue-error">
                  {t(`ticket.ladder.err.${e.code}`, { level: e.level ?? 1, field: e.field ?? '', max: 20 })}
                </li>
              ))}
            </ul>
          )}
        </div>
      ) : null}

      {model ? <DisclosurePanel model={model} built={shownPlan?.built ?? null} /> : null}

      {reviewNote ? (
        <Banner tone="warn" data-testid="order-review-note">
          {typeof reviewNote === 'string' ? reviewNote : (
            <>
              {reviewNote.text}
              <RawDetails text={reviewNote.raw} />
            </>
          )}
        </Banner>
      ) : null}

      {enabled ? gate.element : null}
      <Button variant="primary" block onClick={review} disabled={!canReview} loading={reviewing} data-testid="order-review">
        {t('ticket.review', { side: sideName, type: typeName })}
      </Button>
      <p class="tk-blind">{t('ticket.blindNote')}</p>

      {confirm ? (
        <ConfirmSign
          built={confirm.built}
          expected={confirm.expected}
          title={confirm.title}
          label={confirm.label}
          tokenPowers={props.powers}
          openToken={token.openList ? token : undefined}
          cautionToken={!token.openList && needsOpenCaution(token) ? token : undefined}
          onSubmitted={() => void refresh()}
          onClose={onConfirmClose}
        />
      ) : null}
    </section>
  );
}
