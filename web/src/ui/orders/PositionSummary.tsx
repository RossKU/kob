import type { ComponentChildren } from 'preact';
import { useState } from 'preact/hooks';
import { formatDateTime, formatNumber, t } from '../../i18n';
import type { EventView } from '../../data/indexer-types';
import type { Position } from '../../kob/positions';
import type { TokenInfo } from '../../kob/registry';
import type { Hex } from '../../kob/types';
import { Amount, Button, Loading, scaleOfDecimals, tickKasFraction, tickPriceFraction, toError, useAsync } from '../kit';
import { formatUnits } from '../../kob/units';
import { ratio, realisedPnl, summarizePositionFills, type PositionFills, type PositionSummary } from './position-model';

export interface PositionSummaryProps {
  position: Position;
  summary: PositionSummary;
  token: TokenInfo | undefined;
  /** a pair position (KobIfdPair entry, KobCondPair exits): its quote token B; the prices are B base units per whole A (`quote` unknown: raw units) */
  pair?: { quote: TokenInfo | undefined; quoteId: Hex } | null;
  /** loads the events of one order (the indexer's `/v1/orders/{id}/events`, every page); absent: no fills section */
  loadEvents?: (id: Hex, signal: AbortSignal) => Promise<EventView[]>;
}

/**
 * A state price (sompi per whole token of `scale` base units): KAS per whole token when the token is known, else KAS per `scale` base units.
 * Missing: a dash.
 */
function Price(p: { value: bigint | null; token: TokenInfo | undefined; scale: bigint; pair?: PositionSummaryProps['pair'] }) {
  if (p.value === null) return <>{'—'}</>;
  const tk = p.token;
  if (p.pair) {
    const q = p.pair.quote;
    const base = tk?.ticker ?? '?';
    return <span data-unit="pair">{q ? `${formatUnits(p.value, q.decimals, { group: ',' })} ${q.ticker}/${base}` : `${p.value.toString()} ${t('common.baseUnits')}/${base}`}</span>;
  }
  // fixed decimals from the market's tick: every price of the card (entry, exits, averages) has the same number of decimals
  const pf = tk ? tickPriceFraction(tk.tick, tk.decimals, p.scale) : null;
  const lf = tickKasFraction(tk?.tick ?? null);
  return tk ? (
    <Amount kind="price" value={p.value} decimals={tk.decimals} scale={p.scale} unit={`KAS/${tk.ticker}`} {...(pf === null ? {} : { fraction: pf })} group />
  ) : (
    <Amount kind="kas" value={p.value} unit={`KAS ${t('orders.perScale', { scale: p.scale.toString() })}`} fraction={lf} group />
  );
}

function Prices({ values, token, scale, pair }: { values: readonly bigint[]; token: TokenInfo | undefined; scale: bigint; pair?: PositionSummaryProps['pair'] }) {
  return (
    <>
      {values.map((v, i) => (
        <span key={String(v)} class="pc-price">
          {i ? <span class="pc-sep">{', '}</span> : null}
          <Price value={v} token={token} scale={scale} pair={pair} />
        </span>
      ))}
    </>
  );
}

/** A slim progress bar (0..1); nothing when the ratio is unknown. */
function Bar({ value, label, tone }: { value: number | null; label: string; tone?: 'buy' | 'sell' | 'accent' }) {
  if (value === null) return <div class="pc-bar pc-bar-empty" aria-hidden="true" />;
  const pct = Math.round(value * 1000) / 10;
  return (
    <div class={`pc-bar pc-bar-${tone ?? 'accent'}`} role="progressbar" aria-label={label} aria-valuemin={0} aria-valuemax={100} aria-valuenow={pct}>
      <span style={{ width: `${pct}%` }} />
    </div>
  );
}

function Stat(p: { label: ComponentChildren; aside?: ComponentChildren; children: ComponentChildren; sub?: ComponentChildren; wide?: boolean; class?: string; testid?: string }) {
  return (
    <div class={`pc-stat${p.wide ? ' pc-wide' : ''}${p.class ? ` ${p.class}` : ''}`} data-testid={p.testid}>
      <div class="pc-label">
        <span>{p.label}</span>
        {p.aside ? <span class="pc-aside">{p.aside}</span> : null}
      </div>
      <div class="pc-value num">{p.children}</div>
      {p.sub ? <div class="pc-sub">{p.sub}</div> : null}
    </div>
  );
}

const pctText = (v: number): string => `${v > 0 ? '+' : ''}${formatNumber(v, { minimumFractionDigits: 2, maximumFractionDigits: 2 })}%`;

/** Realised fills of the position: entry and exit legs with their average prices (loaded by the panel on demand). */
function Fills({ fills: f, token, amount }: { fills: PositionFills; token: TokenInfo | undefined; amount: (v: bigint | null) => string }) {
  const spread = f.spreadPerToken;
  return (
    <div class="pc-fills small" data-testid="position-fills">
      <div class="pc-fill-row" data-testid="position-fills-entry">
        <span>{t('orders.position.fills.entry', { amount: amount(f.entry.amount), fills: f.entry.fills })}</span>
        {f.entry.avgPrice !== null ? (
          <span class="pc-fill-avg num">
            <span class="muted">{t('orders.position.fills.avg')}</span> <Price value={f.entry.avgPrice} token={token} scale={f.scale} />
          </span>
        ) : null}
      </div>
      <div class="pc-fill-row" data-testid="position-fills-exits">
        <span>{t('orders.position.fills.exits', { amount: amount(f.exits.amount), fills: f.exits.fills })}</span>
        {f.exits.avgPrice !== null ? (
          <span class="pc-fill-avg num">
            <span class="muted">{t('orders.position.fills.avg')}</span> <Price value={f.exits.avgPrice} token={token} scale={f.scale} />
          </span>
        ) : null}
      </div>
      {spread !== null ? (
        <div class="pc-fill-row" data-testid="position-fills-spread">
          <span>{t('orders.position.fills.spread')}</span>
          {token ? (
            <Amount kind="price" value={spread} decimals={token.decimals} scale={f.scale} signed unit={`KAS/${token.ticker}`} class={spread > 0n ? 'pc-pos' : spread < 0n ? 'pc-neg' : ''} />
          ) : (
            <Amount kind="kas" value={spread} signed unit={`KAS ${t('orders.perScale', { scale: f.scale.toString() })}`} class={spread > 0n ? 'pc-pos' : spread < 0n ? 'pc-neg' : ''} />
          )}
        </div>
      ) : null}
    </div>
  );
}

/** Stat grid of a position card: price path, amounts with progress, repeat progress, realised PnL; realised fills on demand. */
export function PositionSummaryPanel({ position, summary: s, token, loadEvents, pair = null }: PositionSummaryProps) {
  const [showFills, setShowFills] = useState(false);
  const members = position.entry ? [position.entry, ...position.exits] : position.exits;
  const key = members.map((m) => `${m.covenant_id}:${m.filled_amount}:${m.status}`).join(',');
  // the orders' scale: an entry and its exits quote per the same whole token
  const scaleRaw = position.entry?.scale ?? position.exits[0]?.scale ?? null;
  const scale = scaleRaw ? BigInt(scaleRaw) : token ? scaleOfDecimals(token.decimals) : 1n;
  const amount = (v: bigint | null): string => (v === null ? '?' : token ? `${formatUnits(v, token.decimals, { group: ',' })} ${token.ticker}` : v.toString());
  // one events request per member order, only once the user asked for the fills
  const fillsData = useAsync(
    async (signal): Promise<PositionFills | null> => {
      if (!showFills || !loadEvents) return null;
      const lists = await Promise.all(members.map((m) => loadEvents(m.covenant_id, signal)));
      const entryId = position.entry?.covenant_id;
      const entry = members.flatMap((m, i) => (m.covenant_id === entryId ? lists[i]! : []));
      const exits = members.flatMap((m, i) => (m.covenant_id === entryId ? [] : lists[i]!));
      return summarizePositionFills(entry, exits, position.side, scale);
    },
    [key, showFills, !!loadEvents],
  );
  const fills = showFills ? (fillsData.data ?? null) : null;
  const pnl = fills ? realisedPnl(fills) : null;

  const entryStop = s.entryStop !== null && s.entryPrice === null;
  const tps = s.exitTakeProfits;
  const stops = [...new Set(s.exits.filter((e) => e.stop !== null && (e.status === 'open' || e.status === 'partial')).map((e) => e.stop as bigint))];
  const amountLine = t('orders.position.amountLine', { total: amount(s.amountTotal), entered: amount(s.amountEntered), exited: amount(s.amountExited), open: amount(s.amountOpen) });
  const r = s.repeat;
  // the re-arms left as whole cycles of the entry's amount (rounded down)
  const cyclesLeft = r && r.rearmAmount !== null && s.amountTotal !== null && s.amountTotal > 0n ? r.rearmAmount / s.amountTotal : null;
  const repeatTotal = r && cyclesLeft !== null ? BigInt(r.cyclesDone) + cyclesLeft : null;
  const until = r
    ? r.untilUnix !== null
      ? t('orders.position.repeatUntil', { date: formatDateTime(Number(r.untilUnix)) })
      : r.untilDaa !== null
        ? t('orders.position.repeatUntilDaa', { daa: formatNumber(r.untilDaa) })
        : null
    : null;
  const pnlClass = pnl?.pnl ? (pnl.pnl > 0n ? 'pc-pos' : 'pc-neg') : '';

  return (
    <div class="position-summary pc-summary" data-testid="position-summary">
      <div class="pc-grid">
        <div class="pc-contents" data-testid="position-prices">
          <Stat label={entryStop ? t('orders.position.stat.entryStop') : t('orders.position.stat.entry')}>
            <span data-testid="position-entry-price">
              <Price value={entryStop ? s.entryStop : s.entryPrice} token={token} scale={scale} pair={pair} />
            </span>
          </Stat>
          <Stat label={t('orders.position.stat.takeProfit')} class="pc-tp">
            <span data-testid="position-exit-prices">{tps.length ? <Prices values={tps} token={token} scale={scale} pair={pair} /> : '—'}</span>
          </Stat>
          <Stat label={t('orders.position.stat.stop')} class="pc-sl">
            {stops.length ? (
              <span data-testid="position-exit-stops">
                <Prices values={stops} token={token} scale={scale} pair={pair} />
              </span>
            ) : (
              '—'
            )}
          </Stat>
        </div>
        <Stat
          label={t('orders.position.stat.pnl')}
          class="pc-pnl"
          testid="position-pnl"
          sub={
            pnl ? (
              <span title={t('orders.position.stat.pnlNote')}>
                {t('orders.position.stat.pnlClosed', { amount: amount(pnl.amountClosed) })}
                {pnl.returnPct !== null ? <span class={pnlClass}> · {pctText(pnl.returnPct)}</span> : null}
              </span>
            ) : loadEvents && !showFills ? (
              <button type="button" class="pc-link" onClick={() => setShowFills(true)}>
                {t('orders.position.stat.pnlHint')}
              </button>
            ) : null
          }
        >
          {pnl && pnl.pnl !== null ? <Amount kind="kas" value={pnl.pnl} signed unit="KAS" class={pnlClass} data-testid="position-pnl-value" /> : '—'}
        </Stat>

        <Stat
          wide
          label={t('orders.position.stat.amount')}
          aside={
            <>
              {t('orders.position.stat.open')} <strong class="num">{amount(s.amountOpen)}</strong>
            </>
          }
          testid="position-amount"
          class={r ? 'pc-amounts' : 'pc-amounts pc-full'}
        >
          <span class="sr-only">{amountLine}</span>
          <div class="pc-bars" aria-hidden="true">
            <div class="pc-bar-row">
              <span class="pc-bar-label">{t('orders.position.stat.entered')}</span>
              <Bar value={ratio(s.amountEntered, s.amountTotal)} tone={s.side === 'buy' ? 'buy' : 'sell'} label={t('orders.position.stat.enteredBar', { done: amount(s.amountEntered), total: amount(s.amountTotal) })} />
              <span class="pc-bar-fig num" aria-hidden="true">
                {amount(s.amountEntered)}
                <span class="pc-of"> / {amount(s.amountTotal)}</span>
              </span>
            </div>
            <div class="pc-bar-row">
              <span class="pc-bar-label">{t('orders.position.stat.exited')}</span>
              <Bar value={ratio(s.amountExited, s.amountEntered) ?? (s.amountEntered === 0n ? 0 : null)} tone={s.side === 'buy' ? 'sell' : 'buy'} label={t('orders.position.stat.exitedBar', { done: amount(s.amountExited), total: amount(s.amountEntered) })} />
              <span class="pc-bar-fig num" aria-hidden="true">
                {amount(s.amountExited)}
                <span class="pc-of"> / {amount(s.amountEntered)}</span>
              </span>
            </div>
          </div>
        </Stat>

        {r ? (
          <Stat wide label={t('orders.position.stat.repeat')} aside={until} testid="position-repeat" class="pc-repeat">
            <span class="sr-only">
              {cyclesLeft !== null ? t('orders.position.repeatLeft', { left: cyclesLeft.toString(), done: r.cyclesDone }) : t('orders.position.repeatDone', { done: r.cyclesDone })}
              {until ? ` · ${until}` : ''}
            </span>
            <span aria-hidden="true" data-testid="position-cycles">
              {cyclesLeft !== null ? t('orders.position.stat.repeatValue', { done: r.cyclesDone, left: cyclesLeft.toString() }) : t('orders.position.stat.repeatUnbounded', { done: r.cyclesDone })}
            </span>
            {repeatTotal !== null ? (
              <Bar value={ratio(BigInt(r.cyclesDone), repeatTotal) ?? 0} tone="accent" label={t('orders.position.stat.repeatBar', { done: r.cyclesDone, left: cyclesLeft !== null ? cyclesLeft.toString() : '?' })} />
            ) : null}
          </Stat>
        ) : null}
      </div>

      {loadEvents ? (
        <div class="pc-fills-box">
          <Button small variant="ghost" onClick={() => setShowFills(!showFills)} data-testid="position-fills-toggle" aria-expanded={showFills}>
            <span class="pc-caret" aria-hidden="true">{showFills ? '▾' : '▸'}</span> {showFills ? t('orders.position.fills.hide') : t('orders.position.fills.show')}
          </Button>
          {showFills ? (
            fillsData.loading && !fills ? (
              <Loading />
            ) : fillsData.error && !fills ? (
              <span class="small muted" data-testid="position-fills-error">
                {toError(fillsData.error).message}
              </span>
            ) : fills ? (
              <Fills fills={fills} token={token} amount={amount} />
            ) : null
          ) : null}
        </div>
      ) : null}
    </div>
  );
}
