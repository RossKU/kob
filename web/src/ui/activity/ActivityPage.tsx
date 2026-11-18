// `#/activity`: the activity log of every market, full width, like a block explorer's transaction
// list. The page has no market filter, and a link with `?market=...` shows every market too. The
// log on a token or pair page shows that market.
import { FiatNote } from '../../app/use-fiat';
import type { PairFillsView } from '../../data/indexer-types';
import type { Hex } from '../../kob/types';
import { marketKeyOf, type MarketFilter, type MarketRef } from './activity-model';
import { ActivityLog } from './ActivityLog';
import { ActivitySummary } from './ActivitySummary';

/** The rows of a page of the log on a token or pair page until the viewer picks another size. */
const MARKET_PAGE_ROWS = 20;

const EVERY_MARKET: MarketFilter = { kind: 'all' };

export function ActivityPage() {
  return (
    <div class="act-page" data-testid="activity-page">
      {/* the USD display (Settings): what the dollars of the summary and the feed are, or why KAS is shown */}
      <FiatNote />
      <ActivitySummary />
      <ActivityLog market={EVERY_MARKET} prefsScope="page" eager data-testid="activity" />
    </div>
  );
}

/**
 * The log on a token or pair page. It is the same component, showing that market, and
 * "All activity →" opens the Activity page. Another market starts at its first page.
 */
export function MarketActivity(props: { market: MarketRef; pairFills?: { base: Hex; quote: Hex; view: PairFillsView | null | undefined } | null }) {
  return (
    <div class="mkt-area-activity">
      <ActivityLog key={marketKeyOf(props.market)} market={props.market} prefsScope="market" pairFills={props.pairFills ?? null} allLink pageRows={MARKET_PAGE_ROWS} data-testid="activity" />
    </div>
  );
}
