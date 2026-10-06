// Transport policy of the payer and facilitator clients: a signed payment and the facilitator API key
// travel over https. Plain http is accepted for loopback hosts (local development, an in-process test server) and
// when the caller opts in explicitly (`allowInsecureHttp`, e.g. a private network the operator controls).

import { KobX402Error } from './errors.ts';

/** True for `localhost`, `*.localhost`, 127.0.0.0/8 and `::1` (the host part of a URL, IPv6 without brackets or with). */
export function isLoopbackHost(hostname: string): boolean {
  const h = hostname.toLowerCase().replace(/^\[|\]$/g, '');
  if (h === 'localhost' || h.endsWith('.localhost')) return true;
  if (h === '::1') return true;
  const m = /^127\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})$/.exec(h);
  return m !== null && [m[1], m[2], m[3]].every((x) => Number(x) <= 255);
}

/**
 * Throws unless `url` is https, or http to a loopback host, or `allowInsecureHttp` is set. Other schemes are always refused.
 * `what` names the URL in the message ("the resource URL", "the facilitator URL").
 */
export function assertSecureUrl(url: string | URL, what: string, allowInsecureHttp = false): void {
  let u: URL;
  try {
    u = new URL(url);
  } catch (e) {
    throw new KobX402Error('bad_request', `${what} is not an absolute URL: ${String(url)}`, { cause: e });
  }
  if (u.protocol === 'https:') return;
  if (u.protocol === 'http:' && (allowInsecureHttp || isLoopbackHost(u.hostname))) return;
  throw new KobX402Error(
    'bad_request',
    `${what} must use https (plain http only to a loopback host, or with allowInsecureHttp): ${u.protocol}//${u.host}`,
  );
}
