// `#/activity`: the activity log of every market, full width (kaspa.stream's transaction list, for KOB). The page has no market filter for now: an old
// link with `?market=...` shows every market too. The token and pair pages keep theirs.
import { useState } from 'preact/hooks';
import type { PairFillsView } from '../../data/indexer-types';
import type { Hex } from '../../kob/types';
import { marketKeyOf, type MarketFilter, type MarketRef } from './activity-model';
import { ActivityLog } from './ActivityLog';

/** The log on a token or pair page starts shorter (the page has more below it); "Load older" adds as many. */
const MARKET_PAGE_ROWS = 20;

const EVERY_MARKET: MarketFilter = { kind: 'all' };
const noMarketChange = (): void => {};

export function ActivityPage() {
  return (
    <div class="act-page" data-testid="activity-page">
      <ActivityLog market={EVERY_MARKET} onMarketChange={noMarketChange} prefsScope="page" data-testid="activity" />
    </div>
  );
}

/**
 * The log on a token or pair page: the same component, its market filter preset to the page's market. The viewer may widen it (all markets, another
 * token) for this visit; "All activity →" opens the Activity page.
 */
export function MarketActivity(props: { market: MarketRef; pairFills?: { base: Hex; quote: Hex; view: PairFillsView | null | undefined } | null }) {
  // another page (a flip of the pair, another token) starts with its own market again
  return <MarketActivityFor key={marketKeyOf(props.market)} {...props} />;
}

function MarketActivityFor(props: { market: MarketRef; pairFills?: { base: Hex; quote: Hex; view: PairFillsView | null | undefined } | null }) {
  const [filter, setFilter] = useState<MarketFilter>(props.market);
  return (
    <div class="mkt-area-activity">
      <ActivityLog market={filter} onMarketChange={setFilter} pageMarket={props.market} prefsScope="market" pairFills={props.pairFills ?? null} allLink pageRows={MARKET_PAGE_ROWS} data-testid="activity" />
    </div>
  );
}
