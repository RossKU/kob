// A token's round icon: its picture, else a letter on a colour taken from its covenant id. Where
// the picture comes from is decided in use-token-logo.ts. The sources are:
// - kascov.io's checked copy.
// - If the viewer allows creator sites, the creator's own address. It is compared with the
//   `image_hash` of the creation record when it has one, else labelled unchecked. The letter and
//   a note show when the picture at that address is not the committed file.
// - Nothing for a token whose logo a report hid.
// - Nothing for a logo wallets voted down while the viewer hides such logos (the default). The
//   letter is a button that shows it once, titled "Hidden by votes".
// The picture is fetched only while the row is on screen and only when the viewer allows icons
// (Settings). A checked or IPFS picture is shown from a blob: URL. The creator's own https image
// is a plain <img> (CSP img-src https:), with no referrer, lazy, in the icon's fixed box.
import { useEffect, useRef, useState } from 'preact/hooks';
import { logoGate } from '../../data/logo-gate';
import { logoVotes } from '../../data/logo-votes';
import { t } from '../../i18n';
import type { TokenRow } from './token-model';
import { useTokenLogo } from './use-token-logo';

/**
 * The letter of a letter avatar: the first letter or digit of the ticker, else of the covenant id.
 */
export function avatarLetter(ticker: string, covenantId: string): string {
  for (const c of ticker) if (/[\p{L}\p{N}]/u.test(c)) return c.toUpperCase();
  return covenantId.slice(0, 1).toUpperCase() || '?';
}

/** A hue from 0 to 359 from the first bytes of the covenant id, so a token keeps its colour. */
export const avatarHue = (covenantId: string): number => (parseInt(covenantId.slice(0, 4), 16) || 0) % 360;

/**
 * The icon of a token. `size` (px) is the diameter when the icon is not sized by its container
 * (`.idb`, `.ml-name`). The default is 24 px, and the letter follows it.
 */
export function TokenIcon(props: { row: Pick<TokenRow, 'covenantId' | 'ticker' | 'kascov'>; size?: number; /** a repeated icon (the rows of a list) carries no test id of its own */ anonymous?: boolean; /** a view of the wallet's own tokens: a creator-hosted picture waits for the user's "Show logo" unless the token was acquired (data/logo-gate.ts) */ scope?: 'wallet' }) {
  const root = useRef<HTMLSpanElement>(null);
  const [seen, setSeen] = useState(() => typeof IntersectionObserver === 'undefined');
  const [loaded, setLoaded] = useState<string | null>(null);
  const { covenantId, ticker } = props.row;
  const size = props.size;
  const { logo, failed } = useTokenLogo(props.row, seen, props.scope);
  const withheld = logo.state === 'withheld';
  const voted = logo.state === 'voted';

  useEffect(() => {
    const el = root.current;
    if (seen || !el || typeof IntersectionObserver === 'undefined') return;
    const io = new IntersectionObserver((entries) => {
      if (entries.some((e) => e.isIntersecting)) {
        setSeen(true);
        io.disconnect();
      }
    });
    io.observe(el);
    return () => io.disconnect();
  }, [seen]);

  const src = logo.state === 'checked' || logo.state === 'ipfs' || logo.state === 'verified' || logo.state === 'creator' ? logo.src : null;
  const px = size ?? 24;
  // The click shows this token's logo and remembers it. The row under the icon does not see it.
  const show = (e: Event) => {
    e.preventDefault();
    e.stopPropagation();
    logoGate.show(covenantId);
  };
  // A logo hidden by votes is shown for the rest of this visit only.
  const showOnce = (e: Event) => {
    e.preventDefault();
    e.stopPropagation();
    logoVotes.showOnce(covenantId);
  };
  return (
    <span
      ref={root}
      class="tk-icon"
      style={`--tk-h:${avatarHue(covenantId)}${size ? `;--tk-size:${size}px` : ''}`}
      {...(withheld || voted ? {} : { 'aria-hidden': 'true' })}
      data-testid={props.anonymous ? undefined : `token-icon-${covenantId}`}
      data-icon={withheld ? 'withheld' : logo.state === 'checked' || logo.state === 'ipfs' || logo.state === 'verified' ? 'image' : logo.state === 'creator' ? 'creator-image' : 'letter'}
      {...(logo.state === 'creator' ? { title: t('market.logo.unchecked') } : {})}
      {...(logo.state === 'withheld' ? { role: 'button', tabIndex: 0, title: t('market.logo.show.title', { host: logo.host }), 'aria-label': t('market.logo.show'), onClick: show, onKeyDown: (e: KeyboardEvent) => (e.key === 'Enter' || e.key === ' ') && show(e) } : {})}
      {...(voted ? { role: 'button', tabIndex: 0, title: t('market.logo.voted.title'), 'aria-label': t('market.logo.voted.show'), 'data-logo-note': 'voted', onClick: showOnce, onKeyDown: (e: KeyboardEvent) => (e.key === 'Enter' || e.key === ' ') && showOnce(e) } : {})}
      {...(logo.state === 'verified' ? { title: t('market.logo.verified.short') } : {})}
      {...(logo.state === 'mismatch' ? { title: t('market.logo.mismatch'), 'data-logo-note': 'mismatch' } : {})}
    >
      {/* the letter stays under the picture until it has loaded: no empty circle, no jump */}
      {src === null || logo.state === 'creator' ? avatarLetter(ticker, covenantId) : null}
      {src ? (
        logo.state === 'creator' ? (
          <img
            src={src}
            class={loaded === src ? 'tk-creator tk-loaded' : 'tk-creator'}
            alt=""
            width={px}
            height={px}
            decoding="async"
            loading="lazy"
            referrerpolicy="no-referrer"
            onLoad={() => setLoaded(src)}
            onError={failed}
          />
        ) : (
          <img src={src} alt="" width={px} height={px} decoding="async" referrerpolicy="no-referrer" />
        )
      ) : null}
    </span>
  );
}
