// Small helpers shared by the mock server modules (no dependencies beyond node).
import { createHash } from 'node:crypto';

export const HEX64 = /^[0-9a-f]{64}$/;

export const toHex = (b) => Buffer.from(b).toString('hex');
export const fromHex = (h) => Buffer.from(h, 'hex');
export const sha256hex = (s) => createHash('sha256').update(s).digest('hex');
export const isHex = (s) => typeof s === 'string' && /^([0-9a-fA-F]{2})*$/.test(s);

/** Deterministic pseudo ids for synthetic transactions / covenants (seeds, simulated fills). */
export function syntheticId(label, n) {
  return sha256hex(`kob-mock:${label}:${n}`);
}

/** `Error` carrying an HTTP status and an API error code (the indexer body is `{error:{code,message}}`). */
export class ApiError extends Error {
  constructor(status, code, message) {
    super(message);
    this.status = status;
    this.code = code;
  }
}
export const badRequest = (m) => new ApiError(400, 'bad_request', m);
export const notFound = (m) => new ApiError(404, 'not_found', m);

/** A submit the mock node refuses: the HTTP body is `{error: message}` (plain string, like a node RPC error text). */
export class Rejection extends Error {}

export const big = (v, what = 'value') => {
  try {
    return BigInt(v);
  } catch {
    throw badRequest(`${what} must be an integer`);
  }
};

export const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
