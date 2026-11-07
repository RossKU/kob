import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';
import { DEFAULT_REGISTRY_PATH } from '../../registry-pin.mjs';
import { SIGNER_PROTOCOL, parseAppMessage, type TokenHint } from './protocol';
import { canonicalTable, rowsOfRegistryJson, tableHash, type RegistryRow } from './table';
import { trustedHints } from './trusted-hints';
import { buildSend } from '../testing/wallet-fixtures';

const A = 'a'.repeat(64);
const B = 'b'.repeat(64);
const C = 'c'.repeat(64);
const rows = (): RegistryRow[] => [
  { covenantId: B, ticker: 'BBB', decimals: 6 },
  { covenantId: A, ticker: 'AAA', decimals: 8 },
];
const hint = (r: RegistryRow, over: Partial<TokenHint> = {}): TokenHint => ({ ...r, ...over });

describe('G7 (b): the signer frame trusts a listed token only through the pinned registry table', () => {
  it('the hash is of the canonical table: the order of the rows does not matter, every field does', () => {
    const h = tableHash(rows());
    expect(h).toMatch(/^[0-9a-f]{64}$/);
    expect(tableHash([...rows()].reverse())).toBe(h);
    expect(canonicalTable(rows())).toBe(`[["${A}","AAA",8],["${B}","BBB",6]]`);
    expect(tableHash([{ ...rows()[0]!, decimals: 7 }, rows()[1]!])).not.toBe(h);
    expect(tableHash([{ ...rows()[0]!, ticker: 'BBX' }, rows()[1]!])).not.toBe(h);
    expect(tableHash([{ ...rows()[0]!, covenantId: C }, rows()[1]!])).not.toBe(h);
    expect(tableHash([...rows(), { covenantId: C, ticker: 'CCC', decimals: 1 }])).not.toBe(h);
    expect(tableHash(rows().slice(1))).not.toBe(h);
  });

  it('the pinned hash of a build is the hash of registry/tokens.json: a tampered copy of the file is another hash', () => {
    const json = JSON.parse(readFileSync(DEFAULT_REGISTRY_PATH, 'utf8')) as { tokens: { decimals: number; ticker: string }[] };
    const pinned = tableHash(rowsOfRegistryJson(json));
    expect(json.tokens.length).toBeGreaterThan(0);
    const t = structuredClone(json);
    t.tokens[0]!.decimals += 1;
    expect(tableHash(rowsOfRegistryJson(t))).not.toBe(pinned);
    const u = structuredClone(json);
    u.tokens[0]!.ticker = 'EVIL';
    expect(tableHash(rowsOfRegistryJson(u))).not.toBe(pinned);
    // and the table the frame is asked to check against it
    const hints = rowsOfRegistryJson(json).map((r) => hint(r));
    expect(trustedHints(hints, rowsOfRegistryJson(json), pinned)).toEqual(hints);
    expect(trustedHints(hints, rowsOfRegistryJson(t), pinned).every((h) => h.source === 'assumed')).toBe(true);
  });

  it('a hint of a token in the pinned table is trusted as it is', () => {
    const pinned = tableHash(rows());
    const hints = [hint(rows()[0]!, { inverted: true }), hint(rows()[1]!)];
    expect(trustedHints(hints, rows(), pinned)).toEqual(hints);
  });

  it('a tampered table is rejected: every listed hint is shown as assumed, without its ticker', () => {
    const pinned = tableHash(rows());
    const tampered = [{ ...rows()[0]!, decimals: 2 }, rows()[1]!];
    const out = trustedHints([hint({ ...rows()[0]!, decimals: 2 }), hint(rows()[1]!, { inverted: true })], tampered, pinned);
    expect(out).toEqual([
      { covenantId: B, ticker: '', decimals: 0, source: 'assumed' },
      { covenantId: A, ticker: '', decimals: 0, source: 'assumed', inverted: true },
    ]);
  });

  it('no table, an empty table, no pin: nothing is trusted', () => {
    const pinned = tableHash(rows());
    const h = [hint(rows()[0]!)];
    for (const [table, pin] of [[undefined, pinned], [[], pinned], [rows(), undefined], [rows(), ''], [rows(), 'f'.repeat(64)]] as const) {
      expect(trustedHints(h, table as RegistryRow[] | undefined, pin)[0]!.source, JSON.stringify([table, pin])).toBe('assumed');
    }
  });

  it('with the pinned table the frame takes the table\'s ticker and decimals, never the hint\'s; a token the table does not list is assumed', () => {
    const pinned = tableHash(rows());
    const out = trustedHints(
      [hint(rows()[0]!, { ticker: 'XXX' }), hint(rows()[1]!, { decimals: 9 }), hint({ covenantId: C, ticker: 'CCC', decimals: 1 }), hint(rows()[1]!)],
      rows(),
      pinned,
    );
    expect(out).toEqual([
      { covenantId: B, ticker: 'BBB', decimals: 6 },
      { covenantId: A, ticker: 'AAA', decimals: 8 },
      { covenantId: C, ticker: '', decimals: 0, source: 'assumed' },
      { covenantId: A, ticker: 'AAA', decimals: 8 },
    ]);
  });

  it('a hint that carries its own unverified source keeps it', () => {
    const issued = hint({ covenantId: C, ticker: 'MINE', decimals: 3 }, { source: 'issued' });
    const declared = hint({ covenantId: C, ticker: 'DECL', decimals: 3 }, { source: 'declared' });
    expect(trustedHints([issued, declared], undefined, undefined)).toEqual([issued, declared]);
  });

  it('the app message carries the table, and the frame drops a malformed one', () => {
    const built = buildSend();
    const base = { p: SIGNER_PROTOCOL, type: 'sign', id: 'r1', built, tokens: [] };
    const ok = parseAppMessage({ ...base, registry: rows() });
    expect(ok && ok.type === 'sign' && ok.registry).toEqual(rows());
    const upd = parseAppMessage({ p: SIGNER_PROTOCOL, type: 'update', id: 'r1', built, tokens: [], registry: rows() });
    expect(upd && upd.type === 'update' && upd.registry).toEqual(rows());
    expect(parseAppMessage(base)).not.toBeNull();
    for (const bad of [
      [{ covenantId: A, ticker: 'AAA', decimals: 8 }, { covenantId: A, ticker: 'AAB', decimals: 8 }], // the same token twice
      [{ covenantId: A, ticker: '', decimals: 8 }],
      [{ covenantId: A, ticker: 'A\nB', decimals: 8 }],
      [{ covenantId: A, ticker: 'AAA', decimals: 19 }],
      [{ covenantId: 'zz', ticker: 'AAA', decimals: 8 }],
      [{ covenantId: A, ticker: 'AAA', decimals: 8, extra: 1 }],
      'rows',
      Array.from({ length: 4097 }, (_, i) => ({ covenantId: i.toString(16).padStart(64, '0'), ticker: 'T', decimals: 1 })),
    ]) {
      expect(parseAppMessage({ ...base, registry: bad }), JSON.stringify(bad).slice(0, 60)).toBeNull();
      expect(parseAppMessage({ p: SIGNER_PROTOCOL, type: 'update', id: 'r1', built, tokens: [], registry: bad })).toBeNull();
    }
  });
});
