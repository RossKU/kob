import { formatUnits } from '../../kob/units';
import { useMemo, useState } from 'preact/hooks';
import { useServices, useWallet } from '../../app/context';
import { routeToHash, tokenRoute } from '../../app/router';
import { formatDateTime, t } from '../../i18n';
import type { CancelPlan, OrderSnapshot } from '../../kob/cancel';
import type { Position } from '../../kob/positions';
import type { Hex } from '../../kob/types';
import { Banner, Button, ErrorBanner, Loading, Modal, RawDetails, Section, Tabs, showToast, toError, useAsync } from '../kit';
import { useLiveRefresh } from '../market/live';
import { useSystemStatus } from '../shell/StatusProvider';
import { AmendDialog } from './AmendDialog';
import { ReplaceDialog } from './ReplaceDialog';
import { AutoRefundToggle } from './AutoRefund';
import { BalancesPanel } from './BalancesPanel';
import { OrderRow, type OrderRowActions } from './OrderRow';
import { PositionCard } from './PositionCard';
import { RecoverPanel } from './RecoverPanel';
import { StraysPanel } from './StraysPanel';
import { PlanErrors, TxFlow, type FlowOutcome, type FlowStep, type TxFlowProps } from './TxFlow';
import { mergeFailure, mergeStep, mergeSummary, reservedOutpoints } from './merge-model';
import { unwindAmount } from './position-model';
import { fillHistory, fillsCsv, fillsFileName } from './fills-model';
import { filledAmountOf } from './OrderFills';
import { downloadText, exportFileName } from '../kit/download';
import { backupFileText } from './recover';
import {
  abandoningOrders, abandonsStrays, describeActionError, liveExitsOfRepeatEntry, markCancelling, nextMergeLink, rawActionError, planCancelFor, planCancelMany, planMergeFor,
  planRefundFor, planSweepFor, planSweepMany, warnEntryOnly,
} from './actions';
import { amendKind, describeEntries, entryState, countByStatus, type OrderRowModel } from './orders-model';
import { buildListItems, cancellableIds, filterItems, tabCounts, type ListTab } from './list-model';
import { loadOrderEvents, loadOrders } from './orders-data';

interface Flow {
  key: number;
  heading: string;
  steps: FlowStep[];
  /** a chained flow (token merge): builds step `index` once the previous one was submitted */
  next?: TxFlowProps['next'];
  note?: string;
}

const REFRESH_MS = 15_000;

/**
 * `#/orders`: the wallet's orders (indexer + placement records resolved on the node), positions with their exits, balances the wallet does not show,
 * strays, cancel / amend / refund / cancel-all, and recovery. Nothing here signs: every transaction goes through <ConfirmSign>.
 */
export function OrdersView() {
  const services = useServices();
  const wallet = useWallet();
  const { indexer: health } = useSystemStatus();
  const pubkey = wallet.info?.pubkey;
  const canSign = !!wallet.info && !wallet.networkMismatch;

  const data = useAsync(
    async (signal) => (pubkey ? loadOrders(services, pubkey, wallet.records, { signal }) : null),
    [pubkey, wallet.records, services],
  );
  useLiveRefresh(services, ['fills'], () => true, data.reload, REFRESH_MS);

  const [tab, setTab] = useState<ListTab>('active');
  const [flow, setFlow] = useState<Flow | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [failure, setFailure] = useState<string | null>(null);
  const [failureRaw, setFailureRaw] = useState<string | null>(null);
  const [planFail, setPlanFail] = useState<{ plans: CancelPlan[]; failed: { id: string; message: string }[] } | null>(null);
  const [amend, setAmend] = useState<OrderRowModel | null>(null);
  const [replaceMode, setReplaceMode] = useState(false);
  const [confirmAll, setConfirmAll] = useState<{ token: Hex | null; ids: Hex[] } | null>(null);

  const [sweepFirst, setSweepFirst] = useState<{ ids: Hex[]; count: number; proceed: () => void; retry: () => void } | null>(null);
  const [sweepFollowUp, setSweepFollowUp] = useState<{ pending: boolean; retry: () => void } | null>(null);

  const d = data.data ?? null;
  // the maker-wide stray list counts every live stray per order (the list views of the indexer carry no strays)
  const rows = useMemo(() => {
    if (!d) return [];
    const strayCounts = new Map<string, number>();
    for (const s of d.strays) if (!s.spent && !s.lost) strayCounts.set(s.owner, (strayCounts.get(s.owner) ?? 0) + 1);
    return describeEntries(d.entries, { kob: services.kob, clock: d.clock }).map((r) => ((strayCounts.get(r.id) ?? 0) > r.strayCount ? { ...r, strayCount: strayCounts.get(r.id)! } : r));
  }, [d, services.kob]);
  const rowById = useMemo(() => new Map(rows.map((r) => [r.id, r])), [rows]);
  const entryById = useMemo(() => new Map((d?.entries ?? []).map((e) => [e.id, e])), [d]);
  const items = useMemo(() => (d ? buildListItems(d.entries, d.positions) : []), [d]);
  const counts = useMemo(() => tabCounts(items, rowById), [items, rowById]);
  const shown = useMemo(() => filterItems(items, rowById, tab), [items, rowById, tab]);
  const stats = useMemo(() => countByStatus(rows), [rows]);
  // the tokens of each order (its own and a pair order's B): a stray of any other token is foreign
  const sweepable = useMemo(() => new Map(rows.filter((r) => r.token).map((r) => [r.id, [r.token!, ...(r.pair ? [r.pair.quote] : [])]])), [rows]);
  // live orders of the wallet whose strays a sweep can return (the order lives on)
  const sweepableOrders = useMemo(() => new Set(rows.filter((r) => r.live && r.canCancel).map((r) => r.id)), [rows]);

  const tokenOf = (id: string | null) => (id ? services.registry.byCovenantId.get(id) : undefined);
  // an amount of a token (base units) as its token amount with the ticker; base units when the token is unknown
  const coverText = (token: Hex, amount: bigint): string => {
    const tk = tokenOf(token);
    return tk ? `${formatUnits(amount, tk.decimals, { group: ',' })} ${tk.ticker}` : amount.toString();
  };
  const strays = d?.strays ?? [];
  // a failed stray fetch is passed on as "unknown" (null): a record-path cancel then warns instead of silently abandoning strays
  const strayList = d?.straysError ? null : strays;

  const startFlow = (heading: string, steps: FlowStep[], chain: Pick<Flow, 'next' | 'note'> = {}) => {
    setPlanFail(null);
    setFailure(null);
    setFlow({ key: Date.now(), heading, steps, ...chain });
  };

  const guarded = async (id: string, fn: () => Promise<void>) => {
    setBusy(id);
    setFailure(null);
    setPlanFail(null);
    try {
      await fn();
    } catch (e) {
      setFailure(describeActionError(toError(e)));
      setFailureRaw(rawActionError(toError(e)));
    } finally {
      setBusy(null);
    }
  };

  const stepOf = (title: string, plan: CancelPlan, label?: string): FlowStep => ({ title, plan, spends: plan.cancelIds, ...(label ? { label } : {}) });

  /** Sweeps of the given orders, each in its own transaction (the order lives on); after them the caller's cancel can be retried. */
  const sweepMany = (ids: Hex[], busyId: string, retry?: () => void) =>
    void guarded(busyId, async () => {
      if (!pubkey) return;
      const entries = ids.map((id) => entryById.get(id)).filter((e): e is NonNullable<typeof e> => !!e);
      const out = await planSweepMany(services, pubkey, entries, strayList);
      const good = out.plans.filter((p) => p.ok);
      const bad = out.plans.filter((p) => !p.ok);
      const failed = out.failed.map((f) => ({ id: f.id, message: describeActionError(f.error) }));
      if (bad.length || failed.length) setPlanFail({ plans: bad, failed });
      if (!good.length) return;
      setSweepFollowUp(retry ? { pending: true, retry } : null);
      startFlow(
        t('orders.flow.sweepHeading'),
        good.map((p, i) => stepOf(good.length > 1 ? t('orders.flow.sweepStep', { n: i + 1, total: good.length }) : t('orders.flow.sweepTitle'), p)),
      );
    });

  const cancelOne = (row: OrderRowModel) =>
    void guarded(row.id, async () => {
      if (!pubkey) return;
      const planned = await planCancelFor(services, pubkey, row.entry, strayList);
      const plan = warnEntryOnly(planned.plan, liveExitsOfRepeatEntry(row.entry, d?.positions ?? []));
      if (!plan.ok) return setPlanFail({ plans: [plan], failed: [] });
      const go = () => startFlow(t('orders.flow.cancelHeading'), [stepOf(t('orders.flow.cancelTitle'), plan)]);
      // C5-01: more strays than one transaction can move. A sweep first moves them while the order lives; abandoning stays a choice
      if (abandonsStrays(plan) && row.live) {
        const count = plan.issues.filter((i) => i.code === 'cancel.strays-abandoned').reduce((a, i) => a + Number(i.params?.count ?? 0), 0);
        return setSweepFirst({ ids: [row.id], count, proceed: go, retry: () => cancelOne(row) });
      }
      go();
    });

  const actions: OrderRowActions = {
    onCancel: (row) => cancelOne(row),
    onSweep: (row) =>
      void guarded(row.id, async () => {
        if (!pubkey) return;
        const { plan } = await planSweepFor(services, pubkey, row.entry, strayList);
        if (!plan.ok) return setPlanFail({ plans: [plan], failed: [] });
        setSweepFollowUp(null);
        startFlow(t('orders.flow.sweepHeading'), [stepOf(t('orders.flow.sweepTitle'), plan)]);
      }),
    onRefund: (row) =>
      void guarded(row.id, async () => {
        if (!pubkey) return;
        const { plan } = await planRefundFor(services, pubkey, row.entry, strayList);
        if (!plan.ok) return setPlanFail({ plans: [plan], failed: [] });
        startFlow(t('orders.flow.refundHeading'), [stepOf(t('orders.flow.refundTitle'), plan)]);
      }),
    onAmend: (row) => {
      setFailure(null);
      setPlanFail(null);
      setReplaceMode(false);
      setAmend(row);
    },
  };

  const cancelMany = (heading: string, ids: Hex[], busyId: string) =>
    void guarded(busyId, async () => {
      if (!pubkey) return;
      const entries = ids.map((id) => entryById.get(id)).filter((e): e is NonNullable<typeof e> => !!e);
      const out = await planCancelMany(services, pubkey, entries, strayList);
      const good = out.plans.filter((p) => p.ok);
      const bad = out.plans.filter((p) => !p.ok);
      const failed = out.failed.map((f) => ({ id: f.id, message: describeActionError(f.error) }));
      if (bad.length || failed.length) setPlanFail({ plans: bad, failed });
      if (!good.length) return;
      const go = () =>
        startFlow(
          heading,
          good.map((p, i) => stepOf(good.length > 1 ? t('orders.flow.cancelStep', { n: i + 1, total: good.length, count: p.cancelIds.length }) : t('orders.flow.cancelTitle'), p)),
        );
      // C5-01: some orders hold more strays than their cancel can move: offer to sweep those first (they live on), then cancel
      const abandoning = abandoningOrders(good).filter((id) => rowById.get(id)?.live);
      if (abandoning.length) {
        const count = good.flatMap((p) => p.issues.filter((i) => i.code === 'cancel.strays-abandoned')).reduce((a, i) => a + Number(i.params?.count ?? 0), 0);
        return setSweepFirst({ ids: abandoning, count, proceed: go, retry: () => cancelMany(heading, ids, busyId) });
      }
      go();
    });

  // the open orders' custodies and strays: a merge never spends them
  const reserved = useMemo(() => reservedOutpoints(d), [d]);

  /** Merges the wallet's plain UTXOs of `token` into one: a chain of transfers to itself, each signed on its own confirmation screen. */
  const mergeToken = (token: Hex) =>
    void guarded(`merge-${token}`, async () => {
      const info = tokenOf(token);
      if (!pubkey || !info) return;
      const plan = await planMergeFor(services, pubkey, info, reserved);
      if (!plan.ok) return setPlanFail({ plans: [mergeFailure(plan)], failed: [] });
      const summary = mergeSummary(plan, info.ticker);
      const total = plan.links.length;
      let prev = plan.links[0]!;
      startFlow(
        t('orders.merge.heading', { ticker: info.ticker }),
        plan.links.map((l, i) => mergeStep(l, i, total, summary)),
        {
          note: t('orders.merge.note'),
          next: async (k, r) => {
            const link = await nextMergeLink(services, pubkey, info, plan, k, prev, r.txid);
            prev = link;
            return mergeStep(link, k, total, summary);
          },
        },
      );
    });

  const onCancelPosition = (p: Position) => cancelMany(t('orders.flow.positionHeading'), p.cancelIds, p.id);
  // close: cancel the position's live orders, then unwind at market. Buy first: the exits hold the bought tokens, the cancel returns them and
  // step 2 sells the released + free tokens. Sell first: the exits hold the proceeds (+ prefund) in KAS, the cancel returns it and step 2 buys back
  // the amount the entry sold and the exits did not buy back yet (none: the cancel alone closes the position).
  const [closeFollowUp, setCloseFollowUp] = useState<{ token: Hex; pending: boolean; side: 'buy' | 'sell'; amount: bigint } | null>(null);
  const onClosePosition = (p: Position) => {
    const token = p.entry?.token ?? p.exits[0]?.token ?? null;
    setCloseFollowUp(token ? { token, pending: true, side: p.side, amount: unwindAmount(p) } : null);
    cancelMany(t(p.side === 'buy' ? 'orders.flow.closeHeading' : 'orders.flow.closeHeadingSell'), p.cancelIds, p.id);
  };

  const onStepSubmitted = async (step: FlowStep, result: { txid: string }) => {
    // the records stay (marked) until the cancel is final: a fill may win the race, a reorg may undo it (C5-06)
    // (a sweep continues the order: markCancelling leaves its record unmarked, ConfirmSign moved its last state to the continuation)
    if (pubkey) await markCancelling(wallet.records, { cancelIds: step.spends, built: step.plan.built }, result.txid);
    showToast(step.toast ?? (step.plan.sweep ? t('orders.flow.sweepSubmitted', { count: step.plan.sweep.utxos }) : t('orders.flow.submitted', { count: step.spends.length })), 'ok');
  };
  const onFlowDone = (o: FlowOutcome) => {
    setSweepFollowUp((c) => (c && c.pending ? (o.submitted > 0 ? { ...c, pending: false } : null) : c));
    setCloseFollowUp((c) => (c && c.pending ? (o.submitted > 0 ? { ...c, pending: false } : null) : c));
    if (o.submitted > 0) {
      data.reload();
      // the indexer needs a moment to see the new block: look again shortly
      setTimeout(data.reload, 3000);
    }
  };

  const onPlannedAmend = (plan: CancelPlan, snapshot: OrderSnapshot) => {
    const row = amend;
    setAmend(null);
    if (!row) return;
    startFlow(t('orders.flow.amendHeading'), [{ ...stepOf(t('orders.flow.amendTitle'), plan, t('orders.flow.amendLabel')), spends: [snapshot.covenantId] }]);
  };

  if (!wallet.info) {
    return (
      <div class="stack" data-testid="orders-view">
        <h1>{t('orders.title')}</h1>
        <Banner tone="info" data-testid="orders-connect">{t('orders.connect')}</Banner>
      </div>
    );
  }

  const cancellable = cancellableIds(rows, null);
  // fill history (R-8): per order on demand, and every fill of the wallet as one CSV
  const loadEvents = services.indexer && !d?.indexerError ? (id: Hex, signal: AbortSignal) => loadOrderEvents(services.indexer!, id, signal) : null;
  const filledRows = rows.filter((r) => filledAmountOf(r) > 0n);
  const exportFills = () =>
    void guarded('export-fills', async () => {
      if (!loadEvents) return;
      const ctl = new AbortController();
      const out: Parameters<typeof fillsCsv>[0][number][] = [];
      // a few requests at a time: one events request per order that filled
      for (let i = 0; i < filledRows.length; i += 4) {
        const part = filledRows.slice(i, i + 4);
        const lists = await Promise.all(part.map((r) => loadEvents(r.id, ctl.signal)));
        part.forEach((r, k) => {
          const tk = tokenOf(r.token);
          out.push({ info: { ticker: tk?.ticker ?? r.token ?? '', side: r.side, type: r.typeKey, ...(tk ? { decimals: tk.decimals } : {}) }, history: fillHistory(lists[k]!, r.scale) });
        });
      }
      downloadText(fillsFileName(services.config.network), fillsCsv(out), 'text/csv');
      showToast(t('orders.fills.exported', { orders: out.length }), 'ok');
    });
  // orders placed with an older contract version (C5-02): this build cannot derive their scripts, so it can neither find nor cancel them
  const oldTemplate = rows.filter((r) => r.oldTemplate).length;
  const amendState = amend ? entryState(amend.entry, services.kob) : null;
  const amendK0 = amend && amendState ? amendKind(amendState.state, amend.typeKey) : null;
  // the quick form's "more options" switches a conditional order to the full replace form
  const amendK = amendK0 === 'cond' && replaceMode ? 'replace' : amendK0;
  const amendToken = amend ? tokenOf(amend.token) : undefined;
  // a pair order is replaced in the pair ticket: its quote token B must be known too
  const amendQuote = amend?.pair ? tokenOf(amend.pair.quote) : undefined;
  const replaceReady = !!amendToken && (!amend?.pair || !!amendQuote);

  return (
    <div class="stack" data-testid="orders-view">
      <div class="row-between">
        <h1 style="margin:0">{t('orders.title')}</h1>
        <div class="row">
          {d?.clock ? <span class="small muted">{t('common.updated', { time: formatDateTime(Number(d.clock.unixSeconds), { dateStyle: undefined, timeStyle: 'medium' }) })}</span> : null}
          {loadEvents && filledRows.length > 0 ? (
            <Button small onClick={exportFills} loading={busy === 'export-fills'} data-testid="orders-export-fills">{t('orders.fills.exportAll')}</Button>
          ) : null}
          <Button small onClick={data.reload} loading={data.loading && !!d} data-testid="orders-refresh">{t('common.refresh')}</Button>
        </div>
      </div>

      {wallet.networkMismatch ? <Banner tone="error" data-testid="orders-network-blocked">{t('orders.networkBlocked')}</Banner> : null}
      {d?.indexerError ? <Banner tone="warn" data-testid="orders-indexer-down">{t('orders.indexerDown')}</Banner> : null}
      {d?.straysError && !d.indexerError ? <Banner tone="warn" data-testid="orders-strays-down">{t('orders.straysDown')}</Banner> : null}
      {wallet.records.persistent?.() === false && (d?.records.length ?? 0) > 0 ? (
        // C5 W-14: the browser storage refused the placement records: they live in this tab only, the user must keep a backup
        <Banner
          tone="warn"
          title={t('orders.recordsMemory.title')}
          data-testid="orders-records-memory"
          actions={
            <Button small variant="primary" onClick={() => downloadText(exportFileName('backup', services.config.network), backupFileText(d!.records, services.config.network))} data-testid="orders-records-memory-export">
              {t('orders.recover.exportBackup')}
            </Button>
          }
        >
          {t('orders.recordsMemory.body', { count: d?.records.length ?? 0 })}
        </Banner>
      ) : null}
      {d?.nodeError ?<Banner tone="warn" data-testid="orders-node-down">{t('orders.nodeDown', { message: d.nodeError.message })}</Banner> : null}
      {health.assessment.level === 'bad' && !d?.indexerError ? <Banner tone="warn">{t('orders.stale')}</Banner> : null}
      <ErrorBanner error={data.error} onRetry={data.reload} data-testid="orders-error" />
      {failure ? (
        <Banner tone="error" data-testid="orders-action-error">
          {failure}
          <RawDetails text={failureRaw} />
        </Banner>
      ) : null}
      {planFail ? <PlanErrors plans={planFail.plans} failed={planFail.failed} /> : null}
      {oldTemplate > 0 ? (
        <Banner tone="warn" title={t('orders.oldTemplate.title')} data-testid="orders-old-template">
          {t('orders.oldTemplate.body', { count: oldTemplate })}
        </Banner>
      ) : null}

      {closeFollowUp && !closeFollowUp.pending && closeFollowUp.side === 'sell' ? (
        <Banner
          tone="info"
          title={t(closeFollowUp.amount > 0n ? 'orders.close.titleSell' : 'orders.close.titleSellDone')}
          data-testid="orders-close-followup-sell"
          onDismiss={() => setCloseFollowUp(null)}
          actions={
            closeFollowUp.amount > 0n ? (
              <a class="btn btn-primary btn-sm" href={routeToHash(tokenRoute(closeFollowUp.token, 'cover', closeFollowUp.amount))} data-testid="orders-close-buy">
                {t('orders.close.buy', { amount: coverText(closeFollowUp.token, closeFollowUp.amount) })}
              </a>
            ) : undefined
          }
        >
          {t(closeFollowUp.amount > 0n ? 'orders.close.bodySell' : 'orders.close.bodySellDone', { amount: coverText(closeFollowUp.token, closeFollowUp.amount) })}
        </Banner>
      ) : null}
      {closeFollowUp && !closeFollowUp.pending && closeFollowUp.side === 'buy' ? (
        <Banner
          tone="info"
          title={t('orders.close.title')}
          data-testid="orders-close-followup"
          onDismiss={() => setCloseFollowUp(null)}
          actions={
            <a class="btn btn-primary btn-sm" href={routeToHash(tokenRoute(closeFollowUp.token, 'close'))} data-testid="orders-close-sell">
              {t('orders.close.sell', { ticker: tokenOf(closeFollowUp.token)?.ticker ?? '' })}
            </a>
          }
        >
          {t('orders.close.body')}
        </Banner>
      ) : null}
      {sweepFollowUp && !sweepFollowUp.pending ? (
        <Banner
          tone="info"
          title={t('orders.sweepFirst.doneTitle')}
          data-testid="orders-sweep-followup"
          onDismiss={() => setSweepFollowUp(null)}
          actions={
            <Button
              small
              variant="danger"
              disabled={!canSign || !!busy}
              onClick={() => {
                const c = sweepFollowUp;
                setSweepFollowUp(null);
                c.retry();
              }}
              data-testid="orders-sweep-followup-cancel"
            >
              {t('orders.sweepFirst.cancelNow')}
            </Button>
          }
        >
          {t('orders.sweepFirst.doneBody')}
        </Banner>
      ) : null}
      {flow ? (
        <TxFlow
          key={flow.key}
          heading={flow.heading}
          steps={flow.steps}
          onStepSubmitted={onStepSubmitted}
          onDone={onFlowDone}
          {...(flow.next ? { next: flow.next } : {})}
          {...(flow.note ? { note: flow.note } : {})}
        />
      ) : null}

      <Section
        title={t('orders.list.title')}
        data-testid="orders-section"
        actions={
          <Button
            small
            variant="danger"
            disabled={!canSign || cancellable.length === 0 || !!busy}
            title={canSign ? undefined : t('orders.actions.needWallet')}
            onClick={() => setConfirmAll({ token: null, ids: cancellable })}
            data-testid="orders-cancel-all"
          >
            {t('orders.cancelAll', { count: cancellable.length })}
          </Button>
        }
      >
        <p class="small muted" data-testid="orders-race-note">{t('orders.raceNote')}</p>
        <AutoRefundToggle />
        <Tabs
          aria-label={t('orders.list.title')}
          active={tab}
          onChange={setTab}
          tabs={[
            { id: 'active', label: t('orders.tab.active', { count: counts.active }), 'data-testid': 'orders-tab-active' },
            { id: 'history', label: t('orders.tab.history', { count: counts.history }), 'data-testid': 'orders-tab-history' },
            { id: 'all', label: t('orders.tab.all', { count: counts.all }), 'data-testid': 'orders-tab-all' },
          ]}
        />
        <div role="tabpanel" class="stack-sm" data-testid="orders-list" data-live={stats.live}>
          {data.loading && !d ? <Loading /> : null}
          {d && shown.length === 0 ? <p class="muted center" data-testid="orders-empty">{t(tab === 'history' ? 'orders.emptyHistory' : 'orders.empty')}</p> : null}
          {shown.map((item) =>
            item.type === 'single' ? (
              rowById.get(item.id) ? (
                <OrderRow key={item.id} row={rowById.get(item.id)!} token={tokenOf(rowById.get(item.id)!.token)} canSign={canSign} actions={actions} busy={busy === item.id} {...(loadEvents ? { loadEvents } : {})} />
              ) : null
            ) : (
              <PositionCard
                key={item.id}
                position={item.position}
                rows={rowById}
                tokenOf={tokenOf}
                canSign={canSign}
                actions={actions}
                onCancelPosition={onCancelPosition}
                onClosePosition={onClosePosition}
                busy={busy}
                clock={d?.clock ?? null}
                {...(loadEvents ? { loadEvents } : {})}
              />
            ),
          )}
        </div>
      </Section>

      {d ? (
        <BalancesPanel
          data={d}
          pubkey={wallet.info.pubkey}
          canSign={canSign}
          onCancelToken={(token) => setConfirmAll({ token, ids: cancellableIds(rows, token) })}
          reserved={reserved}
          busy={busy}
          onMergeToken={mergeToken}
        />
      ) : null}
      <StraysPanel
        strays={strays}
        orderTokens={sweepable}
        canSign={canSign}
        busy={busy}
        onSweep={(id) => {
          const row = rowById.get(id);
          if (row) actions.onSweep(row);
        }}
        sweepableOrders={sweepableOrders}
      />
      <RecoverPanel
        onCancel={(id) => {
          const row = rowById.get(id);
          if (row) actions.onCancel(row);
        }}
        records={d?.records ?? []} pubkey={wallet.info.pubkey} store={wallet.records} onChanged={data.reload} />

      {amend && amendState && amendK && amendK !== 'replace' ? (
        <AmendDialog
          row={amend}
          kind={amendK}
          state={amendState.state}
          token={amendToken}
          pubkey={wallet.info.pubkey}
          strays={strays}
          onClose={() => setAmend(null)}
          onPlanned={onPlannedAmend}
          {...(amendK === 'cond' && amendToken ? { onMore: () => setReplaceMode(true) } : {})}
        />
      ) : null}
      {amend && amendState && amendK === 'replace' && amendToken && replaceReady ? (
        <ReplaceDialog
          row={amend}
          state={amendState.state}
          token={amendToken}
          {...(amendQuote ? { quote: amendQuote } : {})}
          pubkey={wallet.info.pubkey}
          strays={strays}
          onClose={() => setAmend(null)}
          onPlanned={onPlannedAmend}
        />
      ) : null}
      {amend && amendState && amendK === 'replace' && !replaceReady ? (
        <Modal title={t('orders.amend.title')} onClose={() => setAmend(null)} data-testid="replace-no-token">
          <p>{t('orders.replace.noToken')}</p>
        </Modal>
      ) : null}

      {sweepFirst ? (
        <Modal
          title={t('orders.sweepFirst.title')}
          onClose={() => setSweepFirst(null)}
          data-testid="sweep-first-dialog"
          footer={
            <>
              <Button
                variant="danger"
                onClick={() => {
                  const c = sweepFirst;
                  setSweepFirst(null);
                  c.proceed();
                }}
                data-testid="sweep-first-abandon"
              >
                {t('orders.sweepFirst.abandon', { count: sweepFirst.count })}
              </Button>
              <Button
                variant="primary"
                data-autofocus
                onClick={() => {
                  const c = sweepFirst;
                  setSweepFirst(null);
                  sweepMany(c.ids, c.ids.length === 1 ? c.ids[0]! : 'sweep-first', c.retry);
                }}
                data-testid="sweep-first-confirm"
              >
                {t('orders.sweepFirst.sweep', { orders: sweepFirst.ids.length })}
              </Button>
            </>
          }
        >
          <p>{t('orders.sweepFirst.body', { count: sweepFirst.count, orders: sweepFirst.ids.length })}</p>
          <p class="small muted">{t('orders.sweepFirst.note')}</p>
        </Modal>
      ) : null}

      {confirmAll ? (
        <Modal
          title={confirmAll.token ? t('orders.cancelAll.tokenTitle') : t('orders.cancelAll.title')}
          onClose={() => setConfirmAll(null)}
          data-testid="cancel-all-dialog"
          footer={
            <>
              <Button onClick={() => setConfirmAll(null)} data-autofocus>{t('common.cancel')}</Button>
              <Button
                variant="danger"
                disabled={confirmAll.ids.length === 0}
                onClick={() => {
                  const c = confirmAll;
                  setConfirmAll(null);
                  cancelMany(c.token ? t('orders.flow.tokenHeading') : t('orders.flow.allHeading'), c.ids, c.token ?? 'all');
                }}
                data-testid="cancel-all-confirm"
              >
                {t('orders.cancelAll.confirm', { count: confirmAll.ids.length })}
              </Button>
            </>
          }
        >
          <p>{t('orders.cancelAll.body', { count: confirmAll.ids.length })}</p>
          <p class="small muted">{t('orders.cancelAll.txNote')}</p>
          <p class="small muted">{t('orders.raceNote')}</p>
        </Modal>
      ) : null}
    </div>
  );
}
