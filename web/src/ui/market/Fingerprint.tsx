// The picture of a covenant id (app/fingerprint.ts) and the identity block of a token: its icon beside that picture, so a token is recognised at a
// glance and two tokens with one ticker cannot be mistaken. The picture is inline SVG built from the cells (no image to fetch, nothing to trust); its
// tooltip and accessible name are the three words.
import { fingerprint } from '../../app/fingerprint';
import { t } from '../../i18n';
import { TokenIcon } from './TokenIcon';
import type { TokenRow } from './token-model';

/** Tile of the spec: 100 x 132, 5 x 7 cells of 16 inside a padding of 10, dots of radius 6.4 (accent 7.36), corner radius 16. */
const W = 100;
const H = 132;
const PAD = 10;
const CELL = 16;

/** The fingerprint tile; `height` is its height in px (the width follows the 100 : 132 tile). */
export function Fingerprint(props: { covenantId: string; height?: number; 'data-testid'?: string }) {
  const f = fingerprint(props.covenantId);
  const height = props.height ?? 40;
  const label = t('market.fingerprint.label', { words: f.wordsText });
  return (
    <svg
      class="fp"
      viewBox={`0 0 ${W} ${H}`}
      width={(height * W) / H}
      height={height}
      role="img"
      aria-label={label}
      data-words={f.wordsText}
      data-testid={props['data-testid'] ?? `fingerprint-${props.covenantId}`}
    >
      <title>{label}</title>
      <rect width={W} height={H} rx={16} fill={f.background} />
      {f.cells.flatMap((row, y) =>
        row.map((v, x) =>
          v === 0 ? null : <circle key={`${x}-${y}`} cx={PAD + x * CELL + CELL / 2} cy={PAD + y * CELL + CELL / 2} r={v === 1 ? 0.4 * CELL : 0.46 * CELL} fill={v === 1 ? f.primary : f.accent} />,
        ),
      )}
    </svg>
  );
}

/** Icon and fingerprint side by side, both about 40 px tall (`md`: list rows) or 64 px (`lg`: token and pair headers); the sizes are in market.css, which also narrows them on a phone. */
export function IdentityBlock(props: { row: Pick<TokenRow, 'covenantId' | 'ticker' | 'kascov'>; size?: 'md' | 'lg'; 'data-testid'?: string }) {
  const large = props.size === 'lg';
  return (
    <span class={`idb idb-${large ? 'lg' : 'md'}`} data-testid={props['data-testid'] ?? `identity-${props.row.covenantId}`}>
      <TokenIcon row={props.row} />
      <Fingerprint covenantId={props.row.covenantId} height={large ? 64 : 40} />
    </span>
  );
}
