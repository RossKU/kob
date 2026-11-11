// "Market trades": the recent fills of this market under the order ticket, newest first (time, price coloured by the aggressor's side, amount). It is drawn from the tape the page already reads
// (`/v1/trades`, or the fill events): no read of its own. It lives in the room the row's drag handle (RowResize.tsx) gives the ticket's column under the ticket card, and shows only when there
// are at least 132 px of it (market.css, a container query on the panel's own height); below that the panel is not drawn and the ticket fills the column as before.
import { t } from '../../i18n';
import { formatUnits } from '../../kob/units';
import { amountText, priceText, tapeTime, type Tape } from './market-model';
import { flippedPriceText, oppositeSide } from './orientation';

export function MarketTrades(props: {
  tape: Tape | undefined;
  decimals: number;
  /** price decimals of the shown orientation */
  dp: number;
  /** shown as KAS/TOKEN: prices are tokens per KAS, amounts KAS, the side the other way round */
  inverted?: boolean;
  /** display name of the token */
  ticker: string;
}) {
  const { tape } = props;
  const rows = tape?.rows.filter((r) => r.price !== null && r.amount !== null) ?? [];
  const basis = tape?.basis ?? null;
  return (
    <div class="mkt-trades" data-testid="market-trades-slot">
      <section class="mkt-trades-card" data-testid="market-trades" aria-label={t('market.mtrades.title')}>
        <h3 class="mkt-trades-title">{t('market.mtrades.title')}</h3>
        <div class="mkt-trades-head" aria-hidden="true">
          <span>{t('market.mtrades.time')}</span>
          <span>{t('market.mtrades.price')}</span>
          <span>{t('market.mtrades.amount', { unit: props.inverted ? 'KAS' : props.ticker })}</span>
        </div>
        <div class="mkt-trades-list" role="list">
          {rows.length === 0 || basis === null ? (
            <p class="muted small mkt-trades-empty" data-testid="market-trades-empty">{t('market.mtrades.empty')}</p>
          ) : (
            rows.map((r) => {
              const side = r.side === null ? null : props.inverted ? oppositeSide(r.side) : r.side;
              const price = props.inverted ? flippedPriceText(r.price!, props.decimals, basis, props.dp) : priceText(r.price!, props.decimals, basis, props.dp);
              const amount = props.inverted ? formatUnits(r.quote ?? 0n, 8, { maxFraction: 4, group: ',' }) : amountText(r.amount!, props.decimals, 4);
              return (
                <div key={r.id} class="mkt-trades-row" role="listitem" data-testid="market-trade-row" data-side={side ?? ''}>
                  <span class="muted">{tapeTime(r.timeMs)}</span>
                  <span class={side === 'buy' ? 'buy' : side === 'sell' ? 'sell' : ''}>{price}</span>
                  <span>{amount}</span>
                </div>
              );
            })
          )}
        </div>
      </section>
    </div>
  );
}
