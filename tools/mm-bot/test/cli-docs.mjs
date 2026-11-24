// The JSON documents the real kob CLI prints for the commands mm-bot parses, field for field
// (R8T-2): the stand-in CLI (fake-kob.mjs) builds its answers with these, and cli-shapes.test.mjs
// pins each against the Rust source. The values are made up; the fields are not.

const hex = (b) => b.repeat(32);
const MAKER = 'a1d9fd1347128011ad0036b077949d63274d5d3b8ac8351776c30c945ef2c2fb';

/**
 * An order state as `AnyState` serialises it (`#[serde(tag = "kind", content = "state")]`,
 * crates/kob-protocol/src/state.rs:2302): every field of `AskState` (`:655-688`) or `BidState`
 * (`:690-722`), 64-bit numbers as strings, bytes as hex. `expiryDaa` is the expiry the CLI
 * computed from --gtd: the fake gives each send its own (the sending transaction's id), so that
 * two amends to one price are two scripts, as on chain.
 */
export function orderState({ side, price, amount, minFill = null, expiryDaa = '0', token = hex('01') }) {
  const common = { maker: MAKER, tokenCovId: token, tokenTplHash: hex('7e'), tplPrefixLen: '41', tplSuffixLen: '1200', scale: '1000000',
    minFill: String(minFill ?? 1), price: String(price), tip: '0', tif: '0', activeFrom: '0', expiryDaa: String(expiryDaa), refundTip: '0' };
  if (side === 1) {
    return { kind: 'KobAsk', state: { ...common, interval: '0', maxFill: '0', slope: '0', priceEnd: '0', decayStep: '1', amountLeft: String(amount), extensionCommitment: hex('00') } };
  }
  return { kind: 'KobBid', state: { maker: common.maker, tokenCovId: common.tokenCovId, tokenTplHash: common.tokenTplHash, tplPrefixLen: common.tplPrefixLen,
    tplSuffixLen: common.tplSuffixLen, extensionCommitment: hex('00'), scale: common.scale, minFill: common.minFill, price: common.price, tip: '0', tif: '0',
    activeFrom: '0', expiryDaa: common.expiryDaa, refundTip: '0', reserve: '0', deliveryCarrier: '100000000', interval: '0', maxFill: '0', slope: '0',
    priceEnd: '0', decayStep: '1', mode: '0', recvThreshold: '0', topupTip: '0', bought: '0' } };
}

/**
 * The one --json document of a signing command (`finish`, crates/kob-cli/src/trade/flow.rs:85-93
 * and the result at :152-165): `transaction` is `show::json_of` (show.rs:219-236), `issues`
 * `issues_json` (flow.rs:51-53). `result` is null for a dry run.
 */
export function signDoc({ action, plan, txid, fee, ids = [], submitted = false, issues = [] }) {
  return {
    action, network: 'testnet-10', plan, issues,
    transaction: { txid, inputs: ['in  #0  key …'], outputs: ['out #0  …'], fee: String(fee), net: { kas: `-${fee}`, tokens: {} }, signatures: 1 },
    new_covenant_ids: ids,
    result: submitted ? { status: 'submitted', txid, fee: String(fee), covenant_ids: ids, file: null, engine_validated: true } : null,
  };
}

/** `kob place` plan (crates/kob-cli/src/trade/place.rs:1041-1044): the states, and the request (`CreateOrder`, kob-protocol build.rs:542-568). */
export function placePlan(state, value) {
  return { states: [state], request: { order: state, value: String(value), tokens: [], tokenCarrier: '100000000', funding: [], change: null, lockTime: '0', deadline: null, records: [], fee: {} } };
}

/**
 * `kob replace` plan of a plain ask or bid (crates/kob-cli/src/trade/actions.rs:746): no
 * `states`, also for a cancel-replace; only `replan_replace` (cond, if-done, pair kinds, :894)
 * adds them.
 */
export function replacePlan(order, inPlace, current, replacement, value) {
  return { order, inPlace, current, replacement, value: String(value) };
}

/** `kob balances` (crates/kob-cli/src/trade/views.rs:776-795). */
export function balancesDoc({ maker, kasFree, token, decimals = 6, free, freeUtxos }) {
  return {
    maker, kas: { free: String(kasFree), locked: '0', total: String(kasFree) },
    tokens: [{ token, ticker: 'TUSDC6', declared_ticker: null, decimals, free: String(free), free_utxos: freeUtxos, free_verified: true, in_orders: '0', bought: '0', strays: '0',
      kas_escrow: '0', kas_carriers: '0', kas_locked: '0', orders: 0, text: { name: 'TUSDC6', free: '0', in_orders: '0', bought: '0', strays: '0', kas_locked: '0' } }],
    skipped: [],
  };
}

/** One `kob recover` report entry (crates/kob-cli/src/recover.rs:849-884); `live` adds the node's fields. */
export function recoverEntry({ id, kind, tpl, status, note = null, live = null, cancel = null }) {
  const e = { covenant_id: id, kind, template: `${kind} (pinned)`, template_hash: tpl, maker: MAKER, status, note, source: 'own-orders.json', candidates: 1 };
  if (live) {
    Object.assign(e, { outpoint: `${live.txid}:0`, value: String(live.value), utxo_daa: '1000', state: 'ab', amount_left: live.amountLeft == null ? null : String(live.amountLeft),
      scale: '1000000', min_fill: String(live.minFill ?? 1), found_by: 'known state', custody: { status: 'none' } });
  }
  if (cancel) e.cancel = cancel;
  return e;
}

/** A cancel of `kob recover --cancel` (crates/kob-cli/src/recover.rs:982-991). */
export const recoverCancel = (txid, fee, submitted = true) => ({ status: submitted ? 'submitted' : 'signed', txid, fee: String(fee), funding: 'order', file: null, engine_validated: true });

/** The `kob recover` report (crates/kob-cli/src/recover.rs:888-903). */
export function recoverReport(orders, rejected = []) {
  const n = (s) => orders.filter((o) => o.status === s).length;
  return { network: 'testnet-10', node: 'ws://127.0.0.1:18210', orders, rejected,
    summary: { live: n('live'), not_found: n('not_found'), unsupported: n('unsupported'), other_maker: n('other_maker'), rejected: rejected.length } };
}

/** `kob merge` (crates/kob-cli/src/trade/merge.rs:347-356 and the links at :427-430, :440). */
export function mergeDoc({ token, before, txids, fee }) {
  return { action: 'merge TUSDC6', network: 'testnet-10', token, utxos_before: before, utxos_after: 1, transactions: txids.length, merged_amount: '0', fee: String(fee),
    links: txids.map((txid, i) => ({ link: i + 1, status: 'submitted', txid, fee: String(fee) })) };
}

/** `kob --version` (crates/kob-cli/src/main.rs:184-191). */
export const VERSION_LINE = 'kob 0.1.0 (development build, no network, commit unknown)';
