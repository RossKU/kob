// The common top of every market page: the pair as `BASE / QUOTE` where each side is a drop-down (KAS and the tokens), and the flip control. The same
// markup for a token's KAS market and for a token pair, so the page keeps its layout whichever quote is chosen.
import type { ComponentChildren } from 'preact';
import { t } from '../../i18n';
import { Button } from '../kit';
import { KAS, assetChoices, type AssetChoice, type DisplayedPair } from './market-select';
import { tokenTitle } from './TokenBadges';
import type { TokenRow } from './token-model';

/** A token as an option of the selector: pairable = in the registry (a pair needs both tokens' registry entries). */
const choiceRow = (r: TokenRow) => ({ covenantId: r.covenantId, ticker: r.ticker, pairable: r.info !== null, row: r });

/** One side of the pair: the ticker with a caret; the native drop-down lies invisibly over it (keyboard, screen readers and `select` semantics intact). */
function AssetSelect(props: { value: string; choices: AssetChoice[]; label: string; onChange: (v: string) => void; 'data-testid': string }) {
  const cur = props.choices.find((c) => c.value === props.value);
  const unknown = props.value === KAS ? 'KAS' : props.value.slice(0, 8);
  const options = cur ? props.choices : [{ value: props.value, label: unknown, short: unknown }, ...props.choices];
  const face = (cur ?? options[0]!).short;
  return (
    <span class="asset-select" data-value={props.value}>
      <span class="asset-select-face" aria-hidden="true">
        {face}
        <i class="asset-select-caret">{'▾'}</i>
      </span>
      <select class="asset-select-native" aria-label={props.label} title={props.label} value={props.value} data-testid={props['data-testid']} onChange={(e) => props.onChange((e.currentTarget as HTMLSelectElement).value)}>
        {options.map((o) => (
          <option key={o.value} value={o.value}>
            {o.label}
          </option>
        ))}
      </select>
    </span>
  );
}

/** `BASE / QUOTE`, both sides selectable. `onChange` gets the new displayed pair (market-select.ts decides where it leads). */
export function PairSelector(props: { pair: DisplayedPair; rows: readonly TokenRow[]; onChange: (next: DisplayedPair) => void }) {
  const rows = props.rows.map(choiceRow);
  const label = (r: (typeof rows)[number]) => tokenTitle(r.row);
  const tickerOf = (id: string): string => (id === KAS ? 'KAS' : (props.rows.find((r) => r.covenantId.toLowerCase() === id)?.ticker ?? id.slice(0, 8)));
  const { left, right } = props.pair;
  return (
    <span class="pair-name pair-select" data-testid="market-pair" data-pair={`${tickerOf(left)}/${tickerOf(right)}`} data-left={left} data-right={right} role="group" aria-label={t('market.pair.label')}>
      <AssetSelect value={left} choices={assetChoices(rows, right, label)} label={t('market.pair.base')} onChange={(v) => props.onChange({ left: v, right })} data-testid="market-base-select" />
      <span class="pair-sep" aria-hidden="true">{'/'}</span>
      <AssetSelect value={right} choices={assetChoices(rows, left, label)} label={t('market.pair.quote')} onChange={(v) => props.onChange({ left, right: v })} data-testid="market-quote-select" />
    </span>
  );
}

/**
 * The flip control, an icon-only button placed just left of the pair title: shows the market the other way round (a display flip for a token's KAS market,
 * a base/quote swap for a token pair). No visible text: the tooltip and the accessible name say "Flip pair" (`data-flip-to` names the pair it leads to).
 */
export function FlipButton(props: { pair: string; pressed?: boolean; onClick: () => void }) {
  return (
    <Button
      small
      variant="ghost"
      class="flip-btn"
      onClick={props.onClick}
      data-testid="market-flip"
      data-flip-to={props.pair}
      aria-pressed={props.pressed ? 'true' : 'false'}
      aria-label={t('market.flip')}
      title={t('market.flip')}
    >
      <svg class="flip-icon" viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true" focusable="false">
        <path d="M4 8h15" />
        <path d="M15 4l4 4-4 4" />
        <path d="M20 16H5" />
        <path d="M9 12l-4 4 4 4" />
      </svg>
    </Button>
  );
}

/**
 * The layout every market has: title bar (no "all tokens" line: the nav bar leads to the list), notices, 24 h strip, then the three columns (order book | chart, depth and trades | ticket) and the facts below.
 * A slot a market has no data for stays empty; nothing moves.
 */
export function MarketShell(props: {
  'data-testid': string;
  attrs?: Record<string, string>;
  header: ComponentChildren;
  notices?: ComponentChildren;
  stats?: ComponentChildren;
  book: ComponentChildren;
  chart: ComponentChildren;
  depth?: ComponentChildren;
  tape?: ComponentChildren;
  ticket: ComponentChildren;
  details?: ComponentChildren;
}) {
  return (
    <div class="market-page" data-testid={props['data-testid']} {...props.attrs}>
      {props.header}
      {props.notices}
      {props.stats}
      <div class="mkt-grid">
        {props.book}
        {props.chart}
        {props.depth || props.tape ? (
          <div class="mkt-lower">
            {props.depth}
            {props.tape}
          </div>
        ) : null}
        {props.ticket}
      </div>
      {props.details}
    </div>
  );
}
