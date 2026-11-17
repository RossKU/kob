// Read-only calls to the KOB indexer (keyless HTTP).

const DEFAULT_INDEXER = { mainnet: 'https://kob-api.kaspa.ag', 'testnet-10': 'https://kob-api-tn10.kaspa.ag' };

export class Indexer {
  constructor(cfg, timeoutMs = 15_000) {
    this.url = (cfg.indexer ?? DEFAULT_INDEXER[cfg.network]).replace(/\/+$/, '');
    this.timeoutMs = timeoutMs;
  }

  /** The JSON body, null on 404; throws on other errors. */
  async get(path) {
    const res = await fetch(`${this.url}${path}`, { headers: { accept: 'application/json' }, signal: AbortSignal.timeout(this.timeoutMs) });
    if (res.status === 404) return null;
    if (!res.ok) throw new Error(`indexer ${path.split('?')[0]}: HTTP ${res.status}`);
    return res.json();
  }

  /** As get, but null on any error (used where a missing answer only skips a check). */
  async tryGet(path) {
    try {
      return await this.get(path);
    } catch {
      return null;
    }
  }
}
