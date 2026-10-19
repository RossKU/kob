// The small muted marker next to the name of a token the registry does not list (app/token-display.ts): "unverified" when its name and decimals
// come from this browser's issuance of it, "decimals assumed 8" when nothing states them; the tooltip says why. Nothing for a registry token.
import { decimalsNote } from '../../app/token-display';
import type { TokenInfo } from '../../kob/registry';

export function TokenNote(props: { token: Pick<TokenInfo, 'decimalsSource' | 'decimals'> | null | undefined }) {
  const n = decimalsNote(props.token);
  if (!n) return null;
  return (
    <span class="small muted token-note" title={n.title} data-testid="token-decimals-note" data-source={props.token!.decimalsSource}>
      {' '}
      <span key={n.text}>({n.text})</span>
    </span>
  );
}
