// planPair(env, intent): EVERY order type of the KAS ticket on a token/token pair A/B (`kob/plan.ts` dispatches a PairPlanEnv here).
//
// Exported API (documented for the UI; see also plan-types.ts `PairPlanEnv`, `PairOrderPlan`, `PairDisclosure`):
//   planPair(env: PairPlanEnv, intent: Intent): PairOrderPlan & { cond: CondSummary | null }
//     * the SAME intents as the KAS ticket (intent-simple.ts, intent-cond.ts) with these units: amounts (`amount`, `minFill`, `minTouch`,
//       `sliceAmount`) base units of A; prices (`price`, `priceEnd`, `displayedPrice`, `stop`, `limit`, `takeProfit`, `entry.*`, `exit.*`,
//       `trail.step` / `trail.gap`, `prefund`) B BASE UNITS PER WHOLE A (`scale(A)` base units); `tip`, `exit.tip`, `keeperTip`, `carrier` KAS
//       (sompi; tips per whole A, prefunded on the order UTXO, never part of a B price);
//     * kind mapping: limit / IOC / FOK / market / streaming / close / TWAP / DCA / Dutch -> KobPair (pair-simple.ts); stop-market / stop-limit /
//       trailing / take-profit / OCO -> KobCondPair (pair-cond.ts); IFD / IFO / bracket / repeat -> KobIfdPair with a KobCondPair exit
//       (pair-ifd.ts; `states` lists the entry, then the exit a fill of the whole amount creates);
//     * sides: sell = sell A for B (ASK, holds A), buy = buy A with B (BID, holds a B escrow); a sell-first entry holds A and a B prefund;
//     * the create request draws each custody from the maker's UTXOs of its own token (A: env.tokenUtxos, B: env.pair.quoteTokenUtxos) and is
//       built with kob.build; the built transaction is self-checked (placement record, value, every custody at its amount of its token);
//     * `disclosure` (generic) in the pair meaning: limitPrice / allInPrice / expectedPrice / worstPrice B per whole A (allInPrice = the limit:
//       the tip is KAS), allInTotal B base units (a sell RECEIVES at least, a buy PAYS at most, kob-wasm rounded), tip KAS per whole A, carriers
//       and kasLocked KAS (lines: deliveryCarrier, exitCarrier, tipPrefund, keeperReserve, tokenCarrier / quoteTokenCarrier per custody,
//       tokenChangeCarrier / quoteTokenChangeCarrier kept), tokensEscrowed = A escrowed;
//     * `pair`: PairDisclosure (escrows of A and B, receiveMinB / payMaxB / expectedB / minFillB, tipKasTotal, deliveries, carriers, orderValue,
//       the trigger rule of a stop, the exit and repeat facts, pair note tags);
//     * `cond`: the CondSummary of the KAS conditional planners (legs, trail, keeper, entry, exit, repeat) for conditional / if-done intents.
//   Issue codes: the shared catalogue (common-issues.ts) and COND_* where the meaning and units are the same, PAIR_* (pair-issues.ts) otherwise.
//   Pair note tags (`pair.notes`): pairPricesFromKasBooks, pairTipKas, pairRoute, pairNetting, pairInventory, pairAuction, pairTrigger,
//   pairExitCustody.
import type { CondIntent } from '../intent-cond';
import { COND_TYPES } from '../intent-cond';
import type { SimpleIntent } from '../intent-simple';
import type { PairOrderPlan, PairPlanEnv } from '../plan-types';
import type { CondSummary } from './cond-common';
import { failedPairPlan } from './pair-common';
import { planPairCond } from './pair-cond';
import { pairIssue } from './pair-issues';
import { planPairSimple } from './pair-simple';

export type PairPlan = PairOrderPlan & { cond?: CondSummary | null };

/** Plans any order type on the pair of `env` (A = env.token, B = env.pair.quote). Never throws for user-input problems. */
export function planPair(env: PairPlanEnv, intent: SimpleIntent | CondIntent): PairPlan {
  if (env.token.covenantId === env.pair.quote.covenantId) return failedPairPlan([pairIssue('PAIR_SAME_TOKEN')]);
  return COND_TYPES.includes(intent.type) ? planPairCond(env, intent as CondIntent) : planPairSimple(env, intent as SimpleIntent);
}

