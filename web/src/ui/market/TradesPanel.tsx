import { useRef } from 'preact/hooks';
import { useTxUrl } from '../../app/explorer';
import { formatDateTime, formatNumber, t } from '../../i18n';
import { ErrorBanner, kasText, Section } from '../kit';
import { amountText, tapeTime, type Tape } from './market-model';
import { oppositeSide, tapeColumns } from './orientation';

/** Trade tape: newest first, price coloured by the aggressor side (green = taker bought, red = taker sold), size in tokens, local time. */
export function TradeTape(props: {
  tape: Tape | undefined;
  loading: boolean;
  error: Error | null;
  onRetry: () => void;
  decimals: number;
  /** display name of the token */
  ticker: string;
  /** price decimals of the shown orientation */
  dp: number;
  /** the market's tick (sompi per whole token): fixes the decimals of the size column of the inverted view (KAS) */
  tick?: bigint | null;
  /** shown as KAS/TOKEN: price = tokens per KAS, size = KAS, the aggressor's side flips (the taker who bought the token sold KAS) */
  inverted?: boolean;
}) {
  const tape = props.tape;
  const txUrl = useTxUrl();
  const seen = useRef<Set<string> | null>(null);
  const basis = tape?.basis ?? null;
  const inv = !!props.inverted;
  // fixed decimals per column, chosen once for the whole tape (decimal points line up)
  const cols = tape ? tapeColumns(tape, { decimals: props.decimals, dp: props.dp, inverted: inv, tick: props.tick ?? null }) : [];
  // fill events of an indexer without /v1/trades and a market without a known scale: prices stay per the orders' own basis
  const perBasis = !inv && tape?.source === 'fills' && basis === null;
  // rows that arrive after the first render flash once
  const fresh = new Set<string>();
  if (tape) {
    if (seen.current) for (const r of tape.rows) if (!seen.current.has(r.id)) fresh.add(r.id);
    seen.current = new Set(tape.rows.map((r) => r.id));
  }
  return (
    <Section title={t('market.trades.title')} data-testid="trades-section" class="mkt-card" actions={tape?.source === 'fills' ? <span class="small muted" title={t('market.trades.fallbackHint')}>{t('market.trades.fallback')}</span> : null}>
      <ErrorBanner error={props.error} onRetry={props.onRetry} />
      <div class="tape" data-testid="trades-list" data-source={tape?.source ?? ''} role="table" aria-label={t('market.trades.title')}>
        <div class="tape-head" role="row">
          <span role="columnheader">{perBasis ? t('market.trades.priceBasis') : t('market.trades.priceIn', { unit: inv ? props.ticker : 'KAS' })}</span>
          <span role="columnheader">{perBasis ? t('market.trades.amountIn', { unit: t('common.baseUnits') }) : t('market.trades.amountIn', { unit: inv ? 'KAS' : props.ticker })}</span>
          <span role="columnheader">{t('market.trades.time')}</span>
        </div>
        <div class="tape-body" role="rowgroup">
          {tape === undefined ? (
            props.loading ? (
              Array.from({ length: 10 }, (_, i) => <div key={i} class="skeleton skeleton-line" style={`margin:9px 8px;opacity:${1 - i * 0.08}`} />)
            ) : (
              <div class="mkt-empty">{t('market.trades.empty')}</div>
            )
          ) : tape.rows.length === 0 ? (
            <div class="mkt-empty" data-testid="trades-empty">{t('market.trades.empty')}</div>
          ) : (
            tape.rows.map((r, i) => {
              const { price, size } = cols[i]!;
              const side = inv && r.side ? oppositeSide(r.side) : r.side;
              const detail = inv ? [r.amount !== null ? `${amountText(r.amount, props.decimals)} ${props.ticker}` : '', r.quote !== null ? `${kasText(r.quote)} KAS` : ''].filter(Boolean).map((x) => ` · ${x}`).join('') : r.quote !== null ? ` · ${kasText(r.quote)} KAS` : '';
              const when = r.timeMs !== null ? formatDateTime(Math.floor(r.timeMs / 1000), { timeStyle: 'medium' }) : `DAA ${formatNumber(r.daa)}`;
              const sideText = side ? t(`market.trades.${side}`) : '';
              const href = txUrl(r.txid);
              const rowProps = {
                role: 'row' as const,
                class: `tape-row${fresh.has(r.id) ? ' fresh' : ''}${href ? ' tape-link' : ''}`,
                'data-testid': 'trade-row',
                'data-side': side ?? '',
                'data-txid': r.txid ?? '',
                title: `${sideText} · ${when}${r.settled ? '' : ` (${t('market.trades.unsettled')})`}${detail}${href ? ` · ${t('market.trades.openTx')}` : ''}`,
              };
              const cells = (
                <>
                  <span class="tape-price" role="cell">{price}</span>
                  <span role="cell">{size}</span>
                  <span class="tape-time" role="cell">{tapeTime(r.timeMs)}{r.settled ? '' : '*'}</span>
                </>
              );
              // a click opens the trade's transaction on the block explorer, in a new tab
              return href ? (
                <a key={r.id} {...rowProps} href={href} target="_blank" rel="noopener noreferrer">{cells}</a>
              ) : (
                <div key={r.id} {...rowProps}>{cells}</div>
              );
            })
          )}
        </div>
      </div>
    </Section>
  );
}
