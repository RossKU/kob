// One-time setup (idempotent, state in run/state.json): the TUSD issuance, the asset tokens' issuances (config `token2`, `token3`) and the generated
// registry / allowlist.
import { issueLimits, issueRaw, issuedTokenUtxos, type IssueSpec } from '@/kob/issue';
import type { BuiltTx, KeyUtxo } from '@/kob/types';
import { selectFunding } from '@/kob/funding';
import { recordFee, withUrgency, type FeeEnvFields } from '@/kob/fee-policy';
import { feeContext } from './fees';
import { issuedOf, key, setIssued, type Env, type GenesisOutputs, type IssuedToken } from './env';
import { ASSET_SLOTS, type TokenConfig, type TokenSlot } from './config';
import { logger, Stats } from './log';
import { checkSoakProgram, SOAK_PROGRAM, writeRegistry } from './market';
import { KAS, kas, sleep, unitsOf } from './util';
import { spkStringToAddress } from '@/data/kaspa-sdk';
import { walletFor, type BotWallet } from './wallet';

const log = logger('setup');
const CARRIER = 10n * KAS;

export function walletOf(env: Env, name: string, stats: Stats): BotWallet {
  key(env, name);
  return walletFor(env, name, stats, log.child(name));
}

async function waitFunds(w: BotWallet, need: bigint): Promise<KeyUtxo[]> {
  for (let i = 0; ; i++) {
    const f = await w.funding();
    const have = f.reduce((s, u) => s + BigInt(u.amount), 0n);
    if (have >= need) return f;
    if (i % 6 === 0) log.info('waiting for mature funds', { wallet: w.name, have: kas(have), need: kas(need) });
    await sleep(10_000);
  }
}

/** Submits and waits until the node's UTXO set shows output `index` of the transaction (accepted). */
async function submitAndWait(env: Env, w: BotWallet, built: BuiltTx, what: string, index = 0): Promise<string> {
  const r = await w.submit(built, what);
  if (!r.ok || !r.txid) throw new Error(`${what} failed: ${r.error}`);
  for (let i = 0; i < 120; i++) {
    const out = built.tx.outputs[index];
    const a = spkStringToAddress(env.sdk, out.scriptPublicKey, env.cfg.network);
    const us = await env.node.getUtxosByAddresses([a]);
    if (us.some((u) => u.transactionId === r.txid && u.index === index)) return r.txid;
    await sleep(2000);
  }
  throw new Error(`${what}: not accepted within 4 minutes (${r.txid})`);
}

/**
 * The checks of a token config that must fail BEFORE its issuance is broadcast: the bots price every soak book in sompi per whole token, the
 * order price unit when `scale` = 10^decimals (kob-wasm `defaultScale` caps the scale at 10^9), and the tick is a positive whole sompi count.
 */
export function checkTokenConfig(t: TokenConfig): void {
  if (!Number.isInteger(t.decimals) || t.decimals < 0 || t.decimals > 9) throw new Error(`${t.ticker}: decimals must be 0..9 (the bots quote per whole token)`);
  if (!/^[1-9]\d*$/.test(t.tick)) throw new Error(`${t.ticker}: tick must be a positive whole number of sompi per whole token`);
  unitsOf(t.supply, 0);
}

/** the issuance holders of a token config: each allocation share of the supply, the rest to the bank */
function holdersOf(env: Env, t: TokenConfig, bankPk: string): { supply: bigint; holders: IssueSpec['holders'] } {
  const supply = BigInt(t.supply) * 10n ** BigInt(t.decimals);
  const holders: IssueSpec['holders'] = [];
  let given = 0n;
  for (const [name, share] of Object.entries(t.allocation)) {
    const amount = (supply * BigInt(Math.round(share * 10_000))) / 10_000n;
    holders.push({ owner: key(env, name).publicKey, ownerScheme: 0, amount: amount.toString() });
    given += amount;
  }
  holders.push({ owner: bankPk, ownerScheme: 0, amount: (supply - given).toString() });
  return { supply, holders };
}

/**
 * `setup --recover-issue <txid>`: rebuilds the state of an issuance that was broadcast but not recorded (a setup interrupted between the
 * broadcast and the state write): every holder's genesis output is looked up on the node at the address of its exact token state (the
 * issuance's fixed-supply extension commitment), which also yields the covenant id.
 */
export async function recoverIssue(env: Env, stats: Stats, slot: TokenSlot, txid: string): Promise<void> {
  const t: TokenConfig | undefined = env.cfg[slot];
  if (!t) throw new Error(`no config for ${slot}`);
  if (issuedOf(env.state, slot)) throw new Error(`${slot} is already recorded`);
  const bank = walletOf(env, env.cfg.bank.key, stats);
  const { holders } = holdersOf(env, t, bank.pk);
  const ext = issueLimits(env.kob).extensionCommitment;
  const tpl = env.kob.templates().find((x) => x.name === SOAK_PROGRAM);
  if (!tpl) throw new Error(`kob-wasm has no ${SOAK_PROGRAM}`);
  const owners = new Map(Object.entries(env.keys).map(([n, k]) => [k.publicKey, n]));
  const genesis: GenesisOutputs = {};
  let covenantId: string | null = null;
  for (const h of holders) {
    const state = { amount: h.amount, owner: h.owner, owner_scheme: 0, borrow_scheme: 0, borrow_guard: '00'.repeat(32), extension_commitment: ext };
    const addr = spkStringToAddress(env.sdk, env.kob.tokenScriptPublicKey(SOAK_PROGRAM, state as never), env.cfg.network);
    const u = (await env.node.getUtxosByAddresses([addr])).find((x) => x.transactionId === txid);
    if (!u || !u.covenantId) throw new Error(`genesis output of ${owners.get(h.owner)} not found on the node at ${addr}`);
    if (covenantId && covenantId !== u.covenantId) throw new Error('genesis outputs carry different covenant ids');
    covenantId = u.covenantId;
    const name = owners.get(h.owner)!;
    genesis[name] = { transactionId: txid, index: u.index, amount: String(h.amount), carrier: u.amount };
  }
  const issued: IssuedToken = {
    covenantId: covenantId!,
    issueTxid: txid,
    templateHash: tpl.hash,
    extensionCommitment: ext,
    decimals: t.decimals,
    tick: t.tick,
    ticker: t.ticker,
    name: t.name,
    supply: t.supply,
    description: t.description,
  };
  setIssued(env.state, slot, issued, genesis);
  env.saveState();
  log.info('issuance recovered', { slot, ticker: t.ticker, covenantId, txid, holders: Object.keys(genesis).length });
}

/** Issues the soak token of `slot` (config `token` = TUSD, `token2`, `token3`) with KOB's own issuance (kob-wasm `issue`: KOB's standard program KCC20Ref, 3 / 3, fixed supply). */
export async function issueToken(env: Env, stats: Stats, slot: TokenSlot = 'token'): Promise<void> {
  const have = issuedOf(env.state, slot);
  if (have) {
    // a token of the soak before the switch to the standard program is not tradable on this build: say so before anything runs
    checkSoakProgram(have);
    log.info('token already issued', { slot, ticker: have.ticker, covenantId: have.covenantId });
    return;
  }
  const t: TokenConfig | undefined = env.cfg[slot];
  if (!t) return;
  const description = t.description ?? 'KOB TN10 soak test token: 1 TUSD tracks 1 USD worth of KAS (Binance KASUSDT reference, off chain). No value.';
  const bank = walletOf(env, env.cfg.bank.key, stats);
  // everything that can fail on the configuration fails BEFORE the issuance is broadcast (the state is written only after it)
  checkTokenConfig(t);
  const { supply, holders } = holdersOf(env, t, bank.pk);
  const need = CARRIER * BigInt(holders.length) + KAS;
  const coins = await waitFunds(bank, need + KAS);
  const funding = selectFunding(coins, need, 5_000_000n, 60);
  const spec: IssueSpec = {
    name: t.name,
    ticker: t.ticker,
    decimals: t.decimals,
    supply: supply.toString(),
    holders,
    carrier: CARRIER.toString(),
    funding,
    network: 'testnet-10',
    description,
  };
  // an issuance is a NORMAL-urgency transaction of the fee policy (one-off, so no cap rebuild: its fee is a few hundredths of a KAS)
  const fee: FeeEnvFields = withUrgency({ fees: await feeContext(env) } as FeeEnvFields, 'normal');
  if (fee.feeRate !== undefined) spec.feeRate = fee.feeRate.toString();
  let res;
  try {
    res = issueRaw(env.kob, spec);
  } catch (e) {
    // the coins cannot pay the dearer fee: the relay floor (the estimate must never make the issuance impossible)
    if (fee.feeRate === undefined || fee.feeRate <= env.fees.policy.floor) throw e;
    spec.feeRate = env.fees.policy.floor.toString();
    res = issueRaw(env.kob, spec);
  }
  if (fee.feeRate !== undefined) recordFee(fee, res.built, BigInt(res.built.fee.feeRate));
  const txid = await submitAndWait(env, bank, res.built, `issue:${t.ticker}`);
  const issued = {
    covenantId: res.token.covenantId,
    issueTxid: txid,
    templateHash: res.token.templateHash,
    extensionCommitment: res.token.extensionCommitment,
    decimals: t.decimals,
    tick: t.tick,
    ticker: t.ticker,
    name: t.name,
    supply: t.supply,
    description,
  };
  const genesis: GenesisOutputs = {};
  const utxos = issuedTokenUtxos({ built: res.built, token: res.token });
  const owners = new Map(Object.entries(env.keys).map(([n, k]) => [k.publicKey, n]));
  for (const u of utxos) {
    const name = owners.get(u.state.owner);
    if (!name) continue;
    genesis[name] = { transactionId: txid, index: u.index, amount: u.state.amount, carrier: u.amount };
    const w = walletOf(env, name, stats);
    w.tracker.add(w.pk, { transactionId: txid, index: u.index, tokenCovId: res.token.covenantId, program: res.token.program, state: u.state, carrier: u.amount });
  }
  setIssued(env.state, slot, issued, genesis);
  env.saveState();
  log.info('token issued', { slot, ticker: t.ticker, covenantId: res.token.covenantId, txid, supply: t.supply, holders: holders.length });
}

export async function runSetup(env: Env, recover?: { slot: TokenSlot; txid: string }): Promise<void> {
  const stats = new Stats(`${env.cfg.runPath}/stats`, 'setup');
  if (recover) await recoverIssue(env, stats, recover.slot, recover.txid);
  await issueToken(env, stats, 'token');
  for (const slot of ASSET_SLOTS) if (env.cfg[slot]) await issueToken(env, stats, slot);
  const p = writeRegistry(env);
  log.info('registry written', { path: p });
  stats.flush();
}
