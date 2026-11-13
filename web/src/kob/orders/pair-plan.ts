// planPair(env, intent): every order type of the KAS ticket on a token/token pair A/B.
// `kob/plan.ts` dispatches a PairPlanEnv here. See also plan-types.ts (`PairPlanEnv`,
// `PairOrderPlan`, `PairDisclosure`).
//
// `planPair(env: PairPlanEnv, intent: Intent): PairOrderPlan & { cond: CondSummary | null }`
//
// Intents are the same as the KAS ticket's (intent-simple.ts, intent-cond.ts), with these units:
// - Amounts (`amount`, `minFill`, `minTouch`, `sliceAmount`) are base units of A.
// - Prices (`price`, `priceEnd`, `displayedPrice`, `stop`, `limit`, `takeProfit`, `entry.*`,
//   `exit.*`, `trail.step`, `trail.gap`, `prefund`) are B base units per whole A.
// - `tip`, `exit.tip`, `keeperTip` and `carrier` are KAS in sompi. Tips are per whole A, prefunded
//   on the order UTXO and never part of a B price.
//
// Kind mapping:
// - Limit, IOC, FOK, market, streaming, close, TWAP, DCA and Dutch become a KobPair
//   (pair-simple.ts).
// - Stop-market, stop-limit, trailing, take-profit and OCO become a KobCondPair (pair-cond.ts).
// - IFD, IFO, bracket and repeat become a KobIfdPair with a KobCondPair exit (pair-ifd.ts).
//   `states` lists the entry, then the exit a fill of the whole amount creates.
//
// Sides: a sell sells A for B (ASK, holds A). A buy buys A with B (BID, holds a B escrow). A
// sell-first entry holds A and a B prefund.
//
// The create request draws each custody from the maker's UTXOs of its own token (A from
// `env.tokenUtxos`, B from `env.pair.quoteTokenUtxos`) and is built with kob.build. The built
// transaction is self-checked (placement record, value, every custody at its amount of its token).
//
// The generic `disclosure` has the pair meaning:
// - `limitPrice`, `allInPrice`, `expectedPrice` and `worstPrice` are B per whole A. `allInPrice` is
//   the limit, because the tip is KAS.
// - `allInTotal` is B base units, rounded by kob-wasm: a sell receives at least, a buy pays at
//   most.
// - `tip` is KAS per whole A. Carriers and `kasLocked` are KAS. The carrier lines are
//   deliveryCarrier, exitCarrier, tipPrefund, keeperReserve, tokenCarrier and quoteTokenCarrier
//   per custody, and the kept tokenChangeCarrier and quoteTokenChangeCarrier.
// - `tokensEscrowed` is the A escrowed.
//
// `pair` is the PairDisclosure: the escrows of A and B, receiveMinB, payMaxB, expectedB, minFillB,
// tipKasTotal, deliveries, carriers, orderValue, the trigger rule of a stop, the exit and repeat
// facts, and the pair note tags.
//
// `cond` is the CondSummary of the KAS conditional planners (legs, trail, keeper, entry, exit,
// repeat) for conditional and if-done intents.
//
// Issue codes come from the shared catalogue (common-issues.ts) and COND_* where the meaning and
// units are the same, and from PAIR_* (pair-issues.ts) otherwise.
//
// Pair note tags in `pair.notes` are pairPricesFromKasBooks, pairTipKas, pairRoute, pairNetting,
// pairInventory, pairAuction, pairTrigger and pairExitCustody.
import type { CondIntent } from '../intent-cond';
import { COND_TYPES } from '../intent-cond';
import type { SimpleIntent } from '../intent-simple';
import type { PairOrderPlan, PairPlanEnv } from '../plan-types';
import type { CondSummary } from './cond-common';
import { failedPairPlan } from './pair-common';
import { planPairCond } from './pair-cond';
import { pairIssue } from './pair-issues';
import { planPairSimple } from './pair-simple';

/** A pair plan: the order plan with the optional conditional summary. */
export type PairPlan = PairOrderPlan & { cond?: CondSummary | null };

/**
 * Plans any order type on the pair of `env` (A is env.token, B is env.pair.quote).
 * Never throws for user-input problems.
 */
export function planPair(env: PairPlanEnv, intent: SimpleIntent | CondIntent): PairPlan {
  if (env.token.covenantId === env.pair.quote.covenantId) return failedPairPlan([pairIssue('PAIR_SAME_TOKEN')]);
  return COND_TYPES.includes(intent.type) ? planPairCond(env, intent as CondIntent) : planPairSimple(env, intent as SimpleIntent);
}

