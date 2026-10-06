// PAYMENT-REQUIRED / PAYMENT-SIGNATURE / PAYMENT-RESPONSE: base64 (standard alphabet, padded) of the canonical JSON.

import { canonicalJson } from './canonical.ts';
import { KobX402Error } from './errors.ts';
import type { PaymentPayload, PaymentRequired, SettlementResponse } from './types.ts';

/** Largest encoded header value accepted (bytes of the base64 text). */
export const MAX_HEADER_BYTES = 256 * 1024;
const B64 = /^[A-Za-z0-9+/]*={0,2}$/;

export function encodeHeader(value: unknown): string {
  return Buffer.from(canonicalJson(value), 'utf8').toString('base64');
}

export function decodeHeader<T = unknown>(header: string, maxBytes = MAX_HEADER_BYTES): T {
  const h = header.trim();
  if (h.length === 0 || h.length > maxBytes) throw new KobX402Error('invalid_header', 'header is empty or too large');
  if (h.length % 4 !== 0 || !B64.test(h)) throw new KobX402Error('invalid_header', 'header is not canonical base64');
  const bytes = Buffer.from(h, 'base64');
  try {
    return JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(bytes)) as T;
  } catch (e) {
    throw new KobX402Error('invalid_header', 'header does not contain JSON', { cause: e });
  }
}

export const encodePaymentRequired = (v: PaymentRequired): string => encodeHeader(v);
export const decodePaymentRequired = (h: string): PaymentRequired => decodeHeader<PaymentRequired>(h);
export const encodePaymentSignature = (v: PaymentPayload): string => encodeHeader(v);
export const decodePaymentSignature = (h: string): PaymentPayload => decodeHeader<PaymentPayload>(h);
export const encodePaymentResponse = (v: SettlementResponse): string => encodeHeader(v);
export const decodePaymentResponse = (h: string): SettlementResponse => decodeHeader<SettlementResponse>(h);
