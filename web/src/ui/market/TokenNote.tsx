// The small muted marker next to the name of a token the registry does not list (app/token-display.ts): "unverified" when its name and decimals come from this
// browser's issuance of it, "decimals assumed 8" when nothing states them; "facts disagree" when its registry entry and its declaration differ. The tooltip says why.
// Nothing for an agreeing registry token, and nothing for a token that only rests on its KCC-23 declaration ("declared, bound by ancestry"): that is the normal
// case of a token the registry does not list, so it shows no status; its explanation is in the token's details (TokenHeader.tsx `TokenStatusDetails`).
import { decimalsNote } from '../../app/token-display';
import type { TokenInfo } from '../../kob/registry';

export function TokenNote(props: { token: Pick<TokenInfo, 'decimalsSource' | 'decimals' | 'declarationConflicts'> | null | undefined }) {
  if (props.token && props.token.decimalsSource === 'declared' && !props.token.declarationConflicts?.length) return null;
  const n = decimalsNote(props.token);
  if (!n) return null;
  return (
    <span class="small muted token-note" title={n.title} data-testid="token-decimals-note" data-source={props.token!.declarationConflicts?.length ? 'conflict' : props.token!.decimalsSource}>
      {' '}
      <span key={n.text}>({n.text})</span>
    </span>
  );
}
