// Read-only calls to the KOB indexer.

const DEFAULT_INDEXER = { mainnet: 'https://kob-api.kaspa.ag', 'testnet-10': 'https://kob-api-tn10.kaspa.ag' };

export const indexerUrl = (cfg) => (cfg.indexer ?? DEFAULT_INDEXER[cfg.network]).replace(/\/+$/, '');

/** An indexer order view in the bot's terms (numbers in sompi and base units). */
export function orderOf(v) {
  return {
    id: v.covenant_id,
    contract: v.contract,
    side: v.side === 1 ? 'sell' : v.side === 2 ? 'buy' : null,
    token: v.token,
    price: v.price == null ? null : Number(v.price),
    amountLeft: Number(v.amount_left ?? 0),
    value: Number(v.current?.value ?? 0),
    txid: v.current?.txid ?? null,
    status: v.status,
    expired: v.expired === true, // past its expiry: conforming matchers no longer fill it, a keeper may refund it
    expiryDaa: v.expiry_daa ?? null,
    scale: v.scale,
    live: v.status === 'open' || v.status === 'partial',
  };
}

export class Indexer {
  constructor(url, timeoutMs = 15_000) {
    this.url = url;
    this.timeoutMs = timeoutMs;
    this.views = {}; // token -> the raw views of the last liveOrders read (kob cancel --view reads them without the indexer)
  }

  async get(path) {
    const res = await fetch(`${this.url}${path}`, { headers: { accept: 'application/json' }, signal: AbortSignal.timeout(this.timeoutMs) });
    if (res.status === 404) return null;
    if (!res.ok) throw new Error(`indexer ${path.split('?')[0]}: HTTP ${res.status}`);
    return res.json();
  }

  /** The maker's live plain limit orders of one token (asks and bids on the KAS book). */
  async liveOrders(maker, token) {
    const out = [];
    const raw = [];
    let cursor = null;
    for (let page = 0; page < 10; page++) {
      const body = await this.get(`/v1/orders?maker=${maker}&status=active&limit=100${cursor ? `&cursor=${encodeURIComponent(cursor)}` : ''}`);
      for (const v of body?.items ?? []) if (v.token === token) { out.push(orderOf(v)); raw.push(v); }
      cursor = body?.next_cursor;
      if (!cursor) break;
    }
    this.views[token] = raw;
    return out;
  }

  /** One order, or null when the indexer does not know it (yet). */
  async order(id) {
    const v = await this.get(`/v1/orders/${id}`);
    return v ? orderOf(v) : null;
  }

  /** The token's newest fills, newest first. */
  async fills(token, limit = 100) {
    const body = await this.get(`/v1/fills?token=${token}&limit=${limit}`);
    return body?.items ?? [];
  }
}
