// Scenario records (run/state/scenarios.jsonl): what a bot expects of an action whose result the checker can verify on its own later (the
// checker's `scenarios` step, rules in checker/scenario-rules.ts). One JSON line per action, appended by the bots, read by the checker from
// its cursor. No `@/` imports (the checker's pure tests read these types).
import { appendFileSync, mkdirSync } from 'node:fs';
import { join } from 'node:path';

/** an order of a record and the outpoint the bot's transaction spent of it (`txid:index`) */
export interface SpentOrder {
  id: string;
  spends: string;
}

/** A limit ladder: every rung its own order placed by its own transaction, all of the same size and side (ticket feature e3fe485). */
export interface LadderRecord {
  kind: 'ladder';
  ts: number;
  bot: string;
  token: string;
  side: 'buy' | 'sell';
  /** base units of every rung */
  size: string;
  levels: number;
  rungs: { id: string; price: string; txid: string }[];
  /** the ladder stopped at a rung (a refusal or a failed submit): the rungs placed before it stay */
  stoppedAt?: number;
}

/** One rung of a ladder cancelled on its own: the cancel must close that rung only. */
export interface LadderCancelRecord {
  kind: 'ladderCancel';
  ts: number;
  bot: string;
  token: string;
  /** every rung of the ladder (the cancelled one included) */
  rungs: string[];
  cancelled: SpentOrder;
  txid: string;
}

/**
 * A replacement whose terms the bot knows: a GTC renewal (`renew`, the web's renew path: same terms, expiry now + 90 days) or a pair amend
 * (`pairAmend`, the pair ticket's replace: same pair, side, amount and lifetime, a new price). `expect` is the replacement's state as built.
 */
export interface ReplaceRecord {
  kind: 'renew' | 'pairAmend';
  ts: number;
  bot: string;
  token: string;
  /** the replaced order and the outpoint the replacement spent */
  old: SpentOrder;
  /** the replaced order's expiry (a renewal must extend it) */
  oldExpiryDaa: string;
  /** the replacement's covenant id (an in-place amend keeps the old id) */
  id: string;
  inPlace: boolean;
  txid: string;
  /** state fields of the replaced order (its proven state when it was replaced) */
  before: Record<string, string>;
  /** state fields of the replacement as planned (decimal strings / hex) */
  expect: Record<string, string>;
}

/** A cancel-all (`pair`: every live pair order of one pair of the bot): every order must end, by this cancel or by a fill that won the race. */
export interface CancelAllRecord {
  kind: 'cancelAll';
  ts: number;
  bot: string;
  scope: string;
  orders: SpentOrder[];
  txids: string[];
}

/** A token merge (web "Merge tokens", a chain of transfers 3 -> 1, then 1 + 2 -> 1, ...): one output holding the inputs' total. */
export interface MergeRecord {
  kind: 'merge';
  ts: number;
  bot: string;
  token: string;
  ticker: string;
  /** x-only key of the wallet: owner of every input and of the merged output */
  maker: string;
  inputs: { txid: string; index: number; amount: string }[];
  total: string;
  links: string[];
  final: { txid: string; index: number };
}

export type ScenarioRecord = LadderRecord | LadderCancelRecord | ReplaceRecord | CancelAllRecord | MergeRecord;

export const scenariosPath = (runPath: string): string => join(runPath, 'state', 'scenarios.jsonl');

const replacer = (_k: string, v: unknown) => (typeof v === 'bigint' ? v.toString() : v);

/** appends one record (never throws: a lost record only means one check less) */
export function recordScenario(runPath: string, rec: ScenarioRecord): void {
  try {
    mkdirSync(join(runPath, 'state'), { recursive: true });
    appendFileSync(scenariosPath(runPath), JSON.stringify(rec, replacer) + '\n');
  } catch {
    /* a convenience for the checker */
  }
}
