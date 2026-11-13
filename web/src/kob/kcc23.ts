// KCC-23 readers over kob-wasm. crates/kob-protocol/src/kcc23.rs is the one implementation.
// They verify a launch proof against its covenant id, and decode a preparation payload under KOB's
// token convention (docs/spec/token-metadata.md).
// A proof may come from anyone (the indexer, a list, a file). `verifyLaunchProof` recomputes both
// transaction ids and the covenant id, so a supplier can withhold a declaration but never forge
// one. Decoding alone binds nothing.
import type { Hex } from './types';
import { KobError, type KobWasm } from './wasm';

/**
 * KOB's token fields of a declaration.
 * A field that is absent, or present but not acceptable (named in `issues`), is null.
 */
export interface TokenDeclaration {
  name: string | null;
  symbol: string | null;
  /** 0..=18; null = undeclared. */
  decimals: number | null;
  description: string | null;
  /** `image` (or `logo`): an https:// or ipfs:// address. */
  image: string | null;
  /**
   * `image_hash`: 64 lowercase hex, the SHA-256 of the picture file's bytes. Also read from an
   * `image` of the form `sha256:<hex>`.
   */
  imageHash: string | null;
  website: string | null;
  x: string | null;
  telegram: string | null;
  /** Fields present but not taken, for example "decimals: not an integer from 0 to 18". */
  issues: string[];
}

/** A declaration bound to its covenant id. */
export interface VerifiedDeclaration {
  covenantId: Hex;
  preparationTxid: Hex;
  genesisTxid: Hex;
  /** The JSON text exactly as committed. */
  json: string;
  token: TokenDeclaration;
}

interface RawKcc23 {
  kcc23Verify(proof: string): string;
  kcc23Decode(payloadHex: string): string;
}

function raw(kob: KobWasm): RawKcc23 {
  const r = kob.raw as unknown as Partial<RawKcc23>;
  if (typeof r.kcc23Verify !== 'function' || typeof r.kcc23Decode !== 'function') {
    throw new KobError('the loaded kob-wasm bindings do not export the KCC-23 readers: rebuild them (npm run build:wasm)', 'kcc23');
  }
  return r as RawKcc23;
}

const text = (v: unknown, max: number): string | null => (typeof v === 'string' && v.length > 0 && v.length <= max ? v : null);

/**
 * The token fields as kob-wasm returned them, re-checked for shape.
 * A binding of another build must not put other types in the UI.
 */
function tokenOf(v: unknown): TokenDeclaration {
  const o = (v && typeof v === 'object' ? v : {}) as Record<string, unknown>;
  const d = o.decimals;
  return {
    name: text(o.name, 256),
    symbol: text(o.symbol, 128),
    decimals: typeof d === 'number' && Number.isInteger(d) && d >= 0 && d <= 18 ? d : null,
    description: text(o.description, 2048),
    image: text(o.image, 2048),
    imageHash: typeof o.image_hash === 'string' && /^[0-9a-f]{64}$/.test(o.image_hash) ? o.image_hash : null,
    website: text(o.website, 2048),
    x: text(o.x, 256),
    telegram: text(o.telegram, 256),
    issues: Array.isArray(o.issues) ? o.issues.filter((i): i is string => typeof i === 'string') : [],
  };
}

/**
 * Verifies a launch proof (`kob-kcc23-proof/1`, the object or its JSON text).
 * Throws KobError when it binds nothing to its covenant id.
 */
export function verifyLaunchProof(kob: KobWasm, proof: unknown): VerifiedDeclaration {
  const r = raw(kob);
  let out: Record<string, unknown>;
  try {
    out = JSON.parse(r.kcc23Verify(typeof proof === 'string' ? proof : JSON.stringify(proof))) as Record<string, unknown>;
  } catch (e) {
    throw new KobError(e instanceof Error ? e.message : String(e), 'kcc23Verify');
  }
  const hex32 = (v: unknown): Hex => {
    if (typeof v !== 'string' || !/^[0-9a-f]{64}$/.test(v)) throw new KobError('kcc23Verify returned a malformed id', 'kcc23Verify');
    return v;
  };
  return {
    covenantId: hex32(out.covenantId),
    preparationTxid: hex32(out.preparationTxid),
    genesisTxid: hex32(out.genesisTxid),
    json: typeof out.json === 'string' ? out.json : '',
    token: tokenOf(out.token),
  };
}

/** Decodes a preparation payload (hex). Throws KobError when it is not a KCC-23 declaration. */
export function decodeDeclaration(kob: KobWasm, payloadHex: Hex): { json: string; token: TokenDeclaration } {
  const r = raw(kob);
  let out: Record<string, unknown>;
  try {
    out = JSON.parse(r.kcc23Decode(payloadHex)) as Record<string, unknown>;
  } catch (e) {
    throw new KobError(e instanceof Error ? e.message : String(e), 'kcc23Decode');
  }
  return { json: typeof out.json === 'string' ? out.json : '', token: tokenOf(out.token) };
}
