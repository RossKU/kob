// Deterministic TEST keys shared by the mock server (market maker / dev funding) and the e2e fixtures. Never use on a real network.
import { schnorr } from '@noble/curves/secp256k1.js';
import { sha256hex, toHex } from './util.mjs';

const secret = (name) => sha256hex(`kob-mock-test-key:${name}`);

/** @type {Record<'alice'|'bob'|'carol'|'maker'|'filler', string>} secret keys (hex) */
export const TEST_SECRETS = {
  alice: secret('alice'),
  bob: secret('bob'),
  carol: secret('carol'),
  /** the fictional market maker that owns the seeded book */
  maker: secret('maker'),
  /** the mock's filler (matcher / inventory taker): signs the batches of simulated pair fills and arms (its tokens and KAS are minted on demand) */
  filler: secret('filler'),
};

export const pubkeyOfSecret = (sk) => toHex(schnorr.getPublicKey(Buffer.from(sk, 'hex')));

/** @type {Record<keyof typeof TEST_SECRETS, string>} x-only public keys (hex) */
export const TEST_PUBKEYS = Object.fromEntries(Object.entries(TEST_SECRETS).map(([k, sk]) => [k, pubkeyOfSecret(sk)]));
