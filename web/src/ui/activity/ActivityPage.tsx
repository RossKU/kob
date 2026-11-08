// `#/activity`: the activity log of every market, full width (kaspa.stream's transaction list, for KOB). The page has no market filter: an old link with
// `?market=...` shows every market too. The log on a token or pair page shows that market.
import type { PairFillsView } from '../../data/indexer-types';
import type { Hex } from '../../kob/types';
import { marketKeyOf, type MarketFilter, type MarketRef } from './activity-model';
import { ActivityLog } from './ActivityLog';
import { ActivitySummary } from './ActivitySummary';

/** The rows of a page of the log on a token or pair page until the viewer picks another size (the page has more below it). */
const MARKET_PAGE_ROWS = 20;

const EVERY_MARKET: MarketFilter = { kind: 'all' };

export function ActivityPage() {
  return (
    <div class="act-page" data-testid="activity-page">
      <ActivitySummary />
      <ActivityLog market={EVERY_MARKET} prefsScope="page" eager data-testid="activity" />
    </div>
  );
}

/** The log on a token or pair page: the same component, showing that market; "All activity →" opens the Activity page. Another market starts at its first page. */
export function MarketActivity(props: { market: MarketRef; pairFills?: { base: Hex; quote: Hex; view: PairFillsView | null | undefined } | null }) {
  return (
    <div class="mkt-area-activity">
      <ActivityLog key={marketKeyOf(props.market)} market={props.market} prefsScope="market" pairFills={props.pairFills ?? null} allLink pageRows={MARKET_PAGE_ROWS} data-testid="activity" />
    </div>
  );
}
