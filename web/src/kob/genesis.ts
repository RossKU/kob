// The genesis check of a token no registry lists, over kob-wasm. The one implementation is
// crates/kob-protocol/src/genesis_open.rs.
//
// A token's genesis group is the set of outputs of its first transaction that carry its covenant
// id. The covenant id commits to the authorising outpoint and to the index, value and script
// public key of every one of them. So whoever supplied the bytes can make the check fail but never
// pass a wrong group. Each redeem script is hashed against its output and decoded as an instance
// of a pinned token program. This file only maps the request and the answer. The rules (which
// program, the TokenSeed rebuild, contamination) live in Rust.
import { KobError, type KobWasm } from './wasm';

/** One output of the genesis group as a data source gave it. */
export interface GenesisOutputData {
  index: number;
  /** Sompi, decimal string. */
  value: string;
  /** Script public key bytes (hex). */
  spk: string;
  /** The revealed redeem script (hex). Absent means not revealed. */
  redeemScript?: string | undefined;
}

/** What the check is given: the complete genesis group and the outpoint that authorised it. */
export interface GenesisRequest {
  covenantId: string;
  genesisTxid: string;
  authorizingOutpoint: { txid: string; index: number };
  outputs: GenesisOutputData[];
  /**
   * A template hash a third party named for the token's program. It breaks a tie between programs
   * that verify and does nothing more.
   */
  programHint?: string | undefined;
  /** X-only keys the genesis transaction pays to (candidate owners of an unspent TokenSeed). */
  ownerHints?: string[] | undefined;
}

/** A genesis group that is an instance of a pinned token program. */
export interface GenesisVerified {
  status: 'verified';
  /** The pinned token program the group is an instance of (`KCC20PublicMint`, ...). */
  program: string;
  programHash: string;
  txid: string;
  outputs: number[];
  /** Token units the genesis created in holders. */
  supply: bigint;
  /**
   * Genesis outputs that carry a mint authority (a `PublicMint` of the public-mint app, a KRON
   * minter, a bridge's minter).
   */
  minterOutputs: number[];
  /**
   * The sum of the genesis `PublicMint` allowances: what anyone could still mint when the genesis
   * was made (0 without one).
   */
  mintAllowance: bigint;
  seedOutputs: number[];
  /**
   * Genesis outputs that are companions the program's profile pins by template hash, with their
   * role. `bridge-minter` is a federated bridge's minter, such as kUSDT's BridgeMint. It mints
   * against deposits on another chain and is also among `minterOutputs`.
   */
  companionOutputs: { index: number; role: string }[];
  /** The build of the public-mint app whose actors the genesis holds. */
  issuer: string | null;
  /** Outputs whose redeem script was rebuilt (an unspent TokenSeed). */
  rebuilt: number[];
}
/**
 * The data does not match the covenant id or a hash (`mismatch`), or an output is not an instance
 * of the program (`contaminated`).
 */
export interface GenesisFailed {
  status: 'failed';
  code: 'mismatch' | 'contaminated';
  message: string;
}
/**
 * The check could not be done: a redeem script nobody revealed, a program this build does not pin,
 * or malformed data. It is never a verdict on the token.
 */
export interface GenesisUnavailable {
  status: 'unavailable';
  code: 'unrevealed' | 'unsupported_program' | 'malformed';
  message: string;
}
/** The answer of the genesis check. */
export type GenesisVerdict = GenesisVerified | GenesisFailed | GenesisUnavailable;

interface RawGenesis {
  verifyGenesis(request: string): string;
}

const ints = (v: unknown): number[] => (Array.isArray(v) ? v.filter((x): x is number => Number.isInteger(x)) : []);
const companions = (v: unknown): { index: number; role: string }[] =>
  Array.isArray(v)
    ? v.flatMap((c: unknown) => {
        const o = c && typeof c === 'object' ? (c as Record<string, unknown>) : {};
        return Number.isInteger(o.index) && typeof o.role === 'string' ? [{ index: o.index as number, role: o.role }] : [];
      })
    : [];
const big = (v: unknown): bigint => {
  try {
    return typeof v === 'string' && /^-?\d+$/.test(v) ? BigInt(v) : 0n;
  } catch {
    return 0n;
  }
};

/**
 * Checks a genesis group against the pinned token programs.
 * Throws KobError when the loaded bindings predate the export. Every fault of the data is an
 * answer (`failed`, `unavailable`), never a throw.
 */
export function verifyGenesis(kob: KobWasm, req: GenesisRequest): GenesisVerdict {
  const r = kob.raw as unknown as Partial<RawGenesis>;
  if (typeof r.verifyGenesis !== 'function') {
    throw new KobError('the loaded kob-wasm bindings do not export verifyGenesis: rebuild them (npm run build:wasm)', 'verifyGenesis');
  }
  let out: Record<string, unknown>;
  try {
    out = JSON.parse(r.verifyGenesis(JSON.stringify({ ...req, outputs: req.outputs.map((o) => ({ ...o, spkVersion: 0 })) }))) as Record<string, unknown>;
  } catch (e) {
    throw new KobError(e instanceof Error ? e.message : String(e), 'verifyGenesis');
  }
  const message = typeof out.message === 'string' ? out.message : '';
  switch (out.status) {
    case 'verified':
      return {
        status: 'verified',
        program: typeof out.program === 'string' ? out.program : '',
        programHash: typeof out.programHash === 'string' ? out.programHash : '',
        txid: typeof out.txid === 'string' ? out.txid : req.genesisTxid,
        outputs: ints(out.outputs),
        supply: big(out.supply),
        minterOutputs: ints(out.minterOutputs),
        mintAllowance: big(out.mintAllowance),
        seedOutputs: ints(out.seedOutputs),
        companionOutputs: companions(out.companionOutputs),
        issuer: typeof out.issuer === 'string' ? out.issuer : null,
        rebuilt: ints(out.rebuilt),
      };
    case 'failed':
      return { status: 'failed', code: out.code === 'contaminated' ? 'contaminated' : 'mismatch', message };
    case 'unavailable':
      return { status: 'unavailable', code: out.code === 'unrevealed' || out.code === 'unsupported_program' ? out.code : 'malformed', message };
    default:
      throw new KobError('verifyGenesis returned an unknown status', 'verifyGenesis');
  }
}
