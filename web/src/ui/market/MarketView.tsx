import { TokenList } from './TokenList';
import type { TicketPreset } from '../../app/router';
import { PairPage } from './PairPage';
import { TokenPage } from './TokenPage';

/**
 * `#/market` (token list), `#/market/<covenantId>` (a token's KAS market) and `#/market/<base>/<quote>` (a token pair). The two markets share one layout
 * and one title bar (MarketBar.tsx): choosing the other asset of the pair in the title bar only changes the data in the same places.
 */
export function MarketView(props: { covenantId?: string; pair?: { base: string; quote: string }; invalidToken?: string; ticket?: TicketPreset; amount?: string }) {
  if (props.pair) return <PairPage key={`${props.pair.base}/${props.pair.quote}`} base={props.pair.base} quote={props.pair.quote} />;
  return props.covenantId ? (
    <TokenPage key={props.covenantId} covenantId={props.covenantId} {...(props.ticket ? { ticket: props.ticket } : {})} {...(props.amount ? { amount: props.amount } : {})} />
  ) : (
    <TokenList invalidToken={props.invalidToken} />
  );
}
