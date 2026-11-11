import { t } from '../../i18n';
import { unitParts } from '../kit';
import type { StatsModel } from './market-model';

function Stat(props: { label: string; value: string | null; title?: string | null; tone?: string | null; testid: string; loading: boolean; thin?: boolean }) {
  return (
    <div class="mkt-stat" data-testid={props.testid}>
      <span class="mkt-stat-label">{props.label}</span>
      <span class={`mkt-stat-value${props.tone ? ` tone-${props.tone}` : ''}`} title={props.title ?? undefined} data-thin={props.thin ? 'true' : undefined}>
        {props.value ?? (props.loading ? <span class="skeleton skeleton-line" style="display:inline-block;width:64px;margin:0" /> : '—')}
      </span>
    </div>
  );
}

/**
 * 24 h statistics strip (`/v1/stats`), the figures of the market bar (MarketBar.tsx): last price (coloured by the aggressor side) with its change, high, low, volume in the token and in KAS,
 * trades. One flat row of cells split by thin rules; where the bar has no room it scrolls sideways inside itself (market.css). A price rounded to the market's display decimals has the whole price as its title.
 */
export function MarketStats(props: {
  model: StatsModel;
  /** display name of the token */
  ticker: string;
  /** shown as KAS/TOKEN: price unit TOKEN / KAS, volumes in KAS and in the token */
  inverted?: boolean;
  loading: boolean;
  unsupported: boolean;
  /** the figures are let in: they fade in together */
  open?: boolean;
  /** the KAS tiles are shown in USD (Settings, Display currency; stats-usd.ts): the unit and the volume label say USD */
  usd?: boolean;
  /** the tooltip tail of the last price (its KAS figure and the rate used) */
  lastTitle?: string | null;
  /** a token pair BASE/QUOTE (`ticker` = BASE): the same tiles; `titles` say where each figure comes from */
  quoteTicker?: string;
  /** tooltips of a pair's tiles by where their figure comes from (the exact figure of a volume is added in front) */
  titles?: Partial<Record<'high' | 'low' | 'volume' | 'quoteVolume' | 'trades', string | null>>;
  /** a pair: where the volume and trade tiles come from (`fills`, `fills-partial`, `candles`, `none`) */
  volumeSource?: string;
}) {
  const tip = (exact: string | null | undefined, note: string | null | undefined): string | null => (exact && note ? `${exact} · ${note}` : (exact ?? note ?? null));
  const base = props.quoteTicker ? props.ticker : props.inverted ? 'KAS' : props.ticker;
  // the label of the BASE volume tile: a flipped market's KAS volume reads USD when it is shown in dollars (the price unit above keeps KAS)
  const volumeBase = props.inverted && props.usd ? 'USD' : base;
  const quote = props.quoteTicker ?? (props.inverted ? props.ticker : props.usd ? 'USD' : 'KAS');
  const m = props.model;
  const na = props.quoteTicker ? t('pair.stats.na') : null;
  const lastTone = m.lastSide === 'buy' ? 'up' : m.lastSide === 'sell' ? 'down' : null;
  return (
    <div class={`mkt-stats${props.open ? ' mkt-stats-in' : ''}`} data-testid="market-stats" data-volume-source={props.volumeSource}
      data-state={props.unsupported ? 'unsupported' : props.loading && !m.last ? 'loading' : 'ready'}>
      <div class="mkt-stat mkt-stat-last" data-testid="stat-last">
        <span class="mkt-stat-label">{t('market.stats.last')}</span>
        <span class={`mkt-stat-value${lastTone ? ` tone-${lastTone}` : ''}`} translate={false} title={[m.lastExact, props.lastTitle].filter(Boolean).join(' · ') || undefined}>
          {m.last ?? (props.loading ? <span class="skeleton skeleton-line" style="display:inline-block;width:120px;height:22px;margin:0" /> : '—')}
          <span class="mkt-stat-unit">{unitParts(t('market.stats.unitOf', { quote, base }))}</span>
        </span>
      </div>
      <Stat testid="stat-change" label={t('market.stats.change')} value={m.change} tone={m.changeTone} title={m.changeNote} thin={!!m.changeNote} loading={props.loading} />
      <Stat testid="stat-high" label={t('market.stats.high')} value={m.high} title={props.titles?.high ?? na ?? m.highExact} loading={props.loading} />
      <Stat testid="stat-low" label={t('market.stats.low')} value={m.low} title={props.titles?.low ?? na ?? m.lowExact} loading={props.loading} />
      <Stat testid="stat-volume" label={t('market.stats.volume', { ticker: volumeBase })} value={m.volume} title={tip(m.volumeExact, props.titles?.volume) ?? na} loading={props.loading} />
      <Stat testid="stat-quote-volume" label={t('market.stats.volume', { ticker: quote })} value={m.quoteVolume} title={tip(m.quoteVolumeExact, props.titles?.quoteVolume) ?? na} loading={props.loading} />
      <Stat testid="stat-trades" label={t('market.stats.trades')} value={m.tradesText ?? (m.trades === null ? null : String(m.trades))} title={props.titles?.trades ?? na} loading={props.loading} />
    </div>
  );
}
