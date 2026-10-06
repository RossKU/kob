// `soak.mjs cancel <covenantId...>`: cancels orders with their makers' keys through exactly the web app's path (the order's full view ->
// `snapshotFromOrderView` -> `planCancel` -> sign -> validate -> submit). Used to confirm that an order the UI could not cancel before
// (a booked if-done exit without an extension commitment in its view) is cancellable now.
import type { Env } from '../env';
import { logger, Stats } from '../log';
import { walletFor } from '../wallet';
import { cancel } from './common';

const log = logger('cancel');

export async function cancelOrders(env: Env, ids: string[]): Promise<boolean> {
  const stats = new Stats(`${env.cfg.runPath}/stats`, 'cancel-cli');
  const byPk = new Map(Object.entries(env.keys).map(([n, k]) => [k.publicKey.toLowerCase(), n]));
  let all = true;
  for (const id of ids) {
    const v = await env.indexer.order(id.toLowerCase());
    if (!v) {
      log.warn('no such order', { id });
      all = false;
      continue;
    }
    const name = v.maker ? byPk.get(v.maker.toLowerCase()) : undefined;
    if (!name) {
      log.warn('maker is not a soak key', { id, maker: v.maker });
      all = false;
      continue;
    }
    const w = walletFor(env, name, stats);
    const r = await cancel(env, w, v, `cli_cancel:${v.contract}`);
    log.info(r.ok ? 'cancelled' : 'cancel failed', { id, contract: v.contract, status: v.status, parent: v.parent, maker: name, txid: r.txid, error: r.error });
    if (!r.ok) all = false;
  }
  stats.flush();
  return all;
}
