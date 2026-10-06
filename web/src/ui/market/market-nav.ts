// What choosing a pair in the title bar does: open the token's KAS market (remembering the orientation that was chosen) or the token pair. The URL changes,
// so the browser's back button returns to the previous pair.
import { navigate } from '../../app/router';
import { marketFor, type DisplayedPair } from './market-select';
import { marketKey, writeInverted } from './orientation';

/** Opens the market of `next`. Returns false when there is none (KAS/KAS). */
export function openPair(next: DisplayedPair): boolean {
  const m = marketFor(next);
  if (!m) return false;
  if (m.route.name === 'token' && m.inverted !== null) writeInverted(marketKey('token', m.route.covenantId), m.inverted);
  navigate(m.route);
  return true;
}
