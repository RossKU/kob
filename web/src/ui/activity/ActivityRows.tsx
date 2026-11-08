// The activity log's rows, built to stay still while the list is live: each row is a keyed, memoized component whose cells render again only when
// what they show changed (ActivityStore keeps an unchanged row's object); rows sit at fixed heights and move by a transform when a row arrives above
// them (no layout shift); the confirmation cell follows the chain on its own, from one shared one-second clock, and renders only when its value changes.
import { navigate, pairRoute, tokenRoute } from '../../app/router';
import { memo } from 'preact/compat';
import { useEffect, useState } from 'preact/hooks';
import { formatDateTime, t } from '../../i18n';
import type { Hex } from '../../kob/types';
import { pow10 } from '../../kob/units';
import { compactZeros, fitAmountColumn, unitsFraction, zerosTitle } from '../kit';
import { priceDecimals, priceText } from '../market/market-model';
import { TokenIcon } from '../market/TokenIcon';
import { SETTLE_DAA, confirmationsAt, daaNow, marketKeyOf, rowTime, type ActivityRow, type MarketRef } from './activity-model';
import { KindIcon, kindLabel } from './ActivityIcons';

/** How tokens and markets are named (one object per registry and network, so the rows' props stay equal between renders). */
export interface RowFormat {
  nameOf(token: Hex): string;
  decimalsOf(token: Hex): number;
  marketText(m: MarketRef): string;
  explorerHost: string;
}

// ------------------------------------------------------------------------------------------------ the shared clock

const tickers = new Set<() => void>();
let timer: ReturnType<typeof setInterval> | null = null;

/** Calls `fn` every second while subscribed; one interval for every cell of every log. */
function onTick(fn: () => void): () => void {
  tickers.add(fn);
  timer ??= setInterval(() => tickers.forEach((f) => f()), 1000);
  return () => {
    tickers.delete(fn);
    if (!tickers.size && timer) {
      clearInterval(timer);
      timer = null;
    }
  };
}

/** The confirmation cell: the count and its pie up to SETTLE_DAA, then ✓. Re-renders itself (not its row) when the count it shows changes. */
const ConfCell = memo(function ConfCell(props: { daa: number; confirmations: number | null; settled: boolean; chainDaa: number | null; chainAt: number | null }) {
  const at = () => confirmationsAt(props, daaNow(props.chainDaa !== null && props.chainAt !== null ? { daa: props.chainDaa, atMs: props.chainAt } : null, Date.now()));
  const [conf, setConf] = useState(at);
  useEffect(() => {
    const update = () => {
      const next = at();
      setConf((c) => (c.n === next.n && c.settled === next.settled ? c : next));
    };
    update();
    if (props.settled) return;
    return onTick(update);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [props.daa, props.confirmations, props.settled, props.chainDaa, props.chainAt]);
  const title = conf.settled
    ? t('activity.conf.settledTitle', { total: SETTLE_DAA })
    : conf.n !== null
      ? t('activity.conf.title', { n: conf.n, total: SETTLE_DAA, s: Math.max(0, Math.ceil((SETTLE_DAA - conf.n) / 10)) })
      : undefined;
  return (
    <span class="act-c-conf act-num" role="cell" title={title} data-settled={conf.settled ? 'true' : 'false'}>
      {conf.settled ? (
        <span class="act-ok" aria-hidden="true">{'✓'}</span>
      ) : (
        <>
          {/* always three figures wide (figure spaces before it): a count that grows moves nothing */}
          <span>{(conf.n === null ? '…' : String(conf.n)).padStart(3, ' ')}</span>
          <i class="act-pie" style={`--p:${Math.min(100, Math.round(((conf.n ?? 0) / SETTLE_DAA) * 100))}`} aria-hidden="true" />
        </>
      )}
    </span>
  );
});

// ------------------------------------------------------------------------------------------------ rows

/** The rows' places while the first rows are held: as tall as a row, with its columns. */
export function RowsSkeleton(props: { rows: number }) {
  return (
    <div role="status" aria-label={t('common.loading')} data-testid="activity-loading">
      {Array.from({ length: props.rows }, (_, i) => (
        <div key={i} class="act-row act-skel" aria-hidden="true">
          <span class="act-c-ty"><i class="skeleton act-skel-ty" /></span>
          <span class="act-c-mk"><i class="skeleton act-skel-mk" /></span>
          <span class="act-c-tx"><i class="skeleton skeleton-line" /></span>
          <span class="act-c-who"><i class="skeleton skeleton-line" /></span>
          <span class="act-c-amt act-num"><i class="skeleton skeleton-line" /></span>
          <span class="act-c-px act-num"><i class="skeleton skeleton-line" /></span>
          <span class="act-c-conf act-num"><i class="skeleton skeleton-line" /></span>
          <span class="act-c-time act-num"><i class="skeleton skeleton-line" /></span>
        </div>
      ))}
    </div>
  );
}

export interface RowProps {
  row: ActivityRow;
  /** the row's place in the list: its offset (a transform, never a layout change) */
  i: number;
  fresh: boolean;
  /** one of the first rows, on their way in */
  rise: boolean;
  href: string | null;
  /** the order owners' addresses, newline-separated ('' while none is known) */
  addrs: string;
  /** an owner is still being looked up */
  pending: boolean;
  chainDaa: number | null;
  chainAt: number | null;
  /** today's date (the time column shows HH:MM:SS today, MM-DD HH:MM before) */
  today: string;
  fmt: RowFormat;
}

/** One row: a link to the transaction (new tab), keyed by its txid; moves to its place by a transform. */
export const ActivityRowView = memo(function ActivityRowView(props: RowProps) {
  const r = props.row;
  const kindText = r.kinds.length > 1 ? r.kinds.map((k) => kindLabel(k, r.side)).join(' + ') : kindLabel(r.kind, r.side);
  const fullTime = r.timeMs !== null ? `${formatDateTime(Math.floor(r.timeMs / 1000), { timeStyle: 'medium' })}${r.timeEstimated ? ` (${t('activity.timeEstimated')})` : ''}` : `DAA ${r.daa}`;
  const amount = amountOf(r, props.fmt);
  const label = t('activity.rowLabel', {
    kind: kindText, market: props.fmt.marketText(r.market), amount: amount.text, conf: r.settled ? t('activity.conf.settled') : t('activity.conf.confirming'), time: fullTime, explorer: props.fmt.explorerHost,
  });
  const attrs = {
    role: 'row' as const,
    class: `act-row${props.fresh ? ' fresh' : ''}${props.rise ? ' ml-in' : ''}`,
    style: `--i:${props.i}`,
    'data-testid': 'activity-row',
    'data-kind': r.kind,
    'data-side': r.side ?? '',
    'data-txid': r.txid,
    'data-market': marketKeyOf(r.market),
    'aria-label': label,
    title: `${kindText} · ${t('activity.openTx', { explorer: props.fmt.explorerHost })}`,
  };
  const cells = <RowCells {...props} fullTime={fullTime} />;
  return props.href ? (
    <a {...attrs} href={props.href} target="_blank" rel="noopener noreferrer">
      {cells}
    </a>
  ) : (
    <div {...attrs}>{cells}</div>
  );
});

/** The cells of a row: rendered again only when what they show changed (not when the row moves or stops flashing). */
const RowCells = memo(
  function RowCells(props: RowProps & { fullTime: string }) {
    const r = props.row;
    const fmt = props.fmt;
    const addrs = props.addrs ? props.addrs.split('\n') : [];
    const amount = amountOf(r, fmt);
    const price = r.price ? priceRange(r.price, r.market.kind === 'token' ? fmt.decimalsOf(r.market.token) : 0) : '—';
    return (
      <>
        <span class="act-c-ty" role="cell">
          <KindIcon kind={r.kind} side={r.side} />
        </span>
        <span class="act-c-mk" role="cell" title={fmt.marketText(r.market)}>
          <MarketIcons market={r.market} nameOf={fmt.nameOf} label={fmt.marketText(r.market)} />
          {r.moreMarkets > 0 ? <span class="act-more" title={t('activity.moreMarketsTitle', { n: r.moreMarkets })}>{t('activity.moreMarkets', { n: r.moreMarkets })}</span> : null}
          <span class="act-txm">{r.txid}</span>
        </span>
        {/* the whole id: as much of it as the column has room for, then an ellipsis (the link and the title have it whole) */}
        <span class="act-c-tx" role="cell" title={r.txid}>{r.txid}</span>
        <span class="act-c-who" role="cell" title={addrs.length ? t('activity.wallet.title', { list: addrs.join(', ') }) : undefined} data-testid="activity-wallet">
          {addrs.length ? (
            addrs.slice(0, 2).flatMap((a, i) => [
              ...(i > 0 ? [<i key="s" class="act-sep" aria-hidden="true">{'⇄'}</i>] : []),
              <span key={a} class="act-w">{walletBody(a)}</span>,
            ])
          ) : (
            <span class="act-w muted">{props.pending ? t('activity.wallet.pending') : t('activity.wallet.unknown')}</span>
          )}
          {addrs.length > 2 ? <i class="act-sep">{`+${addrs.length - 2}`}</i> : null}
          {r.placed > 1 ? <i class="act-tag" title={t('activity.wallet.ladderTitle', { n: r.placed })}>{t('activity.wallet.ladder', { n: r.placed })}</i> : null}
          {r.counterparty ? <i class="act-tag">{t(`activity.cp.${r.counterparty}`)}</i> : null}
        </span>
        <span class={`act-c-amt act-num${r.side ? ` act-${r.side}` : ''}`} role="cell" title={amount.exact ?? zerosTitle(amount.text)}>
          <b>{compactZeros(amount.text)}</b>
          {amount.sub ? <span class="act-sub" title={zerosTitle(amount.sub)}>{compactZeros(amount.sub)}</span> : null}
        </span>
        <span class="act-c-px act-num" role="cell" title={price}>{compactZeros(price)}</span>
        <ConfCell daa={r.daa} confirmations={r.confirmations} settled={r.settled} chainDaa={props.chainDaa} chainAt={props.chainAt} />
        <span class="act-c-time act-num" role="cell" title={props.fullTime}>{rowTime(r.timeMs, Date.parse(props.today))}</span>
      </>
    );
  },
  (a, b) => a.row === b.row && a.addrs === b.addrs && a.pending === b.pending && a.chainDaa === b.chainDaa && a.chainAt === b.chainAt && a.today === b.today && a.fmt === b.fmt && a.fullTime === b.fullTime,
);

/** An address without its network prefix (the column shows as much of it as fits). */
const walletBody = (a: string): string => (a.includes(':') ? a.slice(a.indexOf(':') + 1) : a);

/** The amount cell's two lines: the token amount and its KAS value (a pair: the B amount; a pair placement: its side). */
function amountOf(r: ActivityRow, fmt: RowFormat): { text: string; sub: string; exact: string | null } {
  const token = r.amount?.token ?? null;
  const a = r.amount && token ? cellText(r.amount.units, fmt.decimalsOf(token)) : null;
  const text = a && token ? `${a.text} ${fmt.nameOf(token)}` : '—';
  let sub = '';
  // a pair placement keeps its side on the second line (its quote amount is there to turn the row round on the other orientation's page)
  if (r.kind === 'placed' && r.pairSide) sub = t(r.pairSide === 'ask' ? 'activity.pairAsk' : 'activity.pairBid');
  else if (r.value?.kind === 'kas') sub = `${fitAmountColumn([r.value.sompi], 8, { fraction: 2, width: 16 })[0]!.text} KAS`;
  else if (r.value?.kind === 'token') sub = `${cellText(r.value.units, fmt.decimalsOf(r.value.token)).text} ${fmt.nameOf(r.value.token)}`;
  else if (r.pairSide) sub = t(r.pairSide === 'ask' ? 'activity.pairAsk' : 'activity.pairBid');
  return { text, sub, exact: a?.exact && token ? `${a.exact} ${fmt.nameOf(token)}` : null };
}

/** One token icon for a KAS book, `A / B` for a pair (the tickers in the cell's title and the row's label). It leads to that market; the rest of the row to the transaction. */
function MarketIcons(props: { market: MarketRef; nameOf: (t: Hex) => string; label: string }) {
  const m = props.market;
  const icon = (id: Hex) => <TokenIcon row={{ covenantId: id, ticker: props.nameOf(id), kascov: null }} size={22} anonymous />;
  const open = (e: Event) => {
    e.preventDefault();
    e.stopPropagation();
    navigate(m.kind === 'token' ? tokenRoute(m.token) : pairRoute(m.base, m.quote));
  };
  return (
    <span
      class="act-mk act-mk-link"
      role="link"
      tabIndex={0}
      aria-label={props.label}
      data-testid="activity-market-link"
      onClick={open}
      onKeyDown={(e) => {
        if (e.key === 'Enter') open(e);
      }}
    >
      {m.kind === 'token' ? icon(m.token) : (
        <>
          {icon(m.base)}
          <span class="act-sl" aria-hidden="true">/</span>
          {icon(m.quote)}
        </>
      )}
    </span>
  );
}

/** An amount in one cell: the exact amount when it fits, else fewer decimals or K / M / B (the exact amount in `exact`). */
function cellText(units: bigint, decimals: number): { text: string; exact: string | null } {
  return fitAmountColumn([units], decimals, { fraction: Math.min(unitsFraction(units, decimals), 6), width: 12 })[0]!;
}

/** KAS per whole token (the row's own precision); a ladder shows its range. */
function priceRange(p: { low: bigint; high: bigint; scale: bigint }, decimals: number): string {
  const perToken = (x: bigint) => (p.scale > 0n ? (x * pow10(decimals)) / p.scale : x);
  const dp = priceDecimals(perToken(p.high));
  const one = (x: bigint) => priceText(x, decimals, p.scale, dp);
  return p.low === p.high ? one(p.low) : `${one(p.low)}–${one(p.high)}`;
}
