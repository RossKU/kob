// Which picture a token shows and where it comes from (data/logos.ts has the rules). The order:
//
//   1. the viewer turned icons off, or the token is on the blocklist (a report hid its logo): no picture, no request;
//   2. kascov.io's saved copy, when its bytes hash to what the creator committed to ('checked');
//   3. only if the viewer allows creator sites (Settings): the picture address of the token's KCC-23 declaration (`image`, or the older `logo`), then the one
//      kascov.io lists for it. When the declaration carries an `image_hash` (the SHA-256 of the file) the bytes are compared with it first:
//        - `ipfs://` fetched as bytes through the gateway: equal -> 'verified'; a different file -> 'mismatch'; no hash -> 'ipfs';
//        - `https://` read in CORS mode: equal -> 'verified' (a blob); different -> 'mismatch'; not readable (no CORS header, the usual case) or
//          no hash -> a plain <img> the page cannot read or check ('creator');
//   4. none: the letter avatar (with the note, for 'mismatch', that the picture at the creator's address is not the one the creation record names).
import { useEffect, useMemo, useState } from 'preact/hooks';
import { useServices } from '../../app/context';
import { useCreatorImagePref, useIconPref } from '../../app/icon-pref';
import { useDeclarationsVersion } from '../../app/use-declaration';
import { logoBlocklist } from '../../data/logo-blocklist';
import { iconLoader, kascovNetwork } from '../../data/kascov';
import { asImageHash, httpsChecker, ipfsLoader, parseLogoRef, type HttpsOutcome, type LogoRef } from '../../data/logos';
import { declaredToken } from '../../data/token-declarations';

export type TokenLogo =
  | { state: 'off' | 'hidden' | 'pending' | 'none' }
  /** `checked`: bytes that hash to the creator's committed hash (kascov.io's copy); `ipfs`: bytes from the gateway, compared with the CID where that is possible */
  | { state: 'checked' | 'ipfs'; src: string }
  /** bytes from the creator's address that hash to the `image_hash` of the token's creation record */
  | { state: 'verified'; src: string }
  /** the picture at the creator's address is NOT the file the creation record's `image_hash` names: the letter is shown, with a note */
  | { state: 'mismatch'; host: string }
  /** an <img> straight from the creator's site: not checked, that site sees the viewer's IP address */
  | { state: 'creator'; src: string; host: string };

export interface LogoSubject {
  covenantId: string;
  kascov?: { imageHash: string | null; imageUrl: string | null } | null | undefined;
}

/** Whether a report hid this token's logo; the component re-renders when the blocklist changes. */
export function useLogoHidden(covenantId: string): boolean {
  const [, setN] = useState(0);
  useEffect(() => logoBlocklist.subscribe(() => setN((n) => n + 1)), []);
  return logoBlocklist.isHidden(covenantId);
}

const refKey = (r: LogoRef): string => (r.kind === 'https' ? r.url : `ipfs://${r.cid}${r.path}`);
const hostOf = (r: LogoRef, gateway: string): string => {
  try {
    return r.kind === 'https' ? new URL(r.url).hostname : new URL(gateway).hostname;
  } catch {
    return '';
  }
};

/**
 * The logo of a token. `active`: the icon is on screen (nothing is fetched before). Returns the state to draw and `failed()` for an <img> that did not load
 * (the next candidate address is tried, then the letter).
 */
export function useTokenLogo(row: LogoSubject, active: boolean): { logo: TokenLogo; failed: () => void } {
  const { config } = useServices();
  const [iconsOn] = useIconPref();
  const [creatorOn] = useCreatorImagePref();
  useDeclarationsVersion();
  const hidden = useLogoHidden(row.covenantId);
  const { covenantId } = row;
  const net = kascovNetwork(config.network);
  const base = config.tokenNamesUrl;
  const hash = row.kascov?.imageHash ?? null;
  const decl = declaredToken(config.network, covenantId);
  const declared = decl?.image ?? null;
  // the creator's commitment to the file: the declaration's `image_hash` (a token without one is shown as before, unchecked)
  const committed = asImageHash(decl?.imageHash);
  const claimed = row.kascov?.imageUrl ?? null;
  const blocked = !iconsOn || hidden;

  // 2. kascov.io's checked copy
  const checkKey = `${net}|${covenantId}|${hash}|${base}`;
  const [checked, setChecked] = useState<{ key: string; src: string | null } | null>(null);
  useEffect(() => {
    if (blocked || !active) return;
    if (!net || !hash || base === '') {
      setChecked({ key: checkKey, src: null });
      return;
    }
    const ctl = new AbortController();
    void iconLoader.load(net, { covenantId, imageHash: hash, imageUrl: claimed }, { signal: ctl.signal, base }).then((u) => {
      if (!ctl.signal.aborted) setChecked({ key: checkKey, src: u });
    });
    return () => ctl.abort();
  }, [blocked, active, checkKey, net, hash, base, covenantId, claimed]);
  const checkedHere = checked?.key === checkKey ? checked : null;

  // 3. the creator's address: the declaration's first, then the one kascov.io lists
  const candidates = useMemo(() => {
    const out: LogoRef[] = [];
    for (const raw of [declared, claimed]) {
      const r = parseLogoRef(raw);
      if (r && !out.some((x) => refKey(x) === refKey(r))) out.push(r);
    }
    return out;
  }, [declared, claimed]);
  const candKey = `${covenantId}|${candidates.map(refKey).join('|')}|${committed ?? ''}`;
  const [tried, setTried] = useState<{ key: string; n: number }>({ key: candKey, n: 0 });
  const n = tried.key === candKey ? tried.n : 0;
  const next = () => setTried({ key: candKey, n: n + 1 });
  const advance = () => setTried((t) => ({ key: candKey, n: (t.key === candKey ? t.n : 0) + 1 }));
  const cur = !blocked && active && creatorOn && checkedHere !== null && checkedHere.src === null ? candidates[n] : undefined;

  // 3a. ipfs:// through the gateway (compared with the committed hash when there is one)
  const gateway = config.ipfsGatewayUrl;
  const ipfsKey = cur?.kind === 'ipfs' ? `${gateway}|${refKey(cur)}|${committed ?? ''}` : null;
  const [ipfs, setIpfs] = useState<{ key: string; src: string | null; mismatch: boolean } | null>(null);
  useEffect(() => {
    if (!ipfsKey || cur?.kind !== 'ipfs') return;
    let live = true;
    void ipfsLoader.load(gateway, cur, committed).then((u) => {
      if (!live) return;
      const mismatch = u === null && committed !== null && ipfsLoader.mismatched(gateway, cur, committed);
      setIpfs({ key: ipfsKey, src: u, mismatch });
      if (!u && !mismatch) advance();
    });
    return () => {
      live = false;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [ipfsKey]);
  const ipfsHere = ipfs !== null && ipfs.key === ipfsKey ? ipfs : null;

  // 3b. https:// with a committed hash: read the bytes and compare; unreadable falls back to the unchecked <img>
  const httpsKey = cur?.kind === 'https' && committed !== null ? `${cur.url}|${committed}` : null;
  const [https, setHttps] = useState<{ key: string; out: HttpsOutcome } | null>(null);
  useEffect(() => {
    if (!httpsKey || cur?.kind !== 'https' || committed === null) return;
    let live = true;
    void httpsChecker.check(cur.url, committed).then((out) => {
      if (!live) return;
      setHttps({ key: httpsKey, out });
      if (out.kind === 'refused') advance();
    });
    return () => {
      live = false;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [httpsKey]);
  const httpsHere = https !== null && https.key === httpsKey ? https.out : null;

  let logo: TokenLogo;
  if (!iconsOn) logo = { state: 'off' };
  else if (hidden) logo = { state: 'hidden' };
  else if (checkedHere?.src) logo = { state: 'checked', src: checkedHere.src };
  else if (!active || checkedHere === null) logo = { state: 'pending' };
  else if (!creatorOn || !cur) logo = { state: 'none' };
  else if (cur.kind === 'https') {
    if (httpsKey === null || httpsHere?.kind === 'unreadable') logo = { state: 'creator', src: cur.url, host: hostOf(cur, gateway) };
    else if (httpsHere === null) logo = { state: 'pending' };
    else if (httpsHere.kind === 'match') logo = { state: 'verified', src: httpsHere.src };
    else if (httpsHere.kind === 'mismatch') logo = { state: 'mismatch', host: hostOf(cur, gateway) };
    else logo = { state: 'none' };
  } else if (ipfsHere?.src) logo = committed !== null ? { state: 'verified', src: ipfsHere.src } : { state: 'ipfs', src: ipfsHere.src };
  else if (ipfsHere?.mismatch) logo = { state: 'mismatch', host: hostOf(cur, gateway) };
  else logo = ipfsHere ? { state: 'none' } : { state: 'pending' };
  return { logo, failed: next };
}
