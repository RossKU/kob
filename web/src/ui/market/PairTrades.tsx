import { useRef } from 'preact/hooks';
import { useTxUrl } from '../../app/explorer';
import { formatDateTime, formatNumber, has, t } from '../../i18n';
import type { PairToken } from '../../kob/pair';
import { unitsText } from '../../kob/pair';
import { ErrorBanner, Section } from '../kit';
import { tapeTime } from './market-model';
import { pairTapeColumns, type PairFill } from './pair-market';

/** A pair with fewer fills than this gets the short note about what the list contains. */
export const FEW_FILLS = 10;

const counterpartyText = (c: string): string => (has(`pair.trades.cp.${c}`) ? t(`pair.trades.cp.${c}`) : c);

/**
 * Recent fills of a token pair's orders (newest first): amounts of BASE and QUOTE, how each was filled (through the KAS route, by netting against an
 * opposite pair order, from a filler's inventory) and when. A pair fill is VOLUME only: it never makes a price, so there is no price column (the pair
 * price is derived from the two KAS markets). The taker's side colours the row.
 */
export function PairTrades(props: {
  fills: readonly PairFill[] | undefined;
  loading: boolean;
  error: Error | null;
  onRetry: () => void;
  base: PairToken;
  quote: PairToken;
  limit?: number;
}) {
  const rows = props.fills?.slice(0, props.limit ?? 50);
  const txUrl = useTxUrl();
  const seen = useRef<Set<number> | null>(null);
  const cols = rows ? pairTapeColumns(rows, props.base, props.quote) : [];
  const fresh = new Set<number>();
  if (rows) {
    if (seen.current) for (const r of rows) if (!seen.current.has(r.id)) fresh.add(r.id);
    seen.current = new Set(rows.map((r) => r.id));
  }
  return (
    <Section title={t('pair.trades.title')} data-testid="trades-section" class="mkt-card">
      <ErrorBanner error={props.error} onRetry={props.onRetry} />
      <div class="tape pair-tape" data-testid="trades-list" data-source="pair" data-count={rows?.length ?? ''} role="table" aria-label={t('pair.trades.title')}>
        <div class="tape-head" role="row">
          <span role="columnheader">{t('market.trades.amountIn', { unit: props.base.ticker })}</span>
          <span role="columnheader">{t('market.trades.amountIn', { unit: props.quote.ticker })}</span>
          <span role="columnheader">{t('pair.trades.counterparty')}</span>
          <span role="columnheader">{t('market.trades.time')}</span>
        </div>
        <div class="tape-body" role="rowgroup">
          {rows === undefined ? (
            props.loading ? (
              Array.from({ length: 10 }, (_, i) => <div key={i} class="skeleton skeleton-line" style={`margin:9px 8px;opacity:${1 - i * 0.08}`} />)
            ) : (
              <div class="mkt-empty">{t('pair.trades.empty')}</div>
            )
          ) : rows.length === 0 ? (
            <div class="mkt-empty" data-testid="trades-empty">{t('pair.trades.empty')}</div>
          ) : (
            rows.map((r, i) => {
              const c = cols[i]!;
              const when = r.timeMs !== null ? formatDateTime(Math.floor(r.timeMs / 1000), { timeStyle: 'medium' }) : `DAA ${formatNumber(r.daa)}`;
              const quotePart = r.quoteUnits !== null ? ` · ${unitsText(r.quoteUnits, props.quote.decimals, ',')} ${props.quote.ticker}` : '';
              const title = `${t(`market.trades.${r.side}`)} · ${when}${r.settled ? '' : ` (${t('market.trades.unsettled')})`} · ${unitsText(r.baseUnits, props.base.decimals, ',')} ${props.base.ticker}${quotePart} · ${counterpartyText(r.counterparty)}`;
              const href = txUrl(r.txid);
              const rowProps = {
                role: 'row' as const,
                class: `tape-row tape-${r.side}${fresh.has(r.id) ? ' fresh' : ''}${href ? ' tape-link' : ''}`,
                'data-testid': 'trade-row',
                'data-side': r.side,
                'data-counterparty': r.counterparty,
                'data-txid': r.txid,
                title: href ? `${title} · ${t('market.trades.openTx')}` : title,
              };
              const cells = (
                <>
                  <span class="tape-price" role="cell">{c.base}</span>
                  <span role="cell">{c.quote}</span>
                  <span class="muted" role="cell" data-testid="trade-counterparty">{counterpartyText(r.counterparty)}</span>
                  <span class="tape-time" role="cell">{tapeTime(r.timeMs)}{r.settled ? '' : '*'}</span>
                </>
              );
              return href ? (
                <a key={r.id} {...rowProps} href={href} target="_blank" rel="noopener noreferrer">{cells}</a>
              ) : (
                <div key={r.id} {...rowProps}>{cells}</div>
              );
            })
          )}
        </div>
      </div>
      {rows !== undefined && rows.length < FEW_FILLS ? <p class="small muted" style="margin:6px 8px 0" data-testid="pair-trades-note">{t('pair.trades.note')}</p> : null}
    </Section>
  );
}
