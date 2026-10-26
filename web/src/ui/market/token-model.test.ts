import { describe, expect, it } from 'vitest';
import type { IndexerTokenView } from '../../data/indexer-types';
import { normalizeTokenView } from '../../data/indexer';
import { parseRegistry, displayName, type TokenInfo, type TokenRegistry } from '../../kob/registry';
import { loadKobNode } from '../../kob/wasm.node';
import { tokenTitle } from './TokenBadges';
import { buildTokenRows, filterTokens, hasIssuerControl, isNameless, parseCovenantId, sharedTickerIds, templateDetail, tickerOrId, tokenBadges, tokenFactIds, tokenLabel, tradableHere, tradableRows, unknownRow } from './token-model';

const kob = await loadKobNode();
const tpl = kob.templates().find((t) => t.name === 'KCC20Ref_8x8')!;

const tplJson = (review: 'reviewed' | 'pending-review', hash = tpl.hash) => ({
  id: 'kcc20-ref-8x8', family: 'kcc20', template_hash: hash, prefix_len: tpl.prefixLen, suffix_len: tpl.suffixLen, state_len: 112, max_token_inputs: 8, max_token_outputs: 8,
  escrow: { owner_scheme: 4, borrow_scheme: 0 }, review_status: review, source: { path: 'x' },
});
const tokJson = (ticker: string, id: string, verified = true) => ({
  ticker, name: `${ticker} coin`, family: 'kcc20', covenant_id: id, template_id: 'kcc20-ref-8x8', extension_commitment: 'ee'.repeat(32), extension_class: 'fixed-supply-standard',
  // lot_size / tick: legacy protocol v2 fields the registry reads and ignores; v3 orders never read them
  decimals: 8, lot_size: 100_000_000, tick: 100_000, verified,
});
const ID = (c: string) => c.repeat(64);

function registry(review: 'reviewed' | 'pending-review', tokens: Record<string, unknown>[], hash = tpl.hash): TokenRegistry {
  return parseRegistry({ schema_version: 1, network: 'testnet-10', templates: [tplJson(review, hash)], tokens }, { kob, network: 'testnet-10' });
}

/** What an OLDER executor still sends: a per-token `standing` (official / unverified / delisted), a judgement KOB no longer makes and the app ignores. */
const legacyStanding = (standing: string) => ({ standing }) as unknown as Partial<IndexerTokenView>;

const view = (ticker: string, id: string, asks = 0, bids = 0, over: Partial<IndexerTokenView> = {}): IndexerTokenView => ({
  ticker, covenant_id: id, template_hash: tpl.hash, extension_commitment: 'ee'.repeat(32), scale: 100_000_000, decimals: 8, open_asks: asks, open_bids: bids, ...over,
});

describe('tokenBadges', () => {
  it('a tradable token has no badge at all, whatever the registry says of verification', () => {
    const reg = registry('reviewed', [tokJson('GOOD', ID('a'))]);
    expect(reg.tokens[0].tradable).toBe(true);
    expect(tokenBadges(reg.tokens[0])).toEqual([]);
  });

  it('a reviewed template that differs from the one pinned in this build makes a registry token untradable with the reason', () => {
    const reg = registry('reviewed', [tokJson('NEW', ID('b'))], 'ab'.repeat(32));
    const b = tokenBadges(reg.tokens[0]);
    expect(b.map((x) => x.kind)).toEqual(['untradable']);
    expect(b[0].reason).toBe('template-mismatch');
  });

  it('unverified and verified entries are tradable and carry no badge (KOB makes no per-token judgement)', () => {
    const reg = registry('reviewed', [tokJson('UNV', ID('c'), false), tokJson('VER', ID('f'), true)]);
    expect(reg.tokens.map((t) => tokenBadges(t))).toEqual([[], []]);
  });
});

describe('labels', () => {
  it('matches the registry displayName in English, keeps the covenant id fragment and has no state word', () => {
    const reg = registry('reviewed', [tokJson('GOOD', ID('a'))]);
    const t = reg.tokens[0];
    expect(tokenLabel(t)).toBe(displayName(t));
    expect(tokenLabel(t)).toBe('GOOD (aaaa…aaaa)');
  });

  it('an unverified registry entry reads exactly like a verified one', () => {
    const reg = registry('reviewed', [tokJson('PEN', ID('e'), false), tokJson('VER', ID('f'), true)]);
    expect(reg.tokens.map((t) => displayName(t))).toEqual(['PEN (eeee…eeee)', 'VER (ffff…ffff)']);
    expect(reg.tokens.map((t) => tokenLabel(t))).toEqual(reg.tokens.map((t) => displayName(t)));
  });
});

describe('open token list: a registry entry is never stricter than an indexer-only token', () => {
  it('an unverified entry on a reviewed template is tradable and looks like any other token', () => {
    const reg = registry('reviewed', [tokJson('PEN', ID('e'), false)]);
    const [row] = buildTokenRows(reg, [view('PEN', ID('e'), 1, 1)]);
    expect(row).toMatchObject({ source: 'registry', tradable: true, reason: null });
    expect(row.badges).toEqual([]);
    expect(row.issuerControl).toBe(false);
    // the same program as an open-list token (no entry): no badge, tradable through the open path
    const unknown = unknownRow(reg, view('PEN', ID('9'), 1, 1));
    expect(unknown.badges).toEqual([]);
    expect(row.powers).toEqual(unknown.powers);
    // only the program decides: a template this build does not support for trading (pending review) blocks
    const pend = buildTokenRows(registry('pending-review', [tokJson('PEN', ID('e'), false)]), null)[0];
    expect(pend).toMatchObject({ tradable: false, reason: 'template-pending-review' });
    expect(pend.badges.map((b) => b.kind)).toEqual(['untradable']);
  });
});

describe('buildTokenRows', () => {
  const reg = registry('reviewed', [tokJson('KRON', ID('a')), tokJson('GOOD', ID('b'))]);

  it('lists registry tokens first with their indexer counts, then unknown ones (tradable only through the open-list path)', () => {
    const rows = buildTokenRows(reg, [view('GOOD', ID('b'), 3, 4), view('WIDE', ID('9'), 1, 1), view('QUIET', ID('8'), 0, 0)]);
    expect(rows.map((r) => r.ticker)).toEqual(['KRON', 'GOOD', 'WIDE', 'QUIET']);
    expect(rows[1]).toMatchObject({ source: 'registry', openAsks: 3, openBids: 4, tradable: true });
    expect(rows[0].openAsks).toBeNull(); // the indexer has not seen it
    expect(rows[2]).toMatchObject({ source: 'indexer', tradable: false, reason: 'unregistered' });
    expect(rows[2].badges).toEqual([]);
  });

  it('cross-checks the indexer view against the registry', () => {
    const rows = buildTokenRows(reg, [view('GOOD', ID('b'), 0, 0, { template_hash: 'cd'.repeat(32) })]);
    expect(rows[1].indexerProblems).toContain('template-hash-mismatch');
  });

  it('sanitizes hostile text of an unknown token', () => {
    const evil = unknownRow(reg, view('A\u202eB\u0000C', ID('6')));
    expect(evil.ticker).not.toMatch(/[\u202e\u0000]/);
  });

  it('merges pasted tokens without duplicating known ones', () => {
    const rows = buildTokenRows(reg, [], [view('NEW', ID('5')), view('GOOD', ID('b'))]);
    expect(rows.filter((r) => r.covenantId === ID('b'))).toHaveLength(1);
    expect(rows.find((r) => r.covenantId === ID('5'))?.source).toBe('pasted');
  });

  it('works without any indexer data', () => {
    expect(buildTokenRows(reg, null).map((r) => r.ticker)).toEqual(['KRON', 'GOOD']);
  });
});

describe('filterTokens / parseCovenantId', () => {
  const reg = registry('reviewed', [tokJson('KRON', ID('a')), tokJson('GOOD', ID('b'))]);
  const rows = buildTokenRows(reg, null);

  it('searches ticker, name and id, tolerating homoglyphs', () => {
    expect(filterTokens(rows, 'kr').map((r) => r.ticker)).toEqual(['KRON']);
    expect(filterTokens(rows, 'kr0n').map((r) => r.ticker)).toEqual(['KRON']);
    expect(filterTokens(rows, 'good coin').map((r) => r.ticker)).toEqual(['GOOD']);
    expect(filterTokens(rows, 'bbbb').map((r) => r.ticker)).toEqual(['GOOD']);
    expect(filterTokens(rows, '   ')).toHaveLength(2);
    expect(filterTokens(rows, 'zzz')).toEqual([]);
  });

  it('tells a token without metadata (no ticker, no decimals, not in the registry) from the others', () => {
    const named = unknownRow(reg, view('PEN', ID('9')));
    const nameless = unknownRow(reg, view('', ID('8'), 0, 0, { decimals: null }));
    const noDecimals = unknownRow(reg, view('NODEC', ID('7'), 0, 0, { decimals: null }));
    expect([isNameless(rows[0]), isNameless(named), isNameless(nameless), isNameless(noDecimals)]).toEqual([false, false, true, false]);
  });

  it('accepts a pasted covenant id in common spellings only', () => {
    expect(parseCovenantId(`  0x${ID('A')}\n`)).toBe(ID('a'));
    expect(parseCovenantId(ID('a').slice(1))).toBeNull();
    expect(parseCovenantId(ID('g'))).toBeNull();
    expect(parseCovenantId('')).toBeNull();
  });
});

describe('templateDetail', () => {
  it('shows registry hash next to the pinned hash and the verdict', () => {
    const reg = registry('reviewed', [tokJson('GOOD', ID('a'))]);
    const d = templateDetail(reg.tokens[0], reg, kob.templates());
    expect(d).toMatchObject({ registryHash: tpl.hash, pinnedHash: tpl.hash, matches: true, reviewed: true, program: 'KCC20Ref_8x8', problems: [] });
    expect(d.slots).toEqual({ inputs: 8, outputs: 8 });
  });

  it('reports a mismatch with the failing checks', () => {
    const reg = registry('reviewed', [tokJson('BAD', ID('a'))], 'ab'.repeat(32));
    const d = templateDetail(reg.tokens[0], reg, kob.templates());
    expect(d.matches).toBe(false);
    expect(d.registryHash).toBe('ab'.repeat(32));
    expect(d.problems).toContain('hash-mismatch');
    expect(d.reviewed).toBe(true);
  });
});

describe('indexer-only tokens of a program off the strict list', () => {
  it('are not listed (template_id null); a token with no template_id field is still shown', () => {
    const reg = registry('reviewed', [tokJson('GOOD', ID('a'))]);
    const rows = buildTokenRows(reg, [
      view('', ID('1'), 0, 0, { template_id: null, decimals: null, scale: null }),
      view('OLD', ID('2'), 0, 0, { template_id: undefined }),
      view('', ID('3'), 2, 1, { template_id: 'kcc20-ref-8x8' }),
    ]);
    expect(rows.map((r) => r.covenantId)).toEqual([ID('a'), ID('3'), ID('2')]);
  });
});

describe('tokens this build cannot trade are not offered', () => {
  const pinned = kob.templates();
  it('drops registry tokens off the pinned template or unreviewed; keeps the tradable ones', () => {
    const ok = registry('reviewed', [tokJson('GOOD', ID('a')), tokJson('UNV', ID('d'), false)]);
    expect(tradableRows(buildTokenRows(ok, null), pinned).map((r) => r.ticker)).toEqual(['GOOD', 'UNV']);
    // an older executor's `delisted` standing drops nothing: trading depends only on the program
    expect(tradableRows(buildTokenRows(ok, [view('GOOD', ID('a'), 0, 0, legacyStanding('delisted'))]), pinned).map((r) => r.ticker)).toEqual(['GOOD', 'UNV']);
    const bad = registry('reviewed', [tokJson('NEW', ID('b'))], 'ab'.repeat(32));
    expect(tradableRows(buildTokenRows(bad, null), pinned)).toEqual([]);
    const pend = registry('pending-review', [tokJson('PEN', ID('e'), false)]);
    expect(tradableRows(buildTokenRows(pend, null), pinned)).toEqual([]);
  });

  it('indexer-only tokens: kept unless of a template hash this build does not embed', () => {
    const reg = registry('reviewed', [tokJson('GOOD', ID('a'))]);
    const rows = buildTokenRows(reg, [
      view('', ID('1'), 1, 1),
      view('', ID('2'), 1, 1, { template_hash: 'cd'.repeat(32) }),
      view('', ID('3'), 1, 1, legacyStanding('delisted')),
      view('', ID('4'), 1, 1, { template_hash: undefined }),
    ]);
    expect(tradableRows(rows, pinned).map((r) => r.covenantId)).toEqual([ID('a'), ID('1'), ID('3'), ID('4')]);
    expect(tradableHere(rows.find((r) => r.covenantId === ID('2'))!, pinned)).toBe(false);
  });
});

describe('powers and empty tickers (GET /v1/tokens)', () => {
  const reg = registry('reviewed', [tokJson('GOOD', ID('a'))]);

  it("an older executor's standing (official, unverified, delisted) changes nothing about a registry token", () => {
    const [plain] = buildTokenRows(reg, [view('GOOD', ID('a'))]);
    expect(plain.badges).toEqual([]);
    for (const standing of ['official', 'unverified', 'delisted']) {
      const [row] = buildTokenRows(reg, [normalizeTokenView(view('GOOD', ID('a'), 0, 0, legacyStanding(standing)))]);
      expect(row, standing).toMatchObject({ tradable: true, reason: null });
      expect(row.badges, standing).toEqual([]);
      expect(tokenTitle(row), standing).toBe('GOOD (aaaa…aaaa)');
    }
  });

  it('a token outside the registry has no badge whatever the indexer claims; never tradable from the list', () => {
    const rows = buildTokenRows(reg, [
      view('AAA', ID('1')),
      view('BBB', ID('2'), 0, 0, legacyStanding('official')),
      view('CCC', ID('3'), 0, 0, legacyStanding('delisted')),
      view('DDD', ID('4'), 0, 0, legacyStanding('unverified')),
    ]).slice(1);
    const by = Object.fromEntries(rows.map((r) => [r.ticker, r]));
    expect(by.AAA.badges).toEqual([]);
    expect(by.BBB.badges).toEqual([]);
    expect(by.BBB.indexerProblems).toEqual([]);
    expect(tokenLabel(by.BBB)).toBe('BBB (22222222…22222222)');
    expect(by.CCC.badges).toEqual([]);
    expect(by.CCC.reason).toBe('unregistered');
    expect(by.DDD.badges).toEqual([]);
    expect(rows.every((r) => !r.tradable)).toBe(true);
  });

  it('an empty ticker shows the short covenant id instead, everywhere a label is built, and is searchable by it', () => {
    const rows = buildTokenRows(reg, [view('', ID('e'))]);
    const r = rows[1];
    expect(r.ticker).toBe('');
    expect(tokenLabel(r)).toBe('eeeeeeee…eeeeeeee');
    expect(tickerOrId(r.ticker, r.covenantId)).toBe('eeeeeeee…eeeeeeee');
    expect(tickerOrId('GOOD', ID('a'))).toBe('GOOD');
    expect(filterTokens(rows, 'eeee').map((x) => x.covenantId)).toEqual([ID('e')]);
  });

  it('two tokens with the same ticker stay two rows, told apart by covenant id and template hash', () => {
    const rows = buildTokenRows(reg, [view('SAME', ID('5')), view('SAME', ID('6'), 0, 0, { template_hash: 'ff'.repeat(32) })]).slice(1);
    expect(rows.map((r) => tokenLabel(r))).toEqual(['SAME (55555555…55555555)', 'SAME (66666666…66666666)']);
    expect(sharedTickerIds(rows)).toEqual(new Set([ID('5'), ID('6')]));
    expect(rows.map((r) => r.index?.template_hash)).toEqual([tpl.hash, 'ff'.repeat(32)]);
  });

  it('powers: freeze or seize raise the issuer-control warning, other powers do not', () => {
    expect(hasIssuerControl(['freeze'])).toBe(true);
    expect(hasIssuerControl(['burn', 'seize'])).toBe(true);
    expect(hasIssuerControl(['mint-authority', 'public-mint', 'burn', 'blacklist'])).toBe(false);
    expect(hasIssuerControl([])).toBe(false);
    expect(hasIssuerControl(undefined)).toBe(false);
    const rows = buildTokenRows(reg, [view('GOOD', ID('a'), 0, 0, { powers: ['freeze'] }), view('OTH', ID('9'), 0, 0, { powers: ['burn'], template_id: 'kcc20-ref-8x8' })]);
    expect(rows[0]).toMatchObject({ powers: ['freeze'], issuerControl: true }); // indexer-reported powers are added
    expect(rows[1]).toMatchObject({ powers: ['burn'], issuerControl: false, templateId: 'kcc20-ref-8x8' });
    expect(buildTokenRows(reg, [view('GOOD', ID('a'))])[0]).toMatchObject({ powers: [], issuerControl: false });
  });

});

describe('token facts (genesis warnings and program powers)', () => {
  const reg = registry('reviewed', [tokJson('GOOD', ID('a'))]);
  const withJson = (extra: Record<string, unknown>): TokenRegistry => registry('reviewed', [{ ...tokJson('GOOD', ID('a')), ...extra }]);
  const GENESIS = { txid: 'ab'.repeat(32), daa_score: 10, outputs: [1, 2], supply: 1000, minter_outputs: [], checked_at_daa: 20, source: 'test' };

  it('the facts are the genesis warnings and the powers that let someone create more or block addresses, in a fixed order', () => {
    expect(tokenFactIds(['live_mint_authority', 'genesis_unverified', 'other'], ['blacklist', 'burn', 'public-mint', 'freeze'])).toEqual(['genesis_unverified', 'live_mint_authority', 'public-mint', 'blacklist']);
    expect(tokenFactIds([], [])).toEqual([]);
  });

  it('a registry token without a verified genesis is unverified, whatever the indexer says; a verified one has no fact unless the indexer adds one', () => {
    expect(buildTokenRows(reg, null)[0]!.facts).toEqual(['genesis_unverified']);
    const verified = withJson({ genesis_verified: true, genesis: { ...GENESIS, live_minters: [] } });
    expect(buildTokenRows(verified, null)[0]!.facts).toEqual([]);
    expect(buildTokenRows(verified, [normalizeTokenView(view('GOOD', ID('a'), 0, 0, { warnings: ['live_mint_authority'] }))])[0]!.facts).toEqual(['live_mint_authority']);
    const live = withJson({ genesis_verified: true, genesis: { ...GENESIS, live_minters: [`${'cd'.repeat(32)}:3`] } });
    expect(buildTokenRows(live, null)[0]!.facts).toEqual(['live_mint_authority']);
  });

  it('a token outside the registry is unverified; its indexer warnings and powers join, unknown words are not facts', () => {
    const row = buildTokenRows(reg, [normalizeTokenView(view('NEW', ID('7'), 0, 0, { warnings: ['genesis_unverified', 'live_mint_authority', 'official'], powers: ['mint-authority', 'burn'] }))])[1]!;
    expect(row.facts).toEqual(['genesis_unverified', 'live_mint_authority', 'mint-authority']);
    expect(unknownRow(reg, view('NEW', ID('7')), 'pasted').facts).toEqual(['genesis_unverified']);
  });

  it('the client drops warnings that are not short words', () => {
    const v = normalizeTokenView(view('X', ID('7'), 0, 0, { warnings: ['genesis_unverified', 'Bad Word', 5 as unknown as string, '<script>'] }));
    expect(v.warnings).toEqual(['genesis_unverified']);
    expect(normalizeTokenView(view('X', ID('7'), 0, 0, { warnings: 'x' as unknown as string[] })).warnings).toEqual([]);
  });
});

describe('the registry always wins over the indexer for issuer powers', () => {
  const reg = registry('reviewed', [tokJson('GOOD', ID('a'))]);
  const REAL = 'aa'.repeat(32);
  const withCaps = (caps: string[]) => {
    const r = registry('reviewed', [tokJson('REAL', REAL)]);
    const info = { ...r.tokens[0], capabilities: caps } as unknown as TokenInfo;
    return { ...r, tokens: [info], byCovenantId: new Map([[REAL, info]]) } as TokenRegistry;
  };

  it('an indexer that reports no powers cannot remove the registry freeze / seize warning', () => {
    const reg2 = withCaps(['freeze', 'seize']);
    expect(buildTokenRows(reg2, null)[0]).toMatchObject({ powers: ['freeze', 'seize'], issuerControl: true });
    const lying = buildTokenRows(reg2, [normalizeTokenView(view('REAL', REAL, 0, 0, { powers: [] }))])[0];
    expect(lying.powers).toEqual(['freeze', 'seize']);
    expect(lying.issuerControl).toBe(true);
    const more = buildTokenRows(reg2, [normalizeTokenView(view('REAL', REAL, 0, 0, { powers: ['burn', 'freeze'] }))])[0];
    expect(more.powers).toEqual(['freeze', 'seize', 'burn']);
  });

  it('an unregistered token with a registry-known template gets the template capabilities; the indexer only adds', () => {
    const reg2 = { ...reg, templates: reg.templates.map((t) => ({ ...t, capabilities: ['freeze'] })) } as TokenRegistry;
    const row = buildTokenRows(reg2, [view('NEW', ID('7'), 0, 0, { template_id: reg.templates[0].id, powers: [] })])[1];
    expect(row.powers).toEqual(['freeze']);
    expect(row.issuerControl).toBe(true);
  });

  it('an unregistered token is shown like any other whatever ticker (or legacy standing) the indexer chooses: no badge, never tradable from the list', () => {
    const scam = normalizeTokenView(view('KASPER', 'ee'.repeat(32), 0, 0, legacyStanding('official')));
    const row = buildTokenRows(reg, [scam]).find((r) => r.covenantId === scam.covenant_id)!;
    expect(row.info).toBeNull();
    expect(row.badges).toEqual([]);
    expect(tokenLabel(row)).toBe('KASPER (eeeeeeee…eeeeeeee)');
    expect(row.tradable).toBe(false);
  });
});
