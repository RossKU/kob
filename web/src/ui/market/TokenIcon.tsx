// A token's round icon: its picture, else a letter on a colour taken from its covenant id. Where the picture comes from is decided in use-token-logo.ts
// (kascov.io's checked copy; else, if the viewer allows creator sites, the creator's own address, labelled unchecked; nothing for a token whose logo a report
// hid). It is fetched only while the row is on screen and only when the viewer allows icons (Settings). A checked or IPFS picture is shown from a blob: URL;
// the creator's own https image is a plain <img> (CSP img-src https:), no referrer, lazy, in the icon's fixed box.
import { useEffect, useRef, useState } from 'preact/hooks';
import { t } from '../../i18n';
import type { TokenRow } from './token-model';
import { useTokenLogo } from './use-token-logo';

/** The letter of a letter avatar: the first letter or digit of the ticker, else of the covenant id. */
export function avatarLetter(ticker: string, covenantId: string): string {
  for (const c of ticker) if (/[\p{L}\p{N}]/u.test(c)) return c.toUpperCase();
  return covenantId.slice(0, 1).toUpperCase() || '?';
}

/** A hue 0..359 from the first bytes of the covenant id: a token keeps its colour. */
export const avatarHue = (covenantId: string): number => (parseInt(covenantId.slice(0, 4), 16) || 0) % 360;

/** `size` (px) is the diameter when the icon is not sized by its container (`.idb`, `.ml-name`); the default is 24 px; the letter follows it. */
export function TokenIcon(props: { row: Pick<TokenRow, 'covenantId' | 'ticker' | 'kascov'>; size?: number }) {
  const root = useRef<HTMLSpanElement>(null);
  const [seen, setSeen] = useState(() => typeof IntersectionObserver === 'undefined');
  const [loaded, setLoaded] = useState<string | null>(null);
  const { covenantId, ticker } = props.row;
  const size = props.size;
  const { logo, failed } = useTokenLogo(props.row, seen);

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

  const src = logo.state === 'checked' || logo.state === 'ipfs' || logo.state === 'creator' ? logo.src : null;
  const px = size ?? 24;
  return (
    <span
      ref={root}
      class="tk-icon"
      style={`--tk-h:${avatarHue(covenantId)}${size ? `;--tk-size:${size}px` : ''}`}
      aria-hidden="true"
      data-testid={`token-icon-${covenantId}`}
      data-icon={logo.state === 'checked' || logo.state === 'ipfs' ? 'image' : logo.state === 'creator' ? 'creator-image' : 'letter'}
      {...(logo.state === 'creator' ? { title: t('market.logo.unchecked') } : {})}
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
