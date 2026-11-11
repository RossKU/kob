import { formatUnits, quoteOf } from '../../kob/units';
import type { ComponentChildren } from 'preact';
import { useEffect, useMemo, useRef, useState } from 'preact/hooks';
import { useServices, useWallet } from '../../app/context';
import { displayToken } from '../../app/token-display';
import { kasValueText, kasValueTitle } from '../../app/fiat';
import { FiatNote, useFiat } from '../../app/use-fiat';
import { navigate, routeToHash, tokenRoute, type PortfolioTab } from '../../app/router';

/** The market a market page's panel stands on: a token's KAS market (`quote` null) or a token/token pair. */
export interface MarketOf {
  base: Hex;
  quote: Hex | null;
}
import { t } from '../../i18n';
import type { CancelPlan, OrderSnapshot } from '../../kob/cancel';
import type { Position } from '../../kob/positions';
import { longId } from '../../kob/registry';
import type { Hex } from '../../kob/types';
import {
  Banner, Button, EmptyState, ErrorBanner, Modal, RawDetails, Section, Segmented, Tabs, columnFraction, formatRowTimeUnix, rowTimeTitleUnix, showToast, sortBy, sortKeyOf, toError, useAsync, useSortState,
} from '../kit';
import { DataTable, KAS_FRACTION_MAX, MoneyCell, type DtColumn } from '../kit/DataTable';
import { ConnectBar } from '../shell/WalletButton';
import { useLiveRefresh } from '../market/live';
import { RowsControl } from '../market/ListPaging';
import { TipBadge } from '../market/TipBadge';
import { useListPage } from '../market/use-market-list';
import { useKascov } from '../market/use-kascov';
import { kascovIconOf } from '../market/kascov-merge';
import { useSystemStatus } from '../shell/StatusProvider';
import { AmendDialog } from './AmendDialog';
import { ReplaceDialog } from './ReplaceDialog';
import { AutoRefundToggle } from './AutoRefund';
import { BalancesFrame, BalancesPanel } from './BalancesPanel';
import { OrderRow, type OrderRowActions } from './OrderRow';
import { PositionCard } from './PositionCard';
import { RecoverPanel } from './RecoverPanel';
import { StraysPanel } from './StraysPanel';
import { FILL_SORT_KEYS, WalletFills, type FillSortKey } from './WalletFills';
import { PORTFOLIO_PAGE_SIZES, PortfolioPager, pageOf, usePortfolioRows } from './portfolio-paging';
import { useWalletBalances } from './use-balances';
import { PlanErrors, TxFlow, type FlowOutcome, type FlowStep, type TxFlowProps } from './TxFlow';
import { mergeFailure, mergeStep, mergeSummary, mergeTokenFor, reservedOutpoints } from './merge-model';
import { tickerOrId } from '../market/token-model';
import { unwindAmount } from './position-model';
import { fillHistory, fillsCsv, fillsFileName } from './fills-model';
import { filledAmountOf } from './OrderFills';
import { priceDpOf } from './cells';
import { countText, tCount } from './plural';
import { downloadText, exportFileName } from '../kit/download';
import { backupFileText } from './recover';
import {
  abandoningOrders, abandonsStrays, describeActionError, liveExitsOfRepeatEntry, markCancelling, rawActionError, planCancelFor, planCancelMany, planMergeFor,
  planRefundFor, planSweepFor, planSweepMany, warnEntryOnly,
} from './actions';
import { amendKind, describeEntries, entryState, countByStatus, type OrderRowModel } from './orders-model';
import { buildListItems, cancellableIds, filterItems, itemMemberIds, tabCounts } from './list-model';
import { ORDER_SORT_KEYS, orderColumns, orderSortValue, type OrderSortKey } from './orders-sort';
import { loadOrderEvents, loadOrders, type OrdersData } from './orders-data';
import '../market/market.css';

interface Flow {
  key: number;
  heading: string;
  steps: FlowStep[];
  /** a chained flow (token merge): builds step `index` once the previous one was submitted */
  next?: TxFlowProps['next'];
}

const REFRESH_MS = 15_000;

/** Panel width (px, the panel's own, not the screen's) below which the tabs read Open / History. */
const COMPACT_TABS_BELOW = 560;

/**
 * A market page's panel keeps its tab and its "this market only" choice for the visit, per market: the panel can be drawn anew (the page behind it re-reads its
 * market) and must not fall back to Open orders under the viewer's finger.
 */
const embedMemory = new Map<string, { tab: PortfolioTab; onlyHere: boolean }>();

/**
 * The frame of a list under the tabs: on the Portfolio page a titled card (its actions at the right of the title); in a market page's panel no card and no title (the tab already names
 * the list, review M-07): the list stands directly under the tabs and the actions are in the panel's head line.
 */
function ListFrame(props: { embed: boolean; title: string; actions?: ComponentChildren; 'data-testid': string; children: ComponentChildren }) {
  if (props.embed) return <div class="mo-body" data-testid={props['data-testid']}>{props.children}</div>;
  return <Section title={props.title} data-testid={props['data-testid']} {...(props.actions ? { actions: props.actions } : {})}>{props.children}</Section>;
}

/** The placeholder rows until the list is in. */
const SKELETON_ROWS = 3;

/**
 * `#/portfolio`, the Portfolio page: the wallet's balances, orders (indexer + placement records resolved on the node), positions with their exits and
 * fills, one tab each (`#/portfolio/<tab>`, Balances first); strays, automatic refunds and recovery in one section under them, folded. Cancel / amend /
 * refund / cancel-all as before. Nothing here signs: every transaction goes through <TxFlow> (ui/confirm/send.ts).
 */
export function OrdersView(props: { tab?: PortfolioTab; recover?: boolean; embed?: MarketOf } = {}) {
  // on a market page (`embed`): the same tabs, Open orders first, its tab kept here (not in the address), this market only by default
  const embed = props.embed ?? null;
  const memoryKey = embed ? `${embed.base}/${embed.quote ?? 'KAS'}` : '';
  const [embedTab, setEmbedTabState] = useState<PortfolioTab>(() => embedMemory.get(memoryKey)?.tab ?? 'open');
  const [onlyHere, setOnlyHereState] = useState(() => embedMemory.get(memoryKey)?.onlyHere ?? true);
  const remember = (tab: PortfolioTab, only: boolean) => embedMemory.set(memoryKey, { tab, onlyHere: only });
  const setEmbedTab = (next: PortfolioTab) => {
    remember(next, onlyHere);
    setEmbedTabState(next);
  };
  const setOnlyHere = (next: boolean) => {
    remember(embedTab, next);
    setOnlyHereState(next);
  };
  const services = useServices();
  const wallet = useWallet();
  const { indexer: health } = useSystemStatus();
  const pubkey = wallet.info?.pubkey;
  const canSign = !!wallet.info && !wallet.networkMismatch;

  // the list shows once the indexer answered; the node's verdicts on the wallet's records follow in the background (orders-data.ts), and
  // replace it for as long as it is the list they were looked up for (a refresh starts a new one)
  const [late, setLate] = useState<{ base: OrdersData; next: OrdersData } | null>(null);
  const data = useAsync(
    async (signal) => {
      if (!pubkey) return null;
      let base: OrdersData | null = null;
      const pendingNext: OrdersData[] = [];
      base = await loadOrders(services, pubkey, wallet.records, {
        signal,
        onUpdate: (next) => (base ? setLate({ base, next }) : pendingNext.push(next)),
      });
      const b = base;
      if (pendingNext.length) setTimeout(() => setLate({ base: b, next: pendingNext.at(-1)! }), 0);
      return base;
    },
    [pubkey, wallet.records, services],
  );
  useLiveRefresh(services, ['fills'], () => true, data.refresh, REFRESH_MS);

  const tab: PortfolioTab = embed ? embedTab : (props.tab ?? 'balances');
  const setTab = (next: PortfolioTab) => (embed ? setEmbedTab(next) : navigate({ name: 'portfolio', tab: next }));
  // the order list of the Open orders and Order history tabs
  const listTab = tab === 'history' ? 'history' : 'active';
  // rows per page and the page turned to (the lists are read whole: a page is a slice); another tab, size or market filter starts at the first page
  const [rowsPer, setRows] = usePortfolioRows();
  // the viewer's sort of each list (a column and a direction; none: newest first), kept in this browser; applied to the whole list, before it is paged
  const [openSort, setOpenSort] = useSortState<OrderSortKey>('orders.open', ORDER_SORT_KEYS);
  const [historySort, setHistorySort] = useSortState<OrderSortKey>('orders.history', ORDER_SORT_KEYS);
  const [fillsSort, setFillsSort] = useSortState<FillSortKey>('fills', FILL_SORT_KEYS);
  const orderSort = tab === 'history' ? historySort : openSort;
  const setOrderSort = tab === 'history' ? setHistorySort : setOpenSort;
  // the width this panel has (a page's, or a market page's narrower one): its layouts follow it, not the screen's (orders/balances.css, order-line.css: container queries)
  const root = useRef<HTMLDivElement>(null);
  const [panelW, setPanelW] = useState(0);
  useEffect(() => {
    const el = root.current;
    if (!el || typeof ResizeObserver !== 'function') return;
    // the content width: the room the panel's own contents have
    const measure = () => {
      const cs = getComputedStyle(el);
      setPanelW(Math.round(el.clientWidth - (Number.parseFloat(cs.paddingLeft) || 0) - (Number.parseFloat(cs.paddingRight) || 0)));
    };
    const ro = new ResizeObserver(measure);
    ro.observe(el);
    measure();
    return () => ro.disconnect();
  }, [!!wallet.info]);
  // the four tabs stand on one line: the short words where the full ones do not fit, the full ones as their accessible names
  const phone = panelW > 0 && panelW < COMPACT_TABS_BELOW;
  const [pageNo, setPageNo] = useListPage(`${tab}|${rowsPer}|${embed && onlyHere ? 'here' : 'all'}|${sortKeyOf(tab === 'fills' ? fillsSort : orderSort)}`);
  // Strays, automatic refunds and recovery: one folded section; it opens by itself while strays are waiting
  const [settingsOpen, setSettingsOpen] = useState<boolean | null>(props.recover ? true : null);
  useEffect(() => {
    if (props.recover) document.getElementById('portfolio-settings')?.scrollIntoView({ block: 'start' });
  }, [props.recover, !!data.data]);
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

  const d = late && late.base === data.data ? late.next : (data.data ?? null);
  // the wallet's balances, read once for the page: the summary strip over the tabs and the Balances tab show the same figures
  const balances = useWalletBalances(d, pubkey, data.data ?? null);
  // the USD display (Settings): the summary's KAS values in dollars; null = KAS
  const fiat = useFiat();
  // the icons' pictures: kascov.io's checked copy, as on the market list (none: a letter on the token's colour)
  const kascov = useKascov();
  const kascovTokens = kascov.status === 'ok' ? kascov.tokens : null;
  const iconOf = (id: string) => kascovIconOf(kascovTokens, id, services.registry.byCovenantId.get(id)?.templateHash ?? null);
  // the maker-wide stray list counts every live stray per order (the list views of the indexer carry no strays)
  const rows = useMemo(() => {
    if (!d) return [];
    const strayCounts = new Map<string, number>();
    for (const s of d.strays) if (!s.spent && !s.lost) strayCounts.set(s.owner, (strayCounts.get(s.owner) ?? 0) + 1);
    return describeEntries(d.entries, { kob: services.kob, clock: d.clock }).map((r) => ((strayCounts.get(r.id) ?? 0) > r.strayCount ? { ...r, strayCount: strayCounts.get(r.id)! } : r));
  }, [d, services.kob]);
  const rowById = useMemo(() => new Map(rows.map((r) => [r.id, r])), [rows]);
  const entryById = useMemo(() => new Map((d?.entries ?? []).map((e) => [e.id, e])), [d]);
  // an order of the market this panel stands on (a market page with "this market only"): every order elsewhere
  const inMarket = (r: OrderRowModel | undefined): boolean =>
    !!r && (!embed || !onlyHere || (r.token === embed.base && (embed.quote ? r.pair?.quote === embed.quote : !r.pair)));
  const allItems = useMemo(() => (d ? buildListItems(d.entries, d.positions) : []), [d]);
  const items = useMemo(
    () => (embed && onlyHere ? allItems.filter((i) => itemMemberIds(i).some((id) => inMarket(rowById.get(id)))) : allItems),
    [allItems, rowById, embed?.base, embed?.quote, onlyHere],
  );
  const counts = useMemo(() => tabCounts(items, rowById), [items, rowById]);
  const filteredItems = useMemo(() => filterItems(items, rowById, listTab), [items, rowById, listTab]);
  const stats = useMemo(() => countByStatus(rows), [rows]);
  // the tokens of each order (its own and a pair order's B): a stray of any other token is foreign
  const sweepable = useMemo(() => new Map(rows.filter((r) => r.token).map((r) => [r.id, [r.token!, ...(r.pair ? [r.pair.quote] : [])]])), [rows]);
  // live orders of the wallet whose strays a sweep can return (the order lives on)
  const sweepableOrders = useMemo(() => new Set(rows.filter((r) => r.live && r.canCancel).map((r) => r.id)), [rows]);

  // the registry's token: what an action (merge, amend) plans with; `shownToken` is what a row shows (app/token-display.ts: a token the registry
  // does not list is shown on this browser's issuance of it, else on 8 assumed decimals, marked)
  const tokenOf = (id: string | null) => (id ? services.registry.byCovenantId.get(id) : undefined);
  const shownToken = (id: string | null, scale?: bigint | null) => displayToken(services.registry, services.config.network, id, undefined, scale);
  // the market of an order as its line names it (the market column's sort reads the same text)
  const marketText = (r: OrderRowModel): string => {
    const tk = shownToken(r.token, r.scale);
    const b = r.pair ? shownToken(r.pair.quote, r.pair.quoteScale) : undefined;
    return tk && r.pair ? `${tk.ticker}/${b?.ticker ?? longId(r.pair.quote)}` : tk ? `${tk.ticker}/KAS` : r.token ? longId(r.token) : '—';
  };
  // the average fill price of an order (Order history): read from its events, once, for the orders of the page shown (`useAvgPrices` below)
  const avgPrices = useRef(new Map<string, bigint | null>());
  // the lists sorted whole, then paged: page 1 is the top of the sorted list
  const shownAll = sortBy(filteredItems, orderSort, (item, key) => {
    const row = rowById.get(itemMemberIds(item)[0]!);
    return row ? orderSortValue(row, key, { clock: d?.clock ?? null, marketText, decimalsOf: (r) => shownToken(r.token, r.scale)?.decimals, avgOf: (r) => avgPrices.current.get(r.id) ?? null }) : null;
  });
  const { rows: shown, window: shownWindow } = pageOf(shownAll, rowsPer, pageNo);
  const [, setAvgVersion] = useState(0);
  const wantAvg = tab === 'history' ? shown.flatMap((i) => itemMemberIds(i)).map((id) => rowById.get(id)).filter((r): r is OrderRowModel => !!r && filledAmountOf(r) > 0n && !avgPrices.current.has(r.id)) : [];
  const wantAvgKey = wantAvg.map((r) => r.id).join(',');
  useEffect(() => {
    if (!wantAvg.length || !services.indexer || d?.indexerError) return;
    const ctl = new AbortController();
    void (async () => {
      for (let i = 0; i < wantAvg.length; i += 4) {
        await Promise.all(
          wantAvg.slice(i, i + 4).map(async (r) => {
            try {
              avgPrices.current.set(r.id, fillHistory(await loadOrderEvents(services.indexer!, r.id, ctl.signal), r.scale).avgPrice);
            } catch {
              if (!ctl.signal.aborted) avgPrices.current.set(r.id, null);
            }
          }),
        );
        if (!ctl.signal.aborted) setAvgVersion((n) => n + 1);
      }
    })();
    return () => ctl.abort();
  }, [wantAvgKey]);
  // an amount of a token (base units) as its token amount with the ticker
  const coverText = (token: Hex, amount: bigint): string => {
    const tk = shownToken(token)!;
    return `${formatUnits(amount, tk.decimals, { group: ',' })} ${tk.ticker}`;
  };
  const strays = d?.strays ?? [];
  // a failed stray fetch is passed on as "unknown" (null): a record-path cancel then warns instead of silently abandoning strays
  const strayList = d?.straysError ? null : strays;

  const startFlow = (heading: string, steps: FlowStep[], chain: Pick<Flow, 'next'> = {}) => {
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
          good.map((p, i) => stepOf(good.length > 1 ? t('orders.flow.cancelStep', { n: i + 1, total: good.length, orders: countText('order', p.cancelIds.length) }) : t('orders.flow.cancelTitle'), p)),
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

  /**
   * Merges the wallet's smallest plain UTXOs of `token` (up to the program's token inputs, 3) into one: ONE transfer to itself per click, its
   * confirmation stating the KAS it frees and how many mergeable UTXOs remain; the user clicks again to merge more.
   */
  const mergeToken = (token: Hex) =>
    void guarded(`merge-${token}`, async () => {
      // the registry's token, else (any held token the node verified a program for) the one built from that program and the wallet's UTXOs (merge-model.ts)
      const free = balances.free.data;
      const mt = pubkey && free ? mergeTokenFor(token, free.freeTokens, services.registry.byCovenantId, pubkey, reserved, { kob: services.kob, programs: free.programOf ?? free.programs }) : null;
      if (!pubkey || !mt) return;
      const ticker = tokenOf(token)?.ticker ?? tickerOrId(shownToken(token)?.ticker ?? '', token);
      const plan = await planMergeFor(services, pubkey, mt, reserved);
      if (!plan.ok) return setPlanFail({ plans: [mergeFailure(plan)], failed: [] });
      startFlow(t('orders.merge.heading', { ticker }), [mergeStep(plan.links[0]!, mergeSummary(plan, ticker))]);
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
    // (a sweep continues the order: markCancelling leaves its record unmarked, the send moved its last state to the continuation)
    if (pubkey) await markCancelling(wallet.records, { cancelIds: step.spends, built: step.plan.built }, result.txid);
    showToast(step.toast ?? (step.plan.sweep ? tCount('orders.flow.sweepSubmitted', step.plan.sweep.utxos) : tCount('orders.flow.submitted', step.spends.length)), 'ok');
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
    if (embed) return null;
    return (
      <div class="stack" data-testid="orders-view">
        <h1>{t('orders.title')}</h1>
        {/* the one disconnected state of the app: the sentence and the Connect of the header (review M-12) */}
        <EmptyState icon="wallet" action={<ConnectBar gate />} data-testid="orders-connect">{t('orders.connect')}</EmptyState>
      </div>
    );
  }

  const marketRows = embed && onlyHere ? rows.filter((r) => inMarket(r)) : rows;
  const cancellable = cancellableIds(marketRows, null);
  // the number formats of the two order tables (review H-05, M-03): ONE number of decimals for a price column, the finest any order of the whole list needs (so a page turned keeps it), and for the
  // Total column (KAS, at most 4); the exact figures are in the cells' titles
  const tableRows = shownAll.flatMap((item) => itemMemberIds(item)).map((id) => rowById.get(id)).filter((r): r is OrderRowModel => !!r);
  const priceDp = priceDpOf(tableRows.filter((r) => !r.pair).flatMap((r) => {
    const decimals = shownToken(r.token, r.scale)?.decimals ?? 0;
    return [r.price, avgPrices.current.get(r.id)].map((price) => ({ price, decimals, scale: r.scale }));
  }));
  const kasDp = Math.min(
    KAS_FRACTION_MAX,
    columnFraction(tableRows.map((r) => (!r.pair && r.amountTotal !== null && r.price !== null ? quoteOf(r.amountTotal, r.price, r.scale, 'down') : null)), 8),
  );
  // Cancel all: only where there is something to cancel; the cancel race is its tooltip (and the confirmation's text)
  const cancelAllButton =
    tab === 'open' && cancellable.length > 0 ? (
      <Button
        small
        variant="danger"
        disabled={!canSign || !!busy}
        title={canSign ? t('orders.raceNote') : t('orders.actions.needWallet')}
        onClick={() => setConfirmAll({ token: null, ids: cancellable })}
        data-testid="orders-cancel-all"
      >
        {t('orders.cancelAll', { count: cancellable.length })}
      </Button>
    ) : undefined;
  const cols = orderColumns(tab === 'history' ? 'history' : 'open', !!fiat);
  const ownCol = cols.find((c) => c.key === 'time') as DtColumn<OrderSortKey>; // the lists are newest first: the time, largest first
  // fill history (R-8): per order on demand, and every fill of the wallet as one CSV
  const loadEvents = services.indexer && !d?.indexerError ? (id: Hex, signal: AbortSignal) => loadOrderEvents(services.indexer!, id, signal) : null;
  const filledRows = marketRows.filter((r) => filledAmountOf(r) > 0n);
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
          const tk = shownToken(r.token);
          out.push({ info: { ticker: tk?.ticker ?? r.token ?? '', side: r.side, type: r.typeKey, ...(tk ? { decimals: tk.decimals } : {}) }, history: fillHistory(lists[k]!, r.scale) });
        });
      }
      downloadText(fillsFileName(services.config.network), fillsCsv(out), 'text/csv');
      showToast(t('orders.fills.exported', { orders: out.length }), 'ok');
    });
  const exportButton = loadEvents && filledRows.length > 0 ? <Button small onClick={exportFills} loading={busy === 'export-fills'} data-testid="orders-export-fills">{t('orders.fills.exportAll')}</Button> : undefined;
  // strays the wallet can still move: the folded section opens by itself while there are any (its notice says so on top)
  const liveStrays = strays.filter((x) => !x.spent && !x.lost).length;
  const settingsShown = settingsOpen ?? liveStrays > 0;
  const showSettings = () => {
    setSettingsOpen(true);
    setTimeout(() => document.getElementById('portfolio-settings')?.scrollIntoView({ behavior: 'smooth', block: 'start' }), 0);
  };
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
    <div class={embed ? 'stack mkt-card market-orders pf-root' : 'stack pf-root'} ref={root} data-testid={embed ? 'market-orders' : 'orders-view'} {...(embed ? { role: 'region', 'aria-label': t('orders.market.title') } : {})}>
      {/* a market page's panel has no heading row (the tabs name it; its name is a heading for screen readers); the actions of that row (Cancel all or Export, All in Portfolio) are at the right end of the tabs row */}
      {embed ? <h2 class="sr-only">{t('orders.market.title')}</h2> : null}
      <div class="row-between" style={embed ? 'display:none' : undefined}>
        <h1 style="margin:0">{t('orders.title')}</h1>
        <div class="row">
          {d?.clock ? <span class="small muted num" data-testid="orders-updated" title={rowTimeTitleUnix(d.clock.unixSeconds)}>{t('common.updated', { time: formatRowTimeUnix(d.clock.unixSeconds) })}</span> : null}
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
          {tCount('orders.oldTemplate.body', oldTemplate)}
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
                <span class="btn-label">{t('orders.close.buy', { amount: coverText(closeFollowUp.token, closeFollowUp.amount) })}</span>
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
          title={t(closeFollowUp.amount > 0n ? 'orders.close.title' : 'orders.close.titleSellDone')}
          data-testid="orders-close-followup"
          onDismiss={() => setCloseFollowUp(null)}
          actions={
            // the ticket opens on a market sell of what the position bought and did not sell yet (never the whole free balance);
            // nothing bought yet: nothing to sell
            closeFollowUp.amount > 0n ? (
              <a class="btn btn-primary btn-sm" href={routeToHash(tokenRoute(closeFollowUp.token, 'sell', closeFollowUp.amount))} data-testid="orders-close-sell">
                <span class="btn-label">{t('orders.close.sell', { amount: coverText(closeFollowUp.token, closeFollowUp.amount) })}</span>
              </a>
            ) : undefined
          }
        >
          {t(closeFollowUp.amount > 0n ? 'orders.close.body' : 'orders.close.bodyDone', { amount: coverText(closeFollowUp.token, closeFollowUp.amount) })}
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
          onClose={() => setFlow(null)}
          {...(flow.next ? { next: flow.next } : {})}
        />
      ) : null}

      {liveStrays > 0 && !embed ? (
        <Banner
          tone="info"
          data-testid="orders-strays-notice"
          actions={
            <Button small onClick={() => showSettings()} data-testid="orders-strays-notice-open">
              {t('orders.straysNotice.open')}
            </Button>
          }
        >
          {t('orders.straysNotice.body', { count: liveStrays })}
        </Banner>
      ) : null}

      {embed ? null : (
      <div class="pf-strip" data-testid="portfolio-summary" data-currency={fiat ? 'usd' : 'kas'}>
        <div class="pf-stat">
          <div class="pf-kline">
            <span class="pf-k">{t('orders.summary.total')}</span>
            {balances.summary ? (
              <TipBadge
                variant="tag"
                label={t('orders.summary.estimate')}
                detail={balances.summary.unpriced > 0 ? `${t(fiat ? 'orders.summary.estimateTipUsd' : 'orders.summary.estimateTip')} ${tCount('orders.summary.unpriced', balances.summary.unpriced, { n: balances.summary.unpriced })}` : t(fiat ? 'orders.summary.estimateTipUsd' : 'orders.summary.estimateTip')}
                data-testid="portfolio-estimate"
                data-detail-testid="portfolio-estimate-detail"
              />
            ) : null}
          </div>
          <span class="pf-v num" data-testid="portfolio-total" title={balances.summary ? kasValueTitle(balances.summary.total, fiat) : undefined}><MoneyCell kas={balances.summary ? balances.summary.total : null} fraction={2} class="pf-money" /></span>
        </div>
        <div class="pf-stat">
          <div class="pf-kline"><span class="pf-k">{t('orders.summary.available')}</span></div>
          <span class="pf-v num" data-testid="portfolio-available" title={balances.summary ? kasValueTitle(balances.summary.kasFree, fiat) : undefined}><MoneyCell kas={balances.summary ? balances.summary.kasFree : null} fraction={2} class="pf-money" /></span>
        </div>
        <div class="pf-stat">
          <div class="pf-kline"><span class="pf-k">{t('orders.summary.inOrders')}</span></div>
          <span class="pf-v num" data-testid="portfolio-in-orders" title={balances.summary ? kasValueTitle(balances.summary.kasLocked, fiat) : undefined}><MoneyCell kas={balances.summary ? balances.summary.kasLocked : null} fraction={2} class="pf-money" /></span>
        </div>
        <div class="pf-stat">
          <div class="pf-kline"><span class="pf-k">{t('orders.summary.open')}</span></div>
          <span class="pf-v num" data-testid="portfolio-open-count"><span>{d ? String(counts.active) : '—'}</span></span>
        </div>
      </div>
      )}
      {embed ? null : <FiatNote class="pf-fiat-note" />}

      <div class="portfolio" data-testid="portfolio">
        <div class="pf-tabbar">
        <Tabs
          aria-label={t('orders.title')}
          active={tab}
          onChange={setTab}
          tabs={[
            { id: 'balances', label: t('orders.tab.balances'), 'data-testid': 'orders-tab-balances' },
            { id: 'open', label: t(phone ? 'orders.tab.openShort' : 'orders.tab.open', { count: counts.active }), 'data-testid': 'orders-tab-active', ...(phone ? { 'aria-label': t('orders.tab.open', { count: counts.active }) } : {}) },
            { id: 'history', label: t(phone ? 'orders.tab.historyShort' : 'orders.tab.history', { count: counts.history }), 'data-testid': 'orders-tab-history', ...(phone ? { 'aria-label': t('orders.tab.history', { count: counts.history }) } : {}) },
            { id: 'fills', label: t(phone ? 'orders.tab.fillsShort' : 'orders.tab.fills', { count: filledRows.length }), 'data-testid': 'orders-tab-fills', ...(phone ? { 'aria-label': t('orders.tab.fills', { count: filledRows.length }) } : {}) },
          ]}
        />
        {/* a market page's panel: whose orders, in the tab row (review M-07); the choice is kept for the visit */}
        {embed ? (
          <div class="pf-scope">
            <Segmented
              aria-label={t('orders.market.scope')}
              value={onlyHere ? 'here' : 'all'}
              onChange={(v) => setOnlyHere(v === 'here')}
              options={[
                { value: 'here', label: t('orders.market.scopeHere'), 'data-testid': 'market-orders-scope-here' },
                { value: 'all', label: t('orders.market.scopeAll'), 'data-testid': 'market-orders-scope-all' },
              ]}
              data-testid="market-orders-only"
            />
          </div>
        ) : null}
        {/* the rows per page of the lists: right of the tabs (under them on a phone); inert and unseen on Balances, which keeps the row's height */}
        <div class={`pf-rows${tab === 'balances' ? ' pf-rows-off' : ''}`} aria-hidden={tab === 'balances' ? 'true' : undefined} {...(tab === 'balances' ? { inert: true } : {})} data-testid="portfolio-rows-box">
          <RowsControl id="portfolio" size={rowsPer} onSize={(n) => setRows(n)} sizes={PORTFOLIO_PAGE_SIZES} />
        </div>
        {embed ? (
          <div class="pf-embed-actions">
            {tab === 'fills' ? exportButton : cancelAllButton}
            <a class="small mo-all-link" href={routeToHash({ name: 'portfolio', tab })} data-testid="market-orders-portfolio">{t('orders.market.toPortfolio')}</a>
          </div>
        ) : null}
        </div>
        <div role="tabpanel" aria-labelledby={`tab-${tab}`} class="portfolio-panel stack" data-testid={`portfolio-panel-${tab}`}>
          {tab === 'balances' ? (
            // tokens inside the wallet's orders and KAS locked in them, next to the free amounts
            d ? (
              <BalancesPanel
                data={d}
                version={data.data ?? null}
                pubkey={wallet.info.pubkey}
                canSign={canSign}
                onCancelToken={(token) => setConfirmAll({ token, ids: cancellableIds(rows, token) })}
                reserved={reserved}
                busy={busy}
                onMergeToken={mergeToken}
                balances={balances}
                iconOf={iconOf}
                {...(embed ? { compact: true } : {})}
                {...(embed && onlyHere ? { onlyTokens: new Set([embed.base, ...(embed.quote ? [embed.quote] : [])]) } : {})}
              />
            ) : data.loading ? (
              <BalancesFrame />
            ) : null
          ) : null}

          {tab === 'open' || tab === 'history' ? (
            <ListFrame embed={!!embed} title={t(tab === 'history' ? 'orders.list.titleHistory' : 'orders.list.title')} data-testid="orders-section" actions={cancelAllButton}>
              {/* the rows come together, as soon as the indexer's answer is in: a trader acts on them, so they wait for nothing else (the balances, the node's answers) */}
              {/* one table per tab: opening Open orders or Order history draws its rows in (.ml-in), as Balances and Trade history do; a refresh or a page turned keeps the table and replays nothing */}
              <DataTable
                key={tab}
                id="orders"
                grid={tab === 'history' ? 'orders-history' : 'orders-open'}
                label={t(tab === 'history' ? 'orders.list.titleHistory' : 'orders.list.title')}
                cols={cols}
                sort={orderSort}
                own={ownCol}
                onSort={setOrderSort}
                sortable={!!d && shownAll.length > 1}
                headless={!(d && shown.length > 0)}
                body={{ class: d ? 'ml-in' : undefined, 'data-testid': 'orders-list', 'data-live': stats.live }}
              >
                {data.loading && !d ? (
                  <div class="stack-sm" role="status" aria-label={t('common.loading')} data-testid="orders-loading">
                    {Array.from({ length: SKELETON_ROWS }, (_, i) => <div key={i} class="skeleton orders-skel" />)}
                  </div>
                ) : null}
                {d && shownAll.length === 0 ? <EmptyState icon="list" compact={!!embed} data-testid="orders-empty">{t(tab === 'history' ? 'orders.emptyHistory' : 'orders.empty')}</EmptyState> : null}
                {shown.map((item) =>
                  item.type === 'single' ? (
                    rowById.get(item.id) ? (
                      <OrderRow key={item.id} columns={tab === 'history' ? 'history' : 'open'} avg={avgPrices.current.get(item.id)} priceDp={priceDp} kasDp={kasDp} row={rowById.get(item.id)!} token={shownToken(rowById.get(item.id)!.token, rowById.get(item.id)!.scale)} canSign={canSign} actions={actions} busy={busy === item.id} clock={d?.clock ?? null} iconOf={iconOf} {...(embed ? { compact: true } : {})} {...(loadEvents ? { loadEvents } : {})} />
                    ) : null
                  ) : (
                    <PositionCard
                      key={item.id}
                      columns={tab === 'history' ? 'history' : 'open'}
                      priceDp={priceDp}
                      kasDp={kasDp}
                      position={item.position}
                      rows={rowById}
                      tokenOf={shownToken}
                      canSign={canSign}
                      actions={actions}
                      onCancelPosition={onCancelPosition}
                      onClosePosition={onClosePosition}
                      busy={busy}
                      clock={d?.clock ?? null}
                      iconOf={iconOf}
                      {...(embed ? { compact: true } : {})}
                      {...(loadEvents ? { loadEvents } : {})}
                    />
                  ),
                )}
              </DataTable>
              {d ? <PortfolioPager id="portfolio" window={shownWindow} total={shownAll.length} onPage={setPageNo} /> : null}
            </ListFrame>
          ) : null}

          {tab === 'fills' ? (
            <ListFrame embed={!!embed} title={t('orders.fills.title')} data-testid="orders-fills-section" actions={exportButton}>
              <WalletFills rows={filledRows} tokenOf={shownToken} loadEvents={loadEvents} iconOf={iconOf} sort={fillsSort} onSort={setFillsSort} size={rowsPer} page={pageNo} onPage={setPageNo} />
            </ListFrame>
          ) : null}
        </div>
      </div>

      {embed ? null : (
      <details
        class="section portfolio-settings"
        id="portfolio-settings"
        open={settingsShown}
        onToggle={(e) => setSettingsOpen((e.currentTarget as HTMLDetailsElement).open)}
        data-testid="orders-settings"
      >
        <summary data-testid="orders-settings-toggle">{t('orders.settings.title')}</summary>
        <div class="stack" style="margin-top:12px">
          <AutoRefundToggle />
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
        </div>
      </details>
      )}

      {amend && amendState && amendK && amendK !== 'replace' ? (
        <AmendDialog
          row={amend}
          kind={amendK}
          state={amendState.state}
          token={amendToken ?? shownToken(amend.token, amend.scale)}
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
          <p>{t('orders.sweepFirst.body', { strays: countText('utxo', sweepFirst.count), orders: countText('order', sweepFirst.ids.length) })}</p>
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
                {tCount('orders.cancelAll.confirm', confirmAll.ids.length)}
              </Button>
            </>
          }
        >
          <p>{tCount('orders.cancelAll.body', confirmAll.ids.length)}</p>
          <p class="small muted">{t('orders.cancelAll.txNote')}</p>
          <p class="small muted">{t('orders.raceNote')}</p>
        </Modal>
      ) : null}
    </div>
  );
}
