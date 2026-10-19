// Pre-sign model of a TOKEN ISSUANCE (genesis) transaction and, for a creation under KCC-23, of its preparation (the first transaction: the
// wallet's coins to its own output 0 and change, the payload declaring the token details; the genesis spends that output 0). The preparation
// must pay nothing but the wallet, and its declaration must state the claimed name, ticker and decimals. The order decoder (kob/decode.ts) cannot describe a genesis: its outputs are brand-new
// covenants of a token that has no inputs yet. So this module re-derives what the transaction does from `built.tx` + kob-wasm alone and compares
// it with what the planner claims (`IssueToken`): every token output must carry exactly the script `tokenScriptPublicKey(program, state)` of the
// claimed state, be bound to the claimed covenant id, hold the claimed carrier; the amounts must add up to the supply; nothing else may leave the
// wallet except change to its own key; the fee is re-derived from inputs minus outputs. Any disagreement is a BLOCKING finding.
import { issuedTokenUtxos, spendsPreparation, type IssueToken } from '../../kob/issue';
import { decodeDeclaration } from '../../kob/kcc23';
import type { BuiltTx, Hex } from '../../kob/types';
import type { KobWasm } from '../../kob/wasm';
import { formatKas, formatTokenAmount } from '../../kob/units';
import { t, type Params } from '../../i18n';
import { declaredRateLimit, type FeePolicy } from '../../kob/fee-policy';
import type { FeeDisclosure } from './fee-disclosure';
import type { AdvancedModel, ConfirmModel, Finding, Row, Section, Translate } from './confirm-model';
import type { WalletNotice } from '../../kob/decode';

const GROUP = { group: ',' };
const kas = (v: bigint): string => `${formatKas(v, GROUP)} KAS`;
const short = (h: string): string => (h.length > 12 ? `${h.slice(0, 4)}...${h.slice(-4)}` : h);
const p2pkSpk = (pk: Hex): string => `000020${pk}ac`;
/** More than this in fees for a genesis is never legitimate (a 8-output genesis costs a few hundredths of a KAS). */
const MAX_FEE = 100_000_000n;

export interface IssuanceInput {
  kob: KobWasm;
  built: BuiltTx;
  /**
   * the creation's KCC-23 preparation (the genesis spends its output 0): its inputs are the wallet's coins, its outputs the wallet's own (the
   * genesis funding, change), its payload the declaration, which must state the claimed name, ticker and decimals. Absent: a bare genesis.
   */
  prep?: BuiltTx;
  /** the wallet signs both transactions of a creation under one approval (`signChain`); false: it asks once per transaction (said as info) */
  oneApproval?: boolean;
  token: IssueToken;
  /** the wallet's x-only public key */
  maker: Hex;
  /** the fee rate and where it came from (`feeDisclosureOf(built)`) */
  fee?: FeeDisclosure | null;
  /**
   * the wallet's fee policy: a declared fee rate above max(maxRate, floor) (the floor when the policy is off) is blocking, and with a dynamic policy the
   * 1 KAS sanity ceiling is raised to the policy's per-transaction cap (never lowered). Absent = the fixed ceiling, no rate check.
   */
  feePolicy?: Pick<FeePolicy, 'dynamic' | 'floor' | 'maxRate' | 'maxFeeSompi'>;
  tr?: Translate;
}

const row = (id: string, label: string, value: string, tone: Row['tone'] = 'normal', detail?: string): Row => ({ id, label, value, tone, ...(detail ? { detail } : {}) });

/** Verifies the genesis transaction (and the creation's preparation when given) and returns the confirmation model (same shape as the order screens). */
export function buildIssuanceModel(i: IssuanceInput, notices: WalletNotice[] = []): ConfirmModel {
  const tr = i.tr ?? t;
  const { kob, built, token } = i;
  const maker = i.maker.toLowerCase();
  const tx = built.tx;
  const findings: Finding[] = [];
  const block = (code: string, params?: Params, extra: Partial<Finding> = {}) =>
    findings.push({ code, severity: 'blocking', message: code, ...(params ? { params } : {}), text: tr(`confirm.issue.finding.${code}`, params), ...extra });
  const warn = (code: string, params?: Params) => findings.push({ code, severity: 'warning', message: code, ...(params ? { params } : {}), text: tr(`confirm.issue.finding.${code}`, params) });
  const info = (code: string, params?: Params) => findings.push({ code, severity: 'info', message: code, ...(params ? { params } : {}), text: tr(`confirm.issue.finding.${code}`, params) });

  // ---- inputs: only the wallet's own P2PK KAS (of the preparation and of the genesis, which spends the preparation's output 0)
  const ownInputs = (b: BuiltTx) => {
    let sum = 0n;
    b.tx.inputs.forEach((inp, n) => {
      const plan = b.plans[n];
      sum += BigInt(inp.utxo.amount);
      if (!plan || plan.kind !== 'p2pk' || plan.pubkey !== maker || inp.utxo.scriptPublicKey !== p2pkSpk(maker)) block('input', { input: n });
    });
    for (const s of b.sign) {
      if (s.pubkey !== maker) block('signKey', { input: s.inputIndex });
      if (s.sighashType !== 1) block('sighash', { input: s.inputIndex });
    }
    return sum;
  };
  const genesisIn = ownInputs(built);
  const prep = i.prep ?? null;
  // the wallet's coins: the preparation's inputs (the genesis's one input is the preparation's output 0, the wallet's own meanwhile)
  let kasIn = genesisIn;
  let prepFee = 0n;
  let prepChange = 0n;
  let declared: ReturnType<typeof decodeDeclaration> | null = null;
  if (prep) {
    kasIn = ownInputs(prep);
    if (!spendsPreparation(prep, built)) block('prepSpend');
    if (prep.covenants.length > 0) block('prepOutput', { output: 0 });
    prep.tx.outputs.forEach((out, n) => {
      if (out.covenant || out.scriptPublicKey !== p2pkSpk(maker)) block('prepOutput', { output: n });
      else if (n > 0) prepChange += BigInt(out.value);
    });
    const prepOut = prep.tx.outputs.reduce((a, o) => a + BigInt(o.value), 0n);
    prepFee = kasIn - prepOut;
    if (prepFee < 0n || prepFee !== BigInt(prep.fee.fee)) block('fee', { fee: kas(prepFee), declared: kas(BigInt(prep.fee.fee)) });
    // the declaration: KCC-23 with the claimed name, ticker and decimals (what the token will say about itself on chain, forever)
    try {
      declared = decodeDeclaration(kob, prep.tx.payload);
    } catch {
      declared = null;
    }
    const d = declared?.token;
    if (!d || d.name !== token.name || d.symbol !== token.ticker || d.decimals !== token.decimals) block('declaration');
  }

  // ---- token outputs: exactly what the planner claims
  const claimed = issuedTokenUtxos({ built, token });
  const tokenIdx = new Set(token.outputs.map((o) => o.index));
  let minted = 0n;
  let carriers = 0n;
  let toMaker = 0n;
  token.outputs.forEach((o, k) => {
    const out = tx.outputs[o.index];
    const st = claimed[k]!.state;
    minted += BigInt(o.amount);
    if (!out) return block('output', { output: o.index });
    carriers += BigInt(out.value);
    let spk: string | null = null;
    try {
      spk = kob.tokenScriptPublicKey(token.program, st);
    } catch {
      spk = null;
    }
    if (spk === null || spk !== out.scriptPublicKey) block('outputScript', { output: o.index });
    if (out.covenant?.covenantId !== token.covenantId) block('outputCovenant', { output: o.index });
    if (BigInt(out.value) !== BigInt(token.carrier)) block('outputCarrier', { output: o.index });
    if (o.owner === maker && o.ownerScheme === 0) toMaker += BigInt(o.amount);
  });
  if (minted !== BigInt(token.supply)) {
    const words = (v: bigint) => `${formatTokenAmount(v, token.decimals, GROUP)} ${token.ticker}`;
    block('supply', { minted: words(minted), supply: words(BigInt(token.supply)) });
  }
  if (!built.covenants.some((c) => c.covenantId === token.covenantId)) block('covenant');
  const tpl = kob.templates().find((x) => x.name === token.program);
  if (!tpl || tpl.hash !== token.templateHash) block('template');

  // ---- everything else must be change back to the wallet
  let change = prepChange;
  tx.outputs.forEach((out, n) => {
    if (tokenIdx.has(n)) return;
    if (out.covenant || out.scriptPublicKey !== p2pkSpk(maker)) block('outputOther', { output: n });
    else change += BigInt(out.value);
  });

  // ---- fee: the genesis's, plus the preparation's
  const totalOut = tx.outputs.reduce((a, o) => a + BigInt(o.value), 0n);
  const genesisFee = genesisIn - totalOut;
  const fee = genesisFee + prepFee;
  if (genesisFee < 0n || genesisFee !== BigInt(built.fee.fee)) block('fee', { fee: kas(genesisFee), declared: kas(BigInt(built.fee.fee)) });
  else if (fee > (i.feePolicy?.dynamic && i.feePolicy.maxFeeSompi > MAX_FEE ? i.feePolicy.maxFeeSompi : MAX_FEE)) block('feeHigh', { fee: kas(fee) });
  if (i.feePolicy) {
    const limit = declaredRateLimit(i.feePolicy);
    for (const b of prep ? [prep, built] : [built]) {
      let rate: bigint | null;
      try {
        rate = BigInt(b.fee.feeRate);
      } catch {
        rate = null;
      }
      if (rate === null || rate > limit) block('feeRate', { rate: b.fee.feeRate, max: limit.toString() });
    }
  }

  // ---- disclosure findings
  warn('unaudited');
  const others = token.outputs.filter((o) => !(o.owner === maker && o.ownerScheme === 0));
  if (others.length > 0) info('holdersOther', { count: others.length });
  info('walletBlind');
  if (prep && i.oneApproval === false) info('walletTwice');

  const sections: Section[] = [];
  const decimals = token.decimals;
  const supply = BigInt(token.supply);
  const amountText = (v: bigint) => `${formatTokenAmount(v, decimals, GROUP)} ${token.ticker}`;
  sections.push({
    id: 'issue',
    title: tr('confirm.issue.section'),
    note: tr('confirm.issue.sectionNote'),
    rows: [
      row('name', tr('confirm.issue.name'), token.name),
      row('ticker', tr('confirm.issue.ticker'), token.ticker),
      row('decimals', tr('confirm.issue.decimals'), String(decimals)),
      row('supply', tr('confirm.issue.supply'), amountText(supply), 'warn', tr('confirm.issue.supplyHint')),
      row('program', tr('confirm.issue.program'), token.program, 'muted', tr('confirm.issue.programHint')),
      row('covenantId', tr('confirm.issue.covenantId'), token.covenantId, 'muted'),
      row('templateHash', tr('confirm.issue.templateHash'), short(token.templateHash), 'muted'),
      row('extension', tr('confirm.issue.extension'), short(token.extensionCommitment), 'muted'),
      ...token.outputs.map((o, k) =>
        row(`holder-${k}`, tr('confirm.issue.holder', { n: k + 1 }), amountText(BigInt(o.amount)), o.owner === maker && o.ownerScheme === 0 ? 'good' : 'warn',
          o.owner === maker && o.ownerScheme === 0 ? tr('confirm.issue.holderYou') : tr('confirm.issue.holderOther', { owner: short(o.owner), scheme: o.ownerScheme })),
      ),
    ],
  });
  // the declaration (KCC-23): what the token says about itself on chain, committed to by its id
  if (prep && declared) {
    const d = declared.token;
    const opt = (id: string, v: string | null) => (v ? [row(`declare-${id}`, tr(`confirm.issue.declare.${id}`), v, 'muted')] : []);
    sections.push({
      id: 'declare',
      title: tr('confirm.issue.declareSection'),
      note: tr('confirm.issue.declareNote'),
      rows: [
        row('declare-name', tr('confirm.issue.name'), d.name ?? ''),
        row('declare-symbol', tr('confirm.issue.ticker'), d.symbol ?? ''),
        row('declare-decimals', tr('confirm.issue.decimals'), String(d.decimals ?? '')),
        ...opt('description', d.description), ...opt('logo', d.logo), ...opt('website', d.website), ...opt('x', d.x), ...opt('telegram', d.telegram),
      ],
    });
  }
  sections.push({ id: 'spend', title: tr('confirm.section.spend'), rows: [row('kas-spend', tr('confirm.spend.kas'), kas(kasIn))], fold: true });
  sections.push({
    id: 'locked', title: tr('confirm.section.locked'), note: tr('confirm.issue.lockedNote'),
    rows: [row('locked-total', tr('confirm.locked.total'), kas(carriers), 'warn'), row('locked-carriers', tr('confirm.issue.carriers', { count: token.outputs.length }), kas(BigInt(token.carrier)), 'normal', tr('confirm.locked.carriersHint'))],
  });
  if (change > 0n) sections.push({ id: 'back', title: tr('confirm.section.back'), rows: [row('kas-back', tr('confirm.back.kas'), kas(change), 'good')], fold: true });
  const feeDisc = i.fee ?? null;
  sections.push({ id: 'fee', title: tr('confirm.section.fee'), rows: [row('fee', tr('confirm.fee.network'), kas(fee), 'normal', tr('confirm.fee.hint')), ...(feeDisc?.rows ?? [])] });
  const delta = change - kasIn;
  // the net effect first: what the wallet gains and loses
  sections.unshift({
    id: 'net', title: tr('confirm.section.net'),
    rows: [
      row('net-kas', tr('confirm.net.kas'), `${delta < 0n ? '-' : '+'}${formatKas(delta < 0n ? -delta : delta, GROUP)} KAS`, 'normal', tr('confirm.net.kasHint')),
      row('net-token', tr('confirm.issue.netToken', { ticker: token.ticker }), `+${amountText(toMaker)}`, 'good', tr('confirm.issue.netTokenHint')),
      ...(carriers > 0n ? [row('net-locked', tr('confirm.issue.netLocked'), kas(carriers), 'warn', tr('confirm.locked.carriersHint'))] : []),
    ],
  });

  // the wallet's coins are the preparation's inputs; its outputs (the genesis funding, change) follow the genesis's
  const inTx = prep ?? built;
  const advanced: AdvancedModel = {
    txid: tx.id,
    ...(prep ? { prepTxid: prep.tx.id } : {}),
    signatures: built.sign.length + (prep?.sign.length ?? 0),
    fee: {
      paid: kas(fee), declared: kas(BigInt(built.fee.fee)), minimum: kas(BigInt(built.fee.minFee)), mode: built.fee.feeMode ?? 'relay',
      feeMass: built.fee.mass.feeMass, priorityMass: built.fee.mass.priorityMass ?? built.fee.mass.feeMass, storageMass: built.fee.mass.storage,
    },
    inputs: inTx.tx.inputs.map((inp, n) => ({ index: n, text: tr('confirm.adv.input', { index: n, role: inTx.roles[n] ?? '', kas: formatKas(BigInt(inp.utxo.amount), GROUP) }), willSign: inTx.sign.some((s) => s.inputIndex === n) })),
    outputs: [
      ...tx.outputs.map((out, n) => ({
        index: n,
        text: tr('confirm.adv.output', { index: n, kind: tokenIdx.has(n) ? tr('confirm.outkind.issued') : tr('confirm.outkind.kas-change'), kas: formatKas(BigInt(out.value), GROUP) }),
        flagged: findings.some((f) => f.severity === 'blocking' && f.params?.output === n),
      })),
      ...(prep ? prep.tx.outputs.slice(1).map((out, n) => ({ index: tx.outputs.length + n, text: tr('confirm.adv.output', { index: tx.outputs.length + n, kind: tr('confirm.outkind.kas-change'), kas: formatKas(BigInt(out.value), GROUP) }), flagged: false })) : []),
    ],
    payload: declared ? [declared.json] : [],
  };

  const blocking = findings.filter((f) => f.severity === 'blocking');
  return {
    kind: 'issue',
    heading: tr('confirm.kind.issue'),
    intro: tr('confirm.kind.issueIntro'),
    sections,
    blocking,
    warnings: [...findings.filter((f) => f.severity === 'warning'), ...(feeDisc?.warnings ?? [])],
    info: [...findings.filter((f) => f.severity === 'info'), ...(feeDisc?.info ?? [])],
    canSign: blocking.length === 0,
    notices,
    advanced,
  };
}
