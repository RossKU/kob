// HTTP client of the facilitator: GET /supported, POST /verify, POST /settle (binding facilitator profile,
// `spec/facilitator-profile.md`). API key in the `Authorization: Bearer` header (configurable), per-call timeouts,
// bounded response bodies. Retries happen only for NETWORK errors (connection failure, timeout) on calls that are
// safe to repeat: /supported and /verify always; /settle never by default (its outcome after a network error is
// ambiguous; the facilitator is idempotent on txid+requestHash, so a caller that re-sends the same signed artifact
// is safe, but the SDK does not decide that on its own).

import { KobX402Error } from './errors.ts';
import type { FacilitatorRequest, SettlementResponse, SupportedResponse, VerifyResponse } from './types.ts';
import type { FetchLike } from './client.ts';
import { assertSecureUrl } from './url-policy.ts';

const MAX_RESPONSE_BYTES = 1024 * 1024;

export interface FacilitatorOptions {
  /** Base URL, e.g. `http://127.0.0.1:8402`. */
  url: string;
  apiKey?: string;
  /** Header carrying the key. Default `authorization` (sent as `Bearer <key>`); any other name sends the bare key. */
  apiKeyHeader?: string;
  /** Per attempt. Default 10 s (settle observes finality: pass a longer value there via `settleTimeoutMs`). */
  timeoutMs?: number;
  settleTimeoutMs?: number;
  /** Extra attempts after a network error on /supported and /verify. Default 2. */
  retries?: number;
  /** Also retry /settle after a network error (same body: idempotent by txid+requestHash). Default false. */
  retrySettle?: boolean;
  retryDelayMs?: number;
  fetch?: FetchLike;
  /** Allow a plain-http facilitator URL that is not loopback (a private network you control). Default false: https only. */
  allowInsecureHttp?: boolean;
}

export interface Facilitator {
  supported(): Promise<SupportedResponse>;
  verify(req: FacilitatorRequest): Promise<VerifyResponse>;
  settle(req: FacilitatorRequest): Promise<SettlementResponse>;
}

export class FacilitatorClient implements Facilitator {
  #o: FacilitatorOptions;
  #base: string;
  #fetch: FetchLike;

  constructor(options: FacilitatorOptions) {
    assertSecureUrl(options.url, 'the facilitator URL', options.allowInsecureHttp === true);
    this.#o = options;
    this.#base = options.url.replace(/\/+$/, '');
    const f = options.fetch ?? (globalThis.fetch as FetchLike | undefined);
    if (!f) throw new KobX402Error('unsupported', 'no fetch implementation available');
    this.#fetch = f;
  }

  supported(): Promise<SupportedResponse> {
    return this.#call<SupportedResponse>('GET', '/supported', undefined, this.#o.timeoutMs ?? 10_000, true);
  }

  verify(req: FacilitatorRequest): Promise<VerifyResponse> {
    return this.#call<VerifyResponse>('POST', '/verify', req, this.#o.timeoutMs ?? 10_000, true);
  }

  /** Any answer that is a settlement object (`success` present) is returned, whatever the HTTP status. */
  async settle(req: FacilitatorRequest): Promise<SettlementResponse> {
    const r = await this.#call<SettlementResponse>('POST', '/settle', req, this.#o.settleTimeoutMs ?? 30_000, this.#o.retrySettle === true, true);
    if (r === null || typeof r !== 'object' || typeof r.success !== 'boolean') {
      throw new KobX402Error('facilitator', 'the facilitator answered /settle with something that is not a settlement');
    }
    return r;
  }

  async #call<T>(method: string, path: string, body: unknown, timeoutMs: number, retry: boolean, acceptFailureBody = false): Promise<T> {
    const attempts = 1 + (retry ? Math.max(0, this.#o.retries ?? 2) : 0);
    let last: unknown;
    for (let i = 0; i < attempts; i++) {
      if (i > 0) await new Promise((r) => setTimeout(r, (this.#o.retryDelayMs ?? 200) * i));
      let res: Response;
      try {
        const headers: Record<string, string> = { accept: 'application/json' };
        if (body !== undefined) headers['content-type'] = 'application/json';
        if (this.#o.apiKey) {
          const h = (this.#o.apiKeyHeader ?? 'authorization').toLowerCase();
          headers[h] = h === 'authorization' ? `Bearer ${this.#o.apiKey}` : this.#o.apiKey;
        }
        const init: RequestInit = { method, headers, redirect: 'error', signal: AbortSignal.timeout(timeoutMs) };
        if (body !== undefined) init.body = JSON.stringify(body);
        res = await this.#fetch(this.#base + path, init);
      } catch (e) {
        last = e; // network error or timeout: retry when allowed
        continue;
      }
      const text = await readBounded(res);
      let json: unknown;
      try {
        json = JSON.parse(text);
      } catch {
        throw new KobX402Error('facilitator', `${method} ${path}: HTTP ${res.status} with a non-JSON body`, { status: res.status });
      }
      if (res.ok || (acceptFailureBody && json !== null && typeof json === 'object' && typeof (json as { success?: unknown }).success === 'boolean')) {
        return json as T;
      }
      throw new KobX402Error('facilitator', `${method} ${path}: HTTP ${res.status}`, { status: res.status, details: json });
    }
    throw new KobX402Error('facilitator', `${method} ${path}: the facilitator is unreachable: ${(last as Error).message}`, { cause: last });
  }
}

async function readBounded(res: Response): Promise<string> {
  const text = await res.text();
  if (text.length > MAX_RESPONSE_BYTES) throw new KobX402Error('facilitator', 'facilitator response too large');
  return text;
}
