// Offers: parsing a 402, classifying and selecting an entry (payer side), building entries from configuration
// (server side).
//
// Payer rule of the HTTP profile: select a supported Kaspa entry and skip everything else (other schemes,
// networks, assets, profiles, foreign extensions) instead of rejecting the envelope.

import { decodePaymentRequired } from './headers.ts';
import { KobX402Error } from './errors.ts';
import type { KobWasm } from './wasm.ts';
import {
  ASSET_KAS,
  BINDING_EXACT,
  BINDING_SWAP,
  BINDING_INTENT,
  ROUTER_ARTIFACT_ID,
  NETWORKS,
  SCHEME_EXACT,
  TX_ENCODING,
  X402_VERSION,
} from './types.ts';
import type {
  Custody,
  ExactExtra,
  Finality,
  NetworkId,
  OfferKind,
  OfferSpec,
  PaymentRequired,
  PaymentRequirements,
  RouteExtra,
} from './types.ts';

const HEX64 = /^[0-9a-f]{64}$/;
const HEX = /^([0-9a-f]{2})+$/;
const U64_MAX = (1n << 64n) - 1n;

export function isRecord(v: unknown): v is Record<string, unknown> {
  return v !== null && typeof v === 'object' && !Array.isArray(v);
}

/** Canonical positive-or-zero uint64 decimal (no sign, no leading zeros). */
export function parseU64(s: unknown): bigint | null {
  if (typeof s !== 'string' || !/^(0|[1-9][0-9]*)$/.test(s)) return null;
  const v = BigInt(s);
  return v <= U64_MAX ? v : null;
}

/** "0.5" KAS -> "50000000" sompi. Rejects more than 8 decimals and non-decimal input. */
export function kasToSompi(kas: string): string {
  const m = /^(0|[1-9][0-9]*)(?:\.([0-9]{1,8}))?$/.exec(kas.trim());
  if (!m) throw new KobX402Error('bad_request', `not a KAS amount: ${kas}`);
  const v = BigInt(m[1] ?? '0') * 100_000_000n + BigInt((m[2] ?? '').padEnd(8, '0') || '0');
  if (v > U64_MAX) throw new KobX402Error('bad_request', 'KAS amount exceeds uint64 sompi');
  return v.toString();
}

// ---------------------------------------------------------------------------------------------- parsing (payer)

/**
 * Parses a 402: the `PAYMENT-REQUIRED` header value (base64) or the JSON body (text or value). Only the envelope is
 * validated; entries stay untouched (foreign entries are legal).
 */
export function parsePaymentRequired(input: string | unknown): PaymentRequired {
  let v: unknown = input;
  if (typeof input === 'string') {
    const t = input.trim();
    try {
      v = t.startsWith('{') ? JSON.parse(t) : decodePaymentRequired(t);
    } catch (e) {
      throw new KobX402Error('invalid_payment_required', 'the 402 is neither a base64 header nor JSON', { cause: e });
    }
  }
  if (!isRecord(v)) throw new KobX402Error('invalid_payment_required', 'the 402 is not an object');
  if (v.x402Version !== X402_VERSION) throw new KobX402Error('invalid_payment_required', `unsupported x402Version ${String(v.x402Version)}`);
  if (!isRecord(v.resource) || typeof v.resource.url !== 'string') throw new KobX402Error('invalid_payment_required', 'resource.url is missing');
  if (!Array.isArray(v.accepts)) throw new KobX402Error('invalid_payment_required', 'accepts is not an array');
  return v as unknown as PaymentRequired;
}

// ------------------------------------------------------------------------------------------- classification

export interface ClassifiedOffer {
  requirements: PaymentRequirements;
  /** Position in `accepts`. */
  index: number;
  kind: OfferKind;
  /** What the merchant receives. */
  receives: 'kas' | 'kcc20';
  /** Merchant asset (`KAS` or a covenant id). */
  asset: string;
  amount: bigint;
  network: NetworkId;
  finality: Finality;
  custody?: Custody;
  /** Swap-and-pay: assets the payer may pay with (covenant ids, or `KAS`). */
  payAssets: string[];
}

/** Classifies one `accepts` entry; `null` for everything this SDK does not pay (foreign, additive, malformed, unknown critical route). */
export function classifyOffer(entry: unknown, index = 0): ClassifiedOffer | null {
  if (!isRecord(entry) || entry.scheme !== SCHEME_EXACT) return null;
  const network = entry.network;
  if (typeof network !== 'string' || !(NETWORKS as readonly string[]).includes(network)) return null;
  const amount = parseU64(entry.amount);
  if (amount === null || amount === 0n) return null;
  if (typeof entry.asset !== 'string' || typeof entry.payTo !== 'string' || entry.payTo.length === 0) return null;
  if (typeof entry.maxTimeoutSeconds !== 'number' || !Number.isSafeInteger(entry.maxTimeoutSeconds) || entry.maxTimeoutSeconds <= 0) return null;
  const extra = entry.extra;
  if (!isRecord(extra) || extra.binding !== BINDING_EXACT || extra.transactionEncoding !== TX_ENCODING) return null;
  if (typeof extra.payToScriptPublicKey !== 'string' || !HEX.test(extra.payToScriptPublicKey)) return null;
  const finality = extra.finality;
  if (finality !== 'accepted' && finality !== 'confirmed') return null;

  let receives: 'kas' | 'kcc20';
  let custody: Custody | undefined;
  if (extra.profile === 'standard-native') {
    if (entry.asset !== ASSET_KAS) return null;
    receives = 'kas';
  } else if (extra.profile === 'kcc20') {
    if (!HEX64.test(entry.asset)) return null;
    const t = extra.token;
    if (!isRecord(t) || t.family !== 'kcc20') return null;
    if (typeof t.templateHash !== 'string' || !HEX64.test(t.templateHash)) return null;
    if (typeof t.extensionCommitment !== 'string' || !HEX64.test(t.extensionCommitment)) return null;
    if (typeof t.tokenScriptPublicKey !== 'string' || !HEX.test(t.tokenScriptPublicKey)) return null;
    if (parseU64(t.carrier) === null) return null;
    if (t.custody !== 'unconditional' && t.custody !== 'issuer-controlled') return null;
    custody = t.custody;
    receives = 'kcc20';
  } else {
    return null; // additive and unknown profiles
  }

  let payAssets: string[] = [];
  let kind: OfferKind = receives === 'kas' ? 'native' : 'kcc20';
  if (extra.route !== undefined) {
    const r = extra.route as Partial<RouteExtra>;
    if (!isRecord(r) || r.binding !== BINDING_SWAP || !Array.isArray(r.payAssets) || r.payAssets.length === 0) return null;
    for (const p of r.payAssets) {
      if (!isRecord(p) || typeof p.asset !== 'string' || !(HEX64.test(p.asset) || p.asset === ASSET_KAS)) return null;
      payAssets.push(p.asset);
    }
    kind = 'swap';
  }
  const out: ClassifiedOffer = {
    requirements: entry as unknown as PaymentRequirements,
    index,
    kind,
    receives,
    asset: entry.asset,
    amount,
    network: network as NetworkId,
    finality,
    payAssets,
  };
  if (custody) out.custody = custody;
  return out;
}

// ---------------------------------------------------------------------------------------------- selection

export interface PayerCapabilities {
  network: NetworkId;
  /** The payer holds only KAS: no kcc20 entries, and swap-and-pay only through routes that accept KAS. */
  kasOnly?: boolean;
  /** KCC-20 balances the payer holds, by covenant id, in base units. A token that is absent (or zero) is not held. */
  tokens?: Record<string, string | bigint>;
  /** Accept `custody: "issuer-controlled"` tokens as the merchant asset (default false). */
  allowIssuerControlled?: boolean;
  /** Allow swap-and-pay (default true unless `kasOnly`). */
  allowSwap?: boolean;
  /** Per merchant asset ceiling on the offered amount (`KAS` in sompi, tokens in base units); an offer above it is skipped. */
  maxAmount?: Record<string, string | bigint>;
}

export interface SelectedOffer extends ClassifiedOffer {
  /** Swap-and-pay: the asset the payer will pay with (first of the offer's `payAssets` it can pay: `KAS`, or a held token). */
  payAsset?: string;
}

function held(caps: PayerCapabilities, asset: string): bigint {
  const v = caps.tokens?.[asset];
  if (v === undefined) return 0n;
  try {
    return typeof v === 'bigint' ? v : BigInt(v);
  } catch {
    return 0n;
  }
}

/** The pay assets of a swap entry this payer can pay with, in the entry's order: `KAS`, and the tokens it holds (unless KAS-only). */
export function payableAssets(o: ClassifiedOffer, caps: PayerCapabilities): string[] {
  if (o.kind !== 'swap') return [];
  return o.payAssets.filter((a) => a === ASSET_KAS || (!caps.kasOnly && held(caps, a) > 0n));
}

/** Preference class: standard-native, then kcc20, then swap routes that pay KAS, then swap routes that pay a token. */
function rank(o: ClassifiedOffer): number {
  if (o.kind === 'native') return 0;
  if (o.kind === 'kcc20') return 1;
  return o.receives === 'kas' ? 2 : 3;
}

/** Every entry the payer can pay, best first (stable within a class: the server's order). */
export function rankOffers(pr: PaymentRequired, caps: PayerCapabilities): SelectedOffer[] {
  const out: SelectedOffer[] = [];
  pr.accepts.forEach((entry, i) => {
    const o = classifyOffer(entry, i);
    if (!o || o.network !== caps.network) return;
    const cap = caps.maxAmount?.[o.asset];
    if (cap !== undefined && o.amount > BigInt(cap)) return;
    if (o.custody === 'issuer-controlled' && !caps.allowIssuerControlled) return;
    if (o.kind === 'native') {
      out.push(o);
    } else if (o.kind === 'kcc20') {
      if (caps.kasOnly || held(caps, o.asset) < o.amount) return;
      out.push(o);
    } else {
      if (caps.allowSwap === false) return;
      // KAS is always payable; a token only when the payer holds it (and is not KAS-only)
      const payAsset = payableAssets(o, caps)[0];
      if (payAsset === undefined) return;
      out.push({ ...o, payAsset });
    }
  });
  return out.map((o, i) => ({ o, i })).sort((a, b) => rank(a.o) - rank(b.o) || a.i - b.i).map((x) => x.o);
}

/** The best entry the payer can pay, or `null` (never throws on foreign entries). */
export function selectOffer(pr: PaymentRequired, caps: PayerCapabilities): SelectedOffer | null {
  return rankOffers(pr, caps)[0] ?? null;
}

// ------------------------------------------------------------------------------------------ building (server)

export interface BuildOfferContext {
  wasm: KobWasm;
  network: NetworkId;
  /** Merchant address (Schnorr P2PK for token profiles). */
  payTo: string;
  maxTimeoutSeconds: number;
  finality: Finality;
}

function checkAmount(amount: string): void {
  const v = parseU64(amount);
  if (v === null || v === 0n) throw new KobX402Error('bad_request', `offer amount ${amount} is not a positive canonical uint64`);
}

function checkAsset(asset: string | undefined): string {
  if (typeof asset !== 'string' || !HEX64.test(asset)) throw new KobX402Error('bad_request', `token asset must be 64 lowercase hex, got ${String(asset)}`);
  return asset;
}

/** The extension commitment of a KRON token: none, all zero. */
const KRON_EXTENSION = '0'.repeat(64);

/** Builds one `accepts` entry from a configuration spec; every Rust-derived value comes through `KobWasm`. */
export function buildOffer(spec: OfferSpec, ctx: BuildOfferContext): PaymentRequirements {
  checkAmount(spec.amount);
  const finality = spec.finality ?? ctx.finality;
  const extra: ExactExtra = {
    binding: BINDING_EXACT,
    profile: 'standard-native',
    finality,
    transactionEncoding: TX_ENCODING,
    payToScriptPublicKey: ctx.wasm.addressToScriptPublicKey(ctx.payTo),
  };
  let asset = ASSET_KAS;
  const tokenExtra = (asset: string, token: NonNullable<Extract<OfferSpec, { kind: 'kcc20' }>['token']>) => {
    const req: Parameters<KobWasm['tokenOffer']>[0] = { network: ctx.network, asset, payTo: ctx.payTo, amount: spec.amount, custody: token.custody };
    if (token.carrier !== undefined) req.carrier = token.carrier;
    if (token.templateHash !== undefined) req.templateHash = token.templateHash;
    if (token.extensionCommitment !== undefined) req.extensionCommitment = token.extensionCommitment;
    if (token.ticker !== undefined) req.ticker = token.ticker;
    if (token.decimals !== undefined) req.decimals = token.decimals;
    return ctx.wasm.tokenOffer(req);
  };
  if (spec.kind === 'kcc20') {
    asset = checkAsset(spec.asset);
    extra.profile = 'kcc20';
    extra.token = tokenExtra(asset, spec.token);
  } else if (spec.kind === 'swap') {
    if (spec.payAssets.length === 0) throw new KobX402Error('bad_request', 'a swap offer needs at least one pay asset');
    // one entry per asset: a second one with other pins would never be the one a verifier applies
    const ids = spec.payAssets.map((p) => (p.asset === ASSET_KAS ? ASSET_KAS : p.asset.toLowerCase()));
    if (new Set(ids).size !== ids.length) throw new KobX402Error('bad_request', 'a pay asset is listed twice');
    if (spec.receive === 'kcc20') {
      asset = checkAsset(spec.asset);
      if (!spec.token) throw new KobX402Error('bad_request', 'a swap offer that pays a token needs token metadata');
      extra.profile = 'kcc20';
      extra.token = tokenExtra(asset, spec.token);
    }
    extra.route = {
      binding: spec.mode === 'intent' ? BINDING_INTENT : BINDING_SWAP,
      critical: true,
      payAssets: spec.payAssets.map((p) => {
        if (p.asset === ASSET_KAS) {
          if (spec.receive !== 'kcc20') throw new KobX402Error('bad_request', 'KAS can be the pay asset only when the merchant receives a token');
          return { asset: ASSET_KAS };
        }
        const id = checkAsset(p.asset);
        // a KRON token has no extension commitment: all zero when the configuration names the program and the family
        const extension = p.extensionCommitment ?? (p.family === 'kron' ? KRON_EXTENSION : undefined);
        const pinned = p.templateHash && extension ? { templateHash: p.templateHash, extensionCommitment: extension } : ctx.wasm.resolveToken(ctx.network, id);
        return { asset: id, templateHash: p.templateHash ?? pinned.templateHash, extensionCommitment: p.extensionCommitment ?? pinned.extensionCommitment };
      }),
    };
    if (spec.mode === 'intent') extra.route.router = ROUTER_ARTIFACT_ID;
  }
  return {
    scheme: SCHEME_EXACT,
    network: ctx.network,
    amount: spec.amount,
    asset,
    payTo: ctx.payTo,
    maxTimeoutSeconds: ctx.maxTimeoutSeconds,
    extra,
  };
}

export function buildOffers(specs: OfferSpec[], ctx: BuildOfferContext): PaymentRequirements[] {
  if (specs.length === 0) throw new KobX402Error('bad_request', 'at least one offer is required');
  return specs.map((s) => buildOffer(s, ctx));
}
