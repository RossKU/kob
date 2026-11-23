// Which picture a token shows and where it comes from (data/logos.ts has the rules). The order is:
// 1. The viewer turned icons off, or the token is on the blocklist because a report hid its logo.
//    There is no picture and no request.
// 1b. Wallets voted the logo down (data/logo-votes.ts: at least 3 voters, net -1 or lower) and the
//    viewer keeps the default "Show logos at 0 and above" (Settings). The letter shows with a "Hidden
//    by votes" note, and the viewer may show it once. There is no request until then.
// 2. kascov.io's saved copy, when its bytes hash to what the creator committed to ('checked').
// 3. Only if the viewer allows creator sites (Settings): the picture address of the token's
//    KCC-23 declaration (`image`, or `logo`), then the one kascov.io lists for it. When the
//    declaration carries an `image_hash`, the SHA-256 of the file, the bytes are compared with it
//    first.
//    - `ipfs://` is fetched as bytes through the gateway. Equal gives 'verified' and a different
//      file gives 'mismatch'. With no hash it gives 'ipfs' only when the bytes match the CID;
//      otherwise the next address is tried, since unchecked bytes are not shown.
//    - `https://` is read in CORS mode. Equal gives 'verified' (a blob) and different gives
//      'mismatch'. Not readable, which is the usual case with no CORS header, or no hash gives a
//      plain <img> the page cannot read or check ('creator').
//    In a wallet view (`scope: 'wallet'`), and for a token the connected wallet holds wherever it
//    shows, an https address loads only for a token the user acquired or chose to show
//    (data/logo-gate.ts). Otherwise the state is 'withheld'. While the wallet's holdings are
//    being read, it waits ('pending').
// 4. None: the letter avatar. For 'mismatch' it comes with the note that the picture at the
//    creator's address is not the one the creation record names.
import { useContext, useEffect, useMemo, useState } from 'preact/hooks';
import { useServices, WalletContext } from '../../app/context';
import { useStandalone } from '../shell/standalone';
import { useCreatorImagePref, useIconPref, useVoteFilterPref } from '../../app/icon-pref';
import { useDeclarationsVersion } from '../../app/use-declaration';
import { logoBlocklist, logoBlocklistUrlOf } from '../../data/logo-blocklist';
import { hiddenForViewer, logoVotes, logoVotesUrlOf, type LogoTotals, type VoteDir, type VoteVerdict } from '../../data/logo-votes';
import { iconLoader, kascovNetwork } from '../../data/kascov';
import { asImageHash, httpsChecker, ipfsLoader, parseLogoRef, type HttpsOutcome, type LogoRef } from '../../data/logos';
import { logoScope, useLogoAllowed, useLogoHolding, withholdsLogo } from '../../data/logo-gate';
import { declaredToken } from '../../data/token-declarations';

export type TokenLogo =
  | { state: 'off' | 'hidden' | 'pending' | 'none' }
  /**
   * Wallets voted the logo down and the viewer hides such logos (the default). Nothing is
   * requested; the letter shows with a "Hidden by votes" note and a "Show once" control.
   */
  | { state: 'voted' }
  /**
   * A wallet view, with a creator-hosted https logo of a token the user did not acquire. Nothing is
   * requested, and the letter shows with a "Show logo" control.
   */
  | { state: 'withheld'; host: string }
  /**
   * `checked` is bytes that hash to the creator's committed hash (kascov.io's copy). `ipfs` is
   * bytes from the gateway that match the CID.
   */
  | { state: 'checked' | 'ipfs'; src: string }
  /**
   * Bytes from the creator's address that hash to the `image_hash` of the token's creation record.
   */
  | { state: 'verified'; src: string }
  /**
   * The picture at the creator's address is not the file the creation record's `image_hash` names.
   * The letter is shown, with a note.
   */
  | { state: 'mismatch'; host: string }
  /**
   * An <img> straight from the creator's site. It is not checked, and that site sees the viewer's
   * IP address.
   */
  | { state: 'creator'; src: string; host: string };

export interface LogoSubject {
  /** A row's source. A `held` row is gated as a wallet view (data/logo-gate.ts `logoScope`). */
  source?: string;
  covenantId: string;
  kascov?: { imageHash: string | null; imageUrl: string | null } | null | undefined;
}

/** Whether a report hid this token's logo. The component re-renders when the blocklist changes. */
export function useLogoHidden(covenantId: string): boolean {
  const [, setN] = useState(0);
  useEffect(() => logoBlocklist.subscribe(() => setN((n) => n + 1)), []);
  return logoBlocklist.isHidden(covenantId);
}

/** The URLs of the vote totals the app already reads (one timer each, for the whole visit). */
const votesFeeds = new Set<string>();

/**
 * What the votes say of a token's logo. It starts reading the totals and re-renders when they change.
 * In standalone mode the indexer is not asked: `url` is null (no vote can be cast) and the reads
 * pause, while the totals kept from before still apply.
 */
export function useLogoVotes(covenantId: string): { verdict: VoteVerdict; totals: LogoTotals | undefined; shownOnce: boolean; own: VoteDir | undefined; url: string | null } {
  const { config, standalone } = useServices();
  const standaloneNow = useStandalone(standalone)?.active === true;
  const feedUrl = logoVotesUrlOf(logoBlocklistUrlOf(config));
  const [, setN] = useState(0);
  useEffect(() => logoVotes.subscribe(() => setN((n) => n + 1)), []);
  useEffect(() => {
    if (!feedUrl || votesFeeds.has(feedUrl)) return;
    votesFeeds.add(feedUrl);
    logoVotes.start(feedUrl, { paused: () => standalone?.state.active === true });
  }, [feedUrl, standalone]);
  const url = standaloneNow ? null : feedUrl;
  return { verdict: logoVotes.verdict(covenantId), totals: logoVotes.totals(covenantId), shownOnce: logoVotes.shownOnce(covenantId), own: logoVotes.ownVote(covenantId), url };
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
 * The logo of a token. `active` means the icon is on screen, and nothing is fetched before. It
 * returns the state to draw and `failed()` for an <img> that did not load. The next candidate
 * address is then tried, then the letter.
 */
export function useTokenLogo(row: LogoSubject, active: boolean, scope?: 'wallet'): { logo: TokenLogo; failed: () => void } {
  const { config } = useServices();
  const [iconsOn] = useIconPref();
  const [creatorOn] = useCreatorImagePref();
  useDeclarationsVersion();
  const hidden = useLogoHidden(row.covenantId);
  const [hideVoted] = useVoteFilterPref();
  const votes = useLogoVotes(row.covenantId);
  const votedOut = hiddenForViewer(votes.verdict, hideVoted, votes.shownOnce);
  const allowed = useLogoAllowed(row.covenantId);
  const wallet = useContext(WalletContext);
  const holding = useLogoHolding(wallet && !wallet.networkMismatch ? (wallet.info?.pubkey ?? null) : null, row.covenantId);
  const { covenantId } = row;
  const net = kascovNetwork(config.network);
  const base = config.tokenNamesUrl;
  const hash = row.kascov?.imageHash ?? null;
  const decl = declaredToken(config.network, covenantId);
  const declared = decl?.image ?? null;
  // The creator's commitment to the file is the declaration's `image_hash`. A token without one is
  // shown unchecked.
  const committed = asImageHash(decl?.imageHash);
  const claimed = row.kascov?.imageUrl ?? null;
  const blocked = !iconsOn || hidden || votedOut;

  // 2. kascov.io's checked copy.
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

  // 3. The creator's address: the declaration's first, then the one kascov.io lists.
  const candidates = useMemo(() => {
    const out: LogoRef[] = [];
    for (const raw of [declared, claimed]) {
      const r = parseLogoRef(raw);
      // An https SVG is shown only from checked bytes. Without a committed hash it is skipped.
      if (r && r.kind === 'https' && r.svg && committed === null) continue;
      if (r && !out.some((x) => refKey(x) === refKey(r))) out.push(r);
    }
    return out;
  }, [declared, claimed, committed]);
  const candKey = `${covenantId}|${candidates.map(refKey).join('|')}|${committed ?? ''}`;
  const [tried, setTried] = useState<{ key: string; n: number }>({ key: candKey, n: 0 });
  const n = tried.key === candKey ? tried.n : 0;
  const next = () => setTried({ key: candKey, n: n + 1 });
  const advance = () => setTried((t) => ({ key: candKey, n: (t.key === candKey ? t.n : 0) + 1 }));
  const next0 = !blocked && active && creatorOn && checkedHere !== null && checkedHere.src === null ? candidates[n] : undefined;
  // A wallet view, or any view of a token the wallet holds, asks a creator's host nothing for a
  // token the user did not acquire. Until the holdings answer, the token may be one of them.
  const gated = logoScope(scope, row.source, holding);
  const withheld = next0 && withholdsLogo(gated, allowed, next0.kind) ? next0 : null;
  const waits = !withheld && next0?.kind === 'https' && !allowed && holding === 'pending';
  const cur = withheld || waits ? undefined : next0;

  // 3a. ipfs:// through the gateway, compared with the committed hash when there is one.
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

  // 3b. https:// with a committed hash: read the bytes and compare. Unreadable falls back to the
  // unchecked <img>.
  const httpsKey = cur?.kind === 'https' && committed !== null ? `${cur.url}|${committed}` : null;
  const [https, setHttps] = useState<{ key: string; out: HttpsOutcome } | null>(null);
  useEffect(() => {
    if (!httpsKey || cur?.kind !== 'https' || committed === null) return;
    let live = true;
    void httpsChecker.check(cur.url, committed).then((out) => {
      if (!live) return;
      setHttps({ key: httpsKey, out });
      // An SVG that cannot be read is never shown unchecked.
      if (out.kind === 'refused' || (out.kind === 'unreadable' && cur.svg)) advance();
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
  else if (votedOut) logo = { state: 'voted' };
  else if (checkedHere?.src) logo = { state: 'checked', src: checkedHere.src };
  else if (!active || checkedHere === null) logo = { state: 'pending' };
  else if (!creatorOn) logo = { state: 'none' };
  else if (withheld) logo = { state: 'withheld', host: hostOf(withheld, gateway) };
  else if (waits) logo = { state: 'pending' };
  else if (!cur) logo = { state: 'none' };
  else if (cur.kind === 'https') {
    if (cur.svg && (httpsKey === null || httpsHere?.kind === 'unreadable')) logo = { state: 'none' };
    else if (httpsKey === null || httpsHere?.kind === 'unreadable') logo = { state: 'creator', src: cur.url, host: hostOf(cur, gateway) };
    else if (httpsHere === null) logo = { state: 'pending' };
    else if (httpsHere.kind === 'match') logo = { state: 'verified', src: httpsHere.src };
    else if (httpsHere.kind === 'mismatch') logo = { state: 'mismatch', host: hostOf(cur, gateway) };
    else logo = { state: 'none' };
  } else if (ipfsHere?.src) logo = committed !== null ? { state: 'verified', src: ipfsHere.src } : { state: 'ipfs', src: ipfsHere.src };
  else if (ipfsHere?.mismatch) logo = { state: 'mismatch', host: hostOf(cur, gateway) };
  else logo = ipfsHere ? { state: 'none' } : { state: 'pending' };
  return { logo, failed: next };
}
