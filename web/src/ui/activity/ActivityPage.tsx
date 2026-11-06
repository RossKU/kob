// `#/activity`: the activity log of every market, full width (kaspa.stream's transaction list, for KOB). The market filter lives in the URL
// (`?market=<covenant id>` or `?market=<base>/<quote>`), so a link from a token or pair page and this page show the same thing.
import { useState } from 'preact/hooks';
import { navigate, activityRoute, type ActivityMarket } from '../../app/router';
import type { PairFillsView } from '../../data/indexer-types';
import type { Hex } from '../../kob/types';
import { marketKeyOf, type MarketFilter, type MarketRef } from './activity-model';
import { ActivityLog } from './ActivityLog';

/** The log on a token or pair page starts shorter (the page has more below it); "Load older" adds as many. */
const MARKET_PAGE_ROWS = 20;

const filterOf = (m: ActivityMarket | undefined): MarketFilter => (!m ? { kind: 'all' } : m.kind === 'token' ? { kind: 'token', token: m.token } : { kind: 'pair', base: m.base, quote: m.quote });

export function ActivityPage(props: { market?: ActivityMarket }) {
  return (
    <div class="act-page" data-testid="activity-page">
      <ActivityLog
        market={filterOf(props.market)}
        // the filter is the page's address: a change is a new history entry (back returns to the previous filter)
        onMarketChange={(m) => navigate(activityRoute(m.kind === 'all' ? null : m))}
        prefsScope="page"
        data-testid="activity"
      />
    </div>
  );
}

/**
 * The log on a token or pair page: the same component, its market filter preset to the page's market. The viewer may widen it (all markets, another
 * token) for this visit; "All activity →" opens the Activity page with the filter shown.
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
