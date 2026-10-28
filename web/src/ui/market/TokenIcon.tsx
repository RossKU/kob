// A token's round icon in the market list: its picture when the bytes of the picture hash to what its creator committed (data/kascov.ts), else a
// letter on a colour taken from its covenant id. The picture is fetched only while the row is on screen, only when the viewer allows third-party icons
// (Settings), and shown from a blob: URL made from the checked bytes, never from the host's URL.
import { useEffect, useRef, useState } from 'preact/hooks';
import { useServices } from '../../app/context';
import { useIconPref } from '../../app/icon-pref';
import { iconLoader, kascovNetwork } from '../../data/kascov';
import type { TokenRow } from './token-model';

/** The letter of a letter avatar: the first letter or digit of the ticker, else of the covenant id. */
export function avatarLetter(ticker: string, covenantId: string): string {
  for (const c of ticker) if (/[\p{L}\p{N}]/u.test(c)) return c.toUpperCase();
  return covenantId.slice(0, 1).toUpperCase() || '?';
}

/** A hue 0..359 from the first bytes of the covenant id: a token keeps its colour. */
export const avatarHue = (covenantId: string): number => (parseInt(covenantId.slice(0, 4), 16) || 0) % 360;

/** `size` (px) is the diameter when the icon is not sized by its container (`.idb`, `.ml-name`); the default is 24 px; the letter follows it. */
export function TokenIcon(props: { row: Pick<TokenRow, 'covenantId' | 'ticker' | 'kascov'>; size?: number }) {
  const { config } = useServices();
  const [iconsOn] = useIconPref();
  const root = useRef<HTMLSpanElement>(null);
  const [seen, setSeen] = useState(() => typeof IntersectionObserver === 'undefined');
  const [src, setSrc] = useState<string | null>(null);
  const { covenantId, ticker, kascov } = props.row;
  const size = props.size;
  const net = kascovNetwork(config.network);
  const base = config.tokenNamesUrl;
  const hash = kascov?.imageHash ?? null;
  const url = kascov?.imageUrl ?? null;

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

  useEffect(() => {
    setSrc(null);
    if (!iconsOn || !seen || !net || !hash || base === '') return;
    const ctl = new AbortController();
    void iconLoader.load(net, { covenantId, imageHash: hash, imageUrl: url }, { signal: ctl.signal, base }).then((u) => {
      if (!ctl.signal.aborted) setSrc(u);
    });
    return () => ctl.abort();
  }, [iconsOn, seen, net, hash, url, covenantId, base]);

  return (
    <span ref={root} class="tk-icon" style={`--tk-h:${avatarHue(covenantId)}${size ? `;--tk-size:${size}px` : ''}`} aria-hidden="true" data-testid={`token-icon-${covenantId}`} data-icon={src ? 'image' : 'letter'}>
      {src ? <img src={src} alt="" decoding="async" referrerpolicy="no-referrer" /> : avatarLetter(ticker, covenantId)}
    </span>
  );
}
