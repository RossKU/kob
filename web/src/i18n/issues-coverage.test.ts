// Every finding code a user can see must have an English sentence under `issues.<code>` with the same placeholders as the catalogue message.
// Catalogues that are exported as lists are imported; the ones that live inside function bodies (cancel.ts, issue.ts, decode.ts, the wallet and node
// error types, the registry verification helpers) are read from the SOURCE files, so a code added there without a sentence fails this test.
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { describe, expect, it } from 'vitest';
import { allKeys, has, rawEntry } from './index';
import { ISSUE_CATALOG } from '../kob/orders/common-issues';
import { COND_ISSUE_CATALOG } from '../kob/orders/cond-issues';
import { PAIR_ISSUE_CATALOG } from '../kob/orders/pair-issues';
import { SIGNING_ISSUE_CODES } from '../kob/decode';
import { REGISTRY_ISSUE_CODES, UNTRADABLE_REASONS } from '../kob/registry';

const source = (rel: string): string => readFileSync(fileURLToPath(new URL(rel, import.meta.url)), 'utf8');
const unique = (xs: Iterable<string>): string[] => [...new Set(xs)].sort();
const matches = (text: string, re: RegExp, group = 1): string[] => [...text.matchAll(re)].map((m) => m[group]);
const placeholderNames = (s: string): string[] => unique(matches(s, /\{(\w+)(?::\w+)?\}/g));

// ------------------------------------------------------------------------------------------------ code enumeration

const plannerCodes = Object.keys(ISSUE_CATALOG);
const condCodes = Object.keys(COND_ISSUE_CATALOG);
const pairCodes = Object.keys(PAIR_ISSUE_CATALOG);

const cancelSrc = source('../kob/cancel.ts');
const cancelCodes = unique(matches(cancelSrc, /\bissue\('([\w.-]+)'/g));
const snapshotCodes = unique([
  ...matches(cancelSrc, /new SnapshotError\('([\w-]+)'/g),
  ...matches(/readonly code: ([^;]+);\n\s*constructor\(code: SnapshotError/.exec(cancelSrc)?.[1] ?? '', /'([\w-]+)'/g),
]).map((c) => `snapshot.${c}`);

/** Form codes of issue.ts: every dotted code literal, plus the `${field}.invalid` family of the display-text check. */
function issueFormCodes(): string[] {
  const src = source('../kob/issue.ts');
  const literal = matches(src, /'([a-z]+(?:\.[a-z_]+)+)'/g);
  const fields = /field: '(\w+)' \| '(\w+)' \| '(\w+)'/.exec(src)?.slice(1) ?? [];
  const suffixes = matches(src, /`\$\{field\}\.([a-z_]+)`/g);
  return unique([...literal, ...fields.flatMap((f) => suffixes.map((s) => `${f}.${s}`))]);
}

const walletCodes = unique(matches(/class WalletError[\s\S]*?readonly code: ([^;]+);/.exec(source('../wallet/types.ts'))?.[1] ?? '', /'([\w-]+)'/g));
const signFlowOwnCodes = unique(matches(source('../wallet/sign.ts'), /throw new SignFlowError\('\w+', .*?, '([\w-]+)'\);/g));
const nodeCodes = unique(matches(/export type NodeErrorCode =([\s\S]*?);\n/.exec(source('../data/node-error.ts'))?.[1] ?? '', /'([\w-]+)'/g));

const decodeSrc = source('../kob/decode.ts');
const walletNoticeCodes = unique(matches(decodeSrc.slice(decodeSrc.indexOf('export function describeInputsForWallet')), /\bn\('([\w-]+)'/g));

const registrySrc = source('../kob/registry.ts');
const lookalikeLevels = unique(matches(/type LookalikeLevel = ([^;]+);/.exec(registrySrc)?.[1] ?? '', /'([\w-]+)'/g));
const verifyCodes = unique(matches(registrySrc, /problems\.push\(\{ code: '([\w-]+)'/g));

/** dictionary key of every code, by catalogue */
const catalogues: Record<string, string[]> = {
  'planner (ISSUE_CATALOG)': plannerCodes.map((c) => `issues.${c}`),
  'conditional planner (COND_ISSUE_CATALOG)': condCodes.map((c) => `issues.${c}`),
  'pair planner (PAIR_ISSUE_CATALOG)': pairCodes.map((c) => `issues.${c}`),
  'pre-sign findings (SIGNING_ISSUE_CODES)': SIGNING_ISSUE_CODES.map((c) => `issues.${c}`),
  'cancel / refund planners (cancel.ts)': cancelCodes.map((c) => `issues.${c}`),
  'snapshot errors (SnapshotError)': snapshotCodes.map((c) => `issues.${c}`),
  'issuance form (issue.ts)': issueFormCodes().map((c) => `issues.${c}`),
  'registry issues (REGISTRY_ISSUE_CODES)': REGISTRY_ISSUE_CODES.map((c) => `issues.${c}`),
  'untradable reasons (UNTRADABLE_REASONS)': UNTRADABLE_REASONS.map((r) => `issues.untradable.${r}`),
  'lookalike levels': lookalikeLevels.map((l) => `issues.lookalike.${l}`),
  'registry verification problems': verifyCodes.map((c) => `issues.verify.${c}`),
  'wallet errors (WalletError.code)': walletCodes.map((c) => `issues.wallet.${c}`),
  'sign flow errors (SignFlowError.code)': unique([...walletCodes, ...signFlowOwnCodes]).map((c) => `issues.signflow.${c}`),
  'node errors (NodeErrorCode)': nodeCodes.map((c) => `issues.node.${c}`),
  'wallet popup notices (describeInputsForWallet)': walletNoticeCodes.map((c) => `issues.walletnotice.${c}`),
};

/** keys that are not finding codes: helpers used by issue-text.ts */
const HELPER_PREFIXES = ['issues.fmt.', 'issues.where.', 'issues.term.', 'issues.dur.'];

describe('finding catalogues are found in the sources', () => {
  it('reads a plausible number of codes from every catalogue (a broken regex must not pass silently)', () => {
    const minimum: Record<string, number> = {
      'planner (ISSUE_CATALOG)': 40,
      'conditional planner (COND_ISSUE_CATALOG)': 30,
      'pair planner (PAIR_ISSUE_CATALOG)': 17,
      'pre-sign findings (SIGNING_ISSUE_CODES)': 35,
      'cancel / refund planners (cancel.ts)': 17,
      'snapshot errors (SnapshotError)': 5,
      'issuance form (issue.ts)': 33,
      'registry issues (REGISTRY_ISSUE_CODES)': 20,
      'untradable reasons (UNTRADABLE_REASONS)': 5,
      'lookalike levels': 4,
      'registry verification problems': 8,
      'wallet errors (WalletError.code)': 6,
      'sign flow errors (SignFlowError.code)': 8,
      'node errors (NodeErrorCode)': 10,
      'wallet popup notices (describeInputsForWallet)': 7,
    };
    for (const [name, keys] of Object.entries(catalogues)) expect(keys.length, name).toBeGreaterThanOrEqual(minimum[name]);
  });

  it('finds the expected named codes', () => {
    const all = new Set(Object.values(catalogues).flat());
    for (const k of [
      'issues.PRICE_NOT_ON_TICK', 'issues.COND_TP_STOP_ORDER', 'issues.input-script-mismatch', 'issues.cancel.strays-exceed-slots', 'issues.refund.not-yet',
      'issues.snapshot.not-live', 'issues.name.empty', 'issues.ticker.length', 'issues.holder.owner.invalid', 'issues.holders.sum_mismatch', 'issues.holders.consolidation',
      'issues.carrier.zero', 'issues.description.invalid', 'issues.website.invalid', 'issues.icon.invalid', 'issues.website.https', 'issues.icon.scheme',
      'issues.funds.insufficient', 'issues.maker.invalid', 'issues.signflow.signature', 'issues.signflow.validation', 'issues.node.orphan', 'issues.walletnotice.kastle-scripts',
    ]) expect(all.has(k), k).toBe(true);
  });
});

describe('every catalogue code has an English message', () => {
  for (const [name, keys] of Object.entries(catalogues)) {
    it(name, () => {
      expect(keys.filter((k) => !has(k)), 'missing').toEqual([]);
    });
  }

  it('has no stale issues.* keys that no catalogue uses', () => {
    const known = new Set(Object.values(catalogues).flat());
    const stale = allKeys().filter((k) => k.startsWith('issues.') && !known.has(k) && !HELPER_PREFIXES.some((p) => k.startsWith(p)));
    expect(stale).toEqual([]);
  });
});

describe('issues.* message quality', () => {
  const keys = allKeys().filter((k) => k.startsWith('issues.'));
  const emoji = /\p{Extended_Pictographic}/u;

  it('has entries', () => {
    expect(keys.length).toBeGreaterThan(250);
  });

  it('every value is non-empty, free of emoji and English only', () => {
    const cjk = /[\u3040-\u30ff\u4e00-\u9fff]/;
    for (const k of keys) {
      const v = rawEntry(k);
      expect(typeof v === 'string' && v.trim().length > 0, `${k} is empty`).toBe(true);
      expect(emoji.test(v!), `${k} contains an emoji`).toBe(false);
      expect(cjk.test(v!), `${k} contains CJK text`).toBe(false);
    }
  });
});

describe('placeholders agree with the catalogue messages', () => {
  const both: [string, Record<string, { message: string }>][] = [
    ['ISSUE_CATALOG', ISSUE_CATALOG],
    ['COND_ISSUE_CATALOG', COND_ISSUE_CATALOG],
    ['PAIR_ISSUE_CATALOG', PAIR_ISSUE_CATALOG],
  ];
  for (const [name, catalog] of both) {
    it(`${name}: every {param} of the English catalogue message (also {x:kas} / {x:bps}) is in the dictionary under the same name, and no other`, () => {
      for (const [code, entry] of Object.entries(catalog)) {
        const want = placeholderNames(entry.message);
        expect(placeholderNames(rawEntry(`issues.${code}`) ?? ''), code).toEqual(want);
      }
    });
  }

  it('the dictionaries use plain {name} placeholders (the format suffix belongs to issue-text.ts)', () => {
    for (const k of allKeys().filter((x) => x.startsWith('issues.'))) {
      expect(rawEntry(k), k).not.toMatch(/\{\w+:\w+\}/);
    }
  });
});
