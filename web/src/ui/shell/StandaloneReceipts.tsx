// Standalone mode (data/standalone.ts) and the wallet's own orders: while the mode is on, the connected wallet's placement receipts (the records My
// orders and Recover orders keep, kob/records.ts) are resolved on the node, as My orders resolves them without the indexer (ui/orders/orders-data.ts
// `resolveRecords`), and every live one joins the book: an order placed before the mode started is shown when it is the wallet's own.
import { useEffect } from 'preact/hooks';
import { readClock } from '../../app/env';
import { useServices, useWallet } from '../../app/context';
import { snapshotFromRecord, type OrderSnapshot } from '../../kob/cancel';
import { isOldTemplate } from '../../kob/records';
import { isStreamKind } from '../../kob/stream-orders';
import { resolveRecords } from '../orders/orders-data';
import { useStandalone } from './standalone';

/** The receipts are resolved again this often while the mode is on. */
export const RECEIPTS_EVERY_MS = 60_000;

/** Renders nothing: feeds the standalone book with the wallet's own orders. */
export function StandaloneReceipts() {
  const services = useServices();
  const wallet = useWallet();
  const sa = useStandalone(services.standalone);
  const active = !!sa?.active;
  const since = sa?.sinceMs ?? null;
  const pubkey = wallet.info?.pubkey ?? null;
  const records = wallet.records;
  useEffect(() => {
    const c = services.standalone;
    if (!active || !c || !pubkey) return;
    let alive = true;
    let timer: ReturnType<typeof setTimeout> | null = null;
    const run = async () => {
      try {
        const own = (await records.list()).filter(
          (r) => r.network === services.config.network && r.maker === pubkey && isStreamKind(r.kind) && !isOldTemplate(services.kob, r),
        );
        if (!own.length || !alive) return;
        const [found, clock] = await Promise.all([resolveRecords(services, own), readClock(services).catch(() => null)]);
        if (!alive) return;
        const snaps: OrderSnapshot[] = [];
        for (const rec of own) {
          const r = found.get(rec.covenantId);
          if (r?.status !== 'live') continue;
          try {
            // a day order's deadline is the receipt's: the book leaves it out once it passed
            snaps.push(snapshotFromRecord(r, [], rec.deadline ? BigInt(rec.deadline) : null));
          } catch {
            /* not spendable after all */
          }
        }
        c.adopt(snaps, clock?.daa);
      } catch {
        /* the node could not be asked: next round */
      } finally {
        if (alive) timer = setTimeout(() => void run(), RECEIPTS_EVERY_MS);
      }
    };
    void run();
    return () => {
      alive = false;
      if (timer) clearTimeout(timer);
    };
  }, [active, since, pubkey, records, services]);
  return null;
}
