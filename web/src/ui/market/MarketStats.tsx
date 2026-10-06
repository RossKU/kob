import { t } from '../../i18n';
import type { StatsModel } from './market-model';

function Stat(props: { label: string; value: string | null; title?: string | null; tone?: string | null; testid: string; loading: boolean }) {
  return (
    <div class="mkt-stat" data-testid={props.testid}>
      <span class="mkt-stat-label">{props.label}</span>
      <span class={`mkt-stat-value${props.tone ? ` tone-${props.tone}` : ''}`} title={props.title ?? undefined}>
        {props.value ?? (props.loading ? <span class="skeleton skeleton-line" style="display:inline-block;width:64px;margin:0" /> : '—')}
      </span>
    </div>
  );
}

/** 24 h statistics strip (`/v1/stats`): last price (coloured by the aggressor side), change, high, low, volume in the token and in KAS. */
export function MarketStats(props: {
  model: StatsModel;
  /** display name of the token */
  ticker: string;
  /** shown as KAS/TOKEN: price unit TOKEN / KAS, volumes in KAS and in the token */
  inverted?: boolean;
  loading: boolean;
  unsupported: boolean;
  /** a token pair BASE/QUOTE (`ticker` = BASE): the same tiles; `titles` say where each figure comes from */
  quoteTicker?: string;
  /** tooltips of a pair's tiles by where their figure comes from (the exact figure of a volume is added in front) */
  titles?: Partial<Record<'high' | 'low' | 'volume' | 'quoteVolume' | 'trades', string | null>>;
  /** a pair: where the volume and trade tiles come from (`fills`, `fills-partial`, `candles`, `none`) */
  volumeSource?: string;
}) {
  const tip = (exact: string | null | undefined, note: string | null | undefined): string | null => (exact && note ? `${exact} · ${note}` : (exact ?? note ?? null));
  const base = props.quoteTicker ? props.ticker : props.inverted ? 'KAS' : props.ticker;
  const quote = props.quoteTicker ?? (props.inverted ? props.ticker : 'KAS');
  const m = props.model;
  const na = props.quoteTicker ? t('pair.stats.na') : null;
  const lastTone = m.lastSide === 'buy' ? 'up' : m.lastSide === 'sell' ? 'down' : null;
  return (
    <div class="mkt-stats" data-testid="market-stats" data-volume-source={props.volumeSource}
      data-state={props.unsupported ? 'unsupported' : props.loading && !m.last ? 'loading' : 'ready'}>
      <div class="mkt-stat mkt-stat-last" data-testid="stat-last">
        <span class="mkt-stat-label">{t('market.stats.last')}</span>
        <span class={`mkt-stat-value${lastTone ? ` tone-${lastTone}` : ''}`}>
          {m.last ?? (props.loading ? <span class="skeleton skeleton-line" style="display:inline-block;width:120px;height:22px;margin:0" /> : '—')}
          <span class="mkt-stat-unit">{t('market.stats.unitOf', { quote, base })}</span>
        </span>
      </div>
      <Stat testid="stat-change" label={t('market.stats.change')} value={m.change} tone={m.changeTone} loading={props.loading} />
      <Stat testid="stat-high" label={t('market.stats.high')} value={m.high} title={props.titles?.high ?? na} loading={props.loading} />
      <Stat testid="stat-low" label={t('market.stats.low')} value={m.low} title={props.titles?.low ?? na} loading={props.loading} />
      <Stat testid="stat-volume" label={t('market.stats.volume', { ticker: base })} value={m.volume} title={tip(m.volumeExact, props.titles?.volume) ?? na} loading={props.loading} />
      <Stat testid="stat-quote-volume" label={t('market.stats.volume', { ticker: quote })} value={m.quoteVolume} title={tip(m.quoteVolumeExact, props.titles?.quoteVolume) ?? na} loading={props.loading} />
      <Stat testid="stat-trades" label={t('market.stats.trades')} value={m.tradesText ?? (m.trades === null ? null : String(m.trades))} title={props.titles?.trades ?? na} loading={props.loading} />
    </div>
  );
}
