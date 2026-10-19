// The small muted marker next to the name of a token the registry does not list (app/token-display.ts): "declared on chain" when its name,
// ticker and decimals come from its KCC-23 declaration, "unverified" when they come from this browser's issuance of it, "decimals assumed 8"
// when nothing states them; "facts disagree" when its registry entry and its declaration differ. The tooltip says why. Nothing for an agreeing
// registry token.
import { decimalsNote } from '../../app/token-display';
import type { TokenInfo } from '../../kob/registry';

export function TokenNote(props: { token: Pick<TokenInfo, 'decimalsSource' | 'decimals' | 'declarationConflicts'> | null | undefined }) {
  const n = decimalsNote(props.token);
  if (!n) return null;
  return (
    <span class="small muted token-note" title={n.title} data-testid="token-decimals-note" data-source={props.token!.declarationConflicts?.length ? 'conflict' : props.token!.decimalsSource}>
      {' '}
      <span key={n.text}>({n.text})</span>
    </span>
  );
}
