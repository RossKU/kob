// The wallet's balances on the Portfolio page, read once for the page: the free balances (node, token tracker), combined with the live orders and the
// strays (balances-data.ts), and each held token's last price (the indexer's stats), so the summary strip over the tabs and the Balances tab show the
// same figures. A token without a last price has no value in KAS (null): the total then counts KAS and the priced tokens, and says so.
import { useMemo } from 'preact/hooks';
import { useServices } from '../../app/context';
import type { Hex } from '../../kob/types';
import { quoteOf } from '../../kob/units';
import { useAsync } from '../kit';
import { combineFreeBalances, loadFreeBalances } from './balances-data';
import type { TokenBalance } from './balances-model';
import type { OrdersData } from './orders-data';

export function useWalletBalances(data: OrdersData | null, pubkey: Hex | undefined, version: unknown) {
  const services = useServices();
  const free = useAsync(
    (_signal) => (pubkey && data ? loadFreeBalances(services, pubkey, services.registry.tokens, { anyToken: true }) : Promise.resolve(null)),
    [pubkey, version ?? data, !!data],
  );
  const combined = useMemo(() => {
    if (!free.data || !data || !pubkey) return null;
    const live = data.views.filter((v) => v.status === 'open' || v.status === 'partial');
    return combineFreeBalances(free.data, { maker: pubkey, liveOrders: [...live, ...data.snapshots], strays: data.strays });
  }, [free.data, data, pubkey]);
  const tokenKey = combined?.tokens.map((b) => b.token).join(',') ?? '';
  const prices = useAsync(
    async (signal) => {
      const out = new Map<Hex, { last: bigint; basis: bigint }>();
      if (!services.indexer || !combined) return out;
      await Promise.all(
        combined.tokens.map(async (b) => {
          const st = await services.indexer!.stats(b.token, { signal }).catch(() => null);
          if (st?.last && st.price_basis && BigInt(st.price_basis) > 0n) out.set(b.token, { last: BigInt(st.last), basis: BigInt(st.price_basis) });
        }),
      );
      return out;
    },
    [tokenKey, !!services.indexer],
  );
  /** A token balance's value in KAS (sompi) at its market's last price: free, in orders and strays; null without a last price. */
  const valueOf = (b: TokenBalance): bigint | null => {
    const p = prices.data?.get(b.token);
    return p ? quoteOf(b.free + b.escrowed + b.strays, p.last, p.basis, 'down') : null;
  };
  const summary = useMemo(() => {
    if (!combined) return null;
    let tokens = 0n;
    let unpriced = 0;
    for (const b of combined.tokens) {
      const v = valueOf(b);
      if (v === null) unpriced += 1;
      else tokens += v;
    }
    return { total: combined.kas.total + tokens, kasFree: combined.kas.free, kasLocked: combined.kas.locked, unpriced, pricesIn: !!prices.data };
  }, [combined, prices.data]);
  return { free, combined, prices, valueOf, summary };
}

export type WalletBalances = ReturnType<typeof useWalletBalances>;
