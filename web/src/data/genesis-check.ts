// KOB's own genesis check of a token, run when its page opens (founder decision 2026-10-28).
//
// What it is: the bytes of the token's creation (its genesis group) come from a public data source, and the rules of `kob registry verify-genesis`
// run on them inside the app (kob-wasm `verifyGenesis`, crates/kob-protocol/src/genesis_open.rs): the covenant id is recomputed from the authorising
// outpoint and every output of the group, every redeem script is hashed against its output and decoded as an instance of a pinned token program, and
// an unspent TokenSeed is rebuilt from the group's own owner and class. The covenant id commits to all of it, so the source is only a supplier: a
// wrong, incomplete or tampered answer can make the check fail, never pass.
//
// Sources, in order, each asked at most once per token and session (an in-memory cache, plus a saved copy of the EVIDENCE of a token that verified,
// which is re-checked on every use and never trusted):
//   1. kascov.io's per-token file `<data>/<network>/c/<covenant id>.json` (CORS `*`): the genesis transaction id, the group's outputs with their
//      script public keys and revealed redeem scripts, and, for a token with a KCC-23 declaration, the authorising outpoint (the preparation output).
//   2. the Kaspa REST explorer (api.kaspa.org, api-tn10.kaspa.org; CORS `*`), only when 1 is not enough: the genesis transaction (its authorising
//      input, its outputs, the keys it pays to) and, for a group output a spend of which kascov names but did not give the script of, that spend.
// This needs kascov.io for the genesis transaction id (no source maps a covenant id to its genesis); without it the answer is "unavailable".
//
// Result: verified (KOB's own fact), failed (the data does not match, or the genesis is not clean: a warning), or unavailable (the check could not
// be done: never a statement about the token).
import type { GenesisRequest, GenesisVerdict } from '../kob/genesis';
import { familyOfProgram } from '../kob/token-state';
import type { TokenProgram } from '../kob/types';
import { readCapped, kascovNetwork, type KascovNetwork } from './kascov';

/** The public Kaspa REST explorer of each network. */
export const EXPLORER_BASE: Readonly<Record<KascovNetwork, string>> = { mainnet: 'https://api.kaspa.org', 'testnet-10': 'https://api-tn10.kaspa.org' };

export type GenesisUnavailableReason = 'history' | 'program' | 'unreachable';
export type GenesisSource = 'kascov' | 'explorer' | 'saved';

export type GenesisCheck =
  | {
      status: 'verified';
      /** where the bytes came from (the verification itself is always local) */
      source: GenesisSource;
      txid: string;
      program: string;
      /** token units the genesis created in holders */
      supply: bigint;
      /** the allowance of the genesis `PublicMint` outputs (the most anyone could mint beyond `supply` when the genesis was made); null without a public minter */
      mintAllowance: bigint | null;
      /** the genesis holds a KRON minter (`is_minter = 1`): it mints any amount, so there is no allowance to state (`mintAllowance` is null) */
      mintUnbounded?: boolean;
      /**
       * Every genesis public minter is still unspent according to the source, so no mint has used it: the allowance is all still there. False/null:
       * a mint may have used some (how much is left is not checked).
       */
      mintersUnspent: boolean | null;
    }
  | { status: 'failed'; code: 'mismatch' | 'contaminated'; message: string }
  | { status: 'unavailable'; reason: GenesisUnavailableReason };

// ------------------------------------------------------------------------------------------------ source data

/** One genesis output as a source gave it (not checked yet). */
interface SourceOutput {
  index: number;
  value: string;
  spk: string;
  redeem: string | null;
  /** the spend of it, when the source names one */
  spentTxid: string | null;
  /** unspent according to the source (null: not said) */
  live: boolean | null;
}

export interface KascovGenesis {
  genesisTxid: string;
  authorizing: { txid: string; index: number } | null;
  outputs: SourceOutput[];
  /** the file says its list of outputs or revealed scripts is incomplete */
  partial: boolean;
}

const HEX = /^[0-9a-f]*$/;
const HEX64 = /^[0-9a-f]{64}$/;
const isHex = (v: unknown, max = 100_000): v is string => typeof v === 'string' && v.length % 2 === 0 && v.length <= max && HEX.test(v);
const obj = (v: unknown): Record<string, unknown> | null => (v && typeof v === 'object' && !Array.isArray(v) ? (v as Record<string, unknown>) : null);
const sompi = (v: unknown): string | null => (typeof v === 'number' && Number.isSafeInteger(v) && v >= 0 ? String(v) : null);

/**
 * The genesis group out of kascov's per-covenant file; null when the file is not for this covenant on this network or has no usable genesis. Only the
 * outputs the genesis transaction itself created are taken (outpoint `<genesis txid>:<index>`).
 */
export function parseKascovGenesis(doc: unknown, covenantId: string, network: KascovNetwork): KascovGenesis | null {
  const d = obj(doc);
  if (!d || d.covenant_id !== covenantId || (d.network !== undefined && d.network !== network)) return null;
  const genesisTxid = d.genesis_txid;
  if (typeof genesisTxid !== 'string' || !HEX64.test(genesisTxid) || !Array.isArray(d.utxos)) return null;
  const outputs: SourceOutput[] = [];
  for (const raw of d.utxos) {
    const u = obj(raw);
    const m = typeof u?.outpoint === 'string' ? /^([0-9a-f]{64}):(\d{1,5})$/.exec(u.outpoint) : null;
    if (!u || !m || m[1] !== genesisTxid) continue;
    const value = sompi(u.value);
    if (value === null || !isHex(u.script_hex, 200)) return null;
    const spent = typeof u.spent_txid === 'string' && HEX64.test(u.spent_txid) ? u.spent_txid : null;
    outputs.push({
      index: Number(m[2]), value, spk: u.script_hex, redeem: isHex(u.revealed_hex) && u.revealed_hex !== '' ? u.revealed_hex : null,
      spentTxid: spent, live: typeof u.live === 'boolean' ? u.live : null,
    });
  }
  outputs.sort((a, b) => a.index - b.index);
  // (a busy token's file keeps only its newest outputs: the group may be missing here and then comes from the explorer)
  if (outputs.some((o, i) => i > 0 && outputs[i - 1]!.index === o.index)) return null;
  const k = obj(d.kcc23);
  const prep = k?.binding === 'verified' && typeof k.preparation_txid === 'string' && HEX64.test(k.preparation_txid) ? k.preparation_txid : null;
  const at = k?.output_index;
  return {
    genesisTxid,
    authorizing: prep !== null && typeof at === 'number' && Number.isInteger(at) && at >= 0 ? { txid: prep, index: at } : null,
    outputs,
    partial: d.utxos_truncated === true || (typeof d.reveal_bodies_omitted === 'number' && d.reveal_bodies_omitted > 0),
  };
}

export interface ExplorerGenesis {
  authorizing: { txid: string; index: number } | null;
  outputs: { index: number; value: string; spk: string; address: string | null }[];
  /** x-only keys of the transaction's P2PK outputs (the creator's change): candidate owners of an unspent TokenSeed */
  ownerHints: string[];
}

/** The genesis group out of the explorer's transaction JSON (`/transactions/<txid>?inputs=true&outputs=true`); null when it is not that transaction. */
export function parseExplorerGenesis(doc: unknown, txid: string, covenantId: string): ExplorerGenesis | null {
  const d = obj(doc);
  if (!d || d.transaction_id !== txid || !Array.isArray(d.outputs) || !Array.isArray(d.inputs)) return null;
  const outputs: ExplorerGenesis['outputs'] = [];
  const ownerHints: string[] = [];
  let auth: number | null | undefined;
  for (const raw of d.outputs) {
    const o = obj(raw);
    if (!o) return null;
    const spk = o.script_public_key;
    if (isHex(spk, 200) && /^20[0-9a-f]{64}ac$/.test(spk) && !ownerHints.includes(spk.slice(2, 66))) ownerHints.push(spk.slice(2, 66));
    if (o.covenant_id !== covenantId) continue;
    const value = sompi(o.amount);
    const index = o.index;
    if (value === null || !isHex(spk, 200) || typeof index !== 'number' || !Number.isInteger(index) || index < 0) return null;
    const a = typeof o.covenant_authorizing_input === 'number' ? o.covenant_authorizing_input : null;
    if (auth !== undefined && auth !== a) return null; // the group names different authorising inputs: not one genesis group
    auth = a;
    outputs.push({ index, value, spk, address: typeof o.script_public_key_address === 'string' && o.script_public_key_address.length <= 120 && /^[a-z0-9:]+$/.test(o.script_public_key_address) ? o.script_public_key_address : null });
  }
  outputs.sort((x, y) => x.index - y.index);
  let authorizing: ExplorerGenesis['authorizing'] = null;
  if (outputs.length > 0 && typeof auth === 'number') {
    const inp = obj(d.inputs.find((i) => obj(i)?.index === auth));
    const hash = inp?.previous_outpoint_hash;
    const idx = Number(inp?.previous_outpoint_index);
    if (typeof hash === 'string' && HEX64.test(hash) && Number.isInteger(idx) && idx >= 0) authorizing = { txid: hash, index: idx };
  }
  return { authorizing, outputs, ownerHints: ownerHints.slice(0, 8) };
}

/** The last data push of a signature script as hex (the redeem script of a P2SH spend); null when the script is not only pushes or has none. */
export function lastPush(sigHex: string): string | null {
  if (!isHex(sigHex, 400_000)) return null;
  const b = new Uint8Array(sigHex.length / 2);
  for (let i = 0; i < b.length; i++) b[i] = parseInt(sigHex.slice(2 * i, 2 * i + 2), 16);
  let i = 0;
  let last: Uint8Array | null = null;
  while (i < b.length) {
    const op = b[i++]!;
    let len: number;
    if (op === 0) {
      last = new Uint8Array(0);
      continue;
    } else if (op <= 0x4b) len = op;
    else if (op === 0x4c) len = b[i++] ?? -1;
    else if (op === 0x4d) {
      len = (b[i] ?? 0) | ((b[i + 1] ?? 0) << 8);
      i += 2;
    } else if (op === 0x4e) {
      len = ((b[i] ?? 0) | ((b[i + 1] ?? 0) << 8) | ((b[i + 2] ?? 0) << 16) | ((b[i + 3] ?? 0) << 24)) >>> 0;
      i += 4;
    } else if (op === 0x4f || (op >= 0x51 && op <= 0x60)) {
      last = new Uint8Array(0); // a small number: not a script
      continue;
    } else return null;
    if (len < 0 || i + len > b.length) return null;
    last = b.subarray(i, i + len);
    i += len;
  }
  return last && last.length > 0 ? [...last].map((x) => x.toString(16).padStart(2, '0')).join('') : null;
}

/** The redeem script that the spend `spender` (explorer JSON) of `<txid>:<index>` revealed; null when it is not that spend. */
export function revealFromSpend(doc: unknown, spender: string, txid: string, index: number): string | null {
  const d = obj(doc);
  if (!d || d.transaction_id !== spender || !Array.isArray(d.inputs)) return null;
  for (const raw of d.inputs) {
    const i = obj(raw);
    if (i && i.previous_outpoint_hash === txid && Number(i.previous_outpoint_index) === index && typeof i.signature_script === 'string') return lastPush(i.signature_script);
  }
  return null;
}

// ------------------------------------------------------------------------------------------------ the checker

export interface GenesisCheckerOptions {
  /** the facade over kob-wasm `verifyGenesis` (tests pass a stub or the real one) */
  verify: (req: GenesisRequest) => GenesisVerdict;
  fetch?: typeof fetch;
  now?: () => number;
  /** where the evidence of verified tokens is saved (default: this browser's localStorage); null: nowhere */
  storage?: Pick<Storage, 'getItem' | 'setItem'> | null;
  /** each request, headers to last byte (default 8 s) */
  timeoutMs?: number;
  /** how long an `unavailable` answer is kept before the same token is asked again (default 5 min) */
  retryMs?: number;
  /** the explorers (default `EXPLORER_BASE`) */
  explorer?: Readonly<Record<KascovNetwork, string>>;
}

export interface GenesisChecker {
  /**
   * The genesis check of `covenantId`, once per token per session. `kascovBase`: the data set's base (`AppConfig.tokenNamesUrl`; '' = no source).
   * `programHint`: a template hash a third party named for the token's program. Never rejects.
   */
  check(network: string, covenantId: string, o: { kascovBase: string; programHint?: string | null; signal?: AbortSignal | undefined }): Promise<GenesisCheck>;
  /** The answer already known for the token, without asking anything. */
  known(network: string, covenantId: string): GenesisCheck | null;
  clear(): void;
}

const STORE_KEY = 'kob.genesis-evidence.v1';
const STORE_MAX_ENTRIES = 24;
const STORE_MAX_ENTRY_CHARS = 150_000;
const KASCOV_FILE_MAX = 8_000_000;
const EXPLORER_MAX = 1_000_000;
/** most explorer requests spent on finding the spends that revealed a token's genesis scripts */
const MAX_SPEND_REQUESTS = 6;
const ADDRESS_PAGE = 50;

interface Saved {
  k: string;
  req: GenesisRequest;
}

export function createGenesisChecker(o: GenesisCheckerOptions): GenesisChecker {
  const fetchFn = o.fetch ?? ((...a: Parameters<typeof fetch>) => fetch(...a));
  const now = o.now ?? Date.now;
  const timeoutMs = o.timeoutMs ?? 8_000;
  const retryMs = o.retryMs ?? 300_000;
  const explorer = o.explorer ?? EXPLORER_BASE;
  const memory = new Map<string, { result: GenesisCheck; at: number }>();
  const inflight = new Map<string, Promise<GenesisCheck>>();

  const storage = (): Pick<Storage, 'getItem' | 'setItem'> | null => {
    if (o.storage !== undefined) return o.storage;
    try {
      return typeof localStorage !== 'undefined' ? localStorage : null;
    } catch {
      return null;
    }
  };
  const loadSaved = (): Saved[] => {
    try {
      const raw = storage()?.getItem(STORE_KEY);
      const doc = raw ? (JSON.parse(raw) as unknown) : null;
      const list = obj(doc)?.e;
      return Array.isArray(list) ? (list.filter((e) => typeof obj(e)?.k === 'string' && obj(e)?.req) as Saved[]) : [];
    } catch {
      return [];
    }
  };
  const save = (entry: Saved) => {
    try {
      const text = JSON.stringify(entry);
      if (text.length > STORE_MAX_ENTRY_CHARS) return;
      const rest = loadSaved().filter((e) => e.k !== entry.k);
      storage()?.setItem(STORE_KEY, JSON.stringify({ v: 1, e: [...rest, entry].slice(-STORE_MAX_ENTRIES) }));
    } catch {
      /* storage full, blocked or absent: the check just runs again next time */
    }
  };

  /** One GET of JSON; null for anything but a 200 with a JSON body within `max` bytes (CORS, 404, timeout, offline alike). */
  async function getJson(url: string, max: number, signal?: AbortSignal): Promise<{ ok: true; doc: unknown } | { ok: false; notFound: boolean }> {
    const ctl = new AbortController();
    const timer = setTimeout(() => ctl.abort(), timeoutMs);
    const onAbort = () => ctl.abort();
    signal?.addEventListener('abort', onAbort, { once: true });
    try {
      const res = await fetchFn(url, { signal: ctl.signal, credentials: 'omit', referrerPolicy: 'no-referrer', headers: { accept: 'application/json' } });
      if (!res.ok) return { ok: false, notFound: res.status === 404 };
      const body = await readCapped(res, max);
      if (!body) return { ok: false, notFound: false };
      return { ok: true, doc: JSON.parse(new TextDecoder().decode(body)) as unknown };
    } catch {
      return { ok: false, notFound: false };
    } finally {
      clearTimeout(timer);
      signal?.removeEventListener('abort', onAbort);
    }
  }

  function safeVerify(req: GenesisRequest): GenesisVerdict {
    try {
      return o.verify(req);
    } catch (e) {
      return { status: 'unavailable', code: 'malformed', message: e instanceof Error ? e.message : String(e) };
    }
  }

  function verified(v: Extract<GenesisVerdict, { status: 'verified' }>, source: GenesisSource, live: (boolean | null)[], outputs: { index: number }[]): GenesisCheck {
    const minters = v.minterOutputs;
    const kron = familyOfProgram(v.program as TokenProgram) === 'kron';
    const liveOf = (index: number) => live[outputs.findIndex((x) => x.index === index)] ?? null;
    return {
      status: 'verified', source, txid: v.txid, program: v.program, supply: v.supply,
      // a KRON minter has no allowance: it mints without bound (the verdict's allowance 0 is no 'Mint left: 0')
      mintAllowance: minters.length > 0 && !kron ? v.mintAllowance : null,
      ...(minters.length > 0 && kron ? { mintUnbounded: true } : {}),
      mintersUnspent: minters.length === 0 ? null : minters.every((m) => liveOf(m) === true) ? true : minters.some((m) => liveOf(m) === false) ? false : null,
    };
  }

  async function run(network: KascovNetwork, covenantId: string, kascovBase: string, programHint: string | null, signal?: AbortSignal): Promise<GenesisCheck> {
    const hint = programHint && HEX64.test(programHint) ? programHint : undefined;
    const key = `${network}:${covenantId}`;

    // 0. a saved copy of the evidence of an earlier check: used only if it verifies again, here and now
    const saved = loadSaved().find((e) => e.k === key);
    if (saved) {
      const v = safeVerify(saved.req);
      // (whether the minters were unspent is a hint of the source, not part of the evidence: a saved copy never says "the whole allowance is there")
      if (v.status === 'verified' && saved.req.covenantId === covenantId) return verified(v, 'saved', [], saved.req.outputs);
    }

    // 1. kascov's file for this covenant
    const base = kascovBase.replace(/\/+$/, '');
    if (base === '') return { status: 'unavailable', reason: 'history' };
    const file = await getJson(`${base}/${network}/c/${covenantId}.json`, KASCOV_FILE_MAX, signal);
    if (!file.ok) return { status: 'unavailable', reason: file.notFound ? 'history' : 'unreachable' };
    const g = parseKascovGenesis(file.doc, covenantId, network);
    if (!g) return { status: 'unavailable', reason: 'history' };
    const kascovOutputs = (): GenesisRequest['outputs'] => g.outputs.map((x) => ({ index: x.index, value: x.value, spk: x.spk, ...(x.redeem ? { redeemScript: x.redeem } : {}) }));
    const live = g.outputs.map((x) => x.live);
    // kascov's file alone can only make a token 'unavailable', never 'failed' (a mismatch that only a supplier's incomplete or stale file shows is
    // asked again later, audit 2026-10-29 F8): a failure needs the explorer's transaction too, or the self-authenticating contamination
    let kascovMismatch = false;
    let reason: GenesisUnavailableReason = 'history';

    if (g.authorizing && g.outputs.length > 0) {
      const req: GenesisRequest = { covenantId, genesisTxid: g.genesisTxid, authorizingOutpoint: g.authorizing, outputs: kascovOutputs(), programHint: hint };
      const v = safeVerify(req);
      if (v.status === 'verified') {
        save({ k: key, req });
        return verified(v, 'kascov', live, g.outputs);
      }
      if (v.status === 'failed' && (v.code === 'contaminated' || !g.partial)) {
        // contamination is self-authenticating (the covenant id recomputed); a mismatch of a complete file is held back until the explorer has been asked
        if (v.code === 'contaminated') return { status: 'failed', code: 'contaminated', message: v.message };
        kascovMismatch = true;
      }
      if (v.status === 'unavailable' && v.code === 'unsupported_program') reason = 'program';
    }

    // 2. the explorer: the genesis transaction (authorising input, group, the keys it pays to), and the spends that revealed scripts kascov did not give
    const api = explorer[network];
    const tx = await getJson(`${api}/transactions/${g.genesisTxid}?inputs=true&outputs=true&resolve_previous_outpoints=no`, EXPLORER_MAX, signal);
    if (!tx.ok) return { status: 'unavailable', reason: kascovMismatch ? 'unreachable' : tx.notFound ? reason : 'unreachable' };
    const e = parseExplorerGenesis(tx.doc, g.genesisTxid, covenantId);
    if (!e || !e.authorizing || e.outputs.length === 0) return { status: 'unavailable', reason };
    const byIndex = new Map(g.outputs.map((x) => [x.index, x]));
    const outputs: GenesisRequest['outputs'] = [];
    const outLive: (boolean | null)[] = [];
    let requests = 0;
    for (const x of e.outputs) {
      const k = byIndex.get(x.index);
      const same = !!k && k.spk === x.spk;
      // kascov's script is used only for the output whose script public key it matches (the hash check would refuse it anyway)
      let redeem = same ? k.redeem : null;
      if (!redeem && !(same && k.live === true) && requests < MAX_SPEND_REQUESTS) {
        // revealed by the spend of the output: the one kascov names, else the spend found in the history of the output's script address
        // (an output kascov calls unspent has no spend to find: it stays unrevealed, or is rebuilt)
        if (same && k.spentTxid) {
          requests++;
          const sp = await getJson(`${api}/transactions/${k.spentTxid}?inputs=true&outputs=false&resolve_previous_outpoints=no`, EXPLORER_MAX, signal);
          redeem = sp.ok ? revealFromSpend(sp.doc, k.spentTxid, g.genesisTxid, x.index) : null;
        } else if (x.address) {
          for (let offset = 0; !redeem && offset < 3 * ADDRESS_PAGE && requests < MAX_SPEND_REQUESTS; offset += ADDRESS_PAGE) {
            requests++;
            const page = await getJson(`${api}/addresses/${x.address}/full-transactions?limit=${ADDRESS_PAGE}&offset=${offset}&resolve_previous_outpoints=no`, EXPLORER_MAX * 4, signal);
            if (!page.ok || !Array.isArray(page.doc)) break;
            for (const t of page.doc) {
              const id = obj(t)?.transaction_id;
              redeem = typeof id === 'string' ? revealFromSpend(t, id, g.genesisTxid, x.index) : null;
              if (redeem) break;
            }
            if (page.doc.length < ADDRESS_PAGE) break;
          }
        }
      }
      outputs.push({ index: x.index, value: x.value, spk: x.spk, ...(redeem ? { redeemScript: redeem } : {}) });
      outLive.push(same ? k.live : null);
    }
    const req: GenesisRequest = { covenantId, genesisTxid: g.genesisTxid, authorizingOutpoint: e.authorizing, outputs, programHint: hint, ownerHints: e.ownerHints };
    const v = safeVerify(req);
    if (v.status === 'verified') {
      save({ k: key, req });
      return verified(v, 'explorer', outLive, outputs);
    }
    if (v.status === 'failed') return { status: 'failed', code: v.code, message: v.message };
    return v.code === 'unsupported_program' ? { status: 'unavailable', reason: 'program' } : { status: 'unavailable', reason };
  }

  return {
    check(network, covenantId, opts) {
      const n = kascovNetwork(network);
      const id = covenantId.toLowerCase();
      if (!n || !HEX64.test(id)) return Promise.resolve({ status: 'unavailable', reason: 'history' });
      const key = `${n}:${id}`;
      const hit = memory.get(key);
      if (hit && (hit.result.status !== 'unavailable' || now() - hit.at < retryMs)) return Promise.resolve(hit.result);
      let p = inflight.get(key);
      if (!p) {
        p = run(n, id, opts.kascovBase, opts.programHint ?? null, opts.signal)
          .catch((): GenesisCheck => ({ status: 'unavailable', reason: 'unreachable' }))
          .then((result) => {
            // an aborted run (the caller gave up) says nothing about the token: it is not remembered
            if (!opts.signal?.aborted) memory.set(key, { result, at: now() });
            inflight.delete(key);
            return result;
          });
        inflight.set(key, p);
      }
      return p;
    },
    known(network, covenantId) {
      const n = kascovNetwork(network);
      return n ? (memory.get(`${n}:${covenantId.toLowerCase()}`)?.result ?? null) : null;
    },
    clear() {
      memory.clear();
      inflight.clear();
    },
  };
}

